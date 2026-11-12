#!/bin/bash
# Reads the same pages with two usnm_tesseract builds and compares them (#1835).
#
# usage: compare-tesseract-builds.sh --old BIN --new BIN --tessdata DIR --pages FILE --out DIR
#          --compare-tool BIN [--old-name NAME] [--new-name NAME] [--jobs N]
#          [--third BIN --third-name NAME]
#
# --pages is a TSV, one page per line: id, image path, profile (the -l value, e.g. deu_latf+deu),
# reference text path or "-", repetitions. Each page is read by both builds as production reads
# it: `-l PROFILE --psm 3 --dpi 300 txt alto`, OMP_THREAD_LIMIT=1, TESSDATA_PREFIX set. The runs
# of a page alternate (old then new, then new then old) so load drifts hit both. The user and
# system CPU seconds of every run go to runs.tsv, never wall time. The first run of each build
# is compared by tesseract-compare (text, character error rate against the reference, ALTO
# difference classes) into compare.jsonl. Pages run --jobs at a time.
# --third adds a third build to the timing only: the three take turns first, in rotation.
set -uo pipefail
OLD='' NEW='' TESSDATA='' PAGES='' OUT='' TOOL='' OLD_NAME=old NEW_NAME=new JOBS=1 THIRD='' THIRD_NAME=third
while [ $# -gt 0 ]; do
  case $1 in
    --old) OLD=$2;; --new) NEW=$2;; --tessdata) TESSDATA=$2;; --pages) PAGES=$2;; --out) OUT=$2;;
    --compare-tool) TOOL=$2;; --old-name) OLD_NAME=$2;; --new-name) NEW_NAME=$2;; --jobs) JOBS=$2;; --third) THIRD=$2;; --third-name) THIRD_NAME=$2;;
    *) echo "unknown flag $1" >&2; exit 2;;
  esac
  shift 2
done
for v in OLD NEW TESSDATA PAGES OUT TOOL; do [ -n "${!v}" ] || { echo "--${v,,} is required" >&2; exit 2; }; done
mkdir -p "$OUT/work"
OUT=$(cd "$OUT" && pwd)
[ -z "$THIRD" ] || THIRD=$(realpath "$THIRD")
OLD=$(realpath "$OLD") NEW=$(realpath "$NEW") TESSDATA=$(realpath "$TESSDATA") TOOL=$(realpath "$TOOL")

# One build reading one page once. Prints "user sys paragraphs"; leaves page.txt and page.xml.
read_page() { # build image profile dir
  local build=$1 image=$2 profile=$3 dir=$4
  mkdir -p "$dir"; cp "$image" "$dir/input.${image##*.}"
  local TIMEFORMAT='%U %S' t
  t=$( { time ( cd "$dir" && OMP_THREAD_LIMIT=1 TESSDATA_PREFIX="$TESSDATA" "$build" "input.${image##*.}" page \
        -l "$profile" --psm 3 --dpi 300 txt alto >"$dir/stdout.log" 2>&1 ) ; } 2>&1 )
  echo "$t $(grep -c '<TextBlock' "$dir/page.xml" 2>/dev/null || echo 0)"
  rm -f "$dir/input."*
}

one_page() { # a line of the pages file
  local id image profile ref reps
  IFS=$'\t' read -r id image profile ref reps <<<"$1"
  local rep order build name dir
  for ((rep = 1; rep <= reps; rep++)); do
    if [ -n "$THIRD" ]; then
      case $((rep % 3)) in 1) order="old new third";; 2) order="new third old";; *) order="third old new";; esac
    elif ((rep % 2)); then order="old new"; else order="new old"; fi
    for which in $order; do
      case $which in
        old) build=$OLD name=$OLD_NAME;; new) build=$NEW name=$NEW_NAME;; *) build=$THIRD name=$THIRD_NAME;;
      esac
      dir="$OUT/work/$id.$name.$rep"
      read -r user sys paragraphs < <(read_page "$build" "$image" "$profile" "$dir")
      printf '%s\t%s\t%s\t%s\t%s\t%s\t%s\n' "$profile" "$id" "$name" "$rep" "$user" "$sys" "$paragraphs" >> "$OUT/runs.tsv"
      if [ "$rep" -gt 1 ]; then rm -rf "$dir"; fi
    done
  done
  local refarg=(); [ "$ref" = - ] || refarg=(--ref "$ref")
  "$TOOL" --mode compare --page "$id" --txt-a "$OUT/work/$id.$OLD_NAME.1/page.txt" --txt-b "$OUT/work/$id.$NEW_NAME.1/page.txt" \
    --alto-a "$OUT/work/$id.$OLD_NAME.1/page.xml" --alto-b "$OUT/work/$id.$NEW_NAME.1/page.xml" "${refarg[@]}" >> "$OUT/compare.jsonl"
}
export -f one_page read_page
export OLD NEW THIRD TESSDATA OUT TOOL OLD_NAME NEW_NAME THIRD_NAME

grep -v '^#' "$PAGES" | xargs -d '\n' -P "$JOBS" -I{} bash -c 'one_page "$1"' _ {}
