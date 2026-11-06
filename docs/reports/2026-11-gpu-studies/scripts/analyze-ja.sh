#!/usr/bin/env bash
# Japanese: bigram agreement between readers, agreement with the machine references, failure counts. Not accuracy.
set -euo pipefail
W=${GPU_STUDY_ROOT}; cd "$W"; T=${TOOL:-${GPU_STUDY_DEV}/target/release/gpu-study}; A=analysis/ja; mkdir -p "$A"; L=${GPU_STUDY_DEV}/local
jq -r .id ja-set/items.jsonl | LC_ALL=C sort >"$A/ids"
ids() { jq -c --rawfile l "$A/ids" '($l|split("\n")) as $x|select(.id as $i|$x|index($i))' "$1"; }
scripts/decode.sh --kind qwen --in results/s3-qwen/readings.b64 --out "$A/qwen-all.jsonl"; ids "$A/qwen-all.jsonl" >"$A/qwen.jsonl"
jq -c 'select(.text|type=="string")' results/s3-surya/surya-crops.jsonl >"$A/surya-all.jsonl"; ids "$A/surya-all.jsonl" >"$A/surya.jsonl"
scripts/decode.sh --kind plain --in $L/out/ja-jpn_vert.b64 --out "$A/jpn_vert.jsonl"
for f in $L/ndlout/*.txt; do b=$(basename "$f" .txt); jq -nc --arg i "$b" --rawfile t "$f" '{id:$i,text:$t}'; done >"$A/ndl-all.jsonl"; ids "$A/ndl-all.jsonl" >"$A/ndlocr-lite.jsonl"
H=(--hyp ndlocr-lite=$A/ndlocr-lite.jsonl --hyp qwen-8b=$A/qwen.jsonl --hyp surya=$A/surya.jsonl --hyp jpn_vert=$A/jpn_vert.jsonl)
ids analysis/s3/churro-all.jsonl >"$A/churro.jsonl"; H+=(--hyp churro=$A/churro.jsonl)
$T ja-score --items ja-set/items.jsonl "${H[@]}" --out "$A/ja.jsonl"
jq -c 'select(.kind=="reader")|del(.kind)' "$A/ja.jsonl"
jq -c 'select(.kind=="pair" and .group=="all")|[.a,.b,.crops,(.median_f1*1000|round/1000)]' "$A/ja.jsonl"
