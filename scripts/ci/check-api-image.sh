#!/usr/bin/env bash
# The API image ($1) starts on its baked fixtures and serves both the API and
# the site: the app shell with its security headers, a precompressed hashed
# asset, and /v1.
set -euo pipefail
id=$(docker run -d -p 127.0.0.1:18080:8080 "$1")
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
echo "api image serves /v1 and the site ($asset)"
