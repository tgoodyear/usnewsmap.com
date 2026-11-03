#!/usr/bin/env bash
# Study 9, inside vllm/vllm-openai:v0.29.0: serve the pinned Xiaomi-OCR-0 weights from /work/in/model, then read
# the crops (with one kill and resume of the client) and the pages through saturate. Outputs go to /work/out.
set -u
I=/work/in; O=/work/out; mkdir -p "$O"
export HOME=/tmp HF_HUB_OFFLINE=1 TRANSFORMERS_OFFLINE=1
log() { echo "$(date -Is) $*" >>"$O/progress.txt"; }
python3 -m pip install -q --no-cache-dir 'saturate==0.3.0' >"$O/pip.log" 2>&1 || { log "pip install failed"; exit 3; }
python3 -m pip freeze >"$O/pip-freeze.txt" 2>&1
nvidia-smi >"$O/gpu.txt" 2>&1
python3 -c 'import vllm; print(vllm.__version__)' >"$O/vllm.version" 2>&1
log "serve start"
vllm serve "$I/model" --served-model-name xiaomi-research/Xiaomi-OCR-0 --host 127.0.0.1 --port 8000 \
  --max-model-len 16384 --dtype float16 --gpu-memory-utilization 0.88 \
  --limit-mm-per-prompt '{"image": 1}' --mm-processor-cache-gb 0 --no-enable-prefix-caching >"$O/vllm.log" 2>&1 &
V=$!
ready=0
for _ in $(seq 1 120); do
  kill -0 "$V" 2>/dev/null || break
  python3 -c 'import urllib.request; urllib.request.urlopen("http://127.0.0.1:8000/v1/models", timeout=5)' 2>/dev/null && { ready=1; break; }
  sleep 5
done
[ "$ready" -eq 1 ] || { log "server never became ready"; kill "$V" 2>/dev/null; exit 4; }
log "serve ready"
R=(python3 "$I/read.py" --root "$I")
# Crops: start, kill the client after 90 s, run the same command again. Resume must skip what was written.
s=$(date +%s)
"${R[@]}" --list "$I/crops.jsonl" --out "$O/crops" --mode region --max-tokens 2048 --export "$O/crops.jsonl" >"$O/crops-1.log" 2>&1 &
C=$!; sleep 90; kill -9 "$C" 2>/dev/null; wait "$C" 2>/dev/null
log "crops killed after $(( $(date +%s) - s ))s"
"${R[@]}" --list "$I/crops.jsonl" --out "$O/crops" --mode region --max-tokens 2048 --export "$O/crops.jsonl" >"$O/crops-2.log" 2>&1
log "crops exit $? after $(( $(date +%s) - s ))s"
s=$(date +%s)
"${R[@]}" --list "$I/pages.jsonl" --out "$O/pages" --mode page --max-tokens 12000 --max-window 8 --export "$O/pages.jsonl" >"$O/pages.log" 2>&1
log "pages exit $? after $(( $(date +%s) - s ))s"
kill "$V" 2>/dev/null; wait "$V" 2>/dev/null
log "done"
