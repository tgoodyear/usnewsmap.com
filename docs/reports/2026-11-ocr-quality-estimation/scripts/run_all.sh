#!/usr/bin/env bash
# The study end to end, on a host with the lake copied under --work (CT 245 here). Python 3.13 venv with
# numpy, pyarrow, rapidfuzz, wordfreq 3.1.1 (the lake's wheel), torch (CPU) and transformers; Hunspell 1.7.
#   run_all.sh --work /root/work/ocr-qe --proxy-rows PATH
# Period text (once, sequential dump reads): fetch_gutenberg.py and fetch_wikisource.py --out $W/data/qe-resources/wikisource.
# --work holds data/ (968-dev2-20261112, as-gold-20261113/en-as, qe-resources, yi.txt) and models/xlm-roberta-base.
set -euo pipefail
while [ $# -gt 0 ]; do case "$1" in --work) W="$2"; shift 2;; --proxy-rows) B="$2"; shift 2;; *) exit 2;; esac; done
cd "$W/scripts"
PY="nice -n 15 $W/venv/bin/python"
D="$W/data"
$PY prep.py --truth-968 "$D/968-dev2-20261112" --truth-as "$D/as-gold-20261113/en-as" --unlabelled "$D/qe-resources/unlabelled" --out-dir "$W/rows"
$PY confusions.py --truth "$W/rows/truth.jsonl" --out "$W/confusions.json"
COMMON=(--res "$D/qe-resources" --confusions "$W/confusions.json" --yi-list "$D/yi.txt" --out-dir "$W/metrics")
$PY metrics.py --rows "$W/rows/truth.jsonl" --kind truth --langs eng,fra,deu,fin,swe "${COMMON[@]}"
$PY metrics.py --rows "$W/rows/unlabelled.jsonl" --kind unlabelled --langs srp,yid,rus,slv,dan,ces,spa,fra,pol,nor,deu --max-crops 6000 --seed 1 "${COMMON[@]}"
./damage.sh --rows "$W/rows/truth.jsonl" --proxy-rows "$B" --out "$W/damage-truth.jsonl"
./damage.sh --rows "$W/rows/unlabelled.jsonl" --proxy-rows "$B" --out "$W/damage-unlabelled.jsonl"
$PY mlm.py --rows "$W/rows/truth.jsonl" --model "$W/models/xlm-roberta-base" --out "$W/mlm-truth.jsonl" --per-lang 50 --readers loc,qwen8b-local,tess3,tess6,xiaomi-local --threads 2
$PY mlm.py --rows "$W/rows/unlabelled.jsonl" --model "$W/models/xlm-roberta-base" --out "$W/mlm-unlabelled.jsonl" --per-lang 30 --langs srp,ces,spa,fra,yid,pol,rus,slv --threads 2
mkdir -p "$W/out"
$PY validate.py --metrics-dir "$W/metrics" --damage "$W/damage-truth.jsonl" --mlm "$W/mlm-truth.jsonl" --out "$W/out/validation.json" --boot 1000
$PY apply.py --metrics-dir "$W/metrics" --damage "$W/damage-unlabelled.jsonl" --truth-validation "$W/out/validation.json" --mlm "$W/mlm-unlabelled.jsonl" --out "$W/out/verdicts.json" --boot 1000
$PY aggregate_check.py --metrics-dir "$W/metrics" --verdicts "$W/out/verdicts.json" --out "$W/out/aggregate-check.json"
$PY overread_check.py --metrics "$W/metrics/truth-eng.jsonl" --verdicts "$W/out/verdicts.json" --out "$W/out/overread-enop.json"
$PY mlm_same_sample.py --metrics-dir "$W/metrics" --mlm "$W/mlm-truth.jsonl" --verdicts "$W/out/verdicts.json" --out "$W/out/mlm-same-sample.json"
$PY pack_data.py --work "$W" --out "$W/report-data"
$PY tables.py --validation "$W/report-data/validation.json" --verdicts "$W/report-data/verdicts.json" --confusions "$W/report-data/confusions-top.json" --out "$W/report-data/tables.md"
# Character-model training text: Gutenberg topped up from Wikisource, and Wikisource alone
./run_lm_variant.sh --work "$W" --sources gutenberg+wikisource --tag gw
./run_lm_variant.sh --work "$W" --sources wikisource --tag w
$PY compare_lm_sources.py --variant gutenberg="$W/out" --variant gutenberg+wikisource="$W/out-gw" --variant wikisource="$W/out-w" --out "$W/report-data/lm-sources.json"
