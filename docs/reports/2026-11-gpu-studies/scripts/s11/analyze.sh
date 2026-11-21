#!/usr/bin/env bash
# Study 11 tables from the finished readings. --root DIR (default /tank/usnm-dev/s11).
set -euo pipefail
R=/tank/usnm-dev/s11
while [ $# -gt 0 ]; do case "$1" in --root) R=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
T=/tank/usnm-dev/gpu-studies/target/release/gpu-study; X=$R/tools/target/release/s11
H9=/tank/usnm-dev/s9-heldout; A=$R/analysis; D=$R/readings; mkdir -p "$A" "$D"
SETS="de-rz de-cg en-asgold"
b64() { jq -R -c 'split("\t")|{id:.[0],text:(.[1]|@base64d)}' "$@"; }
# Readings per reader, {id,text,...}. The last row per id wins where a reader was resumed.
jq -c '{id,text,finish:.finish_reason,completion_tokens}' "$R/xiaomi/crops.jsonl" >"$D/xiaomi.jsonl"
$T guard-cut --input "$D/xiaomi.jsonl" --out "$D/xiaomi-stop.jsonl" 2>"$A/guard-xiaomi.txt"
jq -s -c 'map(select(.text!=null))|group_by(.id)|map(.[-1])[]|{id,text,finish:.finish_reason,stopped,fitted,completion_tokens,prompt_tokens,ms}' "$R/qwen/readings.jsonl" >"$D/qwen.jsonl"
$T guard-cut --input "$D/qwen.jsonl" --out "$D/qwen-stop.jsonl" 2>"$A/guard-qwen.txt"
for s in $SETS; do b64 "$R/tesseract/$s-regions.b64"; done >"$D/tesseract.jsonl"
for s in $SETS; do jq -c '{id,text}' "$R/rapid/$s-read.jsonl"; done >"$D/rapidocr.jsonl"
: >"$D/vote.jsonl"
for s in $SETS; do
  $X vote --refs "$R/inputs/$s/refs.jsonl" --hyp xiaomi="$D/xiaomi-stop.jsonl" --hyp qwen="$D/qwen-stop.jsonl" --hyp tesseract="$D/tesseract.jsonl" \
    --hyp rapidocr="$D/rapidocr.jsonl" --out "$D/vote-$s.jsonl" 2>"$A/vote-$s.txt"
  cat "$D/vote-$s.jsonl" >>"$D/vote.jsonl"
done
H=(--hyp xiaomi-stop="$D/xiaomi-stop.jsonl" --hyp qwen-stop="$D/qwen-stop.jsonl" --hyp tesseract="$D/tesseract.jsonl" --hyp rapidocr="$D/rapidocr.jsonl" --hyp vote="$D/vote.jsonl" --hyp xiaomi-unguarded="$D/xiaomi.jsonl")
for s in $SETS; do
  jq -c --rawfile l "$H9/$s/subset-not-in-churro-train.txt" '($l|split("\n")|map(select(.!=""))) as $x|select(.page as $p|$x|index($p))' "$R/inputs/$s/refs.jsonl" >"$A/$s-subset-refs.jsonl"
  for v in full subset; do
    rf="$R/inputs/$s/refs.jsonl"; [ $v = subset ] && rf="$A/$s-subset-refs.jsonl"
    $T score --refs "$rf" "${H[@]}" --baseline tesseract --out "$A/$s-$v-vs-tesseract.jsonl"
    $T score --refs "$rf" "${H[@]}" --baseline xiaomi-stop --out "$A/$s-$v-vs-xiaomi.jsonl"
  done
  $T score --fold --refs "$R/inputs/$s/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/$s-full-folded-vs-tesseract.jsonl"
  $T invent --refs "$R/inputs/$s/refs.jsonl" "${H[@]}" --out "$A/$s-invent.jsonl"
done
# Pages: Xiaomi on detected lines, guarded, joined in detection order; Tesseract PSM 3 on the whole page.
jq -c '{id,text,finish:.finish_reason}' "$R/xiaomi-lines/crops.jsonl" >"$D/xiaomi-lines.jsonl"
$T guard-cut --input "$D/xiaomi-lines.jsonl" --out "$D/xiaomi-lines-stop.jsonl" 2>"$A/guard-xiaomi-lines.txt"
jq -s -c 'map({p:(.id|sub(":L[0-9]+$";"")), o:(.id|capture(":L(?<n>[0-9]+)$").n|tonumber), text})|group_by(.p)|map({id:.[0].p, lines:length, text:(sort_by(.o)|map(.text)|join("\n"))})[]' "$D/xiaomi-lines-stop.jsonl" >"$D/pages-xiaomi-lines.jsonl"
for s in $SETS; do b64 "$R/tesseract/$s-pages.b64"; done >"$D/pages-tesseract.jsonl"
P=(--hyp rapid-lines-xiaomi="$D/pages-xiaomi-lines.jsonl" --hyp tesseract-psm3="$D/pages-tesseract.jsonl")
for s in $SETS; do
  $X bag --refs "$R/inputs/$s/page-refs.jsonl" "${P[@]}" --baseline tesseract-psm3 --fold --out "$A/$s-pages-folded.jsonl"
  $X bag --refs "$R/inputs/$s/page-refs.jsonl" "${P[@]}" --baseline tesseract-psm3 --out "$A/$s-pages-unfolded.jsonl"
done
# Tables.
r() { jq -r 'select(.group=="all")|[.reader,.items,.pages,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]|@tsv' "$1"; }
{
for s in $SETS; do
  for v in full subset; do
    echo "## $s $v: reader items pages missing cer lo hi median diff_vs_tesseract lo hi"; r "$A/$s-$v-vs-tesseract.jsonl"
    echo "## $s $v: against xiaomi-stop"; r "$A/$s-$v-vs-xiaomi.jsonl"
  done
  echo "## $s folded (post-hoc)"; r "$A/$s-full-folded-vs-tesseract.jsonl"
  echo "## $s invented: reader crops major invented_run over under word_overlap"
  jq -r 'select(.crops and .group=="all")|[.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]|@tsv' "$A/$s-invent.jsonl"
  echo "## $s counts: reader regions read missing empty looped(guard) finish=length"
  jq -c .id "$R/inputs/$s/refs.jsonl" >"$A/$s-ids.json"
  for x in xiaomi-stop qwen-stop tesseract rapidocr vote; do
    jq -s -r --arg x $x --slurpfile ids "$A/$s-ids.json" '($ids|map({(.):1})|add) as $k|map(select($k[.id]))|[$x, ($ids|length), length, (($ids|length)-length), (map(select((.text//"")|test("^\\s*$")))|length), (map(select(.looped==true or .stopped==true))|length), (map(select(.finish=="length"))|length)]|@tsv' "$D/$x.jsonl"
  done
  echo "## $s vote choices"; cat "$A/vote-$s.txt"
  for f in folded unfolded; do
    echo "## $s pages $f: reader pages missing P lo hi R lo hi F1 lo hi median_page_F1 F1_diff lo hi"
    jq -r 'select(.kind=="reader")|[.reader,.pages,.missing]+([.precision,.precision_lo,.precision_hi,.recall,.recall_lo,.recall_hi,.f1,.f1_lo,.f1_hi,.median_page_f1,(.f1_diff//0),(.f1_diff_lo//0),(.f1_diff_hi//0)]|map(.*1000|round/10))|@tsv' "$A/$s-pages-$f.jsonl"
  done
done
} | tee "$A/tables.txt"
