#!/usr/bin/env bash
# Deploys check.usnewsmap.com (edge/check) when its files differ from what is live.
#
#   edge/check/deploy.sh [--vault usnmtlskvprodffapzy] [--force]
#
# The deployed site serves /build.txt: the git tree hash of edge/check it was built from. A
# run whose tree matches it does nothing, so main's other merges don't make new Worker
# versions; --force deploys anyway. In GitHub Actions it also does nothing unless DEPLOY_SHA is
# still main's head (head-of-main.sh), so a slow run can't replace a newer deploy; the newer
# commit's run compares its own tree and deploys any check change an older run skipped.
# Needs az signed in with read access to the vault's secrets, git, node and curl. Secrets
# (SESSION_KEY, ADMIN_TOKEN, TURNSTILE_SECRET) and D1 migrations are not touched here: see
# README.md. Like edge/fallback-worker/deploy.sh, it lives with the Worker it deploys.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
VAULT=usnmtlskvprodffapzy FORCE=false
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=${2:-}; shift 2 ;;
    --force) FORCE=true; shift ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $VAULT =~ ^[a-z0-9-]+$ ]] || die "bad --vault"
ROOT=$(cd "$(dirname "$0")/../.." && pwd)
cd "$ROOT"
SITE=https://check.usnewsmap.com

if [ "${GITHUB_ACTIONS:-false}" = true ]; then
  current=$(mktemp)
  GITHUB_SHA="${DEPLOY_SHA:?error: DEPLOY_SHA is required in CI}" GITHUB_OUTPUT="$current" \
    scripts/ci/head-of-main.sh
  if ! grep -qx current=true "$current"; then
    rm -f "$current"
    echo "not main's head: the newer commit's run deploys"
    exit 0
  fi
  rm -f "$current"
fi

[ -z "$(git status --porcelain -- edge/check)" ] || die "edge/check has uncommitted changes"
tree=$(git rev-parse HEAD:edge/check)
live=$(curl --silent --max-time 20 --retry 3 --retry-all-errors "$SITE/build.txt" || true)
if [ "$live" = "$tree" ] && ! $FORCE; then
  echo "check.usnewsmap.com already runs edge/check $tree"
  exit 0
fi
echo "deploying edge/check $tree (live: ${live:-unknown})"

ACCOUNT=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-cloudflare-account-id --query value -o tsv)
TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-workers-ci-token --query value -o tsv)
[ -n "$ACCOUNT" ] && [ -n "$TOKEN" ] || die "Cloudflare account or Workers token is empty"
if [ "${GITHUB_ACTIONS:-false}" = true ]; then echo "::add-mask::$TOKEN"; fi

printf '%s\n' "$tree" > edge/check/public/build.txt
trap 'rm -f "$ROOT/edge/check/public/build.txt"' EXIT
(cd edge/check && CLOUDFLARE_ACCOUNT_ID="$ACCOUNT" CLOUDFLARE_API_TOKEN="$TOKEN" WRANGLER_SEND_METRICS=false \
  npx --yes wrangler@4.154.0 deploy --message "edge/check $tree")
unset TOKEN

for _ in $(seq 1 24); do
  [ "$(curl --silent --max-time 10 "$SITE/build.txt?$RANDOM" || true)" = "$tree" ] && break
  sleep 5
done
[ "$(curl --silent --max-time 10 "$SITE/build.txt?$RANDOM" || true)" = "$tree" ] ||
  die "check.usnewsmap.com doesn't serve build $tree two minutes after the deploy"
code=$(curl --silent --output /dev/null --write-out '%{http_code}' --max-time 20 "$SITE/api/languages")
[ "$code" = 200 ] || die "check.usnewsmap.com/api/languages answered $code after the deploy"
echo "verified check.usnewsmap.com serves edge/check $tree"
