#!/usr/bin/env bash
# Compares the live Cloudflare settings outside wrangler.jsonc (zone tracing, the
# Worker's observability, routes and custom domains) with
# edge/fallback-worker/zone-expected.json. Read-only.
#
#   scripts/ci/check-edge-drift.sh --vault NAME --token-secret NAME
#
# --token-secret names the Key Vault secret holding a read-only Cloudflare token with
# Zone Observability Read, Workers Routes Read and Workers Scripts Read. The token
# goes to node through the environment only: never printed or written to disk.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,10s/^# \{0,1\}//p' "$0" >&2; exit 2; }
VAULT="" SECRET=""
while [ $# -gt 0 ]; do
  case $1 in
    --vault) VAULT=${2:-}; shift 2 || usage ;;
    --token-secret) SECRET=${2:-}; shift 2 || usage ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[ -n "$VAULT" ] && [ -n "$SECRET" ] || usage
ROOT=$(cd "$(dirname "$0")/../.." && pwd)

ACCOUNT=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-cloudflare-account-id --query value -o tsv) ||
  die "can't read usnm-prod-cloudflare-account-id from $VAULT"
TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n "$SECRET" --query value -o tsv) ||
  die "can't read $SECRET from $VAULT"
[ -n "$TOKEN" ] || die "$SECRET in $VAULT is empty"

status=0
CLOUDFLARE_API_TOKEN=$TOKEN CLOUDFLARE_ACCOUNT_ID=$ACCOUNT node "$ROOT/edge/fallback-worker/drift.mjs" || status=$?
unset TOKEN
exit "$status"
