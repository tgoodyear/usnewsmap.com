#!/usr/bin/env bash
# Build the API and start it in the background on its baked synthetic
# fixtures (127.0.0.1:8080, logging to api.log), then wait until it's ready.
# It also serves the site from web/dist, as the image does, once
# web-e2e.sh has built it (files are read per request). In CI the build
# was started in the background earlier; cargo waits for it here.
#
# A search containing "slowsearch" takes 4 s and a visitor waits 1 s, so
# the API answers 202 and the app waits for the result, as for a cold search
# on the real corpus (web/e2e/slow.spec.ts).
set -euo pipefail
cargo build --release --locked -p usnm-api
USNM_BIND=127.0.0.1:8080 USNM_RATE_PER_MIN=0 USNM_SITE_DIR=web/dist \
  USNM_SEARCH_TIMEOUT_SECS=1 USNM_FIXTURE_SLOW_TERM=slowsearch USNM_FIXTURE_SLOW_MS=4000 \
  ./target/release/usnm-api > api.log 2>&1 &
for _ in $(seq 1 30); do
  curl -sf http://127.0.0.1:8080/readyz && exit 0
  sleep 1
done
echo "::error::the API did not become ready (see api.log)"
exit 1
