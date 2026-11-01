#!/usr/bin/env bash
# Study 4: the same 40 pages on one card class. --gpu "RTX 3090" --tag 3090 --kind qwen|surya
set -uo pipefail
W=${GPU_STUDY_ROOT}; cd "$W"
QWEN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:6676d88226f050c63ad821c220aacd1205ac9f35c964c02dbdca8b93b031193e
SURYA=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494
GPU=""; TAG=""; KIND=""; MAXDPH=0.30; PREFER=""
while [ $# -gt 0 ]; do case "$1" in --gpu) GPU=$2; shift 2;; --tag) TAG=$2; shift 2;; --kind) KIND=$2; shift 2;; --max-dph) MAXDPH=$2; shift 2;; --prefer-machine) PREFER=$2; shift 2;; *) exit 2;; esac; done
PC=${GPU_STUDY_DEV}/pilot/work; P=push/s4-$KIND-$TAG; rm -rf "$P"; mkdir -p "$P/crops" "$P/pages"
cut -d, -f1 samples/de-pilot-100.csv | head -40 >"$P/pages.ids"
echo "$KIND" >"$P/kind.txt"; cp scripts/jobs/qwen.sh scripts/jobs/surya.sh scripts/jobs/surya_read.py "$P/"
if [ "$KIND" = qwen ]; then
  jq -r '[.id,.page,.file]|@tsv' refs/pilot-crops.jsonl | LC_ALL=C sort -t$'\t' -k2,2 | LC_ALL=C join -t$'\t' -1 2 -2 1 -o 1.1,1.3 - <(LC_ALL=C sort "$P/pages.ids") | awk -F'\t' '{print $1"\tcrops/"$2}' >"$P/list.tsv"
  cut -f2 "$P/list.tsv" | sed 's#crops/##' | while read -r f; do ln -s "$PC/crops/$f" "$P/crops/"; done
  cp push/smoke/params.env "$P/"; IMG=$QWEN
else
  while read -r d; do ln -s "$PC/pages/$d.jpg" "$P/pages/"; echo -e "$d\tpages/$d.jpg"; done <"$P/pages.ids" >"$P/pages.tsv"
  printf 'pages.tsv\ttiled\tsurya-pages\n' >"$P/runs.tsv"; IMG=$SURYA
fi
scripts/rent.sh --study "s4$KIND$TAG" --image "$IMG" --gpu "$GPU" --max-dph "$MAXDPH" --max-hours 8 --cap-dollars 6 --disk 40 --push-dir "$P" --job scripts/jobs/throughput.sh --prefer-machine "$PREFER" --out "results/s4-$KIND-$TAG"
