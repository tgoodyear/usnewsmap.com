#!/usr/bin/env bash
# The archival storage account (infra/archive/, docs/operations.md,
# "Archival storage"): its own deployment stack, usnm-archive, outside every
# environment's, so no provision or teardown of an environment deletes it.
#
#   scripts/archive-store.sh deploy [--subscription ID] [--location REGION]
#   scripts/archive-store.sh unlock [--subscription ID]   # lift the delete lock
#
# `deploy` creates or updates it (and puts the delete lock back) and prints
# the account's resource id, for an environment's USNM_ARCHIVE_ACCOUNT.
# Resources dropped from the template are detached, never deleted, and the
# stack denies deletes outside it except of the lock and of private endpoint
# connections. A delete lock on the account can block removing an
# environment's private endpoint to it, so before an environment drops
# USNM_ARCHIVE_ACCOUNT (or is torn down): `unlock`, provision the
# environment without the setting, then `deploy` again.
#
# Needs az 2.61+, signed in with Owner on the subscription.
set -euo pipefail
usage() { sed -n '2,17s/^# \{0,1\}//p' "$0" >&2; exit 2; }
[ $# -ge 1 ] || usage
ACTION=$1; shift
SUBSCRIPTION="" LOCATION=eastus2
while [ $# -gt 0 ]; do
  case "$1" in
    --subscription) SUBSCRIPTION=$2; shift 2 ;;
    --location) LOCATION=$2; shift 2 ;;
    *) usage ;;
  esac
done
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/.."
STACK=usnm-archive
GROUP=rg-usnm-archive
LOCK=usnm-archive-keep
command -v jq > /dev/null || die "jq is needed"
az account show -o none 2> /dev/null || die "run: az login [--tenant TENANT]"
[ -z "$SUBSCRIPTION" ] || az account set --subscription "$SUBSCRIPTION"
echo "subscription $(az account show --query '[name, id]' -o tsv | paste -sd' ' -)"

case "$ACTION" in
  deploy)
    # A stack's location is fixed once created.
    if location=$(az stack sub show -n "$STACK" --query location -o tsv 2> /dev/null); then
      LOCATION=$location
    fi
    az stack sub create --name "$STACK" --location "$LOCATION" \
      --template-file infra/archive/main.bicep \
      --parameters location="$LOCATION" \
      --action-on-unmanage detachAll \
      --deny-settings-mode denyDelete \
      --deny-settings-excluded-actions \
        Microsoft.Authorization/locks/delete \
        Microsoft.Storage/storageAccounts/privateEndpointConnections/delete \
        Microsoft.Storage/storageAccounts/privateEndpointConnectionProxies/delete \
      --description "usnewsmap archival storage, shared by every environment (scripts/archive-store.sh)" \
      --yes --only-show-errors -o none
    # Stacks can return output names with their case changed (scripts/lib/env.sh).
    id=$(az stack sub show -n "$STACK" --query outputs -o json |
      jq -r 'to_entries[] | select(.key | ascii_upcase == "ARCHIVE_ACCOUNT_ID") | .value.value')
    [ -n "$id" ] || die "the stack has no ARCHIVE_ACCOUNT_ID output"
    echo "archival account: $id"
    echo "an environment uses it with: scripts/settings.sh <env> USNM_ARCHIVE_ACCOUNT $id && scripts/provision.sh <env>"
    ;;
  unlock)
    account=$(az storage account list -g "$GROUP" --query "[0].name" -o tsv)
    [ -n "$account" ] || die "no account in $GROUP"
    az lock delete --name "$LOCK" -g "$GROUP" --resource "$account" \
      --resource-type Microsoft.Storage/storageAccounts -o none
    echo "lock $LOCK lifted from $account; run: scripts/archive-store.sh deploy to put it back"
    ;;
  *) usage ;;
esac
