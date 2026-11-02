#!/usr/bin/env bash
# Study 7: image-token sweep on one Qwen 8B GGUF. /work/in/list.tsv: id, relative image path, max_tokens.
set -u
O=/work/out; I=/work/in; M=/opt/qwen/models; PORT=8090
MODEL=$M/Qwen3VL-8B-Instruct-Q4_K_M.gguf; MM=$M/mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf
PROMPT='Transcribe exactly the printed text in the image. Keep the original spelling, punctuation, line breaks and hyphenation. Do not correct or modernise anything, and do not add anything that is not printed. If a word is unreadable, write [?] for it. Output only the transcription.'
# Native Vulkan build if it finds the card, else the image's CUDA build.
SRV=/app/llama-server; export LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu:/app; RT=cuda
if [ -x $I/rt/llama-server ]; then
  apt-get install -y -qq libvulkan1 >/dev/null 2>&1
  mkdir -p /etc/vulkan/icd.d; [ -f /etc/vulkan/icd.d/nvidia_icd.json ] || printf '{"file_format_version":"1.0.0","ICD":{"library_path":"libGLX_nvidia.so.0","api_version":"1.3"}}\n' >/etc/vulkan/icd.d/nvidia_icd.json
  if LD_LIBRARY_PATH=$I/rt:/usr/lib/x86_64-linux-gnu $I/rt/llama-server --list-devices 2>&1 | tee $O/vulkan-devices.txt | grep -q Vulkan0; then SRV=$I/rt/llama-server; export LD_LIBRARY_PATH=$I/rt:/usr/lib/x86_64-linux-gnu; RT=vulkan; fi
fi
echo "$RT" > $O/runtime.txt; $SRV --version > $O/llama.version 2>&1; [ $RT = cuda ] && $SRV --list-devices 2>&1 | grep -q CUDA0 || [ $RT = vulkan ] || { echo "no GPU device" >> $O/progress.txt; exit 8; }
cd "$(dirname $SRV)"
for v in default 768 1024 1536 2048 3072; do
  CTX=4096; ARG=""; if [ $v != default ]; then ARG="--image-min-tokens $v"; [ $((v+2304)) -gt 4096 ] && CTX=$((v+2304)); fi
  $SRV -m $MODEL --mmproj $MM --host 127.0.0.1 --port $PORT -ngl 99 --threads 8 --ctx-size $CTX --temp 0 --seed 42 --parallel 1 --no-webui $ARG > $O/llama-$v.log 2>&1 &
  LP=$!; for i in $(seq 240); do curl -fsS -m 2 http://127.0.0.1:$PORT/health >/dev/null 2>&1 && break; kill -0 $LP 2>/dev/null || break; sleep 2; done
  kill -0 $LP 2>/dev/null || { echo "$v server failed" >> $O/progress.txt; break; }
  T1=$(date +%s); n=0; fail=0; peak=0
  while IFS=$'\t' read -r id rel mt; do
    f="$I/$rel"; case "$f" in *.png) ty=png;; *) ty=jpeg;; esac
    { printf '{"max_tokens":%s,"temperature":0,"seed":42,"messages":[{"role":"user","content":[{"type":"image_url","image_url":{"url":"data:image/%s;base64,' "$mt" "$ty"; base64 -w0 "$f"; printf '"}},{"type":"text","text":"%s"}]}]}' "$PROMPT"; } > /tmp/b.json
    s=$(date +%s%N); curl -sS -m 900 -H 'Content-Type: application/json' --data-binary @/tmp/b.json http://127.0.0.1:$PORT/v1/chat/completions > /tmp/r.json 2>/dev/null; e=$(date +%s%N)
    printf '%s\t%s\t%s\n' "$id" "$(( (e-s)/1000000 ))" "$(base64 -w0 /tmp/r.json)" >> $O/s7-$v.b64
    u=$(nvidia-smi --query-gpu=memory.used --format=csv,noheader,nounits | tr -d ' '); [ "${u:-0}" -gt "$peak" ] && peak=$u
    n=$((n+1)); [ $((n%10)) = 0 ] && echo "$v $n $(( $(date +%s)-T1 ))s" >> $O/progress.txt
    grep -q 'out of memory\|ErrorOutOfDeviceMemory\|failed to allocate' $O/llama-$v.log && { fail=1; break; }
    kill -0 $LP 2>/dev/null || { fail=1; break; }
  done < $I/list.tsv
  echo "$v seconds $(( $(date +%s)-T1 )) peak_mib $peak fail $fail" >> $O/timing.txt
  kill $LP 2>/dev/null; sleep 3; [ $fail = 1 ] && { echo "$v stopped the sweep" >> $O/progress.txt; break; }
done
echo done >> $O/progress.txt
