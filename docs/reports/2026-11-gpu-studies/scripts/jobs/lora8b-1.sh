#!/usr/bin/env bash
# Study 5 step 2, phase 1: the Study 2 recipe on Qwen3-VL-8B, then validation with the guard named in guard.txt.
set -u
O=/work/out; I=/work/in; PX="--min-pixels 1048576 --max-pixels 2097152"; G=$(cat $I/guard.txt)
unset HF_HUB_OFFLINE TRANSFORMERS_OFFLINE HF_DATASETS_OFFLINE; export HF_HOME=/work/hf
PY=$(command -v python)
REV=$(curl -fsS https://huggingface.co/api/models/Qwen/Qwen3-VL-8B-Instruct | jq -r .sha 2>/dev/null || curl -fsS https://huggingface.co/api/models/Qwen/Qwen3-VL-8B-Instruct | sed -n 's/.*"sha":"\([0-9a-f]\{40\}\)".*/\1/p' | head -1)
echo "$REV" > $O/base-revision.txt
$PY -c "from huggingface_hub import snapshot_download as d; print(d('Qwen/Qwen3-VL-8B-Instruct', revision='$REV', local_dir='/work/base8', allow_patterns=['*.json','*.safetensors','*.txt','*.jinja']))" > $O/download.log 2>&1 || exit 6
$PY $I/lora_train.py --base /work/base8 --base-revision "$REV" --train $I/train.jsonl --root $I --out $O --rank 8 --lr 1e-4 --steps 2000 --accum 4 \
  --checkpoints 250,500,1000,2000 --seed 583 $PX || exit 7
$PY $I/lora_infer.py --base /work/base8 --guard "$G" --list $I/validation.jsonl --root $I --out $O/val-baseline.jsonl $PX
for s in 250 500 1000 2000; do
  $PY $I/lora_infer.py --base /work/base8 --guard "$G" --list $I/validation.jsonl --root $I --adapter $O/adapter-r8-s$s.safetensors --rank 8 --out $O/val-s$s.jsonl $PX
done
