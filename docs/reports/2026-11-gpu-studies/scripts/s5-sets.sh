#!/usr/bin/env bash
# Freeze the two unused human-referenced sets of the Study 5 amendment. --conv holds converted BLN600; --dta holds GT4HistOCR's dta19 tree.
set -euo pipefail
CONV=""; DTA=""; OUT=""
while [ $# -gt 0 ]; do case "$1" in --conv) CONV=$2; shift 2;; --dta) DTA=$2; shift 2;; --out) OUT=$2; shift 2;; *) exit 2;; esac; done
mkdir -p "$OUT/images/bln" "$OUT/images/dta"
hash() { printf '%s' "$1" | sha256sum | cut -d' ' -f1; }
jq -r .crop_id "$CONV/bln600/crops.jsonl" | while read -r c; do echo "$(hash "gpu-studies-202611:bln600:$c") $c"; done | LC_ALL=C sort | head -200 | cut -d' ' -f2 >"$OUT/bln.ids"
jq -c --rawfile l "$OUT/bln.ids" '($l|split("\n")) as $x|select(.crop_id as $i|$x|index($i))|{id:.crop_id,ref:.transcription,page:.crop_id,source:"bln600",lang:"eng",file:.image}' "$CONV/bln600/crops.jsonl" >"$OUT/bln.jsonl"
jq -r .file "$OUT/bln.jsonl" | while read -r f; do cp "$CONV/bln600/$f" "$OUT/images/bln/"; done
# dta19: documents are directories; lines are NNN.nrm.png or .png with .gt.txt beside them
ls "$DTA" | while read -r d; do echo "$(hash "gpu-studies-202611:dta19:$d") $d"; done | LC_ALL=C sort | head -30 | cut -d' ' -f2 >"$OUT/dta.docs"
: >"$OUT/dta.jsonl"
while read -r d; do
  for g in "$DTA/$d"/*.gt.txt; do b=$(basename "$g" .gt.txt); echo "$(hash "gpu-studies-202611:dta19:$d/$b") $b"; done | LC_ALL=C sort | cut -d' ' -f2 >"$OUT/lines"
  n=0
  while read -r b; do
    [ "$n" -lt 10 ] || continue
    g="$DTA/$d/$b.gt.txt"; img=$(ls "$DTA/$d/$b".*png 2>/dev/null | head -1); [ -n "$img" ] || continue
    id="dta19-$(hash "$d/$b" | cut -c1-16)"; cp "$img" "$OUT/images/dta/$id.png"
    jq -nc --arg i "$id" --arg p "$d" --rawfile t "$g" '{id:$i,ref:$t,page:$p,source:"dta19",lang:"deu",file:("images/dta/"+$i+".png")}' >>"$OUT/dta.jsonl"
    n=$((n+1))
  done <"$OUT/lines"
done <"$OUT/dta.docs"
rm -f "$OUT/lines"
wc -l "$OUT/bln.jsonl" "$OUT/dta.jsonl"; sha256sum "$OUT/bln.jsonl" "$OUT/dta.jsonl"
