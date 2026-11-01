#!/usr/bin/env bash
# Study 2: phase 1 trains and reads validation; phase 2 reads the test split once. --step 1|2
set -uo pipefail
W=${GPU_STUDY_ROOT}; cd "$W"
TRAIN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:960ba48cb33e6c164764246bba2c9aded7d791b48a6aed49f39c3a95acff3ee1
STEP=""; PREFER=""; ADAPTER=""
while [ $# -gt 0 ]; do case "$1" in --step) STEP=$2; shift 2;; --prefer-machine) PREFER=$2; shift 2;; --adapter) ADAPTER=$2; shift 2;; *) exit 2;; esac; done
case "$STEP" in
  1) P=push/s2-1; rm -rf $P; mkdir -p $P; ln -s "$PWD/lora/images" $P/images
     cp lora/train.jsonl lora/validation.jsonl scripts/jobs/lora_train.py scripts/jobs/lora_infer.py $P/
     scripts/rent.sh --study s2train --image "$TRAIN" --max-hours 14 --cap-dollars 6 --disk 40 --push-dir $P --job scripts/jobs/lora-1.sh --prefer-machine "$PREFER" --out results/s2-1;;
  2) P=push/s2-2; rm -rf $P; mkdir -p $P/images; ln -s "$PWD/lora/images/bench" $P/images/bench
     cp lora/test.jsonl scripts/jobs/lora_infer.py $P/; cp "$ADAPTER" $P/; basename "$ADAPTER" >$P/selected.txt
     scripts/rent.sh --study s2test --image "$TRAIN" --max-hours 5 --cap-dollars 3 --disk 40 --push-dir $P --job scripts/jobs/lora-2.sh --prefer-machine "$PREFER" --out results/s2-2;;
  *) exit 2;; esac
