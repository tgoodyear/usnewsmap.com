#!/bin/sh
# Experiment driver for #736, run on the Proxmox host that owns the Iris Xe (BOINC stopped).
#   iris-bench.sh LABEL COUNT N [cache-key [offset]]
# Runs ocr/boinc/iris/bench.py in the pinned worker image inside CT $CT with the worker's limits
# (job.toml create_args: read-only root, no network, 9 GiB, tmpfs /tmp), while sampling the whole
# trial from outside the container: GPU engines/frequency (intel_gpu_top), CPU and memory (vmstat).
# Output: $OUT/LABEL.{json,gpu.json,vmstat.txt,time.txt}. A cache-key mounts a persistent writable
# CACHE_DIR (cold the first time; delete $CACHE/KEY to reset).
set -eu
LABEL=$1 COUNT=$2 N=$3 KEY=${4:-} OFFSET=${5:-0}
CT=${CT:-110}
IMAGE=${IMAGE:?set IMAGE to the worker image id}
OUT=${OUT:-/root/iris-bench}
INPUTS=${INPUTS:-/srv/bench/in}
APP=${APP:-/srv/bench/app}
mkdir -p "$OUT"
CACHE_ARGS="" BENCH_CACHE=""
if [ -n "$KEY" ]; then
  pct exec "$CT" -- mkdir -p "/srv/bench/cache/$KEY"
  CACHE_ARGS="-v /srv/bench/cache/$KEY:/cache:rw"
  BENCH_CACHE="--cache-dir /cache"
fi
intel_gpu_top -J -s 500 -o "$OUT/$LABEL.gpu.json" &
GPU_PID=$!
vmstat -t 1 > "$OUT/$LABEL.vmstat.txt" &
VM_PID=$!
trap 'kill $GPU_PID $VM_PID 2>/dev/null || true' EXIT
START=$(date +%s.%N)
pct exec "$CT" -- sh -c "mkdir -p /srv/bench/out && docker run --rm --name iris-bench --network=none --read-only \
  --cap-drop=ALL --security-opt=no-new-privileges --device=/dev/dri/renderD128 --memory=9g --memory-swap=9g \
  --cpus=2 --pids-limit=512 --tmpfs=/tmp:rw,size=1g \
  -v $INPUTS:/inputs:ro -v $APP:/bench/app:ro -v /srv/bench/out:/out:rw $CACHE_ARGS \
  $IMAGE /opt/iris/venv/bin/python -I /bench/app/bench.py --inputs /inputs --count $COUNT --offset $OFFSET --n $N \
  --label $LABEL --out /out/$LABEL.json $BENCH_CACHE" 2> "$OUT/$LABEL.stderr.txt"
END=$(date +%s.%N)
echo "$START $END" > "$OUT/$LABEL.time.txt"
pct exec "$CT" -- cat "/srv/bench/out/$LABEL.json" > "$OUT/$LABEL.json"
