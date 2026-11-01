#!/usr/bin/env bash
# Runs in the usnm-boinc qwen image. Reads /work/in/list.tsv (id<TAB>relative image path), writes
# /work/out/readings.b64 (id<TAB>ms<TAB>base64 of llama-server's answer). Request as request version 2.
set -u
. /work/in/params.env   # MODEL MMPROJ MAX_TOKENS MIN_TOKENS
M=/opt/qwen/models; O=/work/out; L=/work/in/list.tsv; PORT=8090
sha256sum "$M/$MODEL" "$M/$MMPROJ" > "$O/weights.sha256" 2>&1; cp /opt/qwen/weights.digest "$O/" 2>/dev/null
# The driver's libcuda first: the image's compat library refuses GeForce cards and llama.cpp then runs on CPU.
cd /app; export LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu:/app
/app/llama-server --version > "$O/llama.version" 2>&1
/app/llama-server --list-devices 2>&1 | tee "$O/devices.txt" | grep -q CUDA0 || { echo "no CUDA device" >> "$O/progress.txt"; exit 8; }
T0=$(date +%s)
/app/llama-server -m "$M/$MODEL" --mmproj "$M/$MMPROJ" --host 127.0.0.1 --port $PORT -ngl 99 --threads 8 \
  --ctx-size 4096 --temp 0 --parallel 1 --no-webui --image-min-tokens "$MIN_TOKENS" > "$O/llama.log" 2>&1 &
LP=$!
for i in $(seq 300); do curl -fsS -m 2 http://127.0.0.1:$PORT/health >/dev/null 2>&1 && break
  kill -0 $LP 2>/dev/null || { echo "llama-server exited" >> "$O/progress.txt"; exit 9; }; sleep 2; done
echo "model_load_seconds $(( $(date +%s)-T0 ))" > "$O/timing.txt"
PROMPT='Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, line breaks and hyphenation. Do not correct or modernise anything, and do not add anything that is not printed. If a word is unreadable, write [?] for it. Output only the transcription.'
n=0; total=$(wc -l < "$L"); T1=$(date +%s)
while IFS=$'\t' read -r id rel; do
  f="/work/in/$rel"; case "$f" in *.png) mt=png;; *) mt=jpeg;; esac
  { printf '{"max_tokens":%s,"temperature":0,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/%s;base64,' "$MAX_TOKENS" "$mt"
    base64 -w0 "$f"; printf '"}},{"type":"text","text":"%s"}]}]}' "$PROMPT"; } > /tmp/body.json
  s=$(date +%s%N)
  curl -sS -m 600 -H 'Content-Type: application/json' --data-binary @/tmp/body.json http://127.0.0.1:$PORT/v1/chat/completions > /tmp/resp.json 2>/tmp/err
  e=$(date +%s%N)
  printf '%s\t%s\t%s\n' "$id" "$(( (e-s)/1000000 ))" "$(base64 -w0 /tmp/resp.json)" >> "$O/readings.b64"
  n=$((n+1)); [ $((n%10)) = 0 ] && echo "$n/$total $(( $(date +%s)-T1 ))s" >> "$O/progress.txt"
  [ $n = 5 ] && nvidia-smi --query-gpu=memory.used,utilization.gpu --format=csv,noheader >> "$O/gpu.txt" 2>&1
done < "$L"
echo "read_seconds $(( $(date +%s)-T1 ))" >> "$O/timing.txt"
kill %1 2>/dev/null; echo done >> "$O/progress.txt"
