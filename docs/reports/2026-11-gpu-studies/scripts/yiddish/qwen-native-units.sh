#!/usr/bin/env bash
# Runs usnm_qwen units through the installed native app outside BOINC, in a slot laid out the way the
# client lays one out (open names, init_data.xml with the plan class). Reads nothing from BOINC state.
# --units DIR (each subdir holds qwen.json + page.jpg), --app-dir DIR (the project directory),
# --app NAME (the app version file name), --plan-class CLASS, --scratch DIR, --until EPOCH (start no unit after),
# --unit-seconds N (expected seconds a unit; no unit starts unless that much time is left), --gpu-log FILE,
# --preload FILE (unused: the app is static-pie). The app runs under strace, which answers its unlink calls
# with success without deleting, so the final qwen_checkpoint.json (every crop's generation record) stays.
set -uo pipefail
UNITS=""; APPDIR=""; APP=""; PLAN=""; SCRATCH=""; UNTIL=0; UNIT_S=120; GPULOG=""; PRELOAD=""
while [ $# -gt 0 ]; do case "$1" in --units) UNITS=$2; shift 2;; --app-dir) APPDIR=$2; shift 2;; --app) APP=$2; shift 2;; --plan-class) PLAN=$2; shift 2;; --scratch) SCRATCH=$2; shift 2;; --until) UNTIL=$2; shift 2;; --unit-seconds) UNIT_S=$2; shift 2;; --gpu-log) GPULOG=$2; shift 2;; --preload) PRELOAD=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
for v in UNITS APPDIR APP PLAN SCRATCH GPULOG; do [ -n "${!v}" ] || { echo "missing --${v,,}" >&2; exit 2; }; done
mkdir -p "$SCRATCH"
nvidia-smi --query-gpu=timestamp,memory.used,utilization.gpu --format=csv,noheader -lms 1000 >> "$GPULOG" 2>&1 &
SMI=$!; trap "kill $SMI 2>/dev/null" EXIT
for u in $(ls -d "$UNITS"/b* | sort); do
  [ -f "$u/result.json" ] && continue
  now=$(date +%s); [ "$UNTIL" -gt 0 ] && [ $((now + UNIT_S)) -gt "$UNTIL" ] && { echo "$(date -Is) stop: window ends" ; break; }
  s="$SCRATCH/slot"; rm -rf "$s"; mkdir -p "$s"
  for f in "$APPDIR/${APP}"_*; do ln -s "$f" "$s/${f#$APPDIR/${APP}_}"; done
  ln -s "$APPDIR/$APP" "$s/usnm_qwen"
  for m in Qwen3VL-8B-Instruct-Q4_K_M.gguf mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf; do ln -s "$(ls "$APPDIR"/usnm_qwen_*_"$m")" "$s/$m"; done
  cp "$u/qwen.json" "$u/page.jpg" "$s/"
  printf "<app_init_data>\n<plan_class>%s</plan_class>\n<gpu_type>NVIDIA</gpu_type>\n<gpu_device_num>0</gpu_device_num>\n</app_init_data>\n" "$PLAN" > "$s/init_data.xml"
  t0=$(date +%s%N); (cd "$s" && strace -f --seccomp-bpf -qq -o /dev/null -e trace=unlink,unlinkat -e inject=unlink,unlinkat:retval=0 ./usnm_qwen > stdout.txt 2> stderr.txt); rc=$?; t1=$(date +%s%N)
  for f in readings.json result.json diag.txt stdout.txt stderr.txt boinc_finish_called; do [ -f "$s/$f" ] && cp "$s/$f" "$u/"; done
  [ -f "$s/qwen_checkpoint.json" ] && cp "$s/qwen_checkpoint.json" "$u/qwen_checkpoint.final.json"
  echo "$(basename $u) rc=$rc wall_ms=$(( (t1-t0)/1000000 ))" | tee -a "$UNITS/run.log"
done
rm -rf "$SCRATCH/slot"
