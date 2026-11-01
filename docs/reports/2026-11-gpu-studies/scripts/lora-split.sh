#!/usr/bin/env bash
# Freeze the source-disjoint LoRA split from converted public sets and benchmark releases. Writes the row files, one
# JSON summary, and a push directory. Run once, before training; --out must not exist.
set -euo pipefail
CONV=/tank/usnm-dev/gpu-studies/data/conv; REFS=/tank/ocr/gpu-studies-202611/refs; BENCH=/tank/ocr/gpu-studies-202611/bench-v1.0.0/usnm-ocr-bench-v1.0.0; OUT=""
while [ $# -gt 0 ]; do case "$1" in --conv) CONV=$2; shift 2;; --refs) REFS=$2; shift 2;; --bench) BENCH=$2; shift 2;; --out) OUT=$2; shift 2;; *) exit 2;; esac; done
[ -e "$OUT" ] && { echo "--out must not exist" >&2; exit 2; }
mkdir -p "$OUT/images"
rows() { # dataset: crop rows with images copied under the dataset's name
  mkdir -p "$OUT/images/$1"; cp "$CONV/$1"/images/* "$OUT/images/$1/"
  jq -c --arg d "$1" '{id:.crop_id,image:("images/"+$d+"/"+(.image|sub("images/";""))),text:.transcription}' "$CONV/$1/crops.jsonl"; }
for d in austrian newseye-bnf newseye-nlf-sv; do rows "$d"; done >"$OUT/train.jsonl"
for d in nzz newseye-nlf-fi; do rows "$d"; done >"$OUT/validation.jsonl"
mkdir -p "$OUT/images/bench"
for f in rz-700 op-300; do jq -r .image "$REFS/$f.jsonl" | while read -r i; do cp "$BENCH/$i" "$OUT/images/bench/"; done; done
cat "$REFS/rz-700.jsonl" "$REFS/op-300.jsonl" | jq -c '{id,image:("images/bench/"+(.image|sub("images/";"")))}' >"$OUT/test.jsonl"
jq -c '{id:.crop_id,ref:.transcription,page:.title,source:.group,lang:.lang}' "$CONV/nzz/crops.jsonl" "$CONV/newseye-nlf-fi/crops.jsonl" >"$OUT/validation-refs.jsonl"
cat "$REFS/rz-700.jsonl" "$REFS/op-300.jsonl" | jq -c 'del(.image)' >"$OUT/test-refs.jsonl"
s() { sha256sum "$OUT/$1" | cut -d' ' -f1; }
jq -n --arg tr "$(s train.jsonl)" --arg va "$(s validation.jsonl)" --arg te "$(s test.jsonl)" --arg vr "$(s validation-refs.jsonl)" --arg tt "$(s test-refs.jsonl)" \
  --argjson a "$(wc -l <"$OUT/train.jsonl")" --argjson b "$(wc -l <"$OUT/validation.jsonl")" --argjson c "$(wc -l <"$OUT/test.jsonl")" '{
  v:1, train:{sources:["austrian","newseye-bnf","newseye-nlf-sv"],rows:$a,sha256:$tr},
  validation:{sources:["nzz","newseye-nlf-fi"],rows:$b,sha256:$va,refs_sha256:$vr},
  test:{sources:["reichsanzeiger","overproof-ca"],rows:$c,sha256:$te,refs_sha256:$tt}}' >"$OUT/lora-split.json"
sha256sum "$OUT/lora-split.json"
