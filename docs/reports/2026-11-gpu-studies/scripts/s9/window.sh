#!/usr/bin/env bash
# Study 9 on a BOINC GPU host: wait until no GPU task executes, suspend BOINC GPU work for at most --seconds,
# run the job in the vLLM image, sample the GPU every 10 s, then set GPU mode back to auto.
# --work DIR (holds in/ with job.sh), --image REF, --seconds N, --log FILE.
set -uo pipefail
WORK=""; IMAGE=""; SECS=7200; LOG=""
while [ $# -gt 0 ]; do case "$1" in --work) WORK=$2; shift 2;; --image) IMAGE=$2; shift 2;; --seconds) SECS=$2; shift 2;;
  --log) LOG=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$WORK" ] && [ -n "$IMAGE" ] && [ -n "$LOG" ] && [ -f "$WORK/in/job.sh" ] || { echo "need --work (with in/job.sh), --image and --log" >&2; exit 2; }
[[ "$SECS" =~ ^[0-9]+$ ]] && [ "$SECS" -gt 600 ] || { echo "--seconds must be an integer over 600" >&2; exit 2; }
log() { echo "$(date -Is) $*" >>"$LOG"; }
busy() {
  boinccmd --get_tasks | awk '/^[[:space:]]*name: / { name=$2 } /active_task_state: EXECUTING/ && name ~ /^usnm_qwen_/ { b=1 } END { exit !b }' && return 0
  [ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' ')" -gt 300 ]
}
w0=$(date +%s)
while busy; do
  [ $(( $(date +%s) - w0 )) -gt 3600 ] && { log "gave up waiting for an idle GPU"; exit 3; }
  sleep 20
done
boinccmd --set_gpu_mode never "$SECS" || { log "failed to suspend BOINC GPU work"; exit 4; }
S=""
restore() {
  [ -n "$S" ] && kill "$S" 2>/dev/null
  podman rm -f s9-xiaomi >/dev/null 2>&1
  if boinccmd --set_gpu_mode auto; then log "window closed: gpu_mode auto"; else log "failed to restore gpu_mode auto"; fi
}
trap restore EXIT
log "window open: gpu_mode never $SECS"
sleep 5
busy && { log "GPU busy after suspending; not starting"; exit 3; }
mkdir -p "$WORK/out"
( while sleep 10; do nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits | tr -d ' ' | sed "s/^/$(date +%s),/" >>"$WORK/out/gpu-samples.csv"; done ) & S=$!
s=$(date +%s)
timeout --kill-after=30 $(( SECS - 120 )) podman run --rm --name s9-xiaomi --device nvidia.com/gpu=all --security-opt=label=disable \
  --network=host --ipc=host --entrypoint bash -e HOME=/tmp -v "$WORK:/work" "$IMAGE" /work/in/job.sh >"$WORK/out/container.log" 2>&1
log "job exit $? after $(( $(date +%s) - s ))s"
