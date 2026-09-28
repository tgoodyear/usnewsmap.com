#!/usr/bin/env bash
# Delete one usnewsmap environment: its deployment stack with every resource
# and resource group it manages, and its GitHub Environment (08 §8.9). The
# data goes with it (a new environment rebuilds it from LoC). Its local
# settings (.azure/<env>/.env) are kept, renamed, in case they're needed.
#
#   scripts/teardown.sh <env> [--subscription ID] [--repo OWNER/NAME]
#
# Needs: az 2.61+ and gh, signed in, Owner on the subscription and admin on
# the repository. Asks for the environment's name before deleting anything.
set -euo pipefail

usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0"; exit 2; }
[ $# -ge 1 ] || usage
ENV_NAME=$1; shift
case "$ENV_NAME" in -*|"") usage ;; esac
SUBSCRIPTION="" REPO=""
while [ $# -gt 0 ]; do
  case "$1" in
    --subscription) SUBSCRIPTION=$2; shift 2 ;;
    --repo) REPO=$2; shift 2 ;;
    *) usage ;;
  esac
done
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/.."

az account show -o none 2> /dev/null || die "run: az login [--tenant TENANT]"
[ -n "$SUBSCRIPTION" ] && az account set --subscription "$SUBSCRIPTION"
SUBSCRIPTION=$(az account show --query id -o tsv)
gh auth status > /dev/null 2>&1 || die "run: gh auth login"
[ -n "$REPO" ] || REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
STACK="usnm-$ENV_NAME"
az stack sub show -n "$STACK" -o none 2> /dev/null ||
  die "no deployment stack $STACK in subscription $SUBSCRIPTION"

ids=$(az stack sub show -n "$STACK" --query "resources[].id" -o tsv)
groups=$(grep -Ei '^/subscriptions/[^/]+/resourceGroups/[^/]+$' <<< "$ids" | sed 's|.*/||' || true)
# Resources that outlive their resource group: the environment's custom role
# (declared in its group, stored with the subscription), and the guard-rail
# policy definitions, which every environment here shares.
roles=$(grep -i '/providers/Microsoft.Authorization/roleDefinitions/' <<< "$ids" || true)
policies=$(grep -i '/providers/Microsoft.Authorization/policyDefinitions/' <<< "$ids" || true)
others=$(az stack sub list --query "[?starts_with(name, 'usnm-') && name != '$STACK'].name" -o tsv)

echo "This deletes environment $ENV_NAME from subscription $SUBSCRIPTION:"
sed 's/^/  resource group (everything in it) /' <<< "$groups"
[ -n "$roles" ] && sed 's/^/  /' <<< "$roles"
if [ -z "$others" ]; then
  [ -n "$policies" ] && sed 's/^/  /' <<< "$policies"
else
  echo "  (the guard-rail policy definitions stay: they're shared with $others)"
fi
echo "and the GitHub Environment $ENV_NAME in $REPO."
read -r -p "Type the environment name to confirm: " answer
[ "$answer" = "$ENV_NAME" ] || die "not confirmed"

# Detach, then delete explicitly: the stack's deleteAll would stop at a
# resource group that also holds something it doesn't manage, and at the
# policy definitions another environment's stack protects.
az stack sub delete -n "$STACK" --action-on-unmanage detachAll --yes --only-show-errors
for g in $groups; do
  echo "deleting resource group $g"
  az group delete -n "$g" --yes -o none
done
for id in $roles; do az resource delete --ids "$id" -o none; done
if [ -z "$others" ]; then
  for id in $policies; do az resource delete --ids "$id" -o none; done
fi

# Stop the workflows deploying to it.
envs=$(gh variable get USNM_DEPLOY_ENVIRONMENTS -R "$REPO" 2> /dev/null || true)
if grep -q "\"$ENV_NAME\"" <<< "$envs"; then
  names=$(tr -d '[]" ' <<< "$envs" | tr ',' '\n' | grep -vx "$ENV_NAME" | grep -v '^$' || true)
  envs="[$(sed 's/.*/"&"/' <<< "$names" | paste -sd, -)]"
  [ "$envs" = '[""]' ] && envs='[]'
  gh variable set USNM_DEPLOY_ENVIRONMENTS -R "$REPO" --body "$envs"
fi
gh api -X DELETE "repos/$REPO/environments/$ENV_NAME" > /dev/null 2>&1 || true

settings=".azure/$ENV_NAME/.env"
[ -f "$settings" ] && mv "$settings" "$settings.deleted-$(date -u +%Y%m%dT%H%M%SZ)"
echo "deleted environment $ENV_NAME"
