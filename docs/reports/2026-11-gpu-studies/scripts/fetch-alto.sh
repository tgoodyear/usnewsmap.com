#!/usr/bin/env bash
# LoC's ALTO for a frozen page list: S3 bucket ndnp-batches for public batches, one request at a time, then the
# batch archive on disk, which takes minutes per archive to decompress. Nothing goes to LoC's web hosts. Writes <out>/<doc_id>.xml and <out>/provenance.tsv.
set -uo pipefail
SAMPLE=""; OUT=""; INDEX=/tank/ocr/scans/index-full.jsonl; ARCHIVES=/tank/locbulk/ocr; BUCKET=/tank/ocr/manifests/bucket-batches.tsv; GAP=3
while [ $# -gt 0 ]; do case "$1" in
  --sample) SAMPLE=$2; shift 2;; --out) OUT=$2; shift 2;; --index) INDEX=$2; shift 2;;
  --archives) ARCHIVES=$2; shift 2;; --bucket) BUCKET=$2; shift 2;; --gap-seconds) GAP=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
mkdir -p "$OUT"; tmp=$(mktemp -d); cut -d, -f1 "$SAMPLE" | LC_ALL=C sort -u >"$tmp/ids"
jq -r '[.doc_id,.batch,(.batch_version|tostring),.iiif_base]|@tsv' "$INDEX" | LC_ALL=C sort -t$'\t' -k1,1 | LC_ALL=C join -t$'\t' "$tmp/ids" - >"$tmp/pages"
while IFS=$'\t' read -r doc batch ver iiif; do
  [ -s "$OUT/$doc.xml" ] && continue
  v=$(printf '%02d' "$ver"); name="${batch}_ver$v"; rel=${iiif##*:data:}; rel=${rel//://}
  if ! grep -q "^$name"$'\t200' "$BUCKET" && [ -f "$ARCHIVES/$name.tar.bz2" ]; then
    lccn=${doc%%_*}; rest=${doc#*_}; date=${rest%%_*}; rest=${rest#*_}; ed=${rest%%_*}; seq=${rest#*_}
    echo "$name.tar.bz2 $lccn/${date//-//}/$ed/$seq/ocr.xml $doc" >>"$tmp/from-archive"
  else
    url="https://ndnp-batches.s3.amazonaws.com/$name/data/$rel.xml"
    code=$(curl -sS -o "$tmp/x" -w '%{http_code}' --max-time 60 -A 'usnewsmap-gpu-study' "$url")
    if [ "$code" = 200 ] && head -c 2000 "$tmp/x" | grep -q '<alto'; then
      mv "$tmp/x" "$OUT/$doc.xml"; printf '%s\ts3\t%s\t%s\n' "$doc" "$url" "$(sha256sum "$OUT/$doc.xml"|cut -d' ' -f1)" >>"$OUT/provenance.tsv"
    else printf '%s\tmissing\t%s\t%s\n' "$doc" "$url" "$code" >>"$OUT/provenance.tsv"; fi
    sleep "$GAP"
  fi
done <"$tmp/pages"
take() { a=$1; d="$tmp/x-$a"; mkdir -p "$d"; grep "^$a " "$tmp/from-archive" >"$tmp/this-$a"
  nice -n 19 tar -xjf "$ARCHIVES/$a" -C "$d" $(cut -d' ' -f2 "$tmp/this-$a") 2>/dev/null
  while read -r _ path doc; do
    if [ -s "$d/$path" ]; then mv "$d/$path" "$OUT/$doc.xml"; printf '%s\tarchive\t%s\t%s\n' "$doc" "$a" "$(sha256sum "$OUT/$doc.xml"|cut -d' ' -f1)" >>"$OUT/provenance.tsv"
    else printf '%s\tmissing\t%s\tnot-in-archive\n' "$doc" "$a" >>"$OUT/provenance.tsv"; fi
  done <"$tmp/this-$a"; rm -rf "$d"
}
export -f take; export ARCHIVES OUT tmp
if [ -f "$tmp/from-archive" ]; then cut -d' ' -f1 "$tmp/from-archive" | sort -u | xargs -P 3 -I{} bash -c 'take {}'; fi
rm -rf "$tmp"; echo ALTO-DONE
