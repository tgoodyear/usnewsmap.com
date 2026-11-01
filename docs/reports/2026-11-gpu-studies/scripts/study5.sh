#!/usr/bin/env bash
# Study 5 rentals. --step guard-val|new2b|train8|surya6|export2b|deploy2b; run on the node with GPU_STUDY_ROOT set.
set -uo pipefail
W=${GPU_STUDY_ROOT:?}; cd "$W"
TRAIN=ghcr.io/tgoodyear/usnm-boinc-worker@sha256:960ba48cb33e6c164764246bba2c9aded7d791b48a6aed49f39c3a95acff3ee1
STEP=""
while [ $# -gt 0 ]; do case "$1" in --step) STEP=$2; shift 2;; *) exit 2;; esac; done
case "$STEP" in
  guard-val) P=push/s5-val; rm -rf "$P"; mkdir -p "$P"; ln -s "$PWD/lora/images" "$P/images"
    jq -c 'select(.id|startswith("pub-nzz") or startswith("pub-newseye-nlf-fi"))|.' lora/validation.jsonl >"$P/validation.jsonl"
    cp scripts/jobs/lora_infer.py "$P/"; rm -rf "$P/images"; mkdir -p "$P/images"; for d in nzz newseye-nlf-fi; do ln -s "$PWD/lora/images/$d" "$P/images/$d"; done
    scripts/rent.sh --study s5val --image "$TRAIN" --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 6 --cap-dollars 2 --disk 30 --push-dir "$P" --job scripts/jobs/guard-validation.sh --out results/s5-val;;
  new2b) P=push/s5-new; rm -rf "$P"; mkdir -p "$P"; ln -s "$PWD/s5sets/images" "$P/images"
    cat s5sets/bln.jsonl s5sets/dta.jsonl | jq -c '{id,image:(if .source=="bln600" then "images/bln/"+(.file|sub("images/";"")) else .file end)}' >"$P/new.jsonl"
    cp scripts/jobs/lora_infer.py results/s2-1/adapter-r8-s1000.safetensors "$P/"
    scripts/rent.sh --study s5new --image "$TRAIN" --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 4 --cap-dollars 2 --disk 30 --push-dir "$P" --job scripts/jobs/lora-new.sh --out results/s5-new;;
  surya6) P=push/s6; rm -rf "$P"; mkdir -p "$P/pages" "$P/c"; cp scripts/jobs/surya_read.py "$P/"
    B=bench-v1.0.0/usnm-ocr-bench-v1.0.0; C=bench-v1.1.0/usnm-ocr-bench-v1.1.0; N=/tank/usnm-dev/gpu-studies/data/conv/nzz
    for f in op-100 rz-100; do jq -r .image refs/$f.jsonl | while read -r i; do ln -s "$PWD/$B/$i" "$P/c/"; done; done
    jq -r .image refs/ml-52.jsonl | while read -r i; do ln -s "$PWD/$C/$i" "$P/c/"; done
    jq -r .image refs/nzz-360.jsonl | while read -r i; do ln -s "$N/$i" "$P/c/"; done
    cat refs/op-100.jsonl refs/rz-100.jsonl refs/ml-52.jsonl refs/nzz-360.jsonl | jq -r '[.id,("c/"+(.image|sub("images/";"")))]|@tsv' >"$P/list.tsv"
    while IFS=$'\t' read -r i r; do ln -s /tank/usnm-dev/gpu-studies/pilot/work/pages/$i.jpg "$P/pages/"; echo -e "$i\tpages/$i.jpg"; done <samples/order-pages.tsv >"$P/pages.tsv"
    printf 'list.tsv\tline\tsurya-line\npages.tsv\twhole\tsurya-whole\npages.tsv\tcapped\tsurya-capped\n' >"$P/runs.tsv"
    scripts/rent.sh --study s6 --image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:bccf7cba49878481d394f94f9ca313787d8f28886e369009ae4cfe94859fb494 --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 6 --cap-dollars 2 --disk 30 --push-dir "$P" --job scripts/jobs/surya6.sh --out results/s6;;
  export2b) P=push/s5-x2; rm -rf "$P"; mkdir -p "$P"; cp scripts/jobs/merge_lora.py results/s2-1/adapter-r8-s1000.safetensors "$P/"
    printf 'NAME=qwen2b-lora\nADAPTER=adapter-r8-s1000.safetensors\nQUANT=Q8_0\nBASEREPO=\nBASEREV=\n' >"$P/export.env"
    IDLE_MIN=40 scripts/rent.sh --study s5x2 --image "$TRAIN" --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 2 --cap-dollars 2 --disk 40 --push-dir "$P" --job scripts/jobs/export-gguf.sh --max-file-mb 6000 --out results/s5-x2;;
  deploy2b) P=push/s5-d2; rm -rf "$P"; mkdir -p "$P/images"; ln -s "$PWD/lora/images/bench" "$P/images/bench"; ln -s "$PWD/s5sets/images/bln" "$P/images/bln"; ln -s "$PWD/s5sets/images/dta" "$P/images/dta"
    { jq -r '[.id,.image]|@tsv' lora/test.jsonl; cat s5sets/bln.jsonl s5sets/dta.jsonl | jq -r '[.id,(if .source=="bln600" then "images/bln/"+(.file|sub("images/";"")) else .file end)]|@tsv'; } >"$P/list.tsv"
    cp scripts/jobs/qwen.sh results/s5-x2/qwen2b-lora-Q8_0.gguf "$P/"
    printf 'stock2b\tQwen3-VL-2B-Instruct-Q8_0.gguf\tmmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf\nlora2b\t/work/in/qwen2b-lora-Q8_0.gguf\tmmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf\n' >"$P/models.tsv"
    scripts/rent.sh --study s5d2 --image ghcr.io/tgoodyear/usnm-boinc-worker@sha256:6676d88226f050c63ad821c220aacd1205ac9f35c964c02dbdca8b93b031193e --gpu "RTX 3060" --min-gpu-ram-mb 11000 --max-dph 0.12 --max-hours 7 --cap-dollars 2 --disk 30 --push-dir "$P" --job scripts/jobs/deploy.sh --out results/s5-d2;;
  train8) P=push/s5-t8; rm -rf "$P"; mkdir -p "$P"; ln -s "$PWD/lora/images" "$P/images"
    cp lora/train.jsonl lora/validation.jsonl scripts/jobs/lora_train.py scripts/jobs/lora_infer.py "$P/"
    scripts/rent.sh --study s5t8 --image "$TRAIN" --gpu "RTX 3090" --max-hours 14 --cap-dollars 6 --disk 70 --push-dir "$P" --job scripts/jobs/lora8b-1.sh --out results/s5-t8;;
  *) exit 2;; esac
