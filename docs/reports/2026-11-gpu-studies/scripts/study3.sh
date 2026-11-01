#!/usr/bin/env bash
# Study 3: same crops for every reader. --step qwen|surya|hf. Tesseract and American Stories are CPU engines and run on a local node (tesseract-local.sh).
set -uo pipefail
W=/tank/ocr/gpu-studies-202611; cd "$W"
QWEN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:6676d88226f050c63ad821c220aacd1205ac9f35c964c02dbdca8b93b031193e
SURYA=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494
TRAIN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:960ba48cb33e6c164764246bba2c9aded7d791b48a6aed49f39c3a95acff3ee1
STEP=""; PREFER=""
while [ $# -gt 0 ]; do case "$1" in --step) STEP=$2; shift 2;; --prefer-machine) PREFER=$2; shift 2;; *) exit 2;; esac; done
B=bench-v1.0.0/usnm-ocr-bench-v1.0.0; C=bench-v1.1.0/usnm-ocr-bench-v1.1.0; N=/tank/usnm-dev/gpu-studies/data/conv/nzz
build() { P=$1; rm -rf "$P"; mkdir -p "$P/c"
  for f in op-100 rz-100; do jq -r .image refs/$f.jsonl | while read -r i; do ln -s "$PWD/$B/$i" "$P/c/"; done; done
  jq -r .image refs/ml-52.jsonl | while read -r i; do ln -s "$PWD/$C/$i" "$P/c/"; done
  jq -r .image refs/nzz-360.jsonl | while read -r i; do ln -s "$N/$i" "$P/c/"; done
  cat refs/op-100.jsonl refs/rz-100.jsonl refs/ml-52.jsonl refs/nzz-360.jsonl | jq -c '{id,image:("c/"+(.image|sub("images/";"")))}' >"$P/crops.jsonl"
  [ "${JA_TOO:-1}" = 1 ] && { mkdir -p "$P/j"; ln -s "$PWD"/ja-set/c/*.png "$PWD"/ja-set/c/*.jpg "$P/j/"; jq -c '{id,image:("j/"+(.image|sub("c/";"")))}' ja-set/items.jsonl >>"$P/crops.jsonl"; }
  jq -r '[.id,.image]|@tsv' "$P/crops.jsonl" >"$P/list.tsv"; }
case "$STEP" in
  qwen) build push/s3-qwen; cp push/smoke/params.env push/s3-qwen/
    scripts/rent.sh --study s3qwen --image "$QWEN" --max-hours 3 --cap-dollars 2 --disk 40 --push-dir push/s3-qwen --job scripts/jobs/qwen.sh --prefer-machine "$PREFER" --out results/s3-qwen;;
  surya) build push/s3-surya; cp scripts/jobs/surya_read.py push/s3-surya/
    printf 'list.tsv\twhole\tsurya-crops\n' >push/s3-surya/runs.tsv
    if [ -f push/order-pages.tsv ]; then cp push/order-pages.tsv push/s3-surya/pages.tsv; mkdir -p push/s3-surya/pages
      while IFS=$'\t' read -r i r; do ln -s /tank/usnm-dev/gpu-studies/pilot/work/pages/$i.jpg push/s3-surya/pages/; done <push/order-pages.tsv
      printf 'pages.tsv\ttiled\tsurya-pages\n' >>push/s3-surya/runs.tsv; fi
    scripts/rent.sh --study s3surya --image "$SURYA" --max-hours 4 --cap-dollars 2 --disk 40 --push-dir push/s3-surya --job scripts/jobs/surya.sh --prefer-machine "$PREFER" --out results/s3-surya;;
  hf) build push/s3-hf; cp scripts/jobs/hf_engines.py push/s3-hf/
    jq -c 'select(.image|startswith("j/"))' push/s3-hf/crops.jsonl >push/s3-hf/ja-crops.jsonl; jq -c 'select(.image|startswith("c/"))' push/s3-hf/crops.jsonl >push/s3-hf/latin-crops.jsonl
    scripts/rent.sh --study s3hf --image "$TRAIN" --max-hours 8 --cap-dollars 3 --disk 80 --push-dir push/s3-hf --job scripts/jobs/hf-engines.sh --prefer-machine "$PREFER" --out results/s3-hf;;
  *) exit 2;; esac
