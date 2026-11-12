#!/bin/bash
# Run the Linux spot pages and the calibrated English challenge through the macOS build.
# usage: run-mac.sh --arch arm64 --build DIR --jobs DIR --linux-ref DIR --out DIR [--only pages|gold]
set -uo pipefail
ONLY=all
while [ $# -gt 0 ]; do case $1 in --arch) ARCH=$2;; --build) BUILD=$2;; --jobs) JOBS=$2;; --linux-ref) REF=$2;; --out) OUT=$2;; --only) ONLY=$2;; *) echo "bad $1"; exit 2;; esac; shift 2; done
# Absolute paths: the worker runs inside each slot directory (added after the run, for review).
mkdir -p "$OUT"; BUILD=$(cd "$BUILD" && pwd); JOBS=$(cd "$JOBS" && pwd); REF=$(cd "$REF" && pwd); OUT=$(cd "$OUT" && pwd)
link() { for f in "$BUILD"/*; do b=$(basename $f); [ $b = usnm_ocr ] || echo "<soft_link>$f</soft_link>" > "$1/$b"; done; }
if [ $ONLY != gold ]; then
for d in "$JOBS"/pages/*; do
  n=$(basename $d); s="$OUT/pages/$n"; rm -rf "$s"; mkdir -p "$s"
  cp "$d/request.json" "$s/request.json"; cp "$REF/spot/runs/$n/container/page" "$s/page"; link "$s"
  st=$(date +%s); (cd "$s" && "$BUILD/usnm_ocr" > "$OUT/pages/$n.log" 2>&1); rc=$?
  echo "$n rc=$rc $(( $(date +%s) - st ))s"
done
fi
if [ $ONLY != pages ]; then
s="$OUT/gold"; rm -rf "$s"; mkdir -p "$s"; cp "$JOBS"/gold/slot/{gold.json,bundle.tar} "$s/"; link "$s"
cp "$REF"/campaign/cache/*.zip "$s/"
st=$(date +%s); (cd "$s" && "$BUILD/usnm_ocr" > "$OUT/gold.log" 2>&1); echo "gold rc=$? $(( $(date +%s) - st ))s"
fi
echo RUN-DONE
