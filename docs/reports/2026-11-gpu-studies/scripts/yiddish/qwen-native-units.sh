#!/usr/bin/env bash
# Runs usnm_qwen units through the installed native app outside BOINC, in a slot laid out the way the
# client lays one out (open names, init_data.xml with the plan class). Reads nothing from BOINC state.
# --units DIR (each subdir holds qwen.json + page.jpg), --app-dir DIR (the project directory),
# --app NAME (the app version file name), --plan-class CLASS, --scratch DIR, --until EPOCH (hard deadline),
# --boinc-until EPOCH (optional timed GPU suspension expiry), --unit-seconds N (expected unit duration),
# --gpu-log FILE,
# --preload FILE (unused: the app is static-pie). The app runs under strace, which answers its unlink calls
# with success without deleting, so the final qwen_checkpoint.json (every crop's generation record) stays.
set -uo pipefail
UNITS=""; APPDIR=""; APP=""; PLAN=""; SCRATCH=""; UNTIL=0; BOINC_UNTIL=0; UNIT_S=120; GPULOG=""
while [ $# -gt 0 ]; do case "$1" in --units) UNITS=$2; shift 2;; --app-dir) APPDIR=$2; shift 2;; --app) APP=$2; shift 2;; --plan-class) PLAN=$2; shift 2;; --scratch) SCRATCH=$2; shift 2;; --until) UNTIL=$2; shift 2;; --boinc-until) BOINC_UNTIL=$2; shift 2;; --unit-seconds) UNIT_S=$2; shift 2;; --gpu-log) GPULOG=$2; shift 2;; --preload) shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$UNITS" ] && [ -n "$APPDIR" ] && [ -n "$APP" ] && [ -n "$PLAN" ] && [ -n "$SCRATCH" ] && [ -n "$GPULOG" ] || { echo "missing required argument" >&2; exit 2; }
[[ "$UNTIL" =~ ^[0-9]+$ && "$BOINC_UNTIL" =~ ^[0-9]+$ && "$UNIT_S" =~ ^[0-9]+$ ]] || { echo "deadlines and unit time must be nonnegative integers" >&2; exit 2; }
UNTIL=$((10#$UNTIL)); BOINC_UNTIL=$((10#$BOINC_UNTIL)); UNIT_S=$((10#$UNIT_S))
[ "$UNTIL" -gt 0 ] && [ "$UNIT_S" -gt 0 ] || { echo "a deadline and positive unit time are required" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$SCRATCH"
nvidia-smi --query-gpu=timestamp,memory.used,utilization.gpu --format=csv,noheader -lms 1000 >> "$GPULOG" 2>&1 &
SMI=$!; RUNNER_PID=""
cleanup() {
  local status=$?
  trap - EXIT
  trap '' HUP INT TERM
  if [ -n "$RUNNER_PID" ]; then
    kill -TERM "$RUNNER_PID" 2>/dev/null || true
    wait "$RUNNER_PID" 2>/dev/null || true
  fi
  kill "$SMI" 2>/dev/null || true
  wait "$SMI" 2>/dev/null || true
  exit "$status"
}
trap cleanup EXIT
trap 'exit 129' HUP
trap 'exit 130' INT
trap 'exit 143' TERM
for u in "$UNITS"/b*; do
  [ -d "$u" ] || continue
  [ -f "$u/result.json" ] && continue
  now=$(date +%s); [ $((now + UNIT_S)) -gt "$UNTIL" ] && { echo "$(date -Is) stop: window ends" ; break; }
  s="$SCRATCH/slot"; rm -rf "$s"; mkdir -p "$s"
  for f in "$APPDIR/${APP}"_*; do ln -s "$f" "$s/${f#"$APPDIR/${APP}_"}"; done
  ln -s "$APPDIR/$APP" "$s/usnm_qwen"
  for m in Qwen3VL-8B-Instruct-Q4_K_M.gguf mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf; do ln -s "$(ls "$APPDIR"/usnm_qwen_*_"$m")" "$s/$m"; done
  cp "$u/qwen.json" "$u/page.jpg" "$s/"
  printf "<app_init_data>\n<plan_class>%s</plan_class>\n<gpu_type>NVIDIA</gpu_type>\n<gpu_device_num>0</gpu_device_num>\n</app_init_data>\n" "$PLAN" > "$s/init_data.xml"
  t0=$(date +%s%N)
  (cd "$s" && exec python3 -I "$HERE/run-deadline.py" --until "$UNTIL" --boinc-until "$BOINC_UNTIL" -- strace -f --seccomp-bpf -qq -o /dev/null -e trace=unlink,unlinkat -e inject=unlink,unlinkat:retval=0 ./usnm_qwen) > "$s/stdout.txt" 2> "$s/stderr.txt" &
  RUNNER_PID=$!
  wait "$RUNNER_PID"; rc=$?; RUNNER_PID=""; t1=$(date +%s%N)
  for f in diag.txt stdout.txt stderr.txt; do [ -f "$s/$f" ] && cp "$s/$f" "$u/"; done
  if [ "$rc" -eq 0 ]; then
    for f in readings.json result.json boinc_finish_called; do [ -f "$s/$f" ] && cp "$s/$f" "$u/"; done
    [ -f "$s/qwen_checkpoint.json" ] && cp "$s/qwen_checkpoint.json" "$u/qwen_checkpoint.final.json"
  fi
  echo "$(basename "$u") rc=$rc wall_ms=$(( (t1-t0)/1000000 ))" | tee -a "$UNITS/run.log"
  [ "$rc" -eq 0 ] || exit "$rc"
done
rm -rf "$SCRATCH/slot"
