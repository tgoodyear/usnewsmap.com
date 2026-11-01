#!/usr/bin/env bash
# Run in the dev container: frozen pilot pages to JPEG and Qwen units, then cut the crops.
# --pages lists doc_id,tier,lccn. --scans holds the page scans; --alto LoC's ALTO; --work the output.
set -uo pipefail
PAGES=""; SCANS=""; ALTO=""; WORK=""; BIN=${GPU_STUDY_DEV}/target/release
while [ $# -gt 0 ]; do case "$1" in --pages) PAGES=$2; shift 2;; --scans) SCANS=$2; shift 2;; --alto) ALTO=$2; shift 2;; --work) WORK=$2; shift 2;; --bin) BIN=$2; shift 2;; *) exit 2;; esac; done
mkdir -p "$WORK/units" "$WORK/pages"; : >"$WORK/prep.log"
LLAMA=sha256:b0baccbe2e00b5d74e63f85f9f023c93d61d0f329475269e1e7002e87eb875ea
WEIGHTS=sha256:969266d1c24656977e06a6efacae04deac6d5093c77227fe55ac28975035cddd
while IFS=, read -r doc _ _; do
  [ -d "$WORK/units/$doc" ] && continue
  [ -s "$ALTO/$doc.xml" ] || { echo "$doc no-alto" >>"$WORK/prep.log"; continue; }
  src=$(ls "$SCANS/$doc".* 2>/dev/null | grep -E '\.(jp2|jpg)$' | head -1)
  [ -n "$src" ] || { echo "$doc no-scan" >>"$WORK/prep.log"; continue; }
  case "$src" in
    *.jp2) opj_decompress -i "$src" -o "$WORK/pages/$doc.png" >/dev/null 2>&1; "$BIN/gpu-study" jpeg --input "$WORK/pages/$doc.png" --out "$WORK/pages/$doc.jpg"; rm -f "$WORK/pages/$doc.png";;
    *) cp "$src" "$WORK/pages/$doc.jpg";;
  esac
  if "$BIN/usnm-boinc" qwen local-unit --image "$WORK/pages/$doc.jpg" --alto "$ALTO/$doc.xml" --dir "$WORK/units/$doc" --engine-llama $LLAMA --engine-weights $WEIGHTS >>"$WORK/prep.log" 2>&1
  then echo "$doc ok" >>"$WORK/prep.log"; else echo "$doc unit-failed" >>"$WORK/prep.log"; rm -rf "$WORK/units/$doc"; fi
done <"$PAGES"
"$BIN/gpu-study" cut --units "$WORK/units" --out "$WORK/crops"; wc -l "$WORK/crops/crops.jsonl"; echo PREP-DONE
