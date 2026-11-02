#!/usr/bin/env bash
# Tesseract from the pinned usnm_ocr engine image, one container per image as the worker runs it:
# no network, the image env (OMP_THREAD_LIMIT=1, the FPE shim), --dpi 300, the given language and PSM.
# --dir DIR of images, --list FILE (one file name per line), --lang L, --psm N, --image REF, --out DIR, --jobs N.
set -uo pipefail
DIR=""; LIST=""; LANGS=yid; PSM=3; IMG=""; OUT=""; JOBS=2
while [ $# -gt 0 ]; do case "$1" in --dir) DIR=$2; shift 2;; --list) LIST=$2; shift 2;; --lang) LANGS=$2; shift 2;; --psm) PSM=$2; shift 2;; --image) IMG=$2; shift 2;; --out) OUT=$2; shift 2;; --jobs) JOBS=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
for v in DIR LIST IMG OUT; do [ -n "${!v}" ] || { echo "missing --${v,,}" >&2; exit 2; }; done
mkdir -p "$OUT/txt"; export DIR LANGS PSM IMG OUT
one() { f=$1; b=${f%.*}; [ -f "$OUT/txt/$b.txt" ] && return
  s=$(date +%s%N)
  podman run --rm --network none --read-only --cap-drop all -v "$DIR/$f:/work/$f:ro" "$IMG" "/work/$f" - -l "$LANGS" --psm "$PSM" --dpi 300 > "$OUT/txt/$b.tmp" 2>/dev/null; rc=$?
  e=$(date +%s%N); mv "$OUT/txt/$b.tmp" "$OUT/txt/$b.txt"; printf "%s\t%s\t%s\n" "$b" "$(( (e-s)/1000000 ))" "$rc" >> "$OUT/timing.tsv"; }
export -f one
nice -n 15 xargs -a "$LIST" -P "$JOBS" -I{} bash -c "one {}"
echo "done $(date -Is)" >> "$OUT/done"
