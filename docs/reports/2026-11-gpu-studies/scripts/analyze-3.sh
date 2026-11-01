#!/usr/bin/env bash
# Study 3 tables: every reader on the same human-referenced crops, by source. A missing output scores as empty.
set -euo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"; T=${TOOL:-/tank/usnm-dev/gpu-studies/target/release/gpu-study}; A=analysis/s3; mkdir -p "$A"; L=/tank/usnm-dev/gpu-studies/local
cat refs/op-100.jsonl refs/rz-100.jsonl refs/ml-52.jsonl refs/nzz-360.jsonl | jq -c 'del(.image)' >"$A/refs.jsonl"
jq -r .id "$A/refs.jsonl" | LC_ALL=C sort >"$A/ids"
only() { jq -c --rawfile l "$A/ids" '($l|split("\n")) as $x|select(.id as $i|$x|index($i))' "$1"; }
scripts/decode.sh --kind qwen --in results/s3-qwen/readings.b64 --out "$A/qwen-all.jsonl"; only "$A/qwen-all.jsonl" >"$A/qwen.jsonl"
{ for f in op-100-psm6 ml-52-psm6; do scripts/decode.sh --kind plain --in $L/out/$f.b64 --out "$A/t-$f.jsonl" >/dev/null; cat "$A/t-$f.jsonl"; done
  only analysis/s1/frak2021-psm6.jsonl; } >"$A/tesseract.jsonl"
jq -c 'select(.text|type=="string")' results/s3-surya/surya-crops.jsonl >"$A/surya-all.jsonl"; only "$A/surya-all.jsonl" >"$A/surya.jsonl"
# CHURRO answers in XML: keep the Page element's text, one tag boundary one line break. Parsed as text, never run.
jq -c 'select(.text|type=="string")|{id,text:(.text|sub("(?s)^.*?<Page>";"")|sub("(?s)</Page>.*$";"")|gsub("<[^>]+>";"\n")|gsub("&amp;";"&")|gsub("&lt;";"<")|gsub("&gt;";">")|gsub("\\n\\s*\\n+";"\n"))}' results/s3-churro/churro.jsonl >"$A/churro-all.jsonl"; only "$A/churro-all.jsonl" >"$A/churro.jsonl"
H=(--hyp qwen-8b=$A/qwen.jsonl --hyp tesseract=$A/tesseract.jsonl --hyp surya=$A/surya.jsonl --hyp churro=$A/churro.jsonl --hyp american-stories=$A/american-stories.jsonl)
jq -c 'select(.text|type=="string")' results/s3-hf/got.jsonl >"$A/got.jsonl"; H+=(--hyp got=$A/got.jsonl)
{ cat results/s3-granite-3090/granite.jsonl results/s3-granite-local/granite.jsonl 2>/dev/null || true; } | jq -c 'select(.text|type=="string")' >"$A/granite.jsonl"; H+=(--hyp granite=$A/granite.jsonl)
$T score --refs "$A/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare.jsonl"
$T score --fold --refs "$A/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare-folded.jsonl"
$T invent --refs "$A/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
jq -c 'select(.group|startswith("source:"))|[.group,.reader,.items,.pages,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10)]' "$A/compare.jsonl"
jq -c 'select(.crops and .group=="all")|[.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/invent.jsonl"
