#!/usr/bin/env bash
# One bounded GPU window on this BOINC host: wait until no GPU task executes, set GPU mode never for
# --seconds (BOINC reverts it by itself after that), run units, then set GPU mode auto at once.
# --seconds N, --units DIR (repeatable, run in order), --unit-seconds N, --log FILE, --wait-max N.
set -uo pipefail
SECS=2400; UNIT_S=120; LOG=""; WAITMAX=3600; UNITS=()
while [ $# -gt 0 ]; do case "$1" in --seconds) SECS=$2; shift 2;; --units) UNITS+=("$2"); shift 2;; --unit-seconds) UNIT_S=$2; shift 2;; --log) LOG=$2; shift 2;; --wait-max) WAITMAX=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$LOG" ] && [ ${#UNITS[@]} -gt 0 ] || { echo "need --log and --units" >&2; exit 2; }
D=$(cd "$(dirname "$0")/.." && pwd)
busy() { boinccmd --get_tasks | awk "/^ *name: /{n=\$2} /active_task_state: EXECUTING/{print n}" | grep -q "^usnm_qwen_\|^usnm_surya_" && return 0
         [ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d " ")" -gt 300 ]; }
w0=$(date +%s); while busy; do [ $(( $(date +%s) - w0 )) -gt "$WAITMAX" ] && { echo "$(date -Is) gave up waiting for an idle GPU" >> "$LOG"; exit 3; }; sleep 20; done
boinccmd --set_gpu_mode never "$SECS"; start=$(date +%s); echo "$(date -Is) window open: gpu_mode never $SECS" >> "$LOG"
trap "boinccmd --set_gpu_mode auto; echo \"\$(date -Is) window closed: gpu_mode auto\" >> \"$LOG\"" EXIT
sleep 5
until=$((start + SECS - 60))
for u in "${UNITS[@]}"; do
  "$D/scripts/qwen-native-units.sh" --units "$u" --app-dir /var/lib/boinc-client/projects/boinc.usnewsmap.com --app usnm_qwen_1.17_x86_64-pc-linux-gnu_nvidia_vulkan_8b --plan-class nvidia_vulkan_8b --scratch "$D/scratch" --until "$until" --unit-seconds "$UNIT_S" --gpu-log "$D/gpu-$(date +%Y%m%d).csv" --preload "$D/shim/libkeep-checkpoint.so" >> "$LOG" 2>&1
  [ $(( $(date +%s) + UNIT_S )) -gt "$until" ] && break
done
