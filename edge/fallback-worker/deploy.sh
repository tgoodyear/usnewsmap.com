#!/usr/bin/env bash
# Deploys the fallback Worker (ADR-0027, #1098) with Wrangler. Run by hand from the
# operator's machine, signed in to az, from the address the Cloudflare Workers token is
# limited to (ADR-0027 decision 7). Nothing in CI runs it.
#
#   edge/fallback-worker/deploy.sh --account-id ID --azure-origin URL
#     --wrangler-version X.Y.Z --vault NAME --token-secret NAME
#     (--production | --drill-hostname NAME) [--dry-run]
#
# --account-id: the Cloudflare account id (32 hex digits).
# --azure-origin: ca-usnm-prod's default name, https://<name>.azurecontainerapps.io. It is
#   set as the Worker's AZURE_FALLBACK_ORIGIN variable.
# --wrangler-version: the exact Wrangler release to run (npx wrangler@X.Y.Z), so the tool
#   that holds the token is the one you chose.
# --vault and --token-secret: the Key Vault secret holding the Workers-scoped Cloudflare
#   token. It is read with your az sign-in into this process only: never printed, never on
#   a command line, never written to disk.
# --production: deploy the Worker "usnm-fallback" on the routes in wrangler.jsonc (the apex,
#   www and api). This is the Phase 3 cutover.
# --drill-hostname: deploy the drill Worker "usnm-fallback-drill" on this one hostname
#   only (a name under usnewsmap.com that is not www, api or boinc). It adds DNT: 1 to
#   every request, so a drill's searches stay out of the search log (Phase 2).
# --dry-run: print the command it would run, and stop before reading the token or
#   calling Cloudflare.
#
# The route's failure mode (fail open) is a setting on the route in Cloudflare, not in
# wrangler.jsonc; README.md has the steps, before and after a deploy.
set -euo pipefail
usage() {
  cat >&2 << 'EOF'
Usage: edge/fallback-worker/deploy.sh --account-id ID --azure-origin URL
         --wrangler-version X.Y.Z --vault NAME --token-secret NAME
         (--production | --drill-hostname NAME) [--dry-run]
EOF
  exit 2
}
die() { echo "error: $*" >&2; exit 1; }

ACCOUNT="" ORIGIN="" VERSION="" VAULT="" SECRET="" DRILL_HOST="" PRODUCTION=false DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --account-id) ACCOUNT=${2:-}; shift 2 || usage ;;
    --azure-origin) ORIGIN=${2:-}; shift 2 || usage ;;
    --wrangler-version) VERSION=${2:-}; shift 2 || usage ;;
    --vault) VAULT=${2:-}; shift 2 || usage ;;
    --token-secret) SECRET=${2:-}; shift 2 || usage ;;
    --drill-hostname) DRILL_HOST=${2:-}; shift 2 || usage ;;
    --production) PRODUCTION=true; shift ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $ACCOUNT =~ ^[0-9a-f]{32}$ ]] || { echo "error: --account-id: the Cloudflare account id (32 hex digits)" >&2; usage; }
[[ $ORIGIN =~ ^https://([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+azurecontainerapps\.io$ ]] ||
  { echo "error: --azure-origin: https://<name>.azurecontainerapps.io, with no path" >&2; usage; }
[[ $VERSION =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] ||
  { echo "error: --wrangler-version: an exact release, such as 4.152.0" >&2; usage; }
if $PRODUCTION && [ -n "$DRILL_HOST" ]; then
  echo "error: --production and --drill-hostname are alternatives" >&2; usage
fi
if ! $PRODUCTION && [ -z "$DRILL_HOST" ]; then
  echo "error: say where to deploy: --production or --drill-hostname NAME" >&2; usage
fi
if [ -n "$DRILL_HOST" ]; then
  [[ $DRILL_HOST =~ ^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)+usnewsmap\.com$ && ! $DRILL_HOST =~ ^(www|api|boinc)\.usnewsmap\.com$ ]] ||
    { echo "error: --drill-hostname: a name under usnewsmap.com other than www, api and boinc" >&2; usage; }
fi
$DRY_RUN || [[ $VAULT =~ ^[A-Za-z0-9-]{3,24}$ && $SECRET =~ ^[A-Za-z0-9-]{1,127}$ ]] ||
  { echo "error: --vault and --token-secret: the Key Vault secret with the Workers-scoped Cloudflare token" >&2; usage; }
HERE=$(cd "$(dirname "$0")" && pwd)

# Wrangler deletes any variable not named here or in wrangler.jsonc (there is no
# --keep-vars), so a variable set by hand in the dashboard does not survive a deploy.
cmd=(npx --yes "wrangler@$VERSION" deploy --config "$HERE/wrangler.jsonc" --var "AZURE_FALLBACK_ORIGIN:$ORIGIN")
if [ -n "$DRILL_HOST" ]; then
  cmd+=(--name usnm-fallback-drill --route "$DRILL_HOST/*" --var ADD_DNT:true)
fi

echo "The deployment:"
printf '  CLOUDFLARE_API_TOKEN=<from Key Vault> CLOUDFLARE_ACCOUNT_ID=<--account-id> %s\n' "$(printf '%q ' "${cmd[@]}")"
if $DRY_RUN; then
  echo "dry run: nothing read or sent"
  exit 0
fi

TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n "$SECRET" --query value -o tsv) ||
  die "can't read $SECRET from $VAULT"
[ -n "$TOKEN" ] || die "$SECRET in $VAULT is empty"
# The token reaches Wrangler in its environment and nowhere else.
status=0
CLOUDFLARE_API_TOKEN=$TOKEN CLOUDFLARE_ACCOUNT_ID=$ACCOUNT WRANGLER_SEND_METRICS=false "${cmd[@]}" || status=$?
unset TOKEN
[ "$status" -eq 0 ] || die "wrangler deploy failed ($status)"
echo "deployed. Now, in Cloudflare: set the route's failure mode to fail open, and check that Workers Logs are off (README.md)."
