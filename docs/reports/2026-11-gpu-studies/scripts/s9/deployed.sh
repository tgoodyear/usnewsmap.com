#!/usr/bin/env bash
# Study 9 step 2 on a BOINC GPU host: the usnm_qwen 1.17 llama-server, with the app's flags, reading the crops one at a
# time through saturate. Run with BOINC GPU work suspended.
# --llama DIR (the app's llama-server and libraries, unprefixed), --gguf DIR (model and projector), --in DIR (crops,
# crops.jsonl, read.py), --arm gpu|cpu, --out DIR, --python FILE (with saturate 0.3.0 and Pillow), --image-min-tokens N
# (optional, passed to llama-server as the app does).
set -uo pipefail
LLAMA=""; GGUF=""; IN=""; ARM=""; OUT=""; PY=""; MINTOK=()
while [ $# -gt 0 ]; do case "$1" in --llama) LLAMA=$2; shift 2;; --gguf) GGUF=$2; shift 2;; --in) IN=$2; shift 2;;
  --arm) ARM=$2; shift 2;; --out) OUT=$2; shift 2;; --python) PY=$2; shift 2;; --image-min-tokens) MINTOK=(--image-min-tokens "$2"); shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$LLAMA" ] && [ -n "$GGUF" ] && [ -n "$IN" ] && [ -n "$OUT" ] && [ -n "$PY" ] || { echo "need --llama, --gguf, --in, --arm, --out and --python" >&2; exit 2; }
case "$ARM" in gpu) NGL=99; DEV=(--device Vulkan0); export MTMD_BACKEND_DEVICE=Vulkan0;; cpu) NGL=0; DEV=(); export GGML_VK_VISIBLE_DEVICES="";; *) echo "--arm gpu or cpu" >&2; exit 2;; esac
[ -e "$OUT" ] && { echo "--out must not exist" >&2; exit 2; }
[ "$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' ')" -le 300 ] || { echo "GPU busy" >&2; exit 3; }
mkdir -p "$OUT"; log() { echo "$(date -Is) $*" >>"$OUT/progress.txt"; }
PORT=8091
( cd "$LLAMA" && LD_LIBRARY_PATH="$LLAMA" exec ./llama-server -m "$GGUF/xiaomi-ocr-0-q8_0.gguf" --mmproj "$GGUF/mmproj-xiaomi-ocr-0-f16.gguf" \
  --host 127.0.0.1 --port $PORT --ctx-size 16384 --temp 0 --seed 42 --parallel 1 --no-webui -ngl $NGL -t 4 "${DEV[@]}" "${MINTOK[@]}" ) >"$OUT/llama-server.log" 2>&1 &
S=$!; trap 'kill $S 2>/dev/null; wait $S 2>/dev/null' EXIT
LD_LIBRARY_PATH="$LLAMA" "$LLAMA/llama-server" --version >"$OUT/llama.version" 2>&1
for _ in $(seq 1 120); do curl -sf "http://127.0.0.1:$PORT/health" >/dev/null && break; kill -0 $S 2>/dev/null || { log "server exited"; exit 4; }; sleep 2; done
log "serve ready, arm $ARM"
( while sleep 10; do nvidia-smi --query-gpu=utilization.gpu,memory.used --format=csv,noheader,nounits | tr -d ' ' | sed "s/^/$(date +%s),/" >>"$OUT/gpu-samples.csv"; done ) & G=$!
s=$(date +%s)
"$PY" "$IN/read.py" --root "$IN" --list "$IN/crops.jsonl" --out "$OUT/sat" --mode region --max-tokens 2048 --max-window 1 \
  --endpoint "http://127.0.0.1:$PORT/v1" --export "$OUT/crops.jsonl" >"$OUT/read.log" 2>&1
log "read exit $? after $(( $(date +%s) - s ))s"
kill $G 2>/dev/null
