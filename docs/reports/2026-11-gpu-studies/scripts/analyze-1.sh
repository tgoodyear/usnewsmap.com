#!/usr/bin/env bash
# Study 1 tables from the fetched outputs. Everything fetched is parsed as data here and never executed.
set -euo pipefail
W=${GPU_STUDY_ROOT}; cd "$W"; T=${TOOL:-${GPU_STUDY_DEV}/target/release/gpu-study}; A=analysis/s1; mkdir -p "$A"
scripts/decode.sh --kind qwen --in results/s1-qwen/readings.b64 --out "$A/qwen.jsonl"
for n in frak2021-psm3 frak2021-psm6 deu_latf-psm3 deu_latf-psm6 frak2021-page; do
  scripts/decode.sh --kind plain --in results/s1-tess/$n.b64 --out "$A/$n.jsonl"; done
scripts/decode.sh --kind plain --in ${LOCAL_OUT:-${GPU_STUDY_DEV}/local/out}/deu_latf-page.b64 --out "$A/deu_latf-page.jsonl"
cat refs/rz-700.jsonl refs/nzz-360.jsonl | jq -c 'del(.image)' >"$A/refs.jsonl"
H=(--hyp qwen-8b=$A/qwen.jsonl --hyp frak2021-psm3=$A/frak2021-psm3.jsonl --hyp frak2021-psm6=$A/frak2021-psm6.jsonl --hyp deu_latf-psm3=$A/deu_latf-psm3.jsonl --hyp deu_latf-psm6=$A/deu_latf-psm6.jsonl)
$T score --refs "$A/refs.jsonl" "${H[@]}" --baseline frak2021-psm3 --out "$A/measured.jsonl"
$T score --fold --refs "$A/refs.jsonl" "${H[@]}" --baseline frak2021-psm3 --out "$A/measured-folded.jsonl"
$T invent --refs "$A/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
# readings that ran out of tokens: the repetition loops
jq -r 'select(.id|startswith("pub-"))|.finish' "$A/qwen.jsonl" | sort | uniq -c >"$A/qwen-finish-measured.txt"
jq -r 'select(.id|startswith("pub-")|not)|.finish' "$A/qwen.jsonl" | sort | uniq -c >"$A/qwen-finish-pilot.txt"
# the same score with looped readings left out, to show how far they drive the pooled number
jq -c 'select(.finish=="stop")' "$A/qwen.jsonl" >"$A/qwen-stop.jsonl"
jq -r 'select(.finish!="stop")|.id' "$A/qwen.jsonl" >"$A/looped.ids"
jq -c --rawfile l "$A/looped.ids" '($l|split("\n")) as $x|select(.id as $i|$x|index($i)|not)' "$A/refs.jsonl" >"$A/refs-noloop.jsonl"
$T score --refs "$A/refs-noloop.jsonl" --hyp qwen-8b=$A/qwen-stop.jsonl --hyp frak2021-psm3=$A/frak2021-psm3.jsonl --baseline frak2021-psm3 --out "$A/measured-noloop.jsonl"
$T score --fold --refs "$A/refs-noloop.jsonl" --hyp qwen-8b=$A/qwen-stop.jsonl --hyp frak2021-psm3=$A/frak2021-psm3.jsonl --baseline frak2021-psm3 --out "$A/measured-noloop-folded.jsonl"
# agreement on the pilot crops, LoC's words in the box as one reader
jq -c '{id,text:.loc_text}' refs/pilot-crops.jsonl >"$A/loc.jsonl"
jq -c '{id,page,group:"pilot"}' refs/pilot-crops.jsonl >"$A/items.jsonl"
$T agree --items "$A/items.jsonl" "${H[@]}" --hyp loc=$A/loc.jsonl --out "$A/agreement.jsonl"
jq -c '{id,page:.id,group:"page"}' ${GPU_STUDY_DEV}/pilot/columns.jsonl >"$A/page-items.jsonl"
jq -c '{id,text:.loc_text}' ${GPU_STUDY_DEV}/pilot/columns.jsonl >"$A/page-loc.jsonl"
$T agree --items "$A/page-items.jsonl" --hyp frak2021=$A/frak2021-page.jsonl --hyp deu_latf=$A/deu_latf-page.jsonl --hyp loc=$A/page-loc.jsonl --out "$A/page-agreement.jsonl"
jq -c 'select(.group=="all" or (.group|startswith("source:"))) | {group,reader,items,pages,missing,cer:(.cer*10000|round/100),lo:(.cer_lo*10000|round/100),hi:(.cer_hi*10000|round/100),wer:(.wer*10000|round/100),diff:(.cer_diff//null)}' "$A/measured.jsonl"
jq -c 'select(.crops)' "$A/invent.jsonl"
jq -c '{a,b,items,pages,s:.symmetric_edit_rate,lo,hi,d:.mean_dice}' "$A/agreement.jsonl"
