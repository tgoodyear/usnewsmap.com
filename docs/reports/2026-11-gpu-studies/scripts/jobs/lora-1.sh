#!/usr/bin/env bash
# Phase 1: train on the train split, then read the validation split with the baseline and every checkpoint.
set -u
O=/work/out; I=/work/in; PX="--min-pixels 1048576 --max-pixels 2097152"
# Named switches only, so a token in the environment can't reach the results.
for v in HF_DATASETS_OFFLINE HF_HUB_DISABLE_TELEMETRY HF_HUB_OFFLINE TRANSFORMERS_OFFLINE; do [ -z "${!v+x}" ] || echo "$v=${!v}"; done > $O/env.txt
sha256sum /opt/train/worker.py /opt/base/model.safetensors > $O/image.sha256
python $I/lora_train.py --train $I/train.jsonl --root $I --out $O --rank 8 --lr 1e-4 --steps 2000 --accum 4 \
  --checkpoints 250,500,1000,2000 --seed 583 $PX || exit 7
python $I/lora_infer.py --list $I/validation.jsonl --root $I --out $O/val-baseline.jsonl $PX
for s in 250 500 1000 2000; do
  python $I/lora_infer.py --list $I/validation.jsonl --root $I --adapter $O/adapter-r8-s$s.safetensors --rank 8 --out $O/val-s$s.jsonl $PX
done
