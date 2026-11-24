#!/usr/bin/env bash
# Fine-tune for #2128 (R3, R4c): round two's lstmtraining settings (20,000 iterations at most, target error
# 0.005), from the start model built by start_model.sh, then the checkpoint chosen on the evaluation list.
#   train.sh --start DIR --lists DIR --work DIR [--iterations 20000] [--name chr_phoenix]
set -euo pipefail
ITER=20000 NAME=chr_phoenix
while [ $# -gt 0 ]; do case "$1" in
  --start) START=$2; shift 2;; --lists) LISTS=$2; shift 2;; --work) WORK=$2; shift 2;;
  --iterations) ITER=$2; shift 2;; --name) NAME=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
mkdir -p "$WORK/out"
lstmtraining --continue_from "$START/base.lstm" --old_traineddata "$START/base.traineddata" \
  --traineddata "$START/lang/chr/chr.traineddata" --model_output "$WORK/out/$NAME" \
  --train_listfile "$LISTS/train.list" --eval_listfile "$LISTS/eval.list" \
  --max_iterations "$ITER" --target_error_rate 0.005 > "$WORK/train.log" 2>&1
echo "training finished" >> "$WORK/train.log"
python3 "$(dirname "$0")/select_checkpoint.py" --start "$START" --checkpoints "$WORK/out" --name "$NAME" \
  --eval-list "$LISTS/eval.list" --out "$WORK" >> "$WORK/train.log" 2>&1
echo "TRAIN-DONE" >> "$WORK/train.log"
