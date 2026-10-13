#!/usr/bin/env bash
# Watches Azure Scheduled Events. When this VM is about to go away (Spot eviction
# is the "Preempt" event; Azure says it tries to deliver it up to 30 seconds
# ahead, best effort), it tells the BOINC client to stop computing, so tasks
# write a checkpoint and quit, then stops the client service. The event is NOT
# approved: acknowledging an event only makes it happen sooner. A task that is
# lost anyway is sent again by BOINC after its deadline (docs/interface.md).
set -euo pipefail

POLL_SECS=5 DATA_DIR=''
while [ $# -gt 0 ]; do
  case "$1" in
    --poll-secs) POLL_SECS=$2; shift 2 ;;
    --data-dir) DATA_DIR=$2; shift 2 ;;
    *) echo "boinc-evict: unknown option $1" >&2; exit 2 ;;
  esac
done
# boinccmd reads gui_rpc_auth.cfg from its working directory, which is the
# client's data directory (the home of the 'boinc' user on Debian).
[ -n "$DATA_DIR" ] || DATA_DIR=$(getent passwd boinc | cut -d: -f6)
[ -d "$DATA_DIR" ] || { echo "boinc-evict: no data directory" >&2; exit 2; }

IMDS_HOST=169.254.169.254
URL="http://$IMDS_HOST/metadata/scheduledevents?api-version=2020-07-01"
stopped=0
while true; do
  if body=$(curl --silent --max-time 5 -H 'Metadata: true' "$URL"); then
    hit=$(jq -r '[.Events[]? | select(.EventType == "Preempt" or .EventType == "Terminate" or .EventType == "Reboot" or .EventType == "Redeploy")] | length' <<< "$body" 2> /dev/null || echo 0)
    if [ "${hit:-0}" -gt 0 ] && [ "$stopped" = 0 ]; then
      echo "boinc-evict: scheduled event received; suspending BOINC, then stopping the client"
      stopped=1
      # "never" suspends computing: running tasks checkpoint and leave memory.
      (cd "$DATA_DIR" && timeout 10 boinccmd --set_run_mode never) || echo "boinc-evict: boinccmd failed" >&2
      sleep 8
      # SIGTERM makes the client quit and tell its tasks to quit (client/main.cpp).
      systemctl stop boinc-client.service || true
    fi
  fi
  sleep "$POLL_SECS"
done
