#!/usr/bin/env bash
# Runs in the training image with its Hugging Face offline switches off. One pinned revision per engine; the three
# engines share one crop list. A failed engine leaves its output short and the next one still runs.
set -u
O=/work/out; I=/work/in
unset HF_HUB_OFFLINE TRANSFORMERS_OFFLINE HF_DATASETS_OFFLINE
export HF_HOME=/work/hf TOKENIZERS_PARALLELISM=false
PY=$(command -v python)
$PY -m pip -q install --no-cache-dir 'git+https://github.com/stanford-oval/Churro.git@23c3ac65781443b18d87c8cdac5f77445c3319e6' > $O/pip-churro.log 2>&1
$PY -m pip freeze > $O/pip-freeze.txt 2>&1
rev() { curl -fsS "https://huggingface.co/api/models/$1" | sed -n 's/.*"sha":"\([0-9a-f]\{40\}\)".*/\1/p' | head -1; }
GOT=$(rev stepfun-ai/GOT-OCR-2.0-hf); GRANITE=$(rev ibm-granite/granite-docling-258M)
echo "got $GOT"$'\n'"granite $GRANITE"$'\n'"churro ca2150ea465d5a3d67818c50e234b9422619c75d" > $O/revisions.txt
# CHURRO reads everything. GOT and Granite don't claim Japanese, so they get the Latin-script crops only.
for e in "churro ca2150ea465d5a3d67818c50e234b9422619c75d crops" "got $GOT latin-crops" "granite $GRANITE latin-crops"; do set -- $e
  $PY $I/hf_engines.py --engine $1 --revision $2 --list $I/$3.jsonl --root $I --out $O/$1.jsonl > $O/$1.log 2>&1
  echo "done $1 exit $?" >> $O/progress.txt
done
