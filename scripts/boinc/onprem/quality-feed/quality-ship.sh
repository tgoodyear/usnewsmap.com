#!/usr/bin/env bash
# Ships the quality feeder's chunks from pve-laptop to the BOINC server and submits them (#962).
# Runs on pve-laptop as root (usnm-quality-ship.service, from its timer), over the host trust
# pve-laptop -> pve (home-infra scripts/host-ssh-trust.sh). No usnm data leaves the LAN.
#
#   quality-ship.sh --campaign NAME --engine-image sha256:<worker digest>
#     [--outbox /var/lib/usnm-quality-feed/outbox] [--server-host pve.tjg.fyi] [--server-ct 230]
#     [--readers loc-hist,as,ours-ndlocr,ours-paddle,ours-surya,ours-tesseract]
#     [--bundle-size 200] [--max-unsent 300] [--max-chunks 1] [--min-free-gb 40] [--what-if]
#     [--qwen-engine-plan /project/.../plan.json --qwen-models /project/.../models]
#     [--qwen-max-ready 1000] (fresh native original replicas, not retry replicas)
#     [--step-window-minutes 15] (native publication only; 0, the default, is no window)
#
# For each READY chunk under <outbox>/<campaign>/, oldest first, at most --max-chunks a run:
#  1. Stops while usnm_quality has more than --max-unsent results ready to send (the public
#     server_status.php?xml=1), or while CT230's
#     project tree has less than --min-free-gb free. Native Qwen instead preflights strict
#     fresh-pair capacity plus the whole READY reservation before transfer (exit 75 defers).
#  2. Copies the chunk into CT230's <share>/quality/<campaign>/incoming/<chunk>/ (tar over ssh).
#  3. Runs quality_receive.py there: every file against SHA256SUMS, every page's scan present
#     (so quality-stage never falls back to tile.loc.gov), then files the scans and texts.
#  4. usnm-boinc-step quality-stage with QUALITY_SCANS_DIR, then quality-submit.
#  5. quality_receive.py --after-submit frees the server's copies it no longer needs (the
#     received scans and texts, the per-page units' images), then the chunk leaves the
#     outbox, so the feeder makes the next one.
# A step that fails leaves the chunk in the outbox for the next run; staging and submitting
# skip what is already done. quality-stage has its own lock on the host and never waits for
# the controller. quality-submit takes the lock the controller's steps take: it tries for 30
# minutes, then the run exits 75 with the chunk still READY and the timer tries again.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
log() { echo "quality-ship: $*" >&2; }
HERE=$(cd "$(dirname "$0")" && pwd)

CAMPAIGN="" ENGINE="" OUTBOX=/var/lib/usnm-quality-feed/outbox SERVER=pve.tjg.fyi CT=230
READERS=loc-hist,as,ours-ndlocr,ours-paddle,ours-surya,ours-tesseract
BUNDLE=200 MAX_UNSENT=300 MAX_CHUNKS=1 MIN_FREE=40 WHAT_IF=false
SHARE=/srv/usnm-boinc/project STEP=/usr/local/sbin/usnm-boinc-step
QWEN_PLAN="" QWEN_MODELS="" QWEN_MAX_READY=1000 STEP_WINDOW=0
STATUS_URL="https://boinc.usnewsmap.com/server_status.php?xml=1"
while [ $# -gt 0 ]; do
  case $1 in
    --campaign) CAMPAIGN=${2:-}; shift 2 ;;
    --engine-image) ENGINE=${2:-}; shift 2 ;;
    --outbox) OUTBOX=${2:-}; shift 2 ;;
    --server-host) SERVER=${2:-}; shift 2 ;;
    --server-ct) CT=${2:-}; shift 2 ;;
    --readers) READERS=${2:-}; shift 2 ;;
    --bundle-size) BUNDLE=${2:-}; shift 2 ;;
    --max-unsent) MAX_UNSENT=${2:-}; shift 2 ;;
    --max-chunks) MAX_CHUNKS=${2:-}; shift 2 ;;
    --min-free-gb) MIN_FREE=${2:-}; shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    --step-window-minutes) STEP_WINDOW=${2:-}; shift 2 ;;
    --qwen-engine-plan) QWEN_PLAN=${2:-}; shift 2 ;;
    --qwen-models) QWEN_MODELS=${2:-}; shift 2 ;;
    --qwen-max-ready)
      [ $# -ge 2 ] || die "--qwen-max-ready: a positive integer"
      QWEN_MAX_READY=$2; shift 2 ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $CAMPAIGN =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || die "--campaign: 1-41 of [a-z0-9-]"
[[ $ENGINE =~ ^sha256:[0-9a-f]{64}$ ]] || die "--engine-image: sha256:<64 hex>"
[[ $READERS =~ ^[a-z0-9_,-]+$ ]] || die "--readers: comma-separated reader names"
[[ $BUNDLE =~ ^[0-9]+$ && $MAX_UNSENT =~ ^[0-9]+$ && $MAX_CHUNKS =~ ^[0-9]+$ && $CT =~ ^[0-9]+$ && $MIN_FREE =~ ^[0-9]+$ ]] ||
  die "--bundle-size, --max-unsent, --max-chunks, --min-free-gb and --server-ct: numbers"
[[ $SERVER =~ ^[a-z0-9][a-z0-9.-]*$ ]] || die "--server-host: a host name"
[[ $QWEN_MAX_READY =~ ^[1-9][0-9]*$ ]] || die "--qwen-max-ready: a positive integer"
[[ $STEP_WINDOW =~ ^[0-9]{1,2}$ && $((10#$STEP_WINDOW)) -lt 60 ]] || die "--step-window-minutes: 0 to 59"
STEP_WINDOW=$((10#$STEP_WINDOW))
if [ -n "$QWEN_PLAN$QWEN_MODELS" ]; then
  [[ $QWEN_PLAN =~ ^/project/[a-zA-Z0-9_./-]+$ && $QWEN_MODELS =~ ^/project/[a-zA-Z0-9_./-]+$ ]] ||
    die "--qwen-engine-plan and --qwen-models: absolute /project paths required together"
fi
if [ -n "$QWEN_PLAN" ]; then
  # Migration-safe: old inline and new independent ship units cannot copy/clean one chunk concurrently.
  exec 9>"$OUTBOX/.qwen-ship-$CAMPAIGN.lock"
  flock -n 9 || exit 75
fi
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$SERVER")
in_ct() {
  local quoted
  printf -v quoted '%q ' "$@"
  "${SSH[@]}" "pct exec $CT -- $quoted"
}
# With --step-window-minutes N, native publication starts only in the first N minutes of an
# hour, which leaves the rest of the hour to the priority controller. Sets LOCK_WAIT to the
# seconds left to take the lock, so a queued step can't start after the window closes.
wait_for_step_window() {
  LOCK_WAIT=900
  [ "$STEP_WINDOW" -gt 0 ] || return 0
  local now minute second waited=false
  while now=$(date +%M:%S) && minute=$((10#${now%:*})) && second=$((10#${now#*:})) &&
    [ "$minute" -ge "$STEP_WINDOW" ]; do
    [ "$waited" = true ] || log "waiting for the next step window"
    waited=true
    sleep 30
  done
  LOCK_WAIT=$((STEP_WINDOW * 60 - minute * 60 - second))
}
# One bridge step, waiting its turn: the step that holds its lock may run for a while, and the
# priority controller's steps come every few minutes. Tries for up to 30 minutes, then defers
# to the next run (exit 75) rather than waiting into the service's timeout.
run_step() {
  local i err
  err="$OUTBOX/.step-$CAMPAIGN-$$.err"
  (set -o noclobber; : > "$err") || die "step log already exists"
  for i in $(seq 60); do
    if in_ct "$STEP" --env prod --target bridge "$@" > /dev/null 2> "$err"; then
      rm -f "$err"
      return 0
    fi
    grep -q 'another bridge step is running' "$err" || { cat "$err" >&2; rm -f "$err"; return 1; }
    [ "$i" -eq 1 ] && log "the step lock is held; waiting"
    sleep 30
  done
  rm -f "$err"
  log "$2: the lock stayed held for 30 minutes; READY retained"
  exit 75
}

shipped=0
for dir in "$OUTBOX/$CAMPAIGN"/c[0-9][0-9][0-9][0-9][0-9]; do
  [ -f "$dir/READY" ] || continue
  [ "$shipped" -lt "$MAX_CHUNKS" ] || break
  chunk=$(basename "$dir")
  pages=$(cat "$dir/READY")
  if [ -n "$QWEN_PLAN" ]; then
    [[ $pages =~ ^[1-9][0-9]*$ && $pages -le 490 ]] || die "$chunk: invalid bounded READY page count"
  fi
  unsent=native-fresh-pair-gated
  if [ -z "$QWEN_PLAN" ]; then
    unsent=$(curl -fsS --max-time 30 -A usnewsmap-quality-ship/1 "$STATUS_URL" |
      python3 -c '
import sys
import xml.etree.ElementTree as ET
root = ET.parse(sys.stdin).getroot()
apps = [app for app in root.findall("./database_file_states/tasks_by_app/app")
        if app.findtext("name") == "usnm_quality"]
if len(apps) != 1:
    raise SystemExit("Expected exactly one usnm_quality application in server status")
count = apps[0].findtext("unsent", "")
if not count.isascii() or not count.isdecimal():
    raise SystemExit("Invalid usnm_quality unsent count in server status")
print(count)
')
    [[ $unsent =~ ^[0-9]+$ ]] || die "couldn't read usnm_quality's unsent count"
    if [ "$unsent" -gt "$MAX_UNSENT" ]; then
      log "usnm_quality has $unsent results unsent (over $MAX_UNSENT); $chunk waits"
      break
    fi
  fi
  free=$(in_ct df --output=avail -B1G "$SHARE" "$SHARE/quality" "$SHARE/usnmocr/download" |
    tail -n +2 | sort -n | head -1 | tr -d ' G')
  [[ $free =~ ^[0-9]+$ ]] || die "couldn't read CT $CT's free space"
  if [ "$free" -lt "$MIN_FREE" ]; then
    log "CT $CT has $free GB free (under $MIN_FREE); $chunk waits"
    break
  fi
  if [ -n "$QWEN_PLAN" ]; then
    pool=$("${SSH[@]}" lvs --reportformat json --units b --nosuffix -o lv_size,data_percent pve/data |
      python3 -c 'import json,sys; p=json.load(sys.stdin)["report"][0]["lv"][0]; print(int(float(p["lv_size"])*(1-float(p["data_percent"])/100)))')
    need=$((MIN_FREE * 1024**3 + pages * 64 * 1024**2))
    if [ "$pool" -lt "$need" ]; then
      log "thin pool has $pool bytes free, needs $need including per-page reservation; $chunk waits"
      break
    fi
    if in_ct podman exec usnm-boinc-server python3 /project/ops/qwen-feed/qwen_submit.py \
      --preflight --campaign "$CAMPAIGN" --engine-plan "$QWEN_PLAN" --models "$QWEN_MODELS" \
      --max-pages "$pages" --max-ready "$QWEN_MAX_READY"; then
      :
    else
      rc=$?
      [ "$rc" -ne 75 ] || log "$chunk: preflight deferred; transferred 0 pages; READY retained"
      exit "$rc"
    fi
  fi
  log "$chunk: $pages pages, server unsent $unsent"
  if $WHAT_IF; then
    log "would copy $dir to CT $CT on $SERVER, stage it and submit it"
    shipped=$((shipped + 1))
    continue
  fi
  inc="$SHARE/quality/$CAMPAIGN/incoming"
  in_ct mkdir -p "$inc"
  tar -C "$OUTBOX/$CAMPAIGN" -cf - "$chunk" | in_ct tar -x --no-same-owner -C "$inc"
  out=$("${SSH[@]}" pct exec "$CT" -- python3 - --campaign "$CAMPAIGN" --chunk "$chunk" \
    < "$HERE/quality_receive.py")
  IFS=$'\t' read -r sample langs n <<< "$out"
  [[ $sample =~ ^/project/quality/$CAMPAIGN/incoming/$chunk/sample.jsonl$ && $langs =~ ^[a-z,]+$ && $n =~ ^[0-9]+$ ]] ||
    die "$chunk: unexpected receive output: $out"
  log "$chunk: received $n pages ($langs); staging"
  run_step --step quality-stage \
    --var "QUALITY_CAMPAIGN=$CAMPAIGN" --var "QUALITY_SAMPLE=$sample" --var "QUALITY_GROUP=$CAMPAIGN" \
    --var "QUALITY_SCANS_DIR=/project/quality/$CAMPAIGN/scans" --var "QUALITY_READERS=$READERS" \
    --var "QUALITY_ENGINE_IMAGE=$ENGINE" --var "QUALITY_LANGS=$langs" \
    --var "QUALITY_BUNDLE_SIZE=$BUNDLE" --var "QUALITY_MAX=$((n + 10))" --wait-mins 45
  log "$chunk: staged; submitting"
  if [ -n "$QWEN_PLAN" ]; then
    # Native admission, shared models, scientific identities and DB quorum are checked
    # before cleanup; failures leave the original READY chunk and receipts intact.
    wait_for_step_window
    log "$chunk: native submission waiting for normal project mutex"
    if in_ct flock -w "$LOCK_WAIT" -E 75 /run/usnm-boinc-step.lock sh -c \
      'echo "quality-ship: native project mutex acquired" >&2; exec "$@"' sh \
      podman exec usnm-boinc-server python3 /project/ops/qwen-feed/qwen_submit.py \
      --campaign "$CAMPAIGN" --engine-plan "$QWEN_PLAN" --models "$QWEN_MODELS" \
      --sample "$sample" --max-pages "$n" --max-ready "$QWEN_MAX_READY"; then
      :
    else
      rc=$?
      [ "$rc" -ne 75 ] || log "$chunk: native publication deferred; READY retained; no acknowledgement"
      exit "$rc"
    fi
  fi
  run_step --step quality-submit \
    --var "QUALITY_CAMPAIGN=$CAMPAIGN" --var "QUALITY_MAX=$(((n + BUNDLE - 1) / BUNDLE + 5))" --wait-mins 30
  freed=$("${SSH[@]}" pct exec "$CT" -- python3 - --campaign "$CAMPAIGN" --chunk "$chunk" --after-submit \
    < "$HERE/quality_receive.py")
  rm -rf "$dir"
  log "$chunk: submitted, $freed on the server, and removed from the outbox"
  shipped=$((shipped + 1))
done
log "shipped $shipped chunk(s)"
