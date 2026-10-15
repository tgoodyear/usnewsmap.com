#!/usr/bin/env bash
# Create or delete the ephemeral BOINC test client VM (#484) in its own resource group
# usnm-boinc-test-rg-<env>.
#
# Usage:
#   scripts/boinc-test-client.sh --env <prod|dev> --create [--sku Standard_NC4as_T4_v3] [--spot|--paygo]
#     [--authenticator-secret boinc-test-authenticator] [--what-if]
#   scripts/boinc-test-client.sh --env <prod|dev> --delete [--what-if]
#
# The test volunteer account's authenticator is read by Azure Resource Manager directly from the
# BOINC Key Vault via a deployment parameter secret reference, so the secret value never touches
# disk, stdout, or process arguments on this machine.
set -euo pipefail

usage() {
  cat >&2 <<'EOF'
usage: scripts/boinc-test-client.sh --env <prod|dev> (--create | --delete) [options]

  --env <prod|dev>                 Environment to target (required)
  --create                         Create usnm-boinc-test-rg-<env> and deploy the test client VM
  --delete                         Delete usnm-boinc-test-rg-<env> and everything in it
  --sku <SKU>                      Azure VM size (default: Standard_NC4as_T4_v3)
  --spot                           Deploy as an Azure Spot VM (default)
  --paygo                          Deploy as a regular pay-as-you-go VM
  --authenticator-secret <NAME>    Key Vault secret holding the BOINC authenticator (default: boinc-test-authenticator)
  --what-if                        Check settings and Key Vault secret and print the plan without creating or deleting resources
  -h, --help                       Show this help
EOF
  exit 2
}

die() {
  echo "error: $*" >&2
  exit 1
}

ENV_NAME=""
MODE=""
SKU="Standard_NC4as_T4_v3"
SPOT=true
AUTH_SECRET="boinc-test-authenticator"
WHAT_IF=false

while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --create) [ -z "$MODE" ] || usage; MODE=create; shift ;;
    --delete) [ -z "$MODE" ] || usage; MODE=delete; shift ;;
    --sku) [ $# -ge 2 ] || usage; SKU=$2; shift 2 ;;
    --spot) SPOT=true; shift ;;
    --paygo) SPOT=false; shift ;;
    --authenticator-secret) [ $# -ge 2 ] || usage; AUTH_SECRET=$2; shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done

[ -n "$ENV_NAME" ] && [ -n "$MODE" ] || usage
[[ $SKU =~ ^[A-Za-z0-9_]+$ ]] || die "--sku must be an Azure VM size name"
[[ $AUTH_SECRET =~ ^[A-Za-z0-9-]+$ ]] || die "--authenticator-secret must be a Key Vault secret name"

cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib/boinc.sh disable=SC1091
. scripts/lib/boinc.sh
boinc_load_settings
boinc_az_check

TEST_RG="usnm-boinc-test-rg-$ENV_NAME"

if [ "$MODE" = delete ]; then
  if $WHAT_IF; then
    echo "would delete resource group $TEST_RG"
    exit 0
  fi
  if [ "$(baz group exists --name "$TEST_RG")" = true ]; then
    baz group delete --name "$TEST_RG" --yes --no-wait
    echo "deleting $TEST_RG"
  else
    echo "$TEST_RG does not exist"
  fi
  exit 0
fi

vault=$(boinc_output keyVaultName)
[ -n "$vault" ] || die "stack $BOINC_STACK has no keyVaultName output: run scripts/provision-boinc.sh --env $ENV_NAME first"
baz keyvault secret show --vault-name "$vault" --name "$AUTH_SECRET" --query id -o tsv > /dev/null ||
  die "secret $AUTH_SECRET not found in Key Vault $vault (store the test volunteer account's authenticator there first)"

vault_id="/subscriptions/$SUBSCRIPTION/resourceGroups/$BOINC_RG/providers/Microsoft.KeyVault/vaults/$vault"
master_url="${PROJECT_URL}usnmocr/"

params=$(mktemp)
trap 'rm -f "$params"' EXIT
jq -n \
  --arg env "$ENV_NAME" \
  --arg location "$LOCATION" \
  --arg vmSize "$SKU" \
  --argjson spot "$SPOT" \
  --arg projectUrl "$master_url" \
  --arg vaultId "$vault_id" \
  --arg secretName "$AUTH_SECRET" \
  '{
    "$schema": "https://schema.management.azure.com/schemas/2019-04-01/deploymentParameters.json#",
    contentVersion: "1.0.0.0",
    parameters: {
      env: { value: $env },
      location: { value: $location },
      vmSize: { value: $vmSize },
      spot: { value: $spot },
      projectUrl: { value: $projectUrl },
      projectAuthenticator: {
        reference: {
          keyVault: { id: $vaultId },
          secretName: $secretName
        }
      }
    }
  }' > "$params"

if $WHAT_IF; then
  cat <<PLAN
would create $TEST_RG in $LOCATION and deploy contrib-compute/infra/boinc/test-client.bicep:
  vmSize: $SKU (spot=$SPOT)
  projectUrl: $master_url
  projectAuthenticator: Key Vault $vault / $AUTH_SECRET
PLAN
  exit 0
fi

baz group create --name "$TEST_RG" --location "$LOCATION" -o none
baz deployment group create \
  --name "usnm-boinc-test-$ENV_NAME" \
  --resource-group "$TEST_RG" \
  --template-file contrib-compute/infra/boinc/test-client.bicep \
  --parameters "@$params"
