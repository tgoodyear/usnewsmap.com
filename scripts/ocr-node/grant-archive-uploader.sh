#!/usr/bin/env bash
# Apply only the reviewed archive uploader grant, without redeploying live jobs.
#   scripts/ocr-node/grant-archive-uploader.sh --env prod [--dry-run]
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
ENV_NAME="" DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --env) ENV_NAME=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    *) die "expected --env ENV [--dry-run]" ;;
  esac
done
[ -n "$ENV_NAME" ] || die "missing --env"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
# shellcheck source=scripts/lib/env.sh
. scripts/lib/env.sh
SUBSCRIPTION=$(aget AZURE_SUBSCRIPTION_ID)
GROUP=$(aget AZURE_RESOURCE_GROUP)
ACCOUNT=$(aget SCAN_ARCHIVE_ACCOUNT)
PRINCIPAL=$(aget USNM_SCAN_ARCHIVE_UPLOADER_ID)
[[ $SUBSCRIPTION =~ ^[0-9a-f-]{36}$ ]] || die "invalid subscription"
[[ $PRINCIPAL =~ ^[0-9a-f-]{36}$ ]] || die "missing or invalid archive uploader principal"
[[ $ACCOUNT =~ ^[a-z0-9]{3,24}$ ]] || die "missing or invalid archive account"
[[ $GROUP =~ ^[A-Za-z0-9._-]+$ ]] || die "invalid resource group"
machines=$(az resource list --subscription "$SUBSCRIPTION" \
  --resource-type Microsoft.HybridCompute/machines -o json)
printf '%s' "$machines" | python3 -c '
import json, sys
principal = sys.argv[1]
matches = [m for m in json.load(sys.stdin)
           if m.get("identity", {}).get("principalId") == principal]
sys.exit(0 if len(matches) == 1 else 1)
' "$PRINCIPAL" || die "uploader must identify exactly one existing Arc machine"
SCOPE="/subscriptions/$SUBSCRIPTION/resourceGroups/$GROUP/providers/Microsoft.Storage/storageAccounts/$ACCOUNT/blobServices/default/containers/scans"
roles=$(az role definition list --subscription "$SUBSCRIPTION" \
  --scope "/subscriptions/$SUBSCRIPTION/resourceGroups/$GROUP" --custom-role-only true -o json)
ROLE=$(printf '%s' "$roles" | python3 -c '
import json, sys
prefix = "usnm-scans-writer-role-" + sys.argv[1] + " ("
matches = [r for r in json.load(sys.stdin) if r["roleName"].startswith(prefix)]
if len(matches) != 1:
    sys.exit("expected exactly one existing archive writer role")
r = matches[0]
p = r["permissions"]
expected = {
    "Microsoft.Storage/storageAccounts/blobServices/containers/blobs/read",
    "Microsoft.Storage/storageAccounts/blobServices/containers/blobs/write",
}
if len(p) != 1 or p[0]["actions"] or set(p[0]["dataActions"]) != expected:
    sys.exit("archive writer role permissions differ from reviewed read/write role")
print(r["id"])
' "$ENV_NAME") || die "cannot verify existing archive writer role"
PARAMS=$(python3 -c 'import json,sys; print(json.dumps({
    "scope": {"value":sys.argv[1]}, "principal": {"value":sys.argv[2]},
    "role": {"value":sys.argv[3]}}))' "$SCOPE" "$PRINCIPAL" "$ROLE")
MODE=create
[ "$DRY_RUN" = false ] || MODE=what-if
az deployment group "$MODE" --subscription "$SUBSCRIPTION" --resource-group "$GROUP" \
  --name "scan-uploader-grant-$ENV_NAME" \
  --template-file scripts/ocr-node/scan-uploader-grant.bicep --parameters "$PARAMS"
