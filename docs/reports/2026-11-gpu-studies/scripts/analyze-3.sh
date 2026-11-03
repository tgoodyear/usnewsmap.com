#!/usr/bin/env bash
# Study 3 tables: every reader on the same human-referenced crops, by source. A missing output scores as empty.
set -euo pipefail
if [ "${AMERICAN_STORIES_JSONL+x}" = x ] && [ "${AMERICAN_STORIES_B64+x}" = x ]; then
  echo "Set only one of AMERICAN_STORIES_JSONL and AMERICAN_STORIES_B64" >&2
  exit 1
fi
if [ "${AMERICAN_STORIES_JSONL+x}" = x ] && [ -z "$AMERICAN_STORIES_JSONL" ]; then
  echo "AMERICAN_STORIES_JSONL must name an input file" >&2
  exit 1
fi
if [ "${AMERICAN_STORIES_B64+x}" = x ] && [ -z "$AMERICAN_STORIES_B64" ]; then
  echo "AMERICAN_STORIES_B64 must name an input file" >&2
  exit 1
fi
if [ "${AMERICAN_STORIES_REFERENCE+x}" = x ] && [ -z "$AMERICAN_STORIES_REFERENCE" ]; then
  echo "AMERICAN_STORIES_REFERENCE must name an input file" >&2
  exit 1
fi
SCRIPTS=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd -P)
W=${GPU_STUDY_ROOT}; cd "$W"; T=${TOOL:-${GPU_STUDY_DEV}/target/release/gpu-study}; A=analysis/s3; mkdir -p "$A"; L=${GPU_STUDY_DEV}/local
cat refs/op-100.jsonl refs/rz-100.jsonl refs/ml-52.jsonl refs/nzz-360.jsonl | jq -c 'del(.image)' >"$A/refs.jsonl"
jq -r .id "$A/refs.jsonl" | LC_ALL=C sort >"$A/ids"
only() { jq -c --rawfile l "$A/ids" '($l|split("\n")) as $x|select(.id as $i|$x|index($i))' "$1"; }
scripts/decode.sh --kind qwen --in results/s3-qwen/readings.b64 --out "$A/qwen-all.jsonl"; only "$A/qwen-all.jsonl" >"$A/qwen.jsonl"
{ for f in op-100-psm6 ml-52-psm6; do scripts/decode.sh --kind plain --in "$L/out/$f.b64" --out "$A/t-$f.jsonl" >/dev/null; cat "$A/t-$f.jsonl"; done
  only analysis/s1/frak2021-psm6.jsonl; } >"$A/tesseract.jsonl"
jq -c 'select(.text|type=="string")' results/s3-surya/surya-crops.jsonl >"$A/surya-all.jsonl"; only "$A/surya-all.jsonl" >"$A/surya.jsonl"
# CHURRO answers in XML: keep the Page element's text, one tag boundary one line break. Parsed as text, never run.
jq -c 'select(.text|type=="string")|{id,text:(.text|sub("(?s)^.*?<Page>";"")|sub("(?s)</Page>.*$";"")|gsub("<[^>]+>";"\n")|gsub("&amp;";"&")|gsub("&lt;";"<")|gsub("&gt;";">")|gsub("\\n\\s*\\n+";"\n"))}' results/s3-churro/churro.jsonl >"$A/churro-all.jsonl"; only "$A/churro-all.jsonl" >"$A/churro.jsonl"
AS_ALL=$A/american-stories-all.jsonl; AS_FINAL=$A/american-stories.jsonl
AS_REFERENCE=${AMERICAN_STORIES_REFERENCE:-$SCRIPTS/../data/s3/american-stories.jsonl}
if [ ! -f "$AS_REFERENCE" ] || [ ! -r "$AS_REFERENCE" ]; then
  echo "American Stories retained reference is missing or unreadable; refusing to score" >&2
  exit 1
fi
if [ -e "$AS_ALL" ] && [ "$AS_REFERENCE" -ef "$AS_ALL" ]; then
  echo "American Stories retained reference aliases the intermediate output; refusing to score" >&2
  exit 1
fi
if [ "${AMERICAN_STORIES_JSONL+x}" = x ]; then
  scripts/decode.sh --kind american-stories-jsonl --in "$AMERICAN_STORIES_JSONL" --out "$AS_ALL"
else
  scripts/decode.sh --kind american-stories --in "${AMERICAN_STORIES_B64:-$L/out/as.b64}" --out "$AS_ALL"
fi
# The scorer takes the last reading per ID; verify those against the retained study before scoring.
if ! jq -en --rawfile ids "$A/ids" --slurpfile actual "$AS_ALL" --slurpfile retained "$AS_REFERENCE" '
  def by_id:
    reduce .[] as $row ({};
      if ($row.id | type) != "string" or ($row.text | type) != "string"
      then error("invalid American Stories row") else .[$row.id] = $row.text end);
  ($ids | split("\n")) as $selected
  | ($actual | map(select(.id as $id | $selected | index($id))) | by_id) == ($retained | by_id)
' >/dev/null; then
  echo "American Stories results do not match the retained study data; refusing to score" >&2
  exit 1
fi
if [ ! -e "$AS_FINAL" ] || [ ! "$AS_REFERENCE" -ef "$AS_FINAL" ]; then
  only "$AS_ALL" >"$AS_FINAL"
fi
H=(--hyp "qwen-8b=$A/qwen.jsonl" --hyp "tesseract=$A/tesseract.jsonl" --hyp "surya=$A/surya.jsonl" --hyp "churro=$A/churro.jsonl" --hyp "american-stories=$A/american-stories.jsonl")
jq -c 'select(.text|type=="string")' results/s3-hf/got.jsonl >"$A/got.jsonl"; H+=(--hyp "got=$A/got.jsonl")
{ cat results/s3-granite-3090/granite.jsonl results/s3-granite-local/granite.jsonl 2>/dev/null || true; } | jq -c 'select(.text|type=="string")' >"$A/granite.jsonl"; H+=(--hyp "granite=$A/granite.jsonl")
$T score --refs "$A/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare.jsonl"
$T score --fold --refs "$A/refs.jsonl" "${H[@]}" --baseline tesseract --out "$A/compare-folded.jsonl"
$T invent --refs "$A/refs.jsonl" "${H[@]}" --out "$A/invent.jsonl"
jq -c 'select(.group|startswith("source:"))|[.group,.reader,.items,.pages,.missing,(.cer*1000|round/10),(.cer_lo*1000|round/10),(.cer_hi*1000|round/10),(.median_crop_cer*1000|round/10),(.wer*1000|round/10)]' "$A/compare.jsonl"
jq -c 'select(.crops and .group=="all")|[.reader,.crops,.major,.invented_run,.over_length_guard,.under_length_guard,.under_word_overlap_guard]' "$A/invent.jsonl"
