#!/usr/bin/env bash
# Study 9 tables: Xiaomi-OCR-0 on Study 3's crops and reading-order pages, next to the Study 3 readers.
# Needs Study 3's analysis (analyze-3.sh, analyze-ja.sh, analyze-order.sh) and results/s9-xiaomi from scripts/s9/.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:-${GPU_STUDY_DEV}/target/release/gpu-study}; A=analysis/s9; S=analysis/s3; J=analysis/ja; mkdir -p "$A"
X=results/s9-xiaomi
only() { jq -c --rawfile l "$2" '($l|split("\n")) as $x|select(.id as $i|$x|index($i))|{id,text,finish:.finish_reason}' "$1"; }
only "$X/crops.jsonl" "$S/ids" >"$A/xiaomi.jsonl"
$T guard-cut --input "$A/xiaomi.jsonl" --out "$A/xiaomi-stop.jsonl"
$T guard-cut --input "$S/qwen.jsonl" --out "$A/qwen-8b-stop.jsonl"
H=(--hyp xiaomi=$A/xiaomi.jsonl --hyp xiaomi-stop=$A/xiaomi-stop.jsonl --hyp qwen-8b-stop=$A/qwen-8b-stop.jsonl --hyp tesseract=$S/tesseract.jsonl --hyp churro=$S/churro.jsonl)
$T score --refs "$S/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare.jsonl"
$T score --refs "$S/refs.jsonl" --hyp xiaomi-stop=$A/xiaomi-stop.jsonl --hyp qwen-8b-stop=$A/qwen-8b-stop.jsonl --baseline qwen-8b-stop --out "$A/vs-qwen.jsonl"
$T score --refs "$S/refs.jsonl" --hyp xiaomi-stop=$A/xiaomi-stop.jsonl --hyp churro=$S/churro.jsonl --baseline churro --out "$A/vs-churro.jsonl"
$T score --fold --refs "$S/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare-folded.jsonl"
$T invent --refs "$S/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
f() { jq -c 'select(.group|test("^source:|^all$"))|[.group,.reader,.items,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$1"; }
echo "# crops: group reader items cer lo hi median wer diff lo hi"; f "$A/compare.jsonl"
echo "# against qwen-8b-stop"; f "$A/vs-qwen.jsonl"
echo "# against churro"; f "$A/vs-churro.jsonl"
echo "# folded"; f "$A/compare-folded.jsonl"
echo "# invented: group reader crops major invented_run over under word_overlap"
jq -c 'select(.crops and .group=="all")|[.group,.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/invent.jsonl"
echo "# crops: finish=length, guard fired, Han characters, empty"
for r in xiaomi qwen-8b; do
  src=$A/xiaomi-stop.jsonl; [ $r = qwen-8b ] && src=$A/qwen-8b-stop.jsonl
  jq -sc --arg r $r '[length, (map(select(.finish=="length"))|length), (map(select(.looped==true))|length), (map(select((.text//"")|test("[\\p{Han}]")))|length), (map(select((.text//"")|test("^\\s*$")))|length)]|[$r]+.' "$src"
done
# Japanese: the same agreement measures as Study 3.
jq -c --rawfile l "$J/ids" '($l|split("\n")) as $x|select(.id as $i|$x|index($i))|{id,text}' "$X/crops.jsonl" >"$A/ja-xiaomi.jsonl"
$T ja-score --items ja-set/items.jsonl --hyp xiaomi=$A/ja-xiaomi.jsonl --hyp ndlocr-lite=$J/ndlocr-lite.jsonl --hyp qwen-8b=$J/qwen.jsonl \
  --hyp surya=$J/surya.jsonl --hyp churro=$J/churro.jsonl --hyp jpn_vert=$J/jpn_vert.jsonl --out "$A/ja.jsonl"
echo "# Japanese readers"; jq -c 'select(.kind=="reader")|del(.kind)' "$A/ja.jsonl"
echo "# Japanese pairs with xiaomi"; jq -c 'select(.kind=="pair" and .group=="all" and (.a=="xiaomi" or .b=="xiaomi"))|[.a,.b,.crops,(.median_f1*1000|round/1000)]' "$A/ja.jsonl"
# Pages: markdown and OTSL marks out, then the reading-order measure against the geometric column order.
jq -c '{id,text:(.text|gsub("<[^>]+>";" ")|gsub("(?m)^#+\\s*";"")|gsub("[*_`|]";" "))}' "$X/pages.jsonl" >"$A/pages-text.jsonl"
H=(--hyp xiaomi=$A/pages-text.jsonl --hyp loc=$S/order-loc.jsonl --hyp frak2021=analysis/s1/frak2021-page.jsonl)
$T order --reference "$S/order-ref.jsonl" "${H[@]}" --out "$A/order.jsonl"
echo "# pages: reader pages median_tau low under_half mean_matched"
jq -sc 'map(select(.tau!=null))|group_by(.reader)|map([.[0].reader,length,(map(.tau)|sort|if length%2==1 then .[length/2|floor] else (.[length/2-1]+.[length/2])/2 end),(map(.tau)|min),(map(select(.tau<0.5))|length),(map(.matched)|add/length|round)])[]' "$A/order.jsonl"
echo "# pages: n finish=length Han-character pages median_words median_completion_tokens"
jq -sc '[length,(map(select(.finish_reason=="length"))|length),(map(select(.text|test("[\\p{Han}]")))|length),(map(.text|[scan("\\S+")]|length)|sort|.[length/2|floor]),(map(.completion_tokens)|sort|.[length/2|floor])]' "$X/pages.jsonl"
jq -c '[.id,.finish_reason,.completion_tokens,(.text|[scan("\\S+")]|length),(.text|[scan("[\\p{Han}]")]|length)]' "$X/pages.jsonl"
jq -c '[.id,.words]' "$S/order-ref.jsonl"
