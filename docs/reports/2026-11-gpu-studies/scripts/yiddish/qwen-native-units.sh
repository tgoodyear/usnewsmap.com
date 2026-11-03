#!/usr/bin/env bash
# Runs usnm_qwen units through the installed native app outside BOINC, in a slot laid out the way the
# client lays one out (open names, init_data.xml with the plan class). Reads nothing from BOINC state.
# --units DIR (each subdir holds qwen.json + page.jpg), --app-dir DIR (the project directory),
# --app NAME (the app version file name), --plan-class CLASS, --scratch DIR, --until EPOCH (hard deadline),
# --boinc-until EPOCH (timed GPU suspension expiry) or --no-boinc (standalone host),
# --unit-seconds N (expected unit duration),
# --gpu-log FILE,
# --preload FILE (unused: the app is static-pie). The app runs under strace, which answers its unlink calls
# with success without deleting, so the final qwen_checkpoint.json (every crop's generation record) stays.
# On BOINC hosts use gpu-window.sh: this runner neither verifies nor changes GPU mode.
# --no-boinc is only for a host without BOINC GPU work.
# --internal-supervised is reserved for the enclosing supervisor, not direct invocation.
set -uo pipefail
ORIGINAL_ARGS=("$@")
UNITS=""; APPDIR=""; APP=""; PLAN=""; SCRATCH=""; UNTIL=0; BOINC_UNTIL=0; BOINC_MODE=""; UNIT_S=120; GPULOG=""; SUPERVISED=0
while [ $# -gt 0 ]; do case "$1" in --units) UNITS=$2; shift 2;; --app-dir) APPDIR=$2; shift 2;; --app) APP=$2; shift 2;; --plan-class) PLAN=$2; shift 2;; --scratch) SCRATCH=$2; shift 2;; --until) UNTIL=$2; shift 2;; --boinc-until) [ -z "$BOINC_MODE" ] || { echo "choose one BOINC mode" >&2; exit 2; }; BOINC_MODE=timed; BOINC_UNTIL=$2; shift 2;; --no-boinc) [ -z "$BOINC_MODE" ] || { echo "choose one BOINC mode" >&2; exit 2; }; BOINC_MODE=none; shift;; --unit-seconds) UNIT_S=$2; shift 2;; --gpu-log) GPULOG=$2; shift 2;; --preload) shift 2;; --internal-supervised) SUPERVISED=1; shift;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$UNITS" ] && [ -n "$APPDIR" ] && [ -n "$APP" ] && [ -n "$PLAN" ] && [ -n "$SCRATCH" ] && [ -n "$GPULOG" ] || { echo "missing required argument" >&2; exit 2; }
[[ "$UNTIL" =~ ^[0-9]+$ && "$BOINC_UNTIL" =~ ^[0-9]+$ && "$UNIT_S" =~ ^[0-9]+$ ]] || { echo "deadlines and unit time must be nonnegative integers" >&2; exit 2; }
UNTIL=$((10#$UNTIL)); BOINC_UNTIL=$((10#$BOINC_UNTIL)); UNIT_S=$((10#$UNIT_S))
[ "$UNTIL" -gt 0 ] && [ "$UNIT_S" -gt 0 ] || { echo "a deadline and positive unit time are required" >&2; exit 2; }
case "$BOINC_MODE" in
  timed)
    [ "$BOINC_UNTIL" -ge "$UNTIL" ] && [ $((BOINC_UNTIL - UNTIL)) -ge 60 ] || { echo "BOINC suspension must end at least 60 seconds after the deadline" >&2; exit 2; }
    boinc_args=(--boinc-until "$BOINC_UNTIL")
    ;;
  none) boinc_args=(--no-boinc);;
  *) echo "specify --boinc-until or --no-boinc" >&2; exit 2;;
esac
HERE=$(cd "$(dirname "$0")" && pwd)
if [ "$SUPERVISED" -eq 0 ]; then
  exec python3 -I "$HERE/run-deadline.py" --until "$UNTIL" "${boinc_args[@]}" -- "$HERE/qwen-native-units.sh" --internal-supervised "${ORIGINAL_ARGS[@]}"
fi
[ "${USNM_DEADLINE_PARENT_PID:-}" = "$PPID" ] || { echo "internal mode requires an enclosing deadline supervisor" >&2; exit 2; }
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
  (cd "$s" && exec python3 -I "$HERE/run-deadline.py" --until "$UNTIL" "${boinc_args[@]}" -- strace -f --seccomp-bpf -qq -o /dev/null -e trace=unlink,unlinkat -e inject=unlink,unlinkat:retval=0 ./usnm_qwen) > "$s/stdout.txt" 2> "$s/stderr.txt" &
  RUNNER_PID=$!
  wait "$RUNNER_PID"; rc=$?; RUNNER_PID=""; t1=$(date +%s%N)
  for f in diag.txt stdout.txt stderr.txt; do [ -f "$s/$f" ] && cp "$s/$f" "$u/"; done
  if [ "$rc" -eq 0 ]; then
    for f in readings.json boinc_finish_called; do
      if [ -f "$s/$f" ] && ! cp "$s/$f" "$u/"; then echo "cannot copy $f" >&2; exit 1; fi
    done
    if [ -f "$s/qwen_checkpoint.json" ] && ! cp "$s/qwen_checkpoint.json" "$u/qwen_checkpoint.final.json"; then
      echo "cannot copy checkpoint" >&2; exit 1
    fi
    if [ -f "$s/result.json" ]; then
      partial="$u/.result.json.part"
      if ! rm -f "$partial" || ! cp "$s/result.json" "$partial" || ! mv -f "$partial" "$u/result.json"; then
        rm -f "$partial" || true
        echo "cannot publish unit result" >&2; exit 1
      fi
    fi
  fi
  echo "$(basename "$u") rc=$rc wall_ms=$(( (t1-t0)/1000000 ))" | tee -a "$UNITS/run.log"
  [ "$rc" -eq 0 ] || exit "$rc"
done
rm -rf "$SCRATCH/slot"
