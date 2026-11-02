#!/usr/bin/env bash
# Tesseract on a local node's CPU, in the pinned worker image unpacked by jobs/rootfs.sh, with the same models and flags
# as the rented run. --list holds id<TAB>path under --root; output is the same id<TAB>base64 file.
set -uo pipefail
LIST=""; ROOT=""; NAME=""; LANGS=""; PSM=3; RF=""; OUT=""; JOBS=2
while [ $# -gt 0 ]; do case "$1" in --list) LIST=$2; shift 2;; --root) ROOT=$2; shift 2;; --name) NAME=$2; shift 2;; --langs) LANGS=$2; shift 2;; --psm) PSM=$2; shift 2;; --rootfs) RF=$2; shift 2;; --out) OUT=$2; shift 2;; --jobs) JOBS=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
mkdir -p "$OUT" "$RF/work"; [ -d "$RF/work/in" ] || { mkdir -p "$RF/work/in"; }
export RF OUT
one() { IFS=$'\t' read -r id rel lang psm name <<<"$1"
  cp "$2/$rel" "$RF/work/in/$id.img"
  t=$(timeout 1800 chroot "$RF" /usr/bin/env TESSDATA_PREFIX=/work/tessdata OMP_THREAD_LIMIT=1 tesseract "/work/in/$id.img" - -l "$lang" --psm "$psm" --dpi 300 2>/dev/null | base64 -w0)
  rm -f "$RF/work/in/$id.img"; printf '%s\t%s\n' "$id" "$t" >>"$OUT/$name.b64"; }
export -f one
touch "$OUT/$NAME.b64"
awk -F'\t' -v l="$LANGS" -v p="$PSM" -v n="$NAME" '{printf "%s\t%s\t%s\t%s\t%s\n",$1,$2,l,p,n}' "$LIST" |
  xargs -P "$JOBS" -d '\n' -I{} nice -n 19 bash -c 'one "$1" "$2"' _ {} "$ROOT"
echo "done $NAME" >>"$OUT/progress.txt"
