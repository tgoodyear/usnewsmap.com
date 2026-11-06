#!/usr/bin/env bash
# One bounded GPU window on this BOINC host: wait until no GPU task executes, set GPU mode never for
# --seconds, run units, then set GPU mode auto only after descendant cleanup is confirmed.
# --seconds N, --units DIR (repeatable, run in order), --unit-seconds N, --log FILE, --wait-max N.
set -uo pipefail
SECS=2400; UNIT_S=120; LOG=""; WAITMAX=3600; UNITS=()
while [ $# -gt 0 ]; do case "$1" in --seconds) SECS=$2; shift 2;; --units) UNITS+=("$2"); shift 2;; --unit-seconds) UNIT_S=$2; shift 2;; --log) LOG=$2; shift 2;; --wait-max) WAITMAX=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$LOG" ] && [ ${#UNITS[@]} -gt 0 ] || { echo "need --log and --units" >&2; exit 2; }
[[ "$SECS" =~ ^[0-9]+$ && "$UNIT_S" =~ ^[0-9]+$ && "$WAITMAX" =~ ^[0-9]+$ ]] || { echo "seconds must be nonnegative integers" >&2; exit 2; }
SECS=$((10#$SECS)); UNIT_S=$((10#$UNIT_S)); WAITMAX=$((10#$WAITMAX))
[ "$SECS" -gt 60 ] && [ "$UNIT_S" -gt 0 ] || { echo "window must exceed 60 seconds and unit time must be positive" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")" && pwd)
D=$(cd "$HERE/.." && pwd)
log() { echo "$(date -Is) $*" >> "$LOG"; }
busy() {
  local tasks memory
  tasks=$(boinccmd --get_tasks) || return 2
  if printf '%s\n' "$tasks" | awk '
    /^[[:space:]]*name: / { name=$2 }
    /active_task_state: EXECUTING/ && name ~ /^usnm_(qwen|surya)_/ { busy=1 }
    END { exit !busy }
  '; then return 0; fi
  memory=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | awk '
    { gsub(/[[:space:]]/, ""); if ($0 !~ /^[0-9]+$/) invalid=1; else { found=1; if ($0+0 > 300) busy=1 } }
    END { if (invalid || !found) exit 2; print busy ? "busy" : "idle" }
  ') || return 2
  [ "$memory" = busy ]
}
w0=$(date +%s)
while :; do
  if busy; then
    [ $(( $(date +%s) - w0 )) -gt "$WAITMAX" ] && { log "gave up waiting for an idle GPU"; exit 3; }
    sleep 20
  else
    rc=$?
    [ "$rc" -eq 1 ] && break
    log "cannot check GPU activity; not opening a window"
    exit 3
  fi
done
start=$(date +%s)
if ! boinccmd --set_gpu_mode never "$SECS"; then
  log "failed to suspend BOINC GPU work; not opening a window"
  exit 4
fi
worker_pid=""; confirmation=""
restore() {
  local status=$?
  trap - EXIT
  trap '' HUP INT TERM
  if [ -n "$worker_pid" ]; then
    kill -TERM "$worker_pid" 2>/dev/null || true
    wait "$worker_pid" 2>/dev/null || true
  fi
  if [ -n "$confirmation" ]; then
    if [ ! -f "$confirmation" ]; then
      if boinccmd --set_gpu_mode never 0; then
        log "cleanup unconfirmed: gpu_mode never without expiry; operator must inspect and resume"
      else
        log "cleanup unconfirmed: failed to extend GPU suspension; operator must intervene before expiry"
      fi
      [ "$status" -ne 0 ] || status=125
      exit "$status"
    fi
    rm -f "$confirmation"
  fi
  if boinccmd --set_gpu_mode auto; then
    log "window closed: gpu_mode auto"
  else
    log "failed to restore gpu_mode auto"
    [ "$status" -ne 0 ] || status=4
  fi
  exit "$status"
}
trap restore EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
log "window open: gpu_mode never $SECS"
sleep 5
if busy; then
  log "GPU became busy while suspending BOINC; not starting units"
  exit 3
else
  rc=$?
  if [ "$rc" -ne 1 ]; then log "cannot check GPU activity after suspension"; exit 3; fi
fi
until=$((start + SECS - 60))
mkdir -p "$D/scratch" || { log "cannot create supervisor scratch directory"; exit 4; }
for u in "${UNITS[@]}"; do
  confirmation="$D/scratch/.gpu-window-$$-$RANDOM-$RANDOM.done"
  [ ! -e "$confirmation" ] && [ ! -L "$confirmation" ] || { log "supervisor confirmation already exists"; confirmation=""; exit 4; }
  python3 -I "$HERE/run-deadline.py" --until "$until" --boinc-until "$((start + SECS))" --completion-file "$confirmation" -- "$HERE/qwen-native-units.sh" --internal-supervised --units "$u" --app-dir /var/lib/boinc-client/projects/boinc.usnewsmap.com --app usnm_qwen_1.17_x86_64-pc-linux-gnu_nvidia_vulkan_8b --plan-class nvidia_vulkan_8b --scratch "$D/scratch" --until "$until" --boinc-until "$((start + SECS))" --unit-seconds "$UNIT_S" --gpu-log "$D/gpu-$(date +%Y%m%d).csv" --preload "$D/shim/libkeep-checkpoint.so" >> "$LOG" 2>&1 &
  worker_pid=$!
  wait "$worker_pid"; rc=$?; worker_pid=""
  [ -f "$confirmation" ] || { log "supervisor exited without confirming cleanup"; exit 125; }
  completed="$confirmation"
  confirmation=""
  rm -f "$completed"
  [ "$rc" -eq 0 ] || { log "native runner failed: rc=$rc"; exit "$rc"; }
  [ $(( $(date +%s) + UNIT_S )) -gt "$until" ] && break
done
exit 0
