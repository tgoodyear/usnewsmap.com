#!/usr/bin/env bash
# Study 1: build the two push directories from the frozen sets and run them, then decode. Run on the node.
set -uo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"
QWEN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:6676d88226f050c63ad821c220aacd1205ac9f35c964c02dbdca8b93b031193e
TESS=ubuntu:24.04
STEP=""; PREFER=""
while [ $# -gt 0 ]; do case "$1" in --step) STEP=$2; shift 2;; --prefer-machine) PREFER=$2; shift 2;; *) exit 2;; esac; done
B=bench-v1.0.0/usnm-ocr-bench-v1.0.0; N=/tank/usnm-dev/gpu-studies/data/conv/nzz; PC=/tank/usnm-dev/gpu-studies/pilot/work
build() { P=$1; rm -rf "$P"; mkdir -p "$P/rz" "$P/nzz" "$P/pilot" "$P/pages"
  ln -s "$PWD/$B/images"/pub-reichsanzeiger-* "$P/rz/" 2>/dev/null
  { jq -r '[.id,("rz/"+(.image|sub("images/";"")))]|@tsv' refs/rz-700.jsonl
    jq -r '[.id,("nzz/"+(.image|sub("images/";"")))]|@tsv' refs/nzz-360.jsonl
    jq -r '[.id,("pilot/"+.file)]|@tsv' "$PC/crops/crops.jsonl"; } >"$P/list.tsv"
  ln -s "$N"/images/* "$P/nzz/"; ln -s "$PC"/crops/*.png "$P/pilot/"
}
case "$STEP" in
  qwen) build push/s1-qwen; cp push/smoke/params.env push/s1-qwen/
    scripts/rent.sh --study s1qwen --image "$QWEN" --max-hours 7 --cap-dollars 2.5 --disk 40 --push-dir push/s1-qwen --job scripts/jobs/qwen.sh --prefer-machine "$PREFER" --out results/s1-qwen;;
  tess) build push/s1-tess; ln -s "$PC"/pages/*.jpg push/s1-tess/pages/; cp scripts/jobs/rootfs.sh push/s1-tess/
    ls "$PC/pages" | sed -n 's/\.jpg$//p' | awk '{print $1"\tpages/"$1".jpg"}' >push/s1-tess/pages.tsv
    printf 'list.tsv\tfrak2021-psm3\tfrak2021\t3\nlist.tsv\tfrak2021-psm6\tfrak2021\t6\nlist.tsv\tdeu_latf-psm3\tdeu_latf+deu\t3\nlist.tsv\tdeu_latf-psm6\tdeu_latf+deu\t6\npages.tsv\tfrak2021-page\tfrak2021\t3\npages.tsv\tdeu_latf-page\tdeu_latf+deu\t3\n' >push/s1-tess/runs.tsv
    scripts/rent.sh --study s1tess --image "$TESS" --max-hours 3 --cap-dollars 1 --disk 30 --push-dir push/s1-tess --job scripts/jobs/tesseract.sh --prefer-machine "$PREFER" --out results/s1-tess;;
  *) exit 2;; esac
