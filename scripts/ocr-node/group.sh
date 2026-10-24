#!/usr/bin/env bash
# Create one of an environment's on-prem Entra security groups and save its
# object id, so the next provision grants the group its roles.
#
#   scripts/ocr-node/group.sh --env prod [--kind ocr-nodes|onprem] [--dry-run]
#
# --kind ocr-nodes (the default, #296, ADR-0015): usnm-ocr-nodes-grp-<env>,
# saved in USNM_OCR_NODE_GROUP. Its members are the OCR nodes' Arc machine
# identities and get the node roles on the exchange account, so only OCR
# nodes belong in it.
# onboard.sh adds each node, offboard.sh removes it.
# --kind onprem: usnm-onprem-grp-<env>, saved in USNM_ONPREM_GROUP. Its members
# are every on-prem host's Arc machine identity (the OCR nodes, the scan-fetch
# container, the hypervisors and any later host) and get what any host needs
# for local work: the hand-off writer role on boinc-handoff. Add a host with
#   az ad group member add --group <id> --member-id <Arc principal id>
#
# Each is a plain security group (not mail-enabled, not role-assignable).
# Deployment stacks can't hold Microsoft Graph resources, so the Bicep
# doesn't create it. Run once per environment. Run again, it uses the group
# whose id is saved, updates its displayName/mailNickname in place and
# makes you an owner if you aren't one (owners can
# add and remove members). It never adopts a group by name: display names
# aren't unique, and an existing group could hold other principals, which
# the provision would then grant the node roles. It never deletes a group.
#
# --dry-run checks your sign-in and prints the plan; it creates nothing and
# changes no setting.
#
# Needs az signed in to the environment's tenant with the right to create
# security groups (the default user setting, or Groups Administrator).
# Works with macOS's bash 3.2.
set -euo pipefail
usage() { sed -n '6s/^# \{0,1\}//p' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }

ENV_NAME="" DRY_RUN=false KIND=ocr-nodes
while [ $# -gt 0 ]; do
  case "$1" in
    --env) ENV_NAME=${2:-}; shift 2 ;;
    --kind) KIND=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] || { echo "error: missing --env" >&2; usage; }
case "$KIND" in
  ocr-nodes) KEY=USNM_OCR_NODE_GROUP ;;
  onprem) KEY=USNM_ONPREM_GROUP ;;
  *) echo "error: --kind is ocr-nodes or onprem" >&2; usage ;;
esac

ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
# shellcheck source=scripts/lib/env.sh
. scripts/lib/env.sh
# shellcheck source=scripts/lib/names.sh
. scripts/lib/names.sh
[ -s "$ENV_FILE" ] || die "no settings for $ENV_NAME; run scripts/azure/bootstrap.sh --env $ENV_NAME first"

TENANT=$(aget AZURE_TENANT_ID)
SAVED=$(aget "$KEY")
[ -n "$TENANT" ] || die "$ENV_FILE has no AZURE_TENANT_ID; run scripts/azure/provision.sh --env $ENV_NAME"
az account show -o none 2> /dev/null || die "run: az login --tenant $TENANT"
# az ad works in the current account's tenant.
current_tenant=$(az account show --query tenantId -o tsv 2> /dev/null || true)
[ "$current_tenant" = "$TENANT" ] ||
  die "az's current account is in tenant ${current_tenant:-unknown}, not $TENANT: az login --tenant $TENANT"

if [ "$KIND" = onprem ]; then
  GROUP_NAME=$(onprem_group_name "$ENV_NAME")
  LEGACY_NAME=$GROUP_NAME
  DESCRIPTION="usnewsmap $ENV_NAME on-prem hosts' Arc identities; roles for local work come from the provision"
else
  GROUP_NAME=$(ocr_group_name "$ENV_NAME")
  LEGACY_NAME=$(legacy_ocr_group_name "$ENV_NAME")
  DESCRIPTION="usnewsmap $ENV_NAME on-prem OCR nodes' Arc identities (ADR-0015); roles on the exchange account come from the provision"
fi
[ "$DRY_RUN" = false ] || echo "dry run: nothing is changed"

# The saved id is the group's identity. Without one, a group is created,
# and only if none has the name yet: one that does was made by someone or
# something else, or by a run that stopped before saving its id, and a
# person checks its members and saves its id by hand.
GROUP_ID=""
if [ -n "$SAVED" ]; then
  name=$(az ad group show --group "$SAVED" --query displayName -o tsv 2> /dev/null) ||
    die "can't read group $SAVED ($KEY); if it was deleted, clear the setting and run this again"
  GROUP_ID=$SAVED
  nickname=$(az ad group show --group "$SAVED" --query mailNickname -o tsv) ||
    die "can't read group $SAVED's mailNickname"
  if [ "$name" != "$GROUP_NAME" ] || [ "$nickname" != "$GROUP_NAME" ]; then
    if [ "$DRY_RUN" = true ]; then
      echo "would rename displayName and mailNickname of $GROUP_ID to $GROUP_NAME (same id and members)"
    else
      az rest --method patch --url "https://graph.microsoft.com/v1.0/groups/$GROUP_ID" \
        --body "{\"displayName\":\"$GROUP_NAME\",\"mailNickname\":\"$GROUP_NAME\"}" -o none ||
        die "can't update group $GROUP_ID's names; no group or membership was replaced"
      echo "renamed group $GROUP_ID to $GROUP_NAME (same id and members)"
    fi
  fi
  members=$(az ad group member list --group "$GROUP_ID" --query 'length(@)' -o tsv)
  echo "$KEY is $GROUP_ID ($name, $members members)"
  # Owners can add members, and every member gets the group's roles.
  echo "owners:"
  owners=$(az ad group owner list --group "$GROUP_ID" --query '[].[id, userPrincipalName || displayName]' -o tsv) ||
    die "can't list the owners of $GROUP_ID"
  printf '%s\n' "$owners" | while IFS="$(printf '\t')" read -r oid oname; do [ -z "$oid" ] || echo "  $oid $oname"; done
else
  legacy_name=$LEGACY_NAME
  ids=$(az ad group list --filter "displayName eq '$GROUP_NAME' or displayName eq '$legacy_name'" --query '[].id' -o tsv)
  [ -z "$ids" ] ||
    die "a group named $GROUP_NAME or $legacy_name already exists ($(echo "$ids" | tr '\n' ' ')) and $KEY is empty. Check its members and its owners, who can add members (az ad group member list --group <id>; az ad group owner list --group <id>); if it's this environment's, scripts/azure/settings.sh --env $ENV_NAME --key $KEY --value <id>, otherwise delete or rename it"
  if [ "$DRY_RUN" = true ]; then
    echo "would create security group $GROUP_NAME and set $KEY to its id"
  else
    GROUP_ID=$(az ad group create --display-name "$GROUP_NAME" --mail-nickname "$GROUP_NAME" \
      --description "$DESCRIPTION" \
      --query id -o tsv) || die "can't create $GROUP_NAME (you need the right to create security groups)"
    echo "created $GROUP_NAME: $GROUP_ID"
    # Saved straight away, so a run that stops after this picks the group up:
    # in the settings store once there is one, else the local file (#295).
    aset "$KEY" "$GROUP_ID"
    echo "set $KEY to $GROUP_ID"
  fi
fi

me=$(az ad signed-in-user show --query id -o tsv 2> /dev/null || true)
if [ -z "$me" ]; then
  echo "warning: not signed in as a user, so not checking group ownership" >&2
elif [ -z "$GROUP_ID" ]; then
  echo "would make you ($me) an owner of $GROUP_NAME"
elif ! own=$(az ad group owner list --group "$GROUP_ID" --query "[?id=='$me'].id" -o tsv); then
  die "can't list the owners of $GROUP_ID"
elif [ -n "$own" ]; then
  echo "you ($me) own $GROUP_NAME"
elif [ "$DRY_RUN" = true ]; then
  echo "would make you ($me) an owner of $GROUP_NAME"
else
  az ad group owner add --group "$GROUP_ID" --owner-object-id "$me" -o none ||
    die "can't add you as an owner of $GROUP_NAME"
  echo "made you ($me) an owner of $GROUP_NAME"
fi

echo "Grant the group its roles by provisioning (needs Owner on the subscription):"
echo "  scripts/azure/provision.sh --env $ENV_NAME"
