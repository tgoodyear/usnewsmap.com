#!/usr/bin/env bash
# Reading order on the frozen multi-column pages against the geometric column order. Agreement, not truth.
set -euo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"; T=${TOOL:-/tank/usnm-dev/gpu-studies/target/release/gpu-study}; A=analysis/s3; mkdir -p "$A"
cut -f1 samples/order-pages.tsv | jq -R . | jq -s . >"$A/order-ids.json"
jq -c --slurpfile i "$A/order-ids.json" 'select(.id as $x|$i[0]|index($x))|{id,columns,words}' /tank/usnm-dev/gpu-studies/pilot/columns.jsonl >"$A/order-ref.jsonl"
jq -c --slurpfile i "$A/order-ids.json" 'select(.id as $x|$i[0]|index($x))|{id,text:.loc_text}' /tank/usnm-dev/gpu-studies/pilot/columns.jsonl >"$A/order-loc.jsonl"
H=(--hyp loc=$A/order-loc.jsonl --hyp frak2021=analysis/s1/frak2021-page.jsonl)
[ -s results/s3-surya/surya-pages.jsonl ] && { jq -c 'select(.text|type=="string")' results/s3-surya/surya-pages.jsonl >"$A/surya-pages.jsonl"; H+=(--hyp surya=$A/surya-pages.jsonl); }
$T order --reference "$A/order-ref.jsonl" "${H[@]}" --out "$A/order.jsonl"
jq -s 'map(select(.tau!=null))|group_by(.reader)|map({reader:.[0].reader,pages:length,median_tau:(map(.tau)|sort|.[length/2|floor]),low:(map(.tau)|min),under_half:(map(select(.tau<0.5))|length),mean_matched:(map(.matched)|add/length)})' "$A/order.jsonl"
