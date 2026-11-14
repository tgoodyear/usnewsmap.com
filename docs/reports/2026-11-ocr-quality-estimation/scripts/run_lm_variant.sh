#!/bin/sh
# One character-model training-text variant end to end (metrics, validation, verdicts, aggregate check).
#   run_lm_variant.sh --work DIR --sources gutenberg+wikisource --tag gw
set -eu
while [ $# -gt 0 ]; do case "$1" in --work) W="$2"; shift 2;; --sources) SRC="$2"; shift 2;; --tag) T="$2"; shift 2;; *) exit 2;; esac; done
cd "$W/scripts"
PY="nice -n 15 $W/venv/bin/python"
C="--res $W/data/qe-resources --confusions $W/confusions.json --yi-list $W/data/yi.txt --out-dir $W/metrics-$T --lm-sources $SRC"
mkdir -p "$W/out-$T"
$PY metrics.py --rows "$W/rows/truth.jsonl" --kind truth --langs eng,fra,deu,fin,swe $C
$PY metrics.py --rows "$W/rows/unlabelled.jsonl" --kind unlabelled --langs srp,yid,rus,slv,dan,ces,spa,fra,pol,nor,deu --max-crops 6000 --seed 1 $C
$PY validate.py --metrics-dir "$W/metrics-$T" --damage "$W/damage-truth.jsonl" --mlm "$W/mlm-truth.jsonl" --out "$W/out-$T/validation.json" --boot 1000 > "$W/out-$T/validate.log"
$PY apply.py --metrics-dir "$W/metrics-$T" --damage "$W/damage-unlabelled.jsonl" --truth-validation "$W/out-$T/validation.json" --mlm "$W/mlm-unlabelled.jsonl" --out "$W/out-$T/verdicts.json" --boot 1000 > "$W/out-$T/apply.log"
$PY aggregate_check.py --metrics-dir "$W/metrics-$T" --verdicts "$W/out-$T/verdicts.json" --out "$W/out-$T/aggregate-check.json" > "$W/out-$T/aggregate.log"
