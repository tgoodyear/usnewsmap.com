#!/usr/bin/env bash
# On a BOINC GPU host: the usnm_qwen 1.20 bundle's llama-server (Vulkan, 2060 SUPER) with Study 9's Xiaomi GGUF, app-like flags; reads
# the 612 Study 9 crops and the 84 de-rz regions over 2,048 px at native size. --work DIR. Run with BOINC GPU work suspended.
set -uo pipefail
W=""; while [ $# -gt 0 ]; do case "$1" in --work) W=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$W" ] || { echo "need --work" >&2; exit 2; }
L=$W/bundle; log() { echo "$(date -Is) $*" >>"$W/progress.txt"; }
[ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' ')" -le 300 ] || { log "GPU busy"; exit 3; }
LD_LIBRARY_PATH=$L $L/llama-server --version >"$W/llama.version" 2>&1
( cd $L && MTMD_BACKEND_DEVICE=Vulkan0 LD_LIBRARY_PATH=$L exec ./llama-server -m $W/gguf/xiaomi-ocr-0-q8_0.gguf --mmproj $W/gguf/mmproj-xiaomi-ocr-0-f16.gguf \
  --host 127.0.0.1 --port 8093 --ctx-size 16384 --temp 0 --seed 42 --parallel 1 --no-webui -ngl 99 -t 4 --device Vulkan0 ) >"$W/llama-server.log" 2>&1 &
S=$!; trap 'kill $S 2>/dev/null; wait $S 2>/dev/null' EXIT
for _ in $(seq 1 150); do curl -sf http://127.0.0.1:8093/health >/dev/null && break; kill -0 $S 2>/dev/null || { log "server exited"; exit 4; }; sleep 2; done
log "ready"
( while sleep 10; do nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits | tr -d ' ' | sed "s/^/$(date +%s),/" >>"$W/gpu-samples.csv"; done ) & G=$!
s=$(date +%s); python3 -I $W/v5read.py --list $W/s9/crops.jsonl --root $W/s9 --endpoint http://127.0.0.1:8093/v1 --out $W/s9-612.jsonl; log "s9 exit $? after $(( $(date +%s)-s ))s"
s=$(date +%s); python3 -I $W/v5read.py --list $W/rz84/crops.jsonl --root $W/rz84 --endpoint http://127.0.0.1:8093/v1 --out $W/rz84.jsonl; log "rz84 exit $? after $(( $(date +%s)-s ))s"
kill $G 2>/dev/null; log "done"
