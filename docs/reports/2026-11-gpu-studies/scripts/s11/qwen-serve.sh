#!/usr/bin/env bash
# Study 11: Qwen3-VL 8B Q4_K_M (the usnm_qwen files) on the llama.cpp b12084 Vulkan build with the app flags
# (ctx 4096 unless --ctx, temp 0, seed 42, --parallel 1, all layers on Vulkan0, default image tokens), reading
# --in/--list one crop at a time with qwen_read.py (request version 3).
# --llama DIR --model FILE --mmproj FILE --in DIR --list FILE --out DIR --python FILE --script FILE [--ctx N] [--max-tokens N]
set -uo pipefail
LLAMA=""; MODEL=""; MM=""; IN=""; LIST=""; OUT=""; PY=""; SCRIPT=""; CTX=4096; MT=2048
while [ $# -gt 0 ]; do case "$1" in --llama) LLAMA=$2; shift 2;; --model) MODEL=$2; shift 2;; --mmproj) MM=$2; shift 2;;
  --in) IN=$2; shift 2;; --list) LIST=$2; shift 2;; --out) OUT=$2; shift 2;; --python) PY=$2; shift 2;; --script) SCRIPT=$2; shift 2;;
  --ctx) CTX=$2; shift 2;; --max-tokens) MT=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$LLAMA" ] && [ -n "$MODEL" ] && [ -n "$MM" ] && [ -n "$IN" ] && [ -n "$LIST" ] && [ -n "$OUT" ] && [ -n "$PY" ] && [ -n "$SCRIPT" ] || { echo "missing flag" >&2; exit 2; }
[ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d " ")" -le 300 ] || { echo "GPU busy" >&2; exit 3; }
mkdir -p "$OUT"; log() { echo "$(date -Is) $*" >>"$OUT/progress.txt"; }
PORT=8092
export MTMD_BACKEND_DEVICE=Vulkan0
( cd "$LLAMA" && LD_LIBRARY_PATH="$LLAMA" exec ./llama-server -m "$MODEL" --mmproj "$MM" --host 127.0.0.1 --port $PORT --ctx-size "$CTX" \
  --temp 0 --seed 42 --parallel 1 --no-webui -ngl 99 -t 4 --device Vulkan0 ) >>"$OUT/llama-server-$CTX.log" 2>&1 &
S=$!; trap "kill $S 2>/dev/null; wait $S 2>/dev/null" EXIT
LD_LIBRARY_PATH="$LLAMA" "$LLAMA/llama-server" --version >"$OUT/llama.version" 2>&1
for _ in $(seq 1 180); do curl -sf "http://127.0.0.1:$PORT/health" >/dev/null && break; kill -0 $S 2>/dev/null || { log "server exited"; exit 4; }; sleep 2; done
log "serve ready, ctx $CTX, list $LIST"
( while sleep 10; do nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits | tr -d " " | sed "s/^/$(date +%s),/" >>"$OUT/gpu-samples.csv"; done ) & G=$!
s=$(date +%s)
"$PY" "$SCRIPT" --root "$IN" --list "$LIST" --out "$OUT/readings.jsonl" --port $PORT --max-tokens "$MT" >>"$OUT/read.log" 2>&1
log "read exit $? after $(( $(date +%s) - s ))s"
kill $G 2>/dev/null
