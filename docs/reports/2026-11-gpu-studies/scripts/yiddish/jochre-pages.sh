#!/usr/bin/env bash
# Jochre 3 on whole pages through its local REST API (AGPL; run here for scoring only, never shipped).
# --pages DIR of <id>.png; --list file of ids, one per line; --out DIR; --url base URL of the server.
set -uo pipefail
PAGES=""; LIST=""; OUT=""; URL="http://127.0.0.1:3434"
while [ $# -gt 0 ]; do case "$1" in --pages) PAGES=$2; shift 2;; --list) LIST=$2; shift 2;; --out) OUT=$2; shift 2;; --url) URL=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$PAGES" ] && [ -n "$LIST" ] && [ -n "$OUT" ] || { echo "need --pages --list --out" >&2; exit 2; }
[ -f "$LIST" ] && [ -r "$LIST" ] || { echo "cannot read ID list" >&2; exit 2; }
exec 3< "$LIST" || { echo "cannot open ID list" >&2; exit 2; }
if ! mkdir -p "$OUT/zip" || ! touch "$OUT/timing.tsv" || ! rm -f "$OUT/timing.tsv.done"; then
  echo "cannot prepare output directory" >&2
  exit 1
fi
valid_zip() {
  python3 -I - "$1" <<'PY'
import sys, zipfile
try:
    with zipfile.ZipFile(sys.argv[1]) as archive:
        texts = [name for name in archive.namelist() if name.endswith(".txt")]
        if not texts or archive.testzip() is not None:
            sys.exit(1)
        archive.read(texts[0]).decode("utf-8")
except Exception:
    sys.exit(1)
PY
}
failed=0
while IFS= read -r id || [ -n "$id" ]; do
  [ -n "$id" ] || continue
  zip="$OUT/zip/$id.zip"; partial="$zip.part"
  if [ -f "$zip" ] &&
    awk -F '\t' -v id="$id" '$1 == id { seen=1; valid=(NF == 3 && $3 == "200") } END { exit !(seen && valid) }' "$OUT/timing.tsv" &&
    valid_zip "$zip"; then
    continue
  fi
  if ! rm -f "$zip" "$partial"; then failed=1; continue; fi
  s=$(date +%s%N)
  code=$(curl -s -m 1800 -o "$partial" -w "%{http_code}" -X POST "$URL/ocr/file/zip" -H "accept: application/zip" -F "image=@$PAGES/$id.png;type=image/png" -F "outputFormats=Alto4,Text"); rc=$?
  e=$(date +%s%N)
  if [ "$rc" -ne 0 ] || [[ ! "$code" =~ ^[0-9]{3}$ ]]; then code=000; fi
  if [ "$rc" -eq 0 ] && [ "$code" = 200 ] && valid_zip "$partial"; then
    if ! mv -f "$partial" "$zip"; then failed=1; fi
  else
    rm -f "$partial" || true
    failed=1
  fi
  printf "%s\t%s\t%s\n" "$id" "$(( (e-s)/1000000 ))" "$code" >> "$OUT/timing.tsv" || failed=1
done <&3
exec 3<&-
[ "$failed" -eq 0 ] || { echo "one or more Jochre reads failed" >&2; exit 1; }
echo "done $(date -Is)" >> "$OUT/timing.tsv.done"
