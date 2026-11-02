#!/usr/bin/env bash
# Study 5 step 3 for 8B: stock and merged GGUF with the guard, and the export loss against BF16.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s5
for n in stock8b lora8b; do cat results/s5-d8/$n.b64 results/s5-d8s/$n.b64 results/s5-d8l/$n.b64 2>/dev/null >"$A/$n.b64" || true; scripts/decode.sh --kind qwen --in "$A/$n.b64" --out "$A/$n.jsonl"; $T guard-cut --input "$A/$n.jsonl" --out "$A/$n-stop.jsonl"; done
$T score --refs "$A/all-refs.jsonl" --hyp gguf-stock=$A/stock8b-stop.jsonl --hyp gguf-adapter=$A/lora8b-stop.jsonl --baseline gguf-stock --out "$A/8b-gguf.jsonl"
$T invent --refs "$A/all-refs.jsonl" --hyp gguf-stock=$A/stock8b-stop.jsonl --hyp gguf-adapter=$A/lora8b-stop.jsonl --out "$A/8b-gguf-invent.jsonl"
[ -s "$A/8b-test-adapter.jsonl" ] && {
$T score --refs "$A/all-refs.jsonl" --hyp bf16=$A/8b-test-adapter.jsonl --hyp gguf=$A/lora8b-stop.jsonl --baseline bf16 --out "$A/8b-export-adapter.jsonl"; }
for x in 8b-gguf 8b-export-adapter; do echo "== $x"; jq -c 'select(.group|test("^source:|^all$"))|[.group,.reader,.items,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$A/$x.jsonl"; done
jq -c 'select(.crops)|[.group,.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/8b-gguf-invent.jsonl"
