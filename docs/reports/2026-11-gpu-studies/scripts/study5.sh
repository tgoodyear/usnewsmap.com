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
  *) exit 2;; esac
