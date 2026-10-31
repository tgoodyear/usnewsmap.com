#!/usr/bin/env bash
set -euo pipefail
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"

VAULT=usnmtlskvprodffapzy
ACCOUNT=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-cloudflare-account-id --query value -o tsv)
ORIGIN=$(az containerapp show -g rg-usnm-prod -n ca-usnm-prod --query properties.configuration.ingress.fqdn -o tsv)
[ -n "$ACCOUNT" ] && [ -n "$ORIGIN" ] || {
  echo "error: Cloudflare account or Azure standby hostname is empty" >&2
  exit 1
}

edge/fallback-worker/deploy.sh \
  --account-id "$ACCOUNT" \
  --azure-origin "https://$ORIGIN" \
  --wrangler-version 4.154.0 \
  --vault "$VAULT" --token-secret usnm-prod-workers-ci-token \
  --production --health-failover --health-heartbeat \
  --health-hostname usnewsmap.com \
  --healthcheck-hostname healthcheck.usnewsmap.com \
  --allowed-origins https://usnewsmap.com,https://www.usnewsmap.com \
  --native-metrics --worker-logs

headers=$(mktemp)
body=$(mktemp)
trap 'rm -f "$headers" "$body"' EXIT
curl --fail --silent --show-error --max-time 215 \
  --retry 12 --retry-all-errors --retry-delay 5 --retry-max-time 180 -H 'DNT: 1' \
  -D "$headers" -o "$body" https://healthcheck.usnewsmap.com/readyz
[ "$(cat "$body")" = ready ] &&
  grep -Eiq '^x-usnm-origin: (onprem|azure)[[:space:]]*$' "$headers" || {
    echo "error: deployed health domain did not report a ready serving origin" >&2
    exit 1
  }
echo "verified production Worker health domain"
