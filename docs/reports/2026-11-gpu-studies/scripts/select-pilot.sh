#!/usr/bin/env bash
# Freeze the German pilot sample before any OCR. Universe: pages of the pilot list with a scan on disk and LoC's
# ALTO reachable without LoC's web hosts (batch archive on disk, or batch public in the S3 bucket). Ranked by
# sha256("gpu-studies-202611:de-pilot:"+doc_id); 75 bad and 25 moderate_high pages, at most 3 per title.
set -euo pipefail
CSV=""; INDEX=${SCAN_ROOT}/index-full.jsonl; SCANS=${SCAN_ROOT}; ARCHIVES=${LOC_ARCHIVES}; BUCKET=${SCAN_MANIFESTS}/bucket-batches.tsv; OUT=""
while [ $# -gt 0 ]; do case "$1" in
  --csv) CSV=$2; shift 2;; --index) INDEX=$2; shift 2;; --scans) SCANS=$2; shift 2;;
  --archives) ARCHIVES=$2; shift 2;; --bucket) BUCKET=$2; shift 2;; --out) OUT=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
tmp=$(mktemp -d)
{ ls "$ARCHIVES" | grep '\.tar\.bz2$' | sed 's/\.tar\.bz2$//'; awk -F'\t' '$2==200{print $1}' "$BUCKET"; } | LC_ALL=C sort -u >"$tmp/avail"
jq -r '"\(.batch)_ver\(.batch_version|tostring|if length<2 then "0"+. else . end),\(.doc_id)"' "$INDEX" | LC_ALL=C sort -t, -k1,1 |
  LC_ALL=C join -t, "$tmp/avail" - | cut -d, -f2 | LC_ALL=C sort -u >"$tmp/idx"
# rank,tier,_,_,doc_id,lccn,title... titles can hold commas, so take the leading fields and rely on lccn.
tail -n +2 "$CSV" | awk -F, '{print $5","$2","$6}' | LC_ALL=C sort -t, -k1,1 >"$tmp/pilot"
LC_ALL=C join -t, "$tmp/idx" "$tmp/pilot" | while IFS=, read -r doc tier lccn; do
  h=$(printf 'gpu-studies-202611:de-pilot:%s' "$doc" | sha256sum | cut -d' ' -f1); echo "$h,$doc,$tier,$lccn"
done | LC_ALL=C sort >"$tmp/ranked"
awk -F, 'BEGIN{q["bad"]=75;q["moderate_high"]=25}
  {n=q[$3]; if(n>0 && t[$4]<3){t[$4]++; q[$3]--; print $2","$3","$4}}' "$tmp/ranked" >"$OUT"
sha256sum "$OUT"; wc -l "$OUT"; rm -rf "$tmp"
