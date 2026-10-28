#!/usr/bin/env bash
# The API image (--image) starts on its baked fixtures and serves both the API and
# the site: the app shell with its security headers, a precompressed hashed
# asset, robots.txt and the sitemap, a 404 for an unknown path, and /v1.
set -euo pipefail
usage() { echo "usage: scripts/ci/check-api-image.sh --image IMAGE" >&2; exit 2; }
# The positional form (`check-api-image.sh IMAGE`) works for one
# release, with a warning (#277).
if [ $# -eq 1 ] && [[ $1 != -* ]]; then
  echo "warning: \`scripts/ci/check-api-image.sh IMAGE\` is deprecated; use --image (#277)" >&2
  set -- --image "$1"
fi
image=""
while [ $# -gt 0 ]; do
  case $1 in
    --image) [ $# -ge 2 ] || usage; image=$2; shift 2 ;;
    -h|--help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$image" ] || usage
id=$(docker run -d -p 127.0.0.1:18080:8080 "$image")
trap 'docker logs "$id" | tail -n 50; docker rm -f "$id" > /dev/null' EXIT
base=http://127.0.0.1:18080
for _ in $(seq 1 30); do
  curl -sf "$base/readyz" > /dev/null && break
  sleep 1
done
curl -fsS "$base/v1/meta" | grep -q '"index_version"'
headers=$(curl -fsS -D - -o /dev/null "$base/")
grep -qi '^content-type: text/html' <<< "$headers"
grep -qi "^content-security-policy: .*connect-src 'self'" <<< "$headers"
asset=$(curl -fsS "$base/" | grep -o '/assets/index-[^"]*\.js' | head -n 1)
[ -n "$asset" ] || { echo "no script in the app shell"; exit 1; }
curl -fsS -D - -o /dev/null -H 'accept-encoding: br' "$base$asset" | grep -qi '^content-encoding: br'
curl -fsS -D - -o /dev/null "$base/robots.txt" | grep -qi '^content-type: text/plain'
curl -fsS "$base/sitemap.xml" | grep -q '<loc>https://usnewsmap.com/</loc>'
curl -fsS "$base/.well-known/security.txt" | grep -q '^Contact: https://github.com/'
curl -fsS "$base/.well-known/trust.txt" | grep -q '^owner = Trevor Goodyear$'
curl -fsS "$base/.well-known/gpc.json" | grep -q '"gpc": true'
curl -fsS "$base/.well-known/assetlinks.json" | grep -qx '\[\]'
version=$(python3 -c 'import json; print(json.load(open("web/basemap.json"))["version"])')
range_headers=$(curl -fsS -D - -o /dev/null -H 'Range: bytes=0-126' -H 'Accept-Encoding: br, gzip' \
  "$base/basemap/$version/overview.pmtiles.bin")
grep -q ' 206 ' <<< "$range_headers"
grep -qi '^content-range: bytes 0-126/' <<< "$range_headers"
grep -qi '^content-length: 127' <<< "$range_headers"
grep -qi '^cache-control: public, max-age=31536000, immutable' <<< "$range_headers"
if grep -qi '^content-encoding:' <<< "$range_headers"; then
  echo "PMTiles byte ranges must not be HTTP-compressed" >&2
  exit 1
fi
curl -fsS "$base/.well-known/dnt-policy.txt" | grep -q 'Global Privacy Control'
test "$(curl -sS -o /dev/null -w '%{http_code}' "$base/does-not-exist")" = 404
curl -fsS "$base/search/cross-of-gold?from=1896-01-01&to=1896-12-31" | grep -q 'property="og:image" content="https://usnewsmap.com/share-image.png?'
curl -fsS --max-time 45 "$base/share-image.png?page=%2F&q=cross+of+gold&from=1896-01-01&to=1896-12-31" |
  python3 -c 'import struct, sys; b = sys.stdin.buffer.read(); assert b[:8] == b"\x89PNG\r\n\x1a\n"; assert struct.unpack(">II", b[16:24]) == (1200, 630)'
echo "api image serves /v1 and the site ($asset)"
