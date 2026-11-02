#!/usr/bin/env bash
# Runs in the usnm_surya image. /work/in/runs.tsv lines: list<TAB>mode<TAB>name.
set -u
O=/work/out; I=/work/in
export LD_LIBRARY_PATH=/usr/lib/x86_64-linux-gnu:/app PATH=/opt/venv/bin:/app:$PATH LLAMA_CPP_BINARY=/app/llama-server \
  SURYA_GGUF_LOCAL_MODEL_PATH=/opt/surya/models/surya-2.gguf SURYA_GGUF_LOCAL_MMPROJ_PATH=/opt/surya/models/surya-2-mmproj.gguf \
  SURYA_INFERENCE_BACKEND=llamacpp SURYA_INFERENCE_PARALLEL=1 SURYA_INFERENCE_TIMEOUT_SECONDS=3600 \
  FAST_LAYOUT_MODEL_CHECKPOINT=/opt/surya/models/layout2 FAST_ORDER_MODEL_CHECKPOINT=/opt/surya/models/layout2/order \
  TORCH_DEVICE=cpu HF_HUB_OFFLINE=1 DISABLE_TQDM=true PYTHONDONTWRITEBYTECODE=1 HOME=/tmp
cd /app; /app/llama-server --list-devices 2>&1 | tee $O/devices.txt | grep -q CUDA0 || { echo "no CUDA device" >> $O/progress.txt; exit 8; }
cd /work; pip --version > $O/python.txt 2>&1; /opt/venv/bin/pip freeze 2>/dev/null | grep -E '^surya' >> $O/python.txt
while IFS=$'\t' read -r list mode name; do
  s=$(date +%s); /opt/venv/bin/python $I/surya_read.py --list "$I/$list" --mode "$mode" --out "$O/$name.jsonl" > "$O/$name.log" 2>&1
  echo "$name seconds $(( $(date +%s)-s )) exit $?" >> $O/timing.txt
done < $I/runs.tsv
echo done >> $O/progress.txt
