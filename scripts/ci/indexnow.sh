#!/usr/bin/env bash
# After a deploy, submit the sitemap's URLs to IndexNow (Bing, Yandex, Seznam,
# Naver and others; Google does not use it). The search engines check
# ownership by fetching /<key>.txt from the site, so the key is public and
# lives in web/public.
#
# Submits only when the app just rolled (APP in RG) answers on the sitemap's
# host and serves the key there. The site is already live, so a problem here
# is a warning, never a failed deploy: this script always exits 0.
set -uo pipefail
: "${APP:?}" "${RG:?}"
cd "$(dirname "$0")/../.."
skip() {
  echo "::warning::IndexNow: $*; not submitted"
  exit 0
}

key=$(find web/public -maxdepth 1 -type f -name '*.txt' -exec basename {} .txt \; |
  grep -E '^[0-9a-f]{32}$' | head -n 1)
[ -n "$key" ] || skip "no key file (web/public/<32 hex digits>.txt)"
[ "$(cat "web/public/$key.txt")" = "$key" ] || skip "web/public/$key.txt does not hold its key"

urls=()
while IFS= read -r url; do
  [ -z "$url" ] || urls+=("$url")
done < <(grep -o '<loc>[^<]*</loc>' web/public/sitemap.xml 2> /dev/null |
  sed -e 's/<loc>//' -e 's/<\/loc>//' -e 's/^[[:space:]]*//' -e 's/[[:space:]]*$//')
[ "${#urls[@]}" -gt 0 ] || skip "web/public/sitemap.xml has no <loc> entries"
[[ ${urls[0]} =~ ^https://([^/]+)/ ]] || skip "sitemap URL ${urls[0]} is not an absolute https URL"
host=${BASH_REMATCH[1]}

if ! names=$(az containerapp hostname list -n "$APP" -g "$RG" --query "[].name" -o tsv 2>&1); then
  skip "could not list the hostnames of $APP: $names"
fi
grep -Fqx "$host" <<< "$names" || skip "$APP does not answer on $host"
live=$(curl -fsS --max-time 20 --retry 3 --retry-delay 5 --retry-all-errors "https://$host/$key.txt") ||
  skip "https://$host/$key.txt could not be fetched"
[ "$live" = "$key" ] || skip "https://$host/$key.txt does not hold the key"

# IndexNow takes at most 10,000 URLs per request.
batch=10000
total=$(((${#urls[@]} + batch - 1) / batch))
body=$(mktemp)
for ((i = 0; i < ${#urls[@]}; i += batch)); do
  label="batch $((i / batch + 1))/$total"
  payload=$(printf '%s\n' "${urls[@]:i:batch}" | jq -R . | jq -s \
    --arg host "$host" --arg key "$key" --arg loc "https://$host/$key.txt" \
    '{host: $host, key: $key, keyLocation: $loc, urlList: .}') || skip "could not build the request"
  n=$((${#urls[@]} - i < batch ? ${#urls[@]} - i : batch))
  if ! status=$(curl -sS --max-time 30 -o "$body" -w '%{http_code}' -X POST \
    -H 'Content-Type: application/json; charset=utf-8' --data "$payload" \
    https://api.indexnow.org/indexnow); then
    echo "::warning::IndexNow $label: request failed ($n URL(s), $host)"
    continue
  fi
  if [[ $status == 2* ]]; then
    echo "IndexNow $label: HTTP $status ($n URL(s), $host)"
  else
    echo "::warning::IndexNow $label: HTTP $status ($n URL(s), $host): $(head -c 500 "$body")"
  fi
done
rm -f "$body"
exit 0
