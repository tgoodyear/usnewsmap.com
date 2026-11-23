#!/bin/sh
# One-time evaluation of the #2107 detector: inference on every evaluation set, then gutters.
# Run as nobody in a dev container. Scoring is a separate step (score.py, nontext.py) and runs once.
#   run_eval.sh --root DIR --cache DIR --sha256 HEX
set -eu
ROOT=""; CACHE=""; SHA=""
while [ $# -gt 0 ]; do case "$1" in
  --root) ROOT=$2; shift 2;; --cache) CACHE=$2; shift 2;; --sha256) SHA=$2; shift 2;;
  *) echo "unknown flag: $1" >&2; exit 2;; esac; done
cd "$ROOT"
export HOME="$ROOT" YOLO_CONFIG_DIR="$ROOT/.cfg"
PY="$ROOT/venv/bin/python"; C="$ROOT/code/layout_train"
mkdir -p out
N=3
for s in europeana finlam-test liberte-test colorado-val en-asgold; do
  i=0
  while [ $i -lt $N ]; do
    $PY $C/eval_infer.py --weights best.pt --expect-sha256 "$SHA" --cache-dir "$CACHE/$s" --refs refs-$s.json \
      --out out/boxes-$s-$i.json --shard-index $i --shard-count $N --threads 6 > out/log-$s-$i.txt 2>&1 &
    i=$((i+1))
  done
  wait
  $PY $C/eval_gutters.py --boxes out/boxes-$s-0.json out/boxes-$s-1.json out/boxes-$s-2.json --out out/det-$s.json
done
i=0
while [ $i -lt $N ]; do
  $PY $C/eval_infer.py --weights best.pt --expect-sha256 "$SHA" --rows biglam__loc_beyond_words__val.jsonl \
    --images-zip images.zip --out out/boxes-bwval-$i.json --shard-index $i --shard-count $N --threads 6 > out/log-bwval-$i.txt 2>&1 &
  i=$((i+1))
done
wait
echo inference-done > out/INFERENCE_DONE
