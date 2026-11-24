#!/usr/bin/env bash
# Training data for R3 (#2128), from the study's data directory.
#   prep_r3_r4c.sh --root DIR --glyphs BIN [--jobs N]
# DIR holds inputs/ (LoC scans, tessdata, NT text, test-set manifests, ChrEn, the #1621 model B files,
# langdata_lstm), work/ (LoC ALTO from the batch tars, S0's classified lines, phase 1's training-page reads
# and crops, the aligned rows) and out/ (the R4 candidates, the excluded verses). BIN is usnm-glyphs built
# from this commit. Everything is written under DIR/train/; an existing output is never overwritten.
set -euo pipefail
JOBS=4
while [ $# -gt 0 ]; do case "$1" in
  --root) ROOT=$2; shift 2;; --glyphs) GLYPHS=$2; shift 2;; --jobs) JOBS=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
HERE=$(cd "$(dirname "$0")/.." && pwd)
RECIPE=$(cd "$HERE/../../../ocr/models/chr-phoenix/recipe" && pwd)
cd "$ROOT"
T=train
NT=inputs/testset/src/nt/cnts.raw.txt
XV=out/silver/verses-any-aligned.txt
mkdir -p $T
step() { echo "== $(date -u +%FT%TZ) $*"; }

step "exclusions"
[ "$(wc -l < $XV)" -eq 274 ] || { echo "expected 274 excluded verses" >&2; exit 1; }
python3 "$HERE/exclude_manifest.py" --testset-manifest inputs/testset/v2/manifest.json \
  --ghn-test-pages work/ghn-test-pages.txt --out $T/exclude.json

step "GHN training crops read again, with word boxes (chr_phoenix) and stock chr"
[ -s $T/ghn-words.jsonl ] || python3 "$HERE/ghn_words.py" --reads work/reads/train-chr_phoenix.jsonl \
  --crops-dir work/crops --tessdata inputs/tessdata --out $T/ghn-words.jsonl --jobs "$JOBS"

step "ALTO and composite images for the GHN training pages"
[ -d $T/ghn-alto ] || python3 "$HERE/ghn_alto.py" --words $T/ghn-words.jsonl \
  --classified work/s0/ghn-s0-classified.jsonl --pages work/ghn-train-pages.txt --crops-dir work/crops \
  --out-alto $T/ghn-alto --out-images $T/ghn-images

step "glyph bank: LoC's 190 pages and the GHN training pages"
if [ ! -d $T/bank ]; then
  rm -rf $T/alto-all $T/images-all
  mkdir -p $T/alto-all $T/images-all
  cp -as "$PWD/work/loc-alto/." $T/alto-all/
  cp -as "$PWD/$T/ghn-alto/." $T/alto-all/
  for f in inputs/bank/images/*.jpg $T/ghn-images/*.jpg; do ln -s "$PWD/$f" $T/images-all/; done
  "$GLYPHS" build --alto-dir $T/alto-all --images $T/images-all --out $T/bank --threads "$JOBS"
  "$GLYPHS" clean --bank $T/bank --k 9 --min-agree 0.5 --threads "$JOBS" --exclude-manifest $T/exclude.json
fi

step "synthetic text: NT without the excluded verses, plus ChrEn"
[ -s $T/corpus.txt ] || python3 "$HERE/r3_corpus.py" --nt-text $NT --exclude-verses $XV \
  --chren-dir inputs/dict-1621/chren --out $T/corpus.txt --summary $T/corpus-summary.json

step "real LoC lines (round two's chr_real_lines.py), then line-end augmentation"
[ -d $T/real-loc ] || python3 "$RECIPE/chr_real_lines.py" --alto-dir work/loc-alto --images inputs/bank/images \
  --vocab inputs/chr-attested.txt --exclude-manifest $T/exclude.json --out $T/real-loc
[ -d $T/real ] || python3 "$HERE/augment_line_ends.py" --src $T/real-loc --out $T/real --bank $T/bank \
  --exclude-manifest $T/exclude.json --seed 2128

step "synthetic lines"
[ -d $T/synth ] || python3 "$HERE/r3_synth_lines.py" --bank $T/bank --text $T/corpus.txt \
  --exclude-manifest $T/exclude.json --nt-text $NT --exclude-verses $XV --out $T/synth --lines 20000 --seed 2128 \
  --marks-share 0.4 --punct-rate 0.15 --quote-rate 0.08 --edge-rate 0.3

step "start model with Ꮐ"
[ -s $T/start/lang/chr/chr.traineddata ] || bash "$HERE/train/start_model.sh" \
  --base inputs/dict-1621/models/B_stock_nt/chr.traineddata --langdata inputs/langdata_lstm --out $T/start

step "lstmf and lists"
for d in real synth; do bash "$HERE/train/make_lstmf.sh" --src $T/$d --out $T/lstmf --jobs "$JOBS"; done
python3 "$HERE/train/lists.py" --lstmf $T/lstmf --real real --synth synth --out-dir $T/lists-r3
step "PREP-DONE"
