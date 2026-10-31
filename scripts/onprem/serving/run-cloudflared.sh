#!/usr/bin/env bash
# Runs the usnm-serving tunnel's connector (#955) with every query string
# cut from its log lines. install.sh installs it as
# /usr/local/libexec/usnm-serving/run-cloudflared, and
# cloudflared-usnm-serving.service runs it with TUNNEL_TOKEN in its
# environment.
#
#   run-cloudflared
#
# At --loglevel info cloudflared logs no request, but a request that fails
# to reach Caddy is logged at error with its URL (dest=..., and the HTTP
# client's error quotes it), and a search's URL holds the search text in
# q=. 09 §9.4.2 allows neither in a log, so sed removes everything from
# a "?" to the next space or quote before the journal sees the line.
# cloudflared itself is exec'd, so it stays the unit's main process.
set -euo pipefail
metrics=${TUNNEL_METRICS:-127.0.0.1:20241}
[[ $metrics =~ ^127\.0\.0\.1:([0-9]{1,5})$ ]] &&
  [ "${BASH_REMATCH[1]}" -gt 0 ] && [ "${BASH_REMATCH[1]}" -le 65535 ] ||
  { echo "error: TUNNEL_METRICS must be a loopback address and valid port" >&2; exit 1; }
exec /usr/bin/cloudflared --no-autoupdate tunnel --loglevel info --edge-ip-version 4 \
  --metrics "$metrics" run > >(exec sed -u -E 's/\?[^"\\[:space:]]*/?[query removed]/g') 2>&1
