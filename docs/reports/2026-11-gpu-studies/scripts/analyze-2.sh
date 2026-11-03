#!/usr/bin/env bash
# Study 2. --phase validation scores checkpoints and names the selection; --phase test scores the test split once.
set -euo pipefail
PHASE=""
while [ $# -gt 0 ]; do
  case "$1" in
    --phase)
      if [ $# -lt 2 ] || [ -n "$PHASE" ]; then
        echo "Specify --phase validation or --phase test exactly once" >&2; exit 2
      fi
      case "$2" in
        validation|test) PHASE=$2 ;;
        *) echo "--phase must be validation or test" >&2; exit 2 ;;
      esac
      shift 2 ;;
    *) echo "Unknown argument: $1" >&2; exit 2 ;;
  esac
done
if [ -z "$PHASE" ]; then
  echo "Specify --phase validation or --phase test exactly once" >&2; exit 2
fi
W=${GPU_STUDY_ROOT}; cd "$W"; T=${TOOL:-${GPU_STUDY_DEV}/target/release/gpu-study}; A=analysis/s2; mkdir -p "$A"
R=results/s2-1
if [ "$PHASE" = validation ]; then
  H=(--hyp "baseline=$R/val-baseline.jsonl"); for s in 250 500 1000 2000; do H+=(--hyp "s$s=$R/val-s$s.jsonl"); done
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
