#!/usr/bin/env bash
# Study 5 rentals. --step guard-val | ...; run on the node with GPU_STUDY_ROOT set.
set -uo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"
TRAIN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:960ba48cb33e6c164764246bba2c9aded7d791b48a6aed49f39c3a95acff3ee1
STEP=""
while [ $# -gt 0 ]; do case "$1" in --step) STEP=$2; shift 2;; *) exit 2;; esac; done
case "$STEP" in
  guard-val) P=push/s5-val; rm -rf "$P"; mkdir -p "$P"; ln -s "$PWD/lora/images" "$P/images"
    jq -c 'select(.id|startswith("pub-nzz") or startswith("pub-newseye-nlf-fi"))|.' lora/validation.jsonl >"$P/validation.jsonl"
    cp scripts/jobs/lora_infer.py "$P/"; rm -rf "$P/images"; mkdir -p "$P/images"; for d in nzz newseye-nlf-fi; do ln -s "$PWD/lora/images/$d" "$P/images/$d"; done
    scripts/rent.sh --study s5val --image "$TRAIN" --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 6 --cap-dollars 2 --disk 30 --push-dir "$P" --job scripts/jobs/guard-validation.sh --out results/s5-val;;
  new2b) P=push/s5-new; rm -rf "$P"; mkdir -p "$P"; ln -s "$PWD/s5sets/images" "$P/images"
    cat s5sets/bln.jsonl s5sets/dta.jsonl | jq -c '{id,image:(if .source=="bln600" then "images/bln/"+(.file|sub("images/";"")) else .file end)}' >"$P/new.jsonl"
    cp scripts/jobs/lora_infer.py results/s2-1/adapter-r8-s1000.safetensors "$P/"
    scripts/rent.sh --study s5new --image "$TRAIN" --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 4 --cap-dollars 2 --disk 30 --push-dir "$P" --job scripts/jobs/lora-new.sh --out results/s5-new;;
  train8) P=push/s5-t8; rm -rf "$P"; mkdir -p "$P"; ln -s "$PWD/lora/images" "$P/images"
    cp lora/train.jsonl lora/validation.jsonl scripts/jobs/lora_train.py scripts/jobs/lora_infer.py "$P/"
    scripts/rent.sh --study s5t8 --image "$TRAIN" --gpu "RTX 3090" --max-hours 14 --cap-dollars 6 --disk 70 --push-dir "$P" --job scripts/jobs/lora8b-1.sh --out results/s5-t8;;
  *) exit 2;; esac
