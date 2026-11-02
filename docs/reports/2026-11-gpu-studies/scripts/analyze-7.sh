#!/usr/bin/env bash
# Study 7: per value and language. CER on transcribed crops; loops, text kept and time on all crops.
set -euo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"; T=${TOOL:?}; A=analysis/s7; mkdir -p "$A"; R=results/s7
{ jq -c 'del(.image)' refs/ml-52.jsonl; jq -c 'del(.file)' s7/austrian.jsonl; } >"$A/refs.jsonl"
H=()
for v in default 768 1024 1536 2048 3072; do [ -s $R/s7-$v.b64 ] || continue
  scripts/decode.sh --kind qwen --in $R/s7-$v.b64 --out "$A/$v.raw.jsonl" >/dev/null; $T guard-cut --input "$A/$v.raw.jsonl" --out "$A/$v.jsonl" 2>/dev/null
  H+=(--hyp v$v=$A/$v.jsonl); done
$T score --refs "$A/refs.jsonl" "${H[@]}" --baseline v1024 --out "$A/score.jsonl"
$T invent --refs "$A/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
cut -f1,4,5 s7/n529.tsv >"$A/n529.tsv"
jq -R 'split("\t")|{id:.[0],lang:.[1],stratum:.[2]}' "$A/n529.tsv" | jq -s 'map({(.id):.})|add' >"$A/n529.json"
for v in default 768 1024 1536 2048 3072; do [ -s "$A/$v.jsonl" ] || continue
  jq -c --arg v "$v" --slurpfile m "$A/n529.json" -s --slurpfile base "$A/1024.jsonl" '($base|map({(.id):(.text|length)})|add) as $b | ($m[0]) as $meta |
   def lang: if $meta[.id] then $meta[.id].lang else (if (.id|startswith("pub-newseye-bnf")) then "fre" elif (.id|startswith("pub-newseye-nlf-fi")) then "fin" elif (.id|startswith("pub-newseye-nlf-sv")) then "swe" else "deu" end) end;
   group_by(lang)[]|{value:$v,lang:(.[0]|lang),crops:length,loops:(map(select(.looped==true))|length),cap:(map(select(.finish=="length"))|length),
     kept:((map(.text|length)|add)/(map($b[.id]//0)|add)),gpu_s:((map(.ms)|add)/1000/length)}' "$A/$v.jsonl"
done | tee "$A/by-language.jsonl" | jq -c '[.value,.lang,.crops,.loops,.cap,(.kept*100|round),(.gpu_s*10|round/10)]'
jq -c 'select(.group|test("^all$|^lang:"))|[.group,.reader,.items,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),((.cer_diff//0)*1000|round/10),((.cer_diff_lo//0)*1000|round/10),((.cer_diff_hi//0)*1000|round/10)]' "$A/score.jsonl"
jq -c 'select(.crops and .group=="all")|[.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/invent.jsonl"
cat $R/timing.txt
