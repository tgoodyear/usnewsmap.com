#!/usr/bin/env bash
# Tesseract from the pinned usnm_ocr engine image, one container per image as the worker runs it:
# no network, the image env (OMP_THREAD_LIMIT=1, the FPE shim), --dpi 300, the given language and PSM.
# --dir DIR of images, --list FILE (one file name per line), --lang L, --psm N, --image REF, --out DIR, --jobs N.
set -uo pipefail
DIR=""; LIST=""; LANGS=yid; PSM=3; IMG=""; OUT=""; JOBS=2
while [ $# -gt 0 ]; do case "$1" in --dir) DIR=$2; shift 2;; --list) LIST=$2; shift 2;; --lang) LANGS=$2; shift 2;; --psm) PSM=$2; shift 2;; --image) IMG=$2; shift 2;; --out) OUT=$2; shift 2;; --jobs) JOBS=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$DIR" ] && [ -n "$LIST" ] && [ -n "$IMG" ] && [ -n "$OUT" ] || { echo "missing required argument" >&2; exit 2; }
[[ "$JOBS" =~ ^[0-9]+$ ]] && [ "$((10#$JOBS))" -gt 0 ] || { echo "jobs must be positive" >&2; exit 2; }
if ! mkdir -p "$OUT/txt" || ! rm -f "$OUT/done"; then
  echo "cannot prepare output directory" >&2
  exit 1
fi
export DIR LANGS PSM IMG OUT
one() { f=$1; [ -n "$f" ] || return 0; b=${f%.*}
  if [ -f "$OUT/txt/$b.txt" ] && [ -f "$OUT/timing.tsv" ] &&
    awk -F '\t' -v id="$b" '$1 == id { seen=1; valid=(NF == 3 && $3 == "0") } END { exit !(seen && valid) }' "$OUT/timing.tsv"; then
    return 0
  fi
  rm -f "$OUT/txt/$b.txt" "$OUT/txt/$b.tmp" || return 1
  s=$(date +%s%N)
  podman run --rm --network none --read-only --cap-drop all -v "$DIR/$f:/work/$f:ro" "$IMG" "/work/$f" - -l "$LANGS" --psm "$PSM" --dpi 300 > "$OUT/txt/$b.tmp" 2>/dev/null; rc=$?
  e=$(date +%s%N)
  if [ "$rc" -eq 0 ]; then
    mv "$OUT/txt/$b.tmp" "$OUT/txt/$b.txt" || return 1
  else
    rm -f "$OUT/txt/$b.tmp" || return 1
  fi
  printf "%s\t%s\t%s\n" "$b" "$(( (e-s)/1000000 ))" "$rc" >> "$OUT/timing.tsv" || return 1
  [ "$rc" -eq 0 ]
}
export -f one
# The filename is an argument to the fixed shell command, not shell code.
# shellcheck disable=SC2016
tr '\n' '\0' < "$LIST" | nice -n 15 xargs -0 -P "$JOBS" -I{} bash -c 'one "$1"' _ '{}'
rc=$?
[ "$rc" -eq 0 ] || { echo "one or more Tesseract reads failed" >&2; exit "$rc"; }
echo "done $(date -Is)" >> "$OUT/done"
