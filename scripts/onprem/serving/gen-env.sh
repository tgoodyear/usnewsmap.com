#!/usr/bin/env bash
# Writes the serving container's settings (#955) from the sources Bicep
# uses, so the API on-prem runs as the Azure app does, apart from the
# differences listed below. Run from a checkout, as for the other
# environment scripts; Azure is only read (with --check-app).
#
#   scripts/onprem/serving/gen-env.sh --env prod --index-dir PATH --out DIR
#     [--check-app] [--production] [--dry-run]
#
# --index-dir: the index's Quickwit directory as the container sees it, an
#   absolute path. That's the release's run directory plus /qw, mounted
#   read-only at the same path, because the release writes absolute
#   file:// URIs into its metastore (scripts/onprem/release.sh).
# --out: a new directory (mode 0700) for api.env, searcher.env and
#   searcher.yaml, which install.sh --config-dir installs in the container.
# --check-app: also compare api.env with the environment's Azure API app
#   (az containerapp show), and fail on any difference not listed below.
# --dry-run: print the files instead of writing them.
#
# Sources: the committed settings (infra/env/<env>.json) and the stack's
# outputs in the local settings file, read through scripts/lib/env.sh as
# infra/main.bicepparam, main.bicep and modules/containerapp.bicep read
# them; the searcher's node config from infra/quickwit/searcher.yaml, with
# its Azure storage swapped for --index-dir; and the searcher's environment
# as containerapp.bicep sets it (test_serving.py checks the two agree).
#
# On purpose, api.env differs from the Azure app in:
# - USNM_QUICKWIT_URL: the searcher on loopback.
# - USNM_RESPONSE_CACHE_URL: a local directory (the API's LocalStore) in
#   place of the Blob container.
# - USNM_SITEMAP_STATE_URL: a shared durable directory, never cache-cleaned,
#   independent of visitor search logging.
# - USNM_SITEMAP_GENERATE: unset, so on-prem generates. An Azure app at zero
#   replicas sets it false and serves its last checkpoint.
# - USNM_SEARCH_LOG_URL and USNM_OCR_REQUESTS_URL: unset until the public
#   cutover (#955 Phase 3), because the API records same-origin searches on
#   any hostname.
# - USNM_COMPUTE_CONCURRENCY: the whole visitor budget, since one color
#   serves at a time from a searcher of its own.
# - USNM_ALLOWED_ORIGINS: the site's domain, without the Container Apps
#   default hostname.
# - Identity: the container's Arc identity (IDENTITY_ENDPOINT and
#   IMDS_ENDPOINT on loopback, no IDENTITY_HEADER) instead of the app's
#   managed identity, so no AZURE_CLIENT_ID.
# - The client address: USNM_TRUST_CF_CONNECTING_IP=true, so the API takes
#   CF-Connecting-IP from a loopback peer (Caddy, behind cloudflared) and
#   reads neither X-Forwarded-For nor USNM_TRUSTED_PROXY_HOPS. The hops are
#   0, the peer address, for an image from before that setting.
# - USNM_READY_REQUIRES_PUBLISHED_INDEXES=true: /readyz fails while
#   current.json names an index the local searcher lacks. The color keeps
#   serving the version it has; nothing here restarts a color on /readyz,
#   and update-api.sh switches to a new color only once it's ready.
# - Startup readiness: local blue/green rollouts keep the 300 s warm-up;
#   Azure cold standby skips it and keeps the 60 s readiness cap.
# USNM_BIND isn't here: each color's unit sets it.
set -euo pipefail
usage() { sed -n '7,8s/^# \{0,1\}//p' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }

ENV_NAME="" INDEX_DIR="" OUT="" CHECK_APP=false DRY_RUN=false PRODUCTION=false
while [ $# -gt 0 ]; do
  case "$1" in
    --env) ENV_NAME=${2:-}; shift 2 || usage ;;
    --index-dir) INDEX_DIR=${2:-}; shift 2 || usage ;;
    --out) OUT=${2:-}; shift 2 || usage ;;
    --check-app) CHECK_APP=true; shift ;;
    --production) PRODUCTION=true; shift ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] || usage
[[ $INDEX_DIR =~ ^/[A-Za-z0-9._/-]+$ && $INDEX_DIR != *..* && $INDEX_DIR != */ ]] ||
  { echo "error: --index-dir: an absolute path, letters, digits and ._-/ only, no trailing /" >&2; usage; }
$DRY_RUN || [ -n "$OUT" ] || { echo "error: --out: a directory to write to (or --dry-run)" >&2; usage; }
# A relative --out is the caller's, not the repository root's this runs from.
case $OUT in "" | /*) ;; *) OUT=$PWD/$OUT ;; esac
[ -z "$OUT" ] || [ ! -e "$OUT" ] || die "$OUT exists: write into a new directory"
HERE=$(cd "$(dirname "$0")" && pwd)
cd "$HERE/../../.."
# shellcheck source=scripts/lib/env.sh
. scripts/lib/env.sh
command -v jq > /dev/null || die "jq is needed"

# The settings, as the Bicep parameters default them.
get() { local v; v=$(aget "$1") || exit 1; printf '%s\n' "${v:-$2}"; }
need() { local v; v=$(aget "$1") || exit 1; [ -n "$v" ] || die "no $1 for $ENV_NAME: provision it first"; printf '%s\n' "$v"; }
[ "$(get USNM_SEARCH_BACKEND fixtures)" = quickwit ] || die "$ENV_NAME's search backend isn't quickwit"
STORAGE=$(need STORAGE_ACCOUNT)
[[ $STORAGE =~ ^[a-z0-9]{3,24}$ ]] || die "STORAGE_ACCOUNT isn't a storage account name"
COSMOS=$(need COSMOS_ENDPOINT)
INSIGHTS=$(get APPLICATIONINSIGHTS_CONNECTION_STRING "")
ZONE=$(need USNM_DNS_ZONE)
BUDGET=$(get USNM_COMPUTE_CONCURRENCY 2)
[[ $BUDGET =~ ^[1-9][0-9]*$ ]] || die "USNM_COMPUTE_CONCURRENCY isn't a positive number"
CRON=""
[ "$(get USNM_INGEST_JOBS false)" != true ] || [ "$(get USNM_USE_ACR false)" != true ] ||
  CRON=$(get USNM_INGEST_CRON "")

# The ready cap and warm-up budget as containerapp.bicep sets them for an
# app that keeps a replica: one color always serves while the other starts.
WARMUP_SECS=300
API_ENV=$(mktemp "${TMPDIR:-/tmp}/usnm-serving-api.env.XXXXXX")
trap 'rm -f "$API_ENV"' EXIT
{
  echo "USNM_BACKEND=quickwit"
  echo "USNM_QUICKWIT_URL=http://127.0.0.1:7280"
  echo "USNM_REFERENCE_URL=https://$STORAGE.blob.core.windows.net/reference"
  echo "USNM_RESPONSE_CACHE_URL=/srv/usnm-serving/cache"
  echo "USNM_SITEMAP_STATE_URL=/srv/usnm-serving/sitemap-state"
  if $PRODUCTION; then
    echo "USNM_SEARCH_LOG_URL=https://$STORAGE.blob.core.windows.net/searches"
    if [ "$(get USNM_OCR_NODES false)" = true ] && [ "$(get USNM_USE_ACR false)" = true ] &&
      { [ "$(get USNM_INGEST_JOBS false)" = true ] ||
        { [ "$(get USNM_OCR_SYNC_ONPREM false)" = true ] &&
          [[ "$(get USNM_BATCH_HOST "")" =~ [^[:space:]] ]]; }; }; then
      echo "USNM_OCR_REQUESTS_URL=https://$STORAGE.blob.core.windows.net/ocr-requests"
    fi
  fi
  echo "USNM_ALLOWED_ORIGINS=https://$ZONE,https://www.$ZONE"
  echo "USNM_TRUST_CF_CONNECTING_IP=true"
  echo "USNM_TRUSTED_PROXY_HOPS=0"
  echo "USNM_READY_REQUIRES_PUBLISHED_INDEXES=true"
  echo "USNM_COSMOS_ENDPOINT=$COSMOS"
  [ -z "$INSIGHTS" ] || echo "APPLICATIONINSIGHTS_CONNECTION_STRING=$INSIGHTS"
  [ -z "$CRON" ] || echo "USNM_INGEST_CRON=$CRON"
  [ "$(get USNM_AMERICAN_STORIES_SEARCH true)" = true ] || echo "USNM_AMERICAN_STORIES_SEARCH=false"
  [ "$(get USNM_CONGRESS_SEARCH true)" = true ] || echo "USNM_CONGRESS_SEARCH=false"
  filter=$(get USNM_PAGE_LANGUAGE_FILTER "")
  [ -z "$filter" ] || echo "USNM_PAGE_LANGUAGE_FILTER=$filter"
  [ "$(get USNM_BROWSE_PAGES false)" != true ] || echo "USNM_BROWSE_PAGES=true"
  echo "USNM_PREWARM_STARTUP_BUDGET_SECS=$WARMUP_SECS"
  echo "USNM_READY_CAP_SECS=$((WARMUP_SECS + 60))"
  echo "USNM_COMPUTE_CONCURRENCY=$BUDGET"
  echo "IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token"
  echo "IMDS_ENDPOINT=http://localhost:40342"
} > "$API_ENV"
# podman reads KEY=value lines: a value with a newline would break the file.
! grep -qvE '^[A-Z][A-Z0-9_]*=[^[:space:]].*$' "$API_ENV" || die "a setting is empty or spans lines"

# The searcher's environment, as containerapp.bicep's quickwitContainer
# sets it.
SEARCHER_ENV='QW_DISABLE_TELEMETRY=1
QW_NUM_CPUS=4
RAYON_NUM_THREADS=4
QW_TOKIO_RUNTIME_NUM_THREADS=4
QW_LISTEN_ADDRESS=127.0.0.1
RUST_LOG=quickwit=info,quickwit_serve::search_api=warn,quickwit_search=warn,tantivy=warn'

# searcher.yaml with its local state in the unit's state directory, the
# metastore and index root on --index-dir, and no Azure storage. Each line
# replaced must be there exactly once, so a change to the source fails here.
searcher_yaml() {
  echo "# Generated by scripts/onprem/serving/gen-env.sh from"
  echo "# infra/quickwit/searcher.yaml for the serving container (#955): the index"
  echo "# is on local disk (file://), read-only; the caches and limits are Azure's."
  awk -v idx="$INDEX_DIR" '
    BEGIN { head = 1 }
    head && /^#/ { next }
    { head = 0 }
    /^data_dir: / { print "data_dir: /var/lib/usnm-searcher"; n["data"]++; next }
    /^metastore_uri: azure:\/\/qw-index#polling_interval=[0-9]+s$/ {
      sub(/azure:\/\/qw-index/, "file://" idx); print; n["meta"]++; next }
    /^default_index_root_uri: azure:\/\/qw-index$/ { print "default_index_root_uri: file://" idx; n["root"]++; next }
    /^storage:$/ { skip = 1; n["storage"]++; next }
    skip && /^  / { next }
    { skip = 0; print }
    END { if (n["data"] != 1 || n["meta"] != 1 || n["root"] != 1 || n["storage"] != 1) exit 3 }
  ' infra/quickwit/searcher.yaml
}
YAML=$(searcher_yaml) || die "infra/quickwit/searcher.yaml no longer has the lines this replaces; update searcher_yaml()"

if $CHECK_APP; then
  azure_warm=$WARMUP_SECS
  azure_cap=$((WARMUP_SECS + 60))
  min_default=0
  [ "$ENV_NAME" != prod ] || min_default=1
  if [ "$(get USNM_API_MIN_REPLICAS "$min_default")" = 0 ]; then
    azure_warm=0
    azure_cap=60
  fi
  APP=$(need API_APP)
  RG=$(need AZURE_RESOURCE_GROUP)
  azure=$(az containerapp show -n "$APP" -g "$RG" --query "properties.template.containers[?name=='api'] | [0].env" -o json) ||
    die "can't read $APP's settings"
  diffs=$(jq -rn --argjson azure "$azure" --rawfile onprem "$API_ENV" \
    --arg warm "$azure_warm" --arg cap "$azure_cap" --argjson production "$PRODUCTION" '
    ($azure | map({(.name): (.value // "(a secret)")}) | add) as $a
    | ($onprem | split("\n") | map(select(length > 0) | capture("^(?<k>[^=]+)=(?<v>.*)$") | {(.k): .v}) | add) as $o
    | ["USNM_QUICKWIT_URL", "USNM_RESPONSE_CACHE_URL", "USNM_SITEMAP_STATE_URL", "USNM_COMPUTE_CONCURRENCY", "USNM_TRUSTED_PROXY_HOPS", "USNM_PREWARM_STARTUP_BUDGET_SECS", "USNM_READY_CAP_SECS"] as $differ
    | (["AZURE_CLIENT_ID", "USNM_SITEMAP_GENERATE"] + (if $production then [] else ["USNM_SEARCH_LOG_URL", "USNM_OCR_REQUESTS_URL"] end)) as $azure_only
    | ["IDENTITY_ENDPOINT", "IMDS_ENDPOINT", "USNM_SITEMAP_STATE_URL", "USNM_TRUST_CF_CONNECTING_IP", "USNM_READY_REQUIRES_PUBLISHED_INDEXES"] as $onprem_only
    | ($a | keys) as $ak | ($o | keys) as $ok
    | [($ak - $ok - $azure_only)[] as $k | "\($k): only in the Azure app (\($a[$k]))"]
    + [($ok - $ak - $onprem_only)[] as $k | "\($k): only here (\($o[$k]))"]
    + [($ak - ($ak - $ok))[] as $k
        | select($a[$k] != $o[$k] and ($differ | any(. == $k) | not))
        # Here, the domain origins of the Azure list are enough.
        | select($k != "USNM_ALLOWED_ORIGINS" or ((($o[$k] | split(",")) - ($a[$k] | split(","))) | length > 0))
        | "\($k): Azure \($a[$k]), here \($o[$k])"]
    + ([{key: "USNM_PREWARM_STARTUP_BUDGET_SECS", value: $warm},
        {key: "USNM_READY_CAP_SECS", value: $cap}]
       | map(select($a[.key] != .value) | "\(.key): expected Azure \(.value), got \($a[.key])"))
    | .[]')
  if [ -n "$diffs" ]; then
    echo "api.env differs from $APP in ways gen-env.sh doesn't expect:" >&2
    echo "$diffs" >&2
    exit 1
  fi
  echo "api.env matches $APP apart from the expected differences" >&2
fi

if $DRY_RUN; then
  echo "== api.env"
  cat "$API_ENV"
  echo "== searcher.env"
  echo "$SEARCHER_ENV"
  echo "== searcher.yaml"
  echo "$YAML"
  exit 0
fi
(
  umask 077
  mkdir -p "$OUT"
  cp "$API_ENV" "$OUT/api.env"
  echo "$SEARCHER_ENV" > "$OUT/searcher.env"
  echo "$YAML" > "$OUT/searcher.yaml"
)
echo "wrote $OUT/api.env, searcher.env and searcher.yaml"
