#!/usr/bin/env bash
# The Japanese OCR image ($1) runs NDLOCR-Lite as its non-root user and reads
# a known headline (Rocky Shimpo, 1945-01-01, page 4) correctly.
set -euo pipefail
docker run --rm --entrypoint python3 "$1" -c \
  'import onnxruntime, cv2, pyarrow, azure.identity, azure.storage.blob; print("imports ok")'
work=$(mktemp -d)
mkdir -p "$work/img" "$work/out"
chmod 777 "$work/out"
cp ja-ocr/testdata/rocky-shimpo-1945-01-01-seq-4-headline.jpg "$work/img/"
docker run --rm -v "$work/img:/img:ro" -v "$work/out:/out" -w /opt/ndlocr-lite/src \
  --entrypoint python3 "$1" ocr.py --sourcedir /img --output /out > /dev/null
grep -q '去年の大記事' "$work/out/rocky-shimpo-1945-01-01-seq-4-headline.txt" ||
  { echo "NDLOCR-Lite didn't read the headline:" >&2; head -5 "$work"/out/*.txt >&2; exit 1; }
echo "headline read"
