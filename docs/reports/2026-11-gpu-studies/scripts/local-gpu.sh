#!/usr/bin/env bash
# Run a study job on the local RTX 2060 SUPER in its container: same image family and job scripts as a rental, no cost.
# Scratch stays on the container's tmpfs until output is copied; failures retain it for recovery.
set -uo pipefail
CT="";  NAME=""; IMAGE=""; PUSH=""; JOB=""; OUT=""; NEED_MB=2500; NET=none; SCRATCH=/tmp/gpu-studies
while [ $# -gt 0 ]; do case "$1" in --ct) CT=$2; shift 2;; --name) NAME=$2; shift 2;; --image) IMAGE=$2; shift 2;; --push-dir) PUSH=$2; shift 2;;
  --job) JOB=$2; shift 2;; --out) OUT=$2; shift 2;; --need-mb) NEED_MB=$2; shift 2;; --network) NET=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$CT" ] && [ -n "$IMAGE" ] && [ -n "$PUSH" ] && [ -n "$JOB" ] && [ -n "$OUT" ] || {
  echo "need --ct, --name, --image, --push-dir, --job, and --out" >&2; exit 2;
}
[[ $NAME =~ ^[A-Za-z0-9._-]+$ && $NAME != . && $NAME != .. ]] || { echo "invalid --name" >&2; exit 2; }
[ -e "$OUT" ] && { echo "--out must not exist" >&2; exit 2; }
X() { pct exec "$CT" -- "$@"; }
D=$SCRATCH/$NAME; OWNED=0; COPIED=0
cleanup() {
  local status=$?
  trap - EXIT
  if [ "$OWNED" -eq 1 ] && [ "$COPIED" -eq 1 ]; then
    if ! X rm -rf -- "$D"; then
      echo "scratch cleanup failed on container $CT: $D" >&2
      [ "$status" -ne 0 ] || status=6
    fi
  elif [ "$OWNED" -eq 1 ]; then
    echo "scratch retained on container $CT: $D (tmpfs; recover before container restart)" >&2
  fi
  exit "$status"
}
trap cleanup EXIT
until f=$(X nvidia-smi --query-gpu=memory.free --format=csv,noheader,nounits) && [ "$f" -ge "$NEED_MB" ]; do echo "$(date -Is) waiting for ${NEED_MB} MiB free, have $f"; sleep 60; done
X mkdir -p "$SCRATCH" || { echo "cannot create scratch root on container $CT: $SCRATCH" >&2; exit 6; }
X mkdir "$D" || { echo "scratch already exists or could not be created on container $CT: $D" >&2; exit 6; }
OWNED=1
X mkdir -p "$D/in" "$D/out" || { echo "cannot prepare scratch directories" >&2; exit 6; }
if ! tar -C "$PUSH" -chf - . | X tar -C "$D/in" -xf -; then echo "input transfer failed" >&2; exit 6; fi
X sh -c "cat > $D/job.sh" <"$JOB" || { echo "job transfer failed" >&2; exit 6; }
s=$(date +%s); echo "$(date -Is) [$NAME] start, $f MiB free"
( while sleep 60; do X nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits | tr -d ' ' | sed "s/^/$(date +%s),/" >>"/tmp/$NAME.gpu"; done ) & W=$!
X podman run --rm --device nvidia.com/gpu=all --security-opt=label=disable --network="$NET" -e HOME=/tmp -v "$D:/work" "$IMAGE" bash /work/job.sh >"/tmp/$NAME.log" 2>&1
e=$?; kill $W 2>/dev/null; echo "$(date -Is) [$NAME] exit $e after $(( $(date +%s)-s ))s"
mkdir -p "$OUT" || { echo "cannot create output directory: $OUT" >&2; exit 6; }
if ! X tar -C "$D/out" -cf - . | tar -C "$OUT" -xf -; then
  echo "output transfer failed; $OUT may be incomplete" >&2; exit 6
fi
COPIED=1
mv "/tmp/$NAME.gpu" "$OUT/gpu-samples.csv" 2>/dev/null; cp "/tmp/$NAME.log" "$OUT/container.log"
echo "$NAME,$CT,$s,$(date +%s),$e" >>"${GPU_STUDY_ROOT}/local-gpu.csv"
