#!/usr/bin/env bash
# Study 6 (c): the native Surya weights on vLLM with Surya's own server settings, scaled to a 12 GB card, read in line mode.
set -u
O=/work/out; I=/work/in; REV=3b3d4cdf88d6928b0acdc75181b13206ea67c4a3; M=datalab-to/surya-ocr-2
export HF_HOME=/work/hf
PY=$(command -v python3); $PY -m pip -q install --no-cache-dir 'surya-ocr==0.22.1' beautifulsoup4 > $O/pip.log 2>&1
$PY -m vllm.entrypoints.openai.api_server --model $M --revision $REV --no-enforce-eager --max-num-seqs 16 --dtype bfloat16 --max-model-len 18000 \
  --max-num-batched-tokens 4096 --gpu-memory-utilization 0.85 --enable-prefix-caching --mm-processor-kwargs '{"min_pixels":3136,"max_pixels":6291456}' \
  --served-model-name $M --speculative-config '{"method":"mtp","num_speculative_tokens":2}' --port 8000 > $O/vllm.log 2>&1 &
V=$!
for i in $(seq 180); do curl -fsS -m 2 http://127.0.0.1:8000/health >/dev/null 2>&1 && break; kill -0 $V 2>/dev/null || { echo "vllm exited" >> $O/progress.txt; exit 9; }; sleep 5; done
export SURYA_INFERENCE_BACKEND=vllm SURYA_INFERENCE_URL=http://127.0.0.1:8000/v1 SURYA_INFERENCE_PARALLEL=1 VLLM_GPU_TYPE=3090 TORCH_DEVICE=cuda
cp $I/run.py /work/; cd /work; s=$(date +%s); $PY $I/surya_read.py --list $I/list.tsv --mode line --out $O/surya-native.jsonl > $O/native.log 2>&1
echo "native seconds $(( $(date +%s)-s )) exit $?" >> $O/timing.txt; kill $V 2>/dev/null; echo done >> $O/progress.txt
