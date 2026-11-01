#!/usr/bin/env bash
# Study 1 tables from the fetched outputs. Everything fetched is parsed as data here and never executed.
set -euo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"; T=${TOOL:-/tank/usnm-dev/gpu-studies/target/release/gpu-study}; A=analysis/s1; mkdir -p "$A"
scripts/decode.sh --kind qwen --in results/s1-qwen/readings.b64 --out "$A/qwen.jsonl"
for n in frak2021-psm3 frak2021-psm6 deu_latf-psm3 deu_latf-psm6 frak2021-page deu_latf-page; do
  scripts/decode.sh --kind plain --in results/s1-tess/$n.b64 --out "$A/$n.jsonl"; done
cat refs/rz-700.jsonl refs/nzz-360.jsonl | jq -c 'del(.image)' >"$A/refs.jsonl"
H=(--hyp qwen-8b=$A/qwen.jsonl --hyp frak2021-psm3=$A/frak2021-psm3.jsonl --hyp frak2021-psm6=$A/frak2021-psm6.jsonl --hyp deu_latf-psm3=$A/deu_latf-psm3.jsonl --hyp deu_latf-psm6=$A/deu_latf-psm6.jsonl)
$T score --refs "$A/refs.jsonl" "${H[@]}" --baseline frak2021-psm3 --out "$A/measured.jsonl"
$T invent --refs "$A/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
# agreement on the pilot crops, LoC's words in the box as one reader
jq -c '{id,text:.loc_text}' refs/pilot-crops.jsonl >"$A/loc.jsonl"
jq -c '{id,page,group:"pilot"}' refs/pilot-crops.jsonl >"$A/items.jsonl"
$T agree --items "$A/items.jsonl" "${H[@]}" --hyp loc=$A/loc.jsonl --out "$A/agreement.jsonl"
jq -c 'select(.group=="all" or (.group|startswith("source:"))) | {group,reader,items,pages,missing,cer:(.cer*10000|round/100),lo:(.cer_lo*10000|round/100),hi:(.cer_hi*10000|round/100),wer:(.wer*10000|round/100),diff:(.cer_diff//null)}' "$A/measured.jsonl"
jq -c 'select(.crops)' "$A/invent.jsonl"
jq -c '{a,b,items,pages,s:.symmetric_edit_rate,lo,hi,d:.mean_dice}' "$A/agreement.jsonl"
