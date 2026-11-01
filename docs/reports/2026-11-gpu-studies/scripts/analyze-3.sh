#!/usr/bin/env bash
# Study 3 tables: every reader on the same human-referenced crops, by source.
set -euo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"; T=${TOOL:-/tank/usnm-dev/gpu-studies/target/release/gpu-study}; A=analysis/s3; mkdir -p "$A"
cat refs/op-100.jsonl refs/rz-100.jsonl refs/ml-52.jsonl refs/nzz-360.jsonl | jq -c 'del(.image)' >"$A/refs.jsonl"
scripts/decode.sh --kind qwen --in results/s3-qwen/readings.b64 --out "$A/qwen.jsonl"
for f in op-100 rz-100 nzz-360 ml-52; do scripts/decode.sh --kind plain --in results/s3-tess/$f-psm6.b64 --out "$A/t-$f.jsonl"; done
cat "$A"/t-*.jsonl >"$A/tesseract.jsonl"
H=(--hyp qwen-8b=$A/qwen.jsonl --hyp tesseract=$A/tesseract.jsonl --hyp surya=results/s3-surya/surya-crops.jsonl)
for e in churro got granite; do [ -s results/s3-hf/$e.jsonl ] && { jq -c 'select(.text|type=="string")' results/s3-hf/$e.jsonl >"$A/$e.jsonl"; H+=(--hyp $e=$A/$e.jsonl); }; done
[ -s "$A/american-stories.jsonl" ] && H+=(--hyp american-stories=$A/american-stories.jsonl)
$T score --refs "$A/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare.jsonl"
$T score --fold --refs "$A/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare-folded.jsonl"
$T invent --refs "$A/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
jq -c 'select(.group|startswith("source:"))|{group,reader,items,pages,missing,cer,median_crop_cer,wer}' "$A/compare.jsonl"
