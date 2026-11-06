#!/usr/bin/env bash
# Run in the dev container: frozen pilot pages to JPEG and Qwen units, then cut the crops.
# --pages lists doc_id,tier,lccn. --scans holds the page scans; --alto LoC's ALTO; --work the output.
set -uo pipefail
PAGES=""; SCANS=""; ALTO=""; WORK=""; LOG=""; BIN=${GPU_STUDY_DEV}/target/release
while [ $# -gt 0 ]; do case "$1" in --pages) PAGES=$2; shift 2;; --scans) SCANS=$2; shift 2;; --alto) ALTO=$2; shift 2;; --work) WORK=$2; shift 2;; --bin) BIN=$2; shift 2;; *) exit 2;; esac; done

fail() {
  echo "prep-pilot: $*" >&2
  if [ -n "$LOG" ]; then echo "$*" >>"$LOG" || true; fi
  exit 1
}
exists() { [ -e "$1" ] || [ -L "$1" ]; }
regular() { [ -f "$1" ] && [ -r "$1" ] && [ -s "$1" ] && [ ! -L "$1" ]; }
matches() { regular "$1" && cmp -s "$1" "$2"; }
complete() { regular "$1/page.jpg" && regular "$1/qwen.json"; }

[ -n "$PAGES" ] && [ -n "$SCANS" ] && [ -n "$ALTO" ] && [ -n "$WORK" ] || fail "missing required input path"
[ -f "$PAGES" ] && [ -r "$PAGES" ] && [ -s "$PAGES" ] || fail "cannot read pages list $PAGES"
mkdir -p "$WORK/units" "$WORK/pages" || fail "cannot create work directories"
LOG="$WORK/prep.log"
: >>"$LOG" || { LOG=""; fail "cannot append prep.log"; }
LLAMA=sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea
WEIGHTS=sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd

STAGE="$WORK/pages/.prep-pilot-$$"
mkdir -m 700 "$STAGE" || fail "cannot create staging directory $STAGE"
trap 'rm -rf -- "$STAGE"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
while IFS=, read -r doc _ _ || [ -n "${doc:-}" ]; do
  case "$doc" in ""|.*|*/*) fail "invalid document id: $doc";; esac
  unit="$WORK/units/$doc"; saved="$WORK/pages/$doc.jpg"
  if exists "$unit"; then
    [ -d "$unit" ] && [ ! -L "$unit" ] || fail "$doc: unit path is not a directory"
  fi
  if exists "$unit/qwen.json"; then
    complete "$unit" || fail "$doc: ambiguous qwen.json without a complete page; preserved"
    continue
  fi
  if exists "$unit/page.jpg"; then
    regular "$unit/page.jpg" || fail "$doc: invalid existing unit page; preserved"
  fi
  [ -s "$ALTO/$doc.xml" ] || fail "$doc no-alto"
  src=""
  for candidate in "$SCANS/$doc".*; do
    case "$candidate" in
      *.jp2|*.jpg) if exists "$candidate"; then src=$candidate; break; fi;;
    esac
  done
  [ -n "$src" ] && [ -s "$src" ] || fail "$doc no-scan"
  build="$STAGE/$doc"
  mkdir "$build" || fail "$doc: cannot create staging directory"
  case "$src" in
    *.jp2)
      opj_decompress -i "$src" -o "$build/page.png" >/dev/null 2>&1 || fail "$doc decompression-failed"
      "$BIN/gpu-study" jpeg --input "$build/page.png" --out "$build/input.jpg" || fail "$doc jpeg-failed";;
    *) cp "$src" "$build/input.jpg" || fail "$doc scan-copy-failed";;
  esac
  regular "$build/input.jpg" || fail "$doc: conversion produced no JPEG"
  if exists "$unit/page.jpg"; then
    matches "$unit/page.jpg" "$build/input.jpg" || fail "$doc: retained unit page differs; preserved"
  fi
  if exists "$saved"; then
    matches "$saved" "$build/input.jpg" || fail "$doc: saved page differs; preserved"
  else
    cp -n "$build/input.jpg" "$saved" || fail "$doc: cannot save page without overwriting"
    matches "$saved" "$build/input.jpg" || fail "$doc: saved page differs; preserved"
  fi
  # local-unit reads the page before replacing it, then fsyncs each file and its directory.
  image="$saved"
  if exists "$unit/page.jpg"; then image="$unit/page.jpg"; fi
  if exists "$unit/qwen.json"; then fail "$doc: qwen.json appeared during preparation; preserved"; fi
  if ! "$BIN/usnm-boinc" qwen local-unit --image "$image" --alto "$ALTO/$doc.xml" --dir "$unit" \
    --engine-llama "$LLAMA" --engine-weights "$WEIGHTS" >>"$LOG" 2>&1; then
    fail "$doc unit-failed"
  fi
  complete "$unit" || fail "$doc: local-unit produced an incomplete unit"
  echo "$doc ok" >>"$LOG" || fail "$doc: cannot append prep.log"
done <"$PAGES"

verify_units() {
  local doc
  while IFS=, read -r doc _ _ || [ -n "${doc:-}" ]; do
    complete "$WORK/units/$doc" || fail "$doc: frozen unit is incomplete"
  done <"$PAGES"
}
rm -rf -- "$STAGE" || fail "cannot remove staging directory $STAGE"
trap - EXIT
verify_units
"$BIN/gpu-study" cut --units "$WORK/units" --out "$WORK/crops" || fail "cut-failed"
verify_units
wc -l "$WORK/crops/crops.jsonl" || fail "cannot read crop manifest"
echo PREP-DONE
