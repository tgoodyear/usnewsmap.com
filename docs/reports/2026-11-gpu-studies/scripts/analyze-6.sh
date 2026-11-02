#!/usr/bin/env bash
# Study 6: line-mode Surya on Study 3's crops, page modes on the reading-order pages, native against GGUF.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s6; mkdir -p "$A"; S=analysis/s3
jq -c 'select(.text|type=="string")' results/s6/surya-line.jsonl >"$A/surya-line.jsonl"
H=(--hyp surya-line=$A/surya-line.jsonl --hyp surya-study3=$S/surya.jsonl --hyp qwen-8b=$S/qwen.jsonl --hyp churro=$S/churro.jsonl --hyp tesseract=$S/tesseract.jsonl)
$T score --refs "$S/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/line.jsonl"
$T score --fold --refs "$S/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/line-folded.jsonl"
$T invent --refs "$S/refs.jsonl" --hyp surya-line=$A/surya-line.jsonl --out "$A/line-invent.jsonl"
jq -c 'select(.reader=="surya-line" and (.group=="all" or (.group|startswith("source:"))))|[.group,.items,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$A/line.jsonl"
jq -c 'select(.crops and .group=="all")|[.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/line-invent.jsonl"
for m in whole capped; do jq -c 'select(.text|type=="string")' results/s6/surya-$m.jsonl >"$A/surya-$m.jsonl"; done
$T order --reference "$S/order-ref.jsonl" --hyp whole=$A/surya-whole.jsonl --hyp capped=$A/surya-capped.jsonl --hyp tiled=$S/surya-pages.jsonl --hyp frak2021=analysis/s1/frak2021-page.jsonl --hyp loc=$S/order-loc.jsonl --out "$A/order.jsonl"
jq -s 'map(select(.tau!=null))|group_by(.reader)|map({reader:.[0].reader,pages:length,median:(map(.tau)|sort|if length%2==1 then .[length/2|floor] else (.[length/2-1]+.[length/2])/2 end),low:(map(.tau)|min),under_half:(map(select(.tau<0.5))|length),matched:(map(.matched)|add/length)})' "$A/order.jsonl" -c
for m in whole capped; do jq -s 'map(select(.status=="ok"))|{n:length,sec:(map(.ms)|add/1000|floor)}' -c results/s6/surya-$m.jsonl; done
if [ -s results/s6c/surya-native.jsonl ]; then jq -c 'select(.text|type=="string")' results/s6c/surya-native.jsonl >"$A/surya-native.jsonl"; jq -c --rawfile l <(jq -r .id "$A/surya-native.jsonl") '($l|split("\n")) as $x|select(.id as $i|$x|index($i))' "$S/refs.jsonl" >"$A/native-refs.jsonl"
  $T score --refs "$A/native-refs.jsonl" --hyp gguf=$A/surya-line.jsonl --hyp native=$A/surya-native.jsonl --baseline gguf --out "$A/native.jsonl"; jq -c 'select(.group=="all")|[.reader,.items,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$A/native.jsonl"; fi
