#!/usr/bin/env bash
# Deploys one of the contributor Bicep files into the contributor's own
# subscription. With --what-if it only runs `az deployment group what-if`,
# which changes nothing. With --dry-run it prints the command and runs nothing.
# Needs the Azure CLI, already signed in (az login) by the contributor.
set -euo pipefail

die() { echo "deploy-azure: $*" >&2; exit 1; }
usage() {
  cat << 'USAGE'
usage: deploy-azure.sh --phase setup|aci|vmss-spot --subscription ID
         --resource-group NAME [--location REGION] --parameters-file FILE
         [--what-if] [--dry-run]
       deploy-azure.sh --phase set-key --subscription ID --vault-name NAME
         --account-key-file FILE [--dry-run]

  --phase            setup: phase A (Key Vault and the read-only worker identity)
                     set-key: store your BOINC account key in the vault (once)
                     aci or vmss-spot: phase B (BOINC workers)
  --vault-name       set-key only: the vaultName output of phase A
  --account-key-file set-key only: file whose first line is your BOINC account key;
                     the key is never printed or put on a command line
  --subscription     the subscription to use; required so the wrong one is never picked by default
  --resource-group   created if missing (not in --what-if or --dry-run)
  --location         needed only when the resource group does not exist yet
  --parameters-file  a Bicep parameters file or JSON parameters file
  --what-if          show what would change; deploy nothing
  --dry-run          print the az commands; run nothing
USAGE
}

PHASE='' SUB='' RG='' LOC='' PARAMS='' WHATIF=0 DRY=0 VAULT='' KEYFILE=''
while [ $# -gt 0 ]; do
  case "$1" in
    --phase) PHASE=$2; shift 2 ;;
    --subscription) SUB=$2; shift 2 ;;
    --resource-group) RG=$2; shift 2 ;;
    --location) LOC=$2; shift 2 ;;
    --parameters-file) PARAMS=$2; shift 2 ;;
    --vault-name) VAULT=$2; shift 2 ;;
    --account-key-file) KEYFILE=$2; shift 2 ;;
    --what-if) WHATIF=1; shift ;;
    --dry-run) DRY=1; shift ;;
    --help) usage; exit 0 ;;
    *) usage >&2; die "unknown option $1" ;;
  esac
done
[ -n "$PHASE" ] && [ -n "$SUB" ] || { usage >&2; die "--phase and --subscription are required"; }
[[ $SUB =~ ^[0-9a-fA-F-]{36}$ ]] || die "--subscription must be a subscription id (a GUID)"

run() {
  echo "+ $*"
  if [ "$DRY" = 0 ]; then "$@"; fi
}

if [ "$PHASE" = set-key ]; then
  [[ $VAULT =~ ^[a-zA-Z][a-zA-Z0-9-]{1,22}[a-zA-Z0-9]$ ]] || die "--vault-name is required (the vaultName output of phase A)"
  [ -f "$KEYFILE" ] || die "--account-key-file is required and must exist"
  IFS= read -r first < "$KEYFILE" || true
  first=${first//[[:space:]]/}
  [[ $first =~ ^[A-Za-z0-9]{8,128}$ ]] || die "the first line of $KEYFILE is not a key"
  # --output none: az would otherwise print the secret value back.
  run az keyvault secret set --subscription "$SUB" --vault-name "$VAULT" --name boinc-account-key \
    --file "$KEYFILE" --output none
  exit 0
fi
[ -n "$RG" ] && [ -n "$PARAMS" ] || { usage >&2; die "--resource-group and --parameters-file are required"; }
[[ $RG =~ ^[A-Za-z0-9._()-]{1,90}$ ]] || die "--resource-group has characters Azure does not allow"
[ -f "$PARAMS" ] || die "$PARAMS not found"

ROOT=$(cd "$(dirname "$0")/.." && pwd)
case "$PHASE" in
  setup) TEMPLATE=$ROOT/infra/contributor-azure/setup.bicep ;;
  aci) TEMPLATE=$ROOT/infra/contributor-azure/aci.bicep ;;
  vmss-spot) TEMPLATE=$ROOT/infra/contributor-azure/vmss-spot.bicep ;;
  *) die "--phase must be setup, set-key, aci or vmss-spot (enroll was removed: BOINC has no enrollment step)" ;;
esac

if [ "$WHATIF" = 1 ]; then
  # Read-only. The resource group must already exist for what-if to run.
  run az deployment group what-if --subscription "$SUB" --resource-group "$RG" \
    --template-file "$TEMPLATE" --parameters "$PARAMS"
  exit 0
fi

if [ "$DRY" = 0 ] && [ "$(az group exists --subscription "$SUB" --name "$RG")" != true ]; then
  [ -n "$LOC" ] || die "the resource group does not exist; give --location to create it"
  run az group create --subscription "$SUB" --name "$RG" --location "$LOC"
elif [ "$DRY" = 1 ] && [ -n "$LOC" ]; then
  run az group create --subscription "$SUB" --name "$RG" --location "$LOC"
fi
run az deployment group create --subscription "$SUB" --resource-group "$RG" \
  --name "usnm-$PHASE" --template-file "$TEMPLATE" --parameters "$PARAMS"
