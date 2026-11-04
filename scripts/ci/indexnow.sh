#!/usr/bin/env bash
# After a deploy, submit the sitemap's URLs to IndexNow (Bing, Yandex, Seznam,
# Naver and others; Google does not use it). The search engines check
# ownership by fetching /<key>.txt from the site, so the key is public and
# lives in web/public.
#
# Submits only when the app just rolled (APP in RG) answers on the sitemap's
# host and serves the key there. The site is already live, so a problem here
# is a warning, never a failed deploy. Successful state is stored on the
# indexnow-state branch; INDEXNOW_STATE_FILE is a local snapshot only.
set -uo pipefail
skip() {
  echo "::warning::IndexNow: $*; not submitted"
  exit 0
}
if [[ ${GITHUB_EVENT_NAME:-} == pull_request* ]] ||
   [[ -n ${GITHUB_REF:-} && ${GITHUB_REF} != refs/heads/main ]]; then
  echo "IndexNow: not a production main deploy; not submitted"
  exit 0
fi
[ -n "${APP:-}" ] && [ -n "${RG:-}" ] || skip "APP and RG must be set"
[ -n "${INDEXNOW_STATE_FILE:-}" ] || skip "INDEXNOW_STATE_FILE must be set"
[ -n "${GH_TOKEN:-}" ] && [ -n "${GITHUB_REPOSITORY:-}" ] && [ -n "${GITHUB_SHA:-}" ] ||
  skip "GitHub token, repository and commit are required for durable state"
cd "$(dirname "$0")/../.." || skip "could not find the repository root"

key=$(find web/public -maxdepth 1 -type f -name '*.txt' -exec basename {} .txt \; |
  grep -E '^[0-9a-f]{32}$' | head -n 1)
[ -n "$key" ] || skip "no key file (web/public/<32 hex digits>.txt)"
[ "$(cat "web/public/$key.txt")" = "$key" ] || skip "web/public/$key.txt does not hold its key"

sitemap_url=$(sed -n 's/^Sitemap: //p' web/public/robots.txt)
[[ $sitemap_url =~ ^https://([^/]+)/sitemap\.xml$ ]] || skip "robots.txt must name an absolute https sitemap URL"
host=${BASH_REMATCH[1]}

if ! names=$(az containerapp hostname list -n "$APP" -g "$RG" --query "[].name" -o tsv 2>&1); then
  skip "could not list the hostnames of $APP: $names"
fi
grep -Fqx "$host" <<< "$names" || skip "$APP does not answer on $host"
live=$(curl -fsS -H 'DNT: 1' --max-time 20 --retry 3 --retry-delay 5 --retry-all-errors "https://$host/$key.txt") ||
  skip "https://$host/$key.txt could not be fetched"
[ "$live" = "$key" ] || skip "https://$host/$key.txt does not hold the key"

work=$(mktemp -d) || skip "could not create request files"
body="$work/response"
payload="$work/request.json"
next_state="$work/state.json"
expected="$work/deployed-sitemap.xml"
expected_headers="$work/deployed-sitemap-headers"
public_headers="$work/public-sitemap-headers"
encoded_state="$work/state.b64"
trap 'rm -f "$body" "$payload" "$next_state" "$expected" "$expected_headers" "$public_headers" "$encoded_state" "$work/github.err"; rmdir "$work"' EXIT

read_header() {
  local name
  name=$(printf '%s' "$1" | tr '[:upper:]' '[:lower:]') || return 1
  awk -v name="$name:" 'tolower($1) == name {gsub("\r", "", $2); value=$2} END {print value}' "$2"
}

# Read the deployed build's generated sitemap, not a stale source-tree list.
sitemap=$(curl -fsS -D "$public_headers" -H 'DNT: 1' -H 'Cache-Control: no-cache' --max-time 20 --retry 3 --retry-delay 5 --retry-all-errors "$sitemap_url") ||
  skip "$sitemap_url could not be fetched"
public_content_identity=$(read_header x-usnm-sitemap-content-identity "$public_headers") ||
  skip "could not read the public sitemap content identity"

# A deployment notification waits for Azure to generate the deployed build's
# checkpoint, then for the on-prem origin to serve the same page content before
# acknowledging it. Origin-local lastmod dates are not comparable.
# The hourly job retries if either bounded wait expires.
if [ -n "${INDEXNOW_EXPECTED_SITEMAP_URL:-}" ]; then
  [[ $INDEXNOW_EXPECTED_SITEMAP_URL =~ ^https://[a-z0-9.-]+/sitemap\.xml$ ]] ||
    skip "INDEXNOW_EXPECTED_SITEMAP_URL must be an absolute Azure sitemap URL"
  wait_secs=${INDEXNOW_PUBLIC_WAIT_SECS:-1800}
  [[ $wait_secs =~ ^[0-9]+$ ]] || skip "INDEXNOW_PUBLIC_WAIT_SECS must be a nonnegative integer"
  poll_secs=${INDEXNOW_POLL_SECS:-15}
  [[ $poll_secs =~ ^[0-9]+$ ]] || skip "INDEXNOW_POLL_SECS must be a nonnegative integer"
  deadline=$(($(date +%s) + wait_secs))
  build_current=false
  while [ "$build_current" != true ]; do
    if ! status=$(curl -sS -D "$expected_headers" -o "$expected" -w '%{http_code}' \
      -H 'DNT: 1' -H 'Cache-Control: no-cache' --max-time 20 \
      --retry 3 --retry-delay 5 --retry-all-errors "$INDEXNOW_EXPECTED_SITEMAP_URL"); then
      skip "$INDEXNOW_EXPECTED_SITEMAP_URL could not be fetched"
    fi
    if [ "$status" = 200 ]; then
      build_current=$(read_header x-usnm-sitemap-build-current "$expected_headers") ||
        skip "could not read Azure's sitemap build status"
    elif [ "$status" != 503 ]; then
      skip "$INDEXNOW_EXPECTED_SITEMAP_URL returned HTTP $status"
    fi
    [ "$build_current" = true ] && break
    now=$(date +%s)
    [ "$now" -lt "$deadline" ] ||
      skip "Azure sitemap checkpoint has not been generated for the deployed build"
    echo "IndexNow: waiting for Azure to generate the deployed build's sitemap checkpoint"
    sleep "$poll_secs"
  done
  deployed_content_identity=$(read_header x-usnm-sitemap-content-identity "$expected_headers") ||
    skip "could not read Azure's sitemap content identity"
  [ -n "$deployed_content_identity" ] ||
    skip "Azure response is missing its sitemap content identity"
  while ! python3 scripts/ci/indexnow-plan.py --host "$host" --compare "$expected" <<< "$sitemap" ||
      [ "$public_content_identity" != "$deployed_content_identity" ]; do
    now=$(date +%s)
    [ "$now" -lt "$deadline" ] ||
      skip "public sitemap has not caught up with the deployed Azure sitemap"
    echo "IndexNow: waiting for the public sitemap to reflect the deployed sitemap"
    sleep "$poll_secs"
    sitemap=$(curl -fsS -D "$public_headers" -H 'DNT: 1' -H 'Cache-Control: no-cache' --max-time 20 \
      --retry 3 --retry-delay 5 --retry-all-errors "$sitemap_url") ||
      skip "$sitemap_url could not be fetched while waiting for the public sitemap"
    public_content_identity=$(read_header x-usnm-sitemap-content-identity "$public_headers") ||
      skip "could not read the public sitemap content identity"
  done
fi

state_env=${INDEXNOW_ENVIRONMENT:-prod}
[[ $state_env =~ ^[a-z0-9-]+$ ]] || skip "INDEXNOW_ENVIRONMENT must be a lowercase environment name"
state_branch="indexnow-state-$state_env"
state_path=".github/indexnow-state/$state_env.json"
state_endpoint="repos/$GITHUB_REPOSITORY/contents/$state_path"
state_sha=""
if response=$(gh api "$state_endpoint?ref=$state_branch" 2>"$work/github.err"); then
  state_sha=$(jq -er .sha <<< "$response") || skip "durable IndexNow state has no file SHA"
  encoded=$(jq -er .content <<< "$response" | tr -d '[:space:]') ||
    skip "durable IndexNow state has no content"
  mkdir -p "$(dirname "$INDEXNOW_STATE_FILE")" || skip "could not create the state directory"
  printf '%s' "$encoded" | python3 -c 'import base64,sys; sys.stdout.buffer.write(base64.b64decode(sys.stdin.buffer.read()))' > "$INDEXNOW_STATE_FILE" ||
    skip "durable IndexNow state is not valid base64"
elif grep -q 'HTTP 404' "$work/github.err"; then
  # Initialize a clean baseline from the current sitemap; never migrate cache data.
  rm -f "$INDEXNOW_STATE_FILE" || skip "could not discard a legacy local snapshot"
  ref_endpoint="repos/$GITHUB_REPOSITORY/git/ref/heads/$state_branch"
  if ! gh api "$ref_endpoint" >/dev/null 2>"$work/github.err"; then
    grep -q 'HTTP 404' "$work/github.err" ||
      skip "could not check the durable IndexNow state branch"
    if ! gh api --method POST "repos/$GITHUB_REPOSITORY/git/refs" \
      -f "ref=refs/heads/$state_branch" -f "sha=$GITHUB_SHA" >/dev/null 2>"$work/github.err"; then
      gh api "$ref_endpoint" >/dev/null 2>"$work/github.err" ||
        skip "could not initialize the durable IndexNow state branch"
    fi
  fi
else
  skip "could not read durable IndexNow state: $(tail -1 "$work/github.err")"
fi

locations=$(python3 scripts/ci/indexnow-plan.py --host "$host" --key "$key" \
  --state "$INDEXNOW_STATE_FILE" --output-state "$next_state" <<< "$sitemap") ||
  skip "$sitemap_url is not a valid URL sitemap or its submission state is invalid"
urls=()
while IFS= read -r url; do
  [ -z "$url" ] || urls+=("$url")
done <<< "$locations"
[ "${#urls[@]}" -gt 0 ] || {
  echo "IndexNow: sitemap URLs unchanged since the last successful submission; not submitted"
  exit 0
}
for url in "${urls[@]}"; do
  [[ $url == "https://$host/"* ]] || skip "sitemap URL $url is not on $host"
done

# IndexNow takes at most 10,000 URLs per request.
batch=10000
total=$(((${#urls[@]} + batch - 1) / batch))
submitted=true
for ((i = 0; i < ${#urls[@]}; i += batch)); do
  label="batch $((i / batch + 1))/$total"
  printf '%s\n' "${urls[@]:i:batch}" | jq -R . | jq -s \
    --arg host "$host" --arg key "$key" --arg loc "https://$host/$key.txt" \
    '{host: $host, key: $key, keyLocation: $loc, urlList: .}' > "$payload" || skip "could not build the request"
  n=$((${#urls[@]} - i < batch ? ${#urls[@]} - i : batch))
  if ! status=$(curl -sS --max-time 30 -o "$body" -w '%{http_code}' -X POST \
    -H 'Content-Type: application/json; charset=utf-8' --data-binary "@$payload" \
    https://api.indexnow.org/indexnow); then
    echo "::warning::IndexNow $label: request failed ($n URL(s), $host)"
    submitted=false
    continue
  fi
  if [[ $status == 2* ]]; then
    echo "IndexNow $label: HTTP $status ($n URL(s), $host)"
  else
    echo "::warning::IndexNow $label: HTTP $status ($n URL(s), $host): $(head -c 500 "$body")"
    submitted=false
  fi
done
[ "$submitted" = true ] || exit 0
mkdir -p "$(dirname "$INDEXNOW_STATE_FILE")" || skip "could not create the submission state directory"
# Never acknowledge a rejected or partial batch.
base64 < "$next_state" | tr -d '[:space:]' > "$encoded_state" ||
  skip "could not encode the successful submission state"
write_args=(--method PUT "$state_endpoint" -f "message=ci: acknowledge IndexNow sitemap" \
  -F "content=@$encoded_state" -f "branch=$state_branch")
[ -z "$state_sha" ] || write_args+=(-f "sha=$state_sha")
if ! gh api "${write_args[@]}" >/dev/null 2>"$work/github.err"; then
  skip "IndexNow accepted the URLs but durable state was not saved: $(tail -1 "$work/github.err")"
fi
cp "$next_state" "$INDEXNOW_STATE_FILE" || skip "could not update the local state snapshot"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "submitted=true" >> "$GITHUB_OUTPUT" || skip "could not report the successful submission"
fi
exit 0
