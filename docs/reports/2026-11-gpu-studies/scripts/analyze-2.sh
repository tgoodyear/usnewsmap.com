#!/usr/bin/env bash
# Study 2. --phase validation scores checkpoints and names the selection; --phase test scores the test split once.
set -euo pipefail
W=${GPU_STUDY_ROOT}; cd "$W"; T=${TOOL:-${GPU_STUDY_DEV}/target/release/gpu-study}; A=analysis/s2; mkdir -p "$A"
PHASE=""
while [ $# -gt 0 ]; do case "$1" in --phase) PHASE=$2; shift 2;; *) exit 2;; esac; done
R=results/s2-1
if [ "$PHASE" = validation ]; then
  H=(--hyp baseline=$R/val-baseline.jsonl); for s in 250 500 1000 2000; do H+=(--hyp s$s=$R/val-s$s.jsonl); done
  $T score --refs lora/validation-refs.jsonl "${H[@]}" --baseline baseline --out "$A/validation.jsonl"
  jq -c 'select(.group=="all")|{reader,items,pages,missing,cer,cer_lo,cer_hi,wer,median_crop_cer,cer_diff,cer_diff_lo,cer_diff_hi}' "$A/validation.jsonl"
  jq -s 'map(select(.group=="all"))|sort_by(.cer)|.[0].reader' "$A/validation.jsonl"
else
  H=(--hyp baseline=results/s2-2/test-baseline.jsonl --hyp adapter=results/s2-2/test-adapter.jsonl)
  $T score --refs lora/test-refs.jsonl "${H[@]}" --baseline baseline --out "$A/test.jsonl"
  $T score --fold --refs lora/test-refs.jsonl "${H[@]}" --baseline baseline --out "$A/test-folded.jsonl"
  $T invent --refs lora/test-refs.jsonl "${H[@]}" --out "$A/invent.jsonl"
  jq -c 'select(.group|startswith("source:") or .=="all")|{group,reader,items,pages,missing,cer,cer_lo,cer_hi,wer,wer_lo,wer_hi,median_crop_cer,cer_diff,cer_diff_lo,cer_diff_hi}' "$A/test.jsonl"
  jq -c 'select(.crops)' "$A/invent.jsonl"
fi
