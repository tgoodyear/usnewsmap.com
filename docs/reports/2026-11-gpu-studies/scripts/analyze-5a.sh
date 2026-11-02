#!/usr/bin/env bash
# Study 5 guard selection, on the validation split only. Prints each candidate's numbers and the rule's choice.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s5; mkdir -p "$A"
V=results/s5-val
H=(--hyp none=results/s2-1/val-baseline.jsonl --hyp stop=$V/val-stop.jsonl --hyp rp105=$V/val-rp105.jsonl --hyp rp110=$V/val-rp110.jsonl --hyp stop-retry=$V/val-stop-retry.jsonl)
$T score --refs lora/validation-refs.jsonl "${H[@]}" --baseline none --out "$A/guard-validation.jsonl"
$T invent --refs lora/validation-refs.jsonl "${H[@]}" --out "$A/guard-invent.jsonl"
jq -c 'select(.group=="all")|[.reader,(.cer*10000|round/100),(.cer_lo*10000|round/100),(.cer_hi*10000|round/100),(.wer*1000|round/10),(.median_crop_cer*1000|round/10)]' "$A/guard-validation.jsonl"
jq -c 'select(.crops and .group=="all")|[.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/guard-invent.jsonl"
