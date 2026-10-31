#!/usr/bin/env bash
# Deploys the fallback Worker (ADR-0027, #1098) with Wrangler. Run by hand from the
# operator's machine, signed in to az, from the address the Cloudflare Workers token is
# limited to (ADR-0027 decision 7). Nothing in CI runs it.
#
#   edge/fallback-worker/deploy.sh --account-id ID --azure-origin URL
#     --wrangler-version X.Y.Z --vault NAME --token-secret NAME
#     (--production | --drill-hostname NAME) [--error-layer --allowed-origins CSV] [--dry-run]
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
# --error-layer: disable Azure fallback and enable the Phase 4 cached-shell/error mode.
#   Production use requires the accepted Phase 4 gate; isolated drills can exercise it.
# --allowed-origins: the API's exact USNM_ALLOWED_ORIGINS list; required for --error-layer.
# --health-failover: readiness-driven origin selection; requires --allowed-origins and
#   cannot be combined with --error-layer. Default deployments retain legacy retries.
# --health-heartbeat: opt-in minute cron using wrangler.health.jsonc; requires health mode.
# --health-hostname: incoming and scheduled probes target exactly usnewsmap.com,
#   dev.usnewsmap.com, drill.usnewsmap.com or failover-drill.usnewsmap.com.
# --native-metrics: explicitly omit Analytics Engine; only native invocation aggregates
#   remain. Origin-specific counts require synthetic measurement (README.md).
# --healthcheck-hostname: exactly healthcheck.usnewsmap.com, an additional Worker custom
#   domain on production health deployments, never a tunnel/origin DNS target.
# --worker-logs: approved platform invocation logs with query redaction; no custom logs.
#
# The route's failure mode (fail open) is a setting on the route in Cloudflare, not in
# wrangler.jsonc; README.md has the steps, before and after a deploy.
set -euo pipefail
usage() {
  cat >&2 << 'EOF'
Usage: edge/fallback-worker/deploy.sh --account-id ID --azure-origin URL
         --wrangler-version X.Y.Z --vault NAME --token-secret NAME
         (--production | --drill-hostname NAME)
         [--error-layer | --health-failover [--health-heartbeat] [--health-hostname HOST]]
         [--allowed-origins CSV] [--native-metrics]
         [--healthcheck-hostname healthcheck.usnewsmap.com] [--worker-logs] [--dry-run]
EOF
  exit 2
}
die() { echo "error: $*" >&2; exit 1; }

ACCOUNT="" ORIGIN="" VERSION="" VAULT="" SECRET="" DRILL_HOST="" ALLOWED="" PRODUCTION=false DRY_RUN=false ERROR_LAYER=false
HEALTH_FAILOVER=false HEALTH_HEARTBEAT=false HEALTH_HOST=""
NATIVE_METRICS=false GENERATED_CONFIG="" HEALTHCHECK_HOST="" WORKER_LOGS=false
cleanup() {
  if [ -n "$GENERATED_CONFIG" ]; then rm -f -- "$GENERATED_CONFIG"; fi
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
while [ $# -gt 0 ]; do
  case "$1" in
    --account-id) ACCOUNT=${2:-}; shift 2 || usage ;;
    --azure-origin) ORIGIN=${2:-}; shift 2 || usage ;;
    --wrangler-version) VERSION=${2:-}; shift 2 || usage ;;
    --vault) VAULT=${2:-}; shift 2 || usage ;;
    --token-secret) SECRET=${2:-}; shift 2 || usage ;;
    --drill-hostname) DRILL_HOST=${2:-}; shift 2 || usage ;;
    --production) PRODUCTION=true; shift ;;
    --error-layer) ERROR_LAYER=true; shift ;;
    --health-failover) HEALTH_FAILOVER=true; shift ;;
    --health-heartbeat) HEALTH_HEARTBEAT=true; shift ;;
    --health-hostname) HEALTH_HOST=${2:-}; shift 2 || usage ;;
    --native-metrics) NATIVE_METRICS=true; shift ;;
    --healthcheck-hostname) HEALTHCHECK_HOST=${2:-}; shift 2 || usage ;;
    --worker-logs) WORKER_LOGS=true; shift ;;
    --allowed-origins) ALLOWED=${2:-}; shift 2 || usage ;;
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
if $ERROR_LAYER && $HEALTH_FAILOVER; then
  echo "error: --error-layer and --health-failover are alternatives" >&2; usage
fi
if { $ERROR_LAYER || $HEALTH_FAILOVER; } && [ -z "$ALLOWED" ]; then
  echo "error: --error-layer/--health-failover requires the API's exact --allowed-origins list" >&2; usage
fi
if $HEALTH_HEARTBEAT && ! $HEALTH_FAILOVER; then
  echo "error: --health-heartbeat requires --health-failover" >&2; usage
fi
if [ -n "$HEALTHCHECK_HOST" ]; then
  if [ "$HEALTHCHECK_HOST" != "healthcheck.usnewsmap.com" ] || ! $HEALTH_FAILOVER || ! $PRODUCTION; then
    echo "error: --healthcheck-hostname requires healthcheck.usnewsmap.com, --health-failover and --production" >&2; usage
  fi
fi
if [ -n "$HEALTH_HOST" ]; then
  $HEALTH_FAILOVER || { echo "error: --health-hostname requires --health-failover" >&2; usage; }
  case "$HEALTH_HOST" in
    usnewsmap.com|dev.usnewsmap.com|drill.usnewsmap.com|failover-drill.usnewsmap.com) ;;
    *) echo "error: --health-hostname: exactly usnewsmap.com, dev.usnewsmap.com, drill.usnewsmap.com or failover-drill.usnewsmap.com" >&2; usage ;;
  esac
fi
if [ -n "$ALLOWED" ]; then
  node -e 'try { for (const s of process.argv[1].split(",")) { const u = new URL(s); if (!["http:", "https:"].includes(u.protocol) || u.origin !== s) process.exit(1); } } catch { process.exit(1); }' "$ALLOWED" ||
    { echo "error: --allowed-origins: exact comma-separated HTTP(S) origins, no wildcards or paths" >&2; usage; }
fi
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
CONFIG="$HERE/wrangler.jsonc"
if $HEALTH_FAILOVER; then
  if [ -z "$HEALTH_HOST" ]; then
    if [ -n "$DRILL_HOST" ]; then
      case "$DRILL_HOST" in
        dev.usnewsmap.com|drill.usnewsmap.com|failover-drill.usnewsmap.com) HEALTH_HOST=$DRILL_HOST ;;
        *) echo "error: drill health mode requires an explicit allowed --health-hostname" >&2; usage ;;
      esac
    else
      HEALTH_HOST=usnewsmap.com
    fi
  fi
fi
if $HEALTH_HEARTBEAT; then CONFIG="$HERE/wrangler.health.jsonc"; fi
if $NATIVE_METRICS || $WORKER_LOGS || [ -n "$HEALTHCHECK_HOST" ]; then
  # Exclusive 0600 creation in the owned edge directory, never a system temp path.
  # The config contains no token/account credentials; cleanup also runs on failures.
  MODE=custom-domain
  if $NATIVE_METRICS; then MODE=native; fi
  LOG_MODE=disabled
  if $WORKER_LOGS; then LOG_MODE=worker-logs; fi
  GENERATED_CONFIG=$(node "$HERE/native-config.mjs" "$CONFIG" "$MODE" "$HEALTHCHECK_HOST" "$LOG_MODE") ||
    die "can't generate deployment configuration"
  CONFIG=$GENERATED_CONFIG
fi

# Wrangler deletes any variable not named here or in wrangler.jsonc (there is no
# --keep-vars), so a variable set by hand in the dashboard does not survive a deploy.
cmd=(npx --yes "wrangler@$VERSION" deploy --config "$CONFIG" --var "AZURE_FALLBACK_ORIGIN:$ORIGIN")
if $NATIVE_METRICS; then cmd+=(--var METRICS_MODE:native); fi
if [ -n "$HEALTHCHECK_HOST" ]; then cmd+=(--var "HEALTHCHECK_HOSTNAME:$HEALTHCHECK_HOST"); fi
if $HEALTH_FAILOVER; then
  cmd+=(--var HEALTH_FAILOVER:true --var "HEALTH_HOSTNAME:$HEALTH_HOST")
fi
if $HEALTH_HEARTBEAT; then
  cmd+=(--var HEALTH_HEARTBEAT:true)
fi
if $ERROR_LAYER; then
  cmd+=(--var ERROR_LAYER:true)
fi
if [ -n "$ALLOWED" ]; then
  cmd+=(--var "ALLOWED_ORIGINS:$ALLOWED")
fi
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
echo "deployed. Now verify route failure mode is fail open and the explicitly selected Workers Logs policy in Cloudflare (README.md)."
