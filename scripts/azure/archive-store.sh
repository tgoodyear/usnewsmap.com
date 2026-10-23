#!/usr/bin/env bash
# The archival storage account (infra/archive/, docs/operations.md,
# "Archival storage"): its own deployment stack, usnm-archive, outside every
# environment's, so no provision or teardown of an environment deletes it.
#
#   scripts/azure/archive-store.sh what-if [--subscription ID] [--location REGION]
#   scripts/azure/archive-store.sh deploy [--subscription ID] [--location REGION]
#   scripts/azure/archive-store.sh unlock [--subscription ID]   # lift the delete lock
#
# `what-if` uses GA stack-aware previews for both stacks, including unmanage and
# deny-setting changes, and creates no managed resources. `deploy` first creates
# or updates the guard-rail stack, usnm-guardrails, whose
# policy definitions the account's group assigns (as an environment's
# provision does), then the account (putting the delete lock back), and
# prints the account's resource id, for an environment's USNM_ARCHIVE_ACCOUNT.
# Resources dropped from the template are detached, never deleted, and the
# stack denies deletes outside it except for the lock, private endpoint
# connections and role definitions: an environment's stack declares its
# writer role in the account's group, and the group's deny assignment would
# keep that stack from deleting it when the environment detaches. A delete
# lock on the account can block removing an environment's private endpoint
# to it, so before an environment drops USNM_ARCHIVE_ACCOUNT (or is torn
# down): `unlock`, provision the environment without the setting, then
# `deploy` again.
#
# Needs az 2.61+, signed in with Owner on the subscription.
set -euo pipefail
usage() { sed -n '2,24s/^# \{0,1\}//p' "$0" >&2; exit 2; }
ACTION="" SUBSCRIPTION="" LOCATION=eastus2
while [ $# -gt 0 ]; do
  case "$1" in
    what-if|deploy|unlock) [ -z "$ACTION" ] || usage; ACTION=$1; shift ;;
    --subscription) [ $# -ge 2 ] || usage; SUBSCRIPTION=$2; shift 2 ;;
    --location) [ $# -ge 2 ] || usage; LOCATION=$2; shift 2 ;;
    -h|--help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$ACTION" ] || usage
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/../.."
# shellcheck source=scripts/lib/guardrails.sh
. scripts/lib/guardrails.sh
STACK=usnm-archive
GROUP=rg-usnm-archive
LOCK=usnm-archive-keep
command -v jq > /dev/null || die "jq is needed"
az account show -o none 2> /dev/null || die "run: az login [--tenant TENANT]"
[ -z "$SUBSCRIPTION" ] || az account set --subscription "$SUBSCRIPTION"
echo "subscription $(az account show --query '[name, id]' -o tsv | paste -sd' ' -)"
# A stack's location is fixed once created. Only "not found" means a first
# deployment; any other error stops here rather than fall back to LOCATION.
keep_stack_location() {
  local location
  if location=$(az stack sub show -n "$STACK" --query location -o tsv 2>&1); then
    LOCATION=$location
  else
    grep -qiE 'NotFound|could not be found' <<< "$location" ||
      die "can't check $STACK: $location"
  fi
}

case "$ACTION" in
  what-if)
    keep_stack_location
    az stack-whatif sub create --help > /dev/null ||
      die "this Azure CLI lacks GA stack-whatif; update Azure CLI before previewing"
    subscription_id=$(az account show --query id -o tsv) || die "can't read the subscription id"
    guardrails_params=""
    if guardrails_params=$(az stack sub show -n "$GUARDRAILS_STACK" \
      --query "[keys(parameters || \`{}\`), values(parameters || \`{}\`)[].value]" -o tsv 2>&1); then
      guardrails_location=$(az stack sub show -n "$GUARDRAILS_STACK" --query location -o tsv) ||
        die "can't read the location of $GUARDRAILS_STACK"
      guardrails_emails=$(stack_param "$guardrails_params" devBudgetEmails)
      if [ -n "$guardrails_emails" ]; then
        guardrails_start=$(stack_param "$guardrails_params" devBudgetStart)
      else
        guardrails_start=""
      fi
      guardrails_dev_location=$(stack_param "$guardrails_params" devBudgetLocation)
    else
      grep -qiE 'NotFound|could not be found' <<< "$guardrails_params" ||
        die "can't check $GUARDRAILS_STACK: $guardrails_params"
      guardrails_location=$LOCATION
      guardrails_emails=""
      guardrails_start=""
      guardrails_dev_location=eastus2
    fi
    [ -n "$guardrails_start" ] || guardrails_start=$(date -u +%Y-%m-01)
    [ -n "$guardrails_dev_location" ] || guardrails_dev_location=eastus2
    stamp=$(date -u +%Y%m%dT%H%M%SZ)-$$
    guardrails_preview="$GUARDRAILS_STACK-$stamp"
    guardrails_id="/subscriptions/$subscription_id/providers/Microsoft.Resources/deploymentStacks/$GUARDRAILS_STACK"
    echo "guard-rail definitions ($GUARDRAILS_STACK):"
    az stack-whatif sub create --subscription "$subscription_id" --name "$guardrails_preview" \
      --stack-id "$guardrails_id" --location "$guardrails_location" \
      --template-file infra/guardrails.bicep \
      --parameters devBudgetEmails="$guardrails_emails" devBudgetStart="$guardrails_start" \
        devBudgetLocation="$guardrails_dev_location" \
      --action-on-unmanage deleteResources --deny-settings-mode denyDelete \
      --description "usnewsmap guard-rail policy definitions, shared by every environment in the subscription and the archival account" \
      --retention-interval PT3H --no-color || die "the stack-aware what-if of $GUARDRAILS_STACK failed"
    echo "archival account ($STACK):"
    archive_preview="$STACK-$stamp"
    archive_id="/subscriptions/$subscription_id/providers/Microsoft.Resources/deploymentStacks/$STACK"
    az stack-whatif sub create --subscription "$subscription_id" --name "$archive_preview" \
      --stack-id "$archive_id" --location "$LOCATION" \
      --template-file infra/archive/main.bicep --parameters location="$LOCATION" \
      --action-on-unmanage detachAll --deny-settings-mode denyDelete \
      --deny-settings-excluded-actions "Microsoft.Authorization/locks/delete Microsoft.Authorization/roleDefinitions/delete Microsoft.Storage/storageAccounts/privateEndpointConnections/delete Microsoft.Storage/storageAccounts/privateEndpointConnectionProxies/delete" \
      --description "usnewsmap archival storage, shared by every environment (scripts/azure/archive-store.sh)" \
      --retention-interval PT3H --no-color || die "the stack-aware what-if of $STACK failed"
    echo "guard-rail preview retained for up to three hours: /subscriptions/$subscription_id/providers/Microsoft.Resources/deploymentStacksWhatIfResults/$guardrails_preview"
    echo "archive preview retained for up to three hours: /subscriptions/$subscription_id/providers/Microsoft.Resources/deploymentStacksWhatIfResults/$archive_preview"
    ;;
  deploy)
    need_stack_az
    keep_stack_location
    # The account's group assigns the guard-rail definitions.
    deploy_guardrails "$LOCATION" || die "can't deploy $GUARDRAILS_STACK"
    az stack sub create --name "$STACK" --location "$LOCATION" \
      --template-file infra/archive/main.bicep \
      --parameters location="$LOCATION" \
      --action-on-unmanage detachAll \
      --deny-settings-mode denyDelete \
      --deny-settings-excluded-actions "Microsoft.Authorization/locks/delete Microsoft.Authorization/roleDefinitions/delete Microsoft.Storage/storageAccounts/privateEndpointConnections/delete Microsoft.Storage/storageAccounts/privateEndpointConnectionProxies/delete" \
      --description "usnewsmap archival storage, shared by every environment (scripts/azure/archive-store.sh)" \
      --yes --only-show-errors -o none
    # Stacks can return output names with their case changed (scripts/lib/env.sh).
    id=$(az stack sub show -n "$STACK" --query outputs -o json |
      jq -r 'to_entries[] | select(.key | ascii_upcase == "ARCHIVE_ACCOUNT_ID") | .value.value')
    [ -n "$id" ] || die "the stack has no ARCHIVE_ACCOUNT_ID output"
    echo "archival account: $id"
    echo "an environment uses it with: scripts/azure/settings.sh --env <env> --key USNM_ARCHIVE_ACCOUNT --value $id && scripts/azure/provision.sh --env <env>"
    ;;
  unlock)
    account=$(az storage account list -g "$GROUP" --query "[0].name" -o tsv)
    [ -n "$account" ] || die "no account in $GROUP"
    az lock delete --name "$LOCK" -g "$GROUP" --resource "$account" \
      --resource-type Microsoft.Storage/storageAccounts -o none
    echo "lock $LOCK lifted from $account; run: scripts/azure/archive-store.sh deploy to put it back"
    ;;
  *) usage ;;
esac
