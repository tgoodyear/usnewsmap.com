#!/usr/bin/env bash
# The Japanese crop set of the protocol's amendment. --ja holds images/, sample.csv and crops/ from the #128 sample.
set -euo pipefail
JA=""; TOOL=""; OUT=""
while [ $# -gt 0 ]; do case "$1" in --ja) JA=$2; shift 2;; --tool) TOOL=$2; shift 2;; --out) OUT=$2; shift 2;; *) exit 2;; esac; done
mkdir -p "$OUT/c"
ls "$JA/crops/images" | sed 's/__crop\.jpg$//' | LC_ALL=C sort >"$OUT/cut.ids"
ls "$JA/images" | sed 's/\.jpg$//' | LC_ALL=C sort | LC_ALL=C comm -23 - "$OUT/cut.ids" | while read -r p; do
  printf '%s %s\n' "$(printf 'gpu-studies-202611:ja:%s' "$p" | sha256sum | cut -d' ' -f1)" "$p"; done | LC_ALL=C sort | cut -d' ' -f2 >"$OUT/ranked.ids"
{ grep -E '^sn8302551[78]_' "$OUT/ranked.ids" | head -20; grep -vE '^sn8302551[78]_' "$OUT/ranked.ids" | head -20; } >"$OUT/new.ids"
while read -r p; do case "$p" in sn8302551[78]_*) g=typeset;; *) g=camp;; esac
  jq -nc --arg i "$p.new" --arg p "$p" --arg g "$g" --arg f "$JA/images/$p.jpg" '{id:$i,page:$p,group:$g,image_path:$f}'; done <"$OUT/new.ids" >"$OUT/new-pages.jsonl"
"$TOOL" ja-cut --pages "$OUT/new-pages.jsonl" --out "$OUT/c"
{ jq -c '{id,page,group,image:("c/"+.file)}' "$OUT/c/crops.jsonl"
  for f in "$JA"/crops/images/*.jpg; do b=$(basename "$f" .jpg); p=${b%__crop}; cp "$f" "$OUT/c/$b.jpg"
    case "$p" in sn8302551[78]_*) g=typeset;; *) g=camp;; esac
    if [ -f "$JA/crops/refs/$b.txt" ]; then r="$JA/crops/refs/$b.txt"; else r=/dev/null; fi
    jq -nc --arg i "$b" --arg p "$p" --arg g "$g" --arg im "c/$b.jpg" --rawfile t "$r" '{id:$i,page:$p,group:$g,image:$im}+(if ($t|length)>0 then {ref:$t} else {} end)'; done; } >"$OUT/items.jsonl"
wc -l "$OUT/items.jsonl"; jq -r '[.group,(has("ref"))]|@tsv' "$OUT/items.jsonl" | sort | uniq -c; sha256sum "$OUT/items.jsonl"
