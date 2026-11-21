#!/usr/bin/env bash
# Study 11 RapidOCR runs in CT 240: page line detection for the page arm, then region readings. --threads N.
set -uo pipefail
TH=2
while [ $# -gt 0 ]; do case "$1" in --threads) TH=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
S=/root/work/s11; O=$S/rapid; PY=/root/work/s9-rapid/venv/bin/python; mkdir -p "$O"
"$PY" -c "import rapidocr, onnxruntime; print(\"rapidocr\", rapidocr.__version__ if hasattr(rapidocr, \"__version__\") else \"?\", \"onnxruntime\", onnxruntime.__version__)" > "$O/versions.txt" 2>&1
"$PY" -m pip freeze 2>/dev/null | grep -i -E "rapidocr|onnxruntime|opencv" >> "$O/versions.txt"
for kind in detect read; do for s in de-rz de-cg en-asgold; do
  l=crops.jsonl; [ $kind = detect ] && l=pages.jsonl
  t=$(date +%s)
  "$PY" -I $S/scripts/rapid.py --in-dir $S/inputs/$s --list $S/inputs/$s/$l --out "$O/$s-$kind.jsonl" --mode $kind --threads "$TH" > "$O/$s-$kind.log" 2>&1
  echo "$(date -Is) $s $kind exit $? $(( $(date +%s) - t ))s" >> "$O/timings.txt"
done; done
echo "$(date -Is) all done" >> "$O/timings.txt"
