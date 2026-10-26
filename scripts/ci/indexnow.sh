#!/usr/bin/env bash
# After a deploy, submit the sitemap's URLs to IndexNow (Bing, Yandex, Seznam,
# Naver and others; Google does not use it). The search engines check
# ownership by fetching /<key>.txt from the site, so the key is public and
# lives in web/public.
#
# Submits only when the app just rolled (APP in RG) answers on the sitemap's
# host and serves the key there. The site is already live, so a problem here
# is a warning, never a failed deploy: this script always exits 0. CI restores
# INDEXNOW_STATE_FILE so only a changed URL set is submitted.
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
live=$(curl -fsS --max-time 20 --retry 3 --retry-delay 5 --retry-all-errors "https://$host/$key.txt") ||
  skip "https://$host/$key.txt could not be fetched"
[ "$live" = "$key" ] || skip "https://$host/$key.txt does not hold the key"

# Read the deployed build's generated sitemap, not a stale source-tree list.
sitemap=$(curl -fsS --max-time 20 --retry 3 --retry-delay 5 --retry-all-errors "$sitemap_url") ||
  skip "$sitemap_url could not be fetched"
locations=$(python3 -c '
import sys
import xml.etree.ElementTree as ET

root = ET.fromstring(sys.stdin.read())
ns = "{http://www.sitemaps.org/schemas/sitemap/0.9}"
if root.tag != ns + "urlset":
    raise ValueError("expected a sitemap urlset")
for entry in root.findall(ns + "url"):
    location = entry.findtext(ns + "loc", "").strip()
    if not location or any(c.isspace() for c in location):
        raise ValueError("missing or invalid sitemap URL")
    print(location)
' <<< "$sitemap") || skip "$sitemap_url is not a valid URL sitemap"
urls=()
while IFS= read -r url; do
  [ -z "$url" ] || urls+=("$url")
done <<< "$locations"
[ "${#urls[@]}" -gt 0 ] || skip "$sitemap_url has no <loc> entries"
for url in "${urls[@]}"; do
  [[ $url == "https://$host/"* ]] || skip "sitemap URL $url is not on $host"
done

digest=$(printf '%s\n' "$key" "${urls[@]}" | python3 -c '
import hashlib
import sys
print(hashlib.sha256("\n".join(sorted(set(sys.stdin.read().splitlines()))).encode()).hexdigest())
') || skip "could not fingerprint the sitemap URLs"
if [ -f "$INDEXNOW_STATE_FILE" ]; then
  previous=$(cat "$INDEXNOW_STATE_FILE") || skip "could not read the last submission"
  if [ "$previous" = "$digest" ]; then
    echo "IndexNow: sitemap URLs unchanged since the last successful submission; not submitted"
    exit 0
  fi
fi

# IndexNow takes at most 10,000 URLs per request.
batch=10000
total=$(((${#urls[@]} + batch - 1) / batch))
work=$(mktemp -d) || skip "could not create request files"
body="$work/response"
payload="$work/request.json"
trap 'rm -f "$body" "$payload"; rmdir "$work"' EXIT
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
printf '%s\n' "$digest" > "$INDEXNOW_STATE_FILE" || skip "could not save the successful submission"
if [ -n "${GITHUB_OUTPUT:-}" ]; then
  echo "submitted=true" >> "$GITHUB_OUTPUT" || skip "could not report the successful submission"
fi
exit 0
