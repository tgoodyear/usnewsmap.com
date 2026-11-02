#!/usr/bin/env bash
# Jochre 3 on whole pages through its local REST API (AGPL; run here for scoring only, never shipped).
# --pages DIR of <id>.png; --list file of ids, one per line; --out DIR; --url base URL of the server.
set -uo pipefail
PAGES=""; LIST=""; OUT=""; URL="http://127.0.0.1:3434"
while [ $# -gt 0 ]; do case "$1" in --pages) PAGES=$2; shift 2;; --list) LIST=$2; shift 2;; --out) OUT=$2; shift 2;; --url) URL=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$PAGES" ] && [ -n "$LIST" ] && [ -n "$OUT" ] || { echo "need --pages --list --out" >&2; exit 2; }
mkdir -p "$OUT/zip"; touch "$OUT/timing.tsv"
while read -r id; do
  [ -n "$id" ] || continue
  grep -q "^$id	" "$OUT/timing.tsv" && continue
  s=$(date +%s%N)
  code=$(curl -s -m 1800 -o "$OUT/zip/$id.zip" -w "%{http_code}" -X POST "$URL/ocr/file/zip" -H "accept: application/zip" -F "image=@$PAGES/$id.png;type=image/png" -F "outputFormats=Alto4,Text")
  e=$(date +%s%N)
  printf "%s\t%s\t%s\n" "$id" "$(( (e-s)/1000000 ))" "$code" >> "$OUT/timing.tsv"
done < "$LIST"
echo "done $(date -Is)" >> "$OUT/timing.tsv.done"
