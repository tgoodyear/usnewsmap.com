#!/usr/bin/env bash
# Study 5 step 2: pick the 8B checkpoint on validation, then, with --test, score the test once.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s5; mkdir -p "$A"; PHASE=${1:-val}
if [ "$PHASE" = val ]; then
  R=results/s5-t8; H=(--hyp baseline=$R/val-baseline.jsonl); for s in 250 500 1000 2000; do H+=(--hyp s$s=$R/val-s$s.jsonl); done
  $T score --refs lora/validation-refs.jsonl "${H[@]}" --baseline baseline --out "$A/8b-validation.jsonl"
  $T invent --refs lora/validation-refs.jsonl "${H[@]}" --out "$A/8b-validation-invent.jsonl"
  jq -c 'select(.group=="all")|[.reader,.items,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.wer*1000|round/10),(.median_crop_cer*1000|round/10)]' "$A/8b-validation.jsonl"
  jq -c 'select(.crops and .group=="all")|[.reader,.major,.invented_run,.under_length_guard,.under_word_overlap_guard]' "$A/8b-validation-invent.jsonl"
  jq -c '{group:"train",peak:.peak_gpu_bytes,seconds,skipped,examples,base_revision}' $R/train-report.json
  sha256sum $R/*.safetensors | cut -c1-96
else
  R=results/s5-t82; H=(--hyp baseline=$R/test-baseline.jsonl --hyp adapter=$R/test-adapter.jsonl)
  $T score --refs "$A/all-refs.jsonl" "${H[@]}" --baseline baseline --out "$A/8b-test.jsonl"
  $T invent --refs "$A/all-refs.jsonl" "${H[@]}" --out "$A/8b-test-invent.jsonl"
  jq -c 'select(.group|test("^source:|^all$"))|[.group,.reader,.items,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$A/8b-test.jsonl"
  jq -c 'select(.crops)|[.group,.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/8b-test-invent.jsonl"
fi
