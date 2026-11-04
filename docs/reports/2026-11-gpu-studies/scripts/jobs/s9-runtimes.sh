#!/usr/bin/env bash
# Study 9 step 3 (a): the step 2 GGUF on a rented card, with the usnm_qwen 1.17 Vulkan build (/work/in/rt) when the
# container exposes a Vulkan device, then with the worker image's CUDA build (/app). /work/in/req holds the step 2
# request bodies, /work/in/list.tsv maps their names to crop ids. Requests go one at a time, with the app's flags.
set -u
O=/work/out; I=/work/in; PORT=8090
G=$I/gguf; MODEL=$G/xiaomi-ocr-0-q8_0.gguf; MM=$G/mmproj-xiaomi-ocr-0-f16.gguf
nvidia-smi --query-gpu=name,driver_version,memory.total --format=csv,noheader > $O/gpu.txt 2>&1
run_arm() { # NAME SERVER LIBPATH EXTRA...
  local name=$1 srv=$2 lib=$3; shift 3
  ( cd "$(dirname "$srv")" && LD_LIBRARY_PATH=$lib exec "$srv" -m $MODEL --mmproj $MM --host 127.0.0.1 --port $PORT --ctx-size 16384 \
      --temp 0 --seed 42 --parallel 1 --no-webui -ngl 99 -t 4 "$@" ) > $O/llama-$name.log 2>&1 &
  local lp=$!
  for _ in $(seq 240); do curl -fsS -m 2 http://127.0.0.1:$PORT/health >/dev/null 2>&1 && break; kill -0 $lp 2>/dev/null || break; sleep 2; done
  kill -0 $lp 2>/dev/null || { echo "$name server failed" >> $O/progress.txt; return; }
  LD_LIBRARY_PATH=$lib "$srv" --version > $O/llama-$name.version 2>&1
  local t1; t1=$(date +%s); n=0
  while IFS=$'\t' read -r f id; do
    [[ $f =~ ^[0-9]{4}$ ]] || continue
    s=$(date +%s%N)
    curl -sS -m 600 -H 'Content-Type: application/json' --data-binary @"$I/req/$f.json" http://127.0.0.1:$PORT/v1/chat/completions > /tmp/r.json 2>/dev/null
    e=$(date +%s%N)
    printf '%s\t%s\t%s\n' "$id" "$(( (e-s)/1000000 ))" "$(base64 -w0 /tmp/r.json)" >> $O/$name.b64
    n=$((n+1)); [ $((n%100)) = 0 ] && echo "$name $n $(( $(date +%s)-t1 ))s" >> $O/progress.txt
    kill -0 $lp 2>/dev/null || { echo "$name server died at $n" >> $O/progress.txt; break; }
  done < $I/list.tsv
  echo "$name seconds $(( $(date +%s)-t1 )) items $n" >> $O/timing.txt
  kill $lp 2>/dev/null; wait $lp 2>/dev/null; sleep 3
}
apt-get install -y -qq libvulkan1 >/dev/null 2>&1
mkdir -p /etc/vulkan/icd.d; [ -f /etc/vulkan/icd.d/nvidia_icd.json ] || printf '{"file_format_version":"1.0.0","ICD":{"library_path":"libGLX_nvidia.so.0","api_version":"1.3"}}\n' >/etc/vulkan/icd.d/nvidia_icd.json
chmod +x $I/rt/llama-server
if LD_LIBRARY_PATH=$I/rt:/usr/lib/x86_64-linux-gnu $I/rt/llama-server --list-devices 2>&1 | tee $O/vulkan-devices.txt | grep -q Vulkan0; then
  MTMD_BACKEND_DEVICE=Vulkan0 run_arm vulkan $I/rt/llama-server $I/rt:/usr/lib/x86_64-linux-gnu --device Vulkan0
else
  echo "no Vulkan device" >> $O/progress.txt
fi
if /app/llama-server --list-devices 2>&1 | tee $O/cuda-devices.txt | grep -q CUDA0; then
  run_arm cuda /app/llama-server /usr/lib/x86_64-linux-gnu:/app
else
  echo "no CUDA device" >> $O/progress.txt
fi
echo "done" >> $O/progress.txt
