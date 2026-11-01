#!/usr/bin/env bash
# Runs on a plain base image, on CPU, in the unpacked usnm-boinc Tesseract worker image under chroot. Lists hold id<TAB>relative image path.
# /work/in/runs.tsv lines: list<TAB>name<TAB>lang<TAB>psm. Output: /work/out/<name>.b64, id<TAB>base64 of the text.
set -u
O=/work/out; R=/work/rootfs
apt-get update -qq >/dev/null 2>&1; apt-get install -y -qq curl ca-certificates jq >/dev/null 2>&1
bash /work/in/rootfs.sh --repo tgoodyear/usnm-boinc-worker --digest sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa
mkdir -p "$R/work" "$R/tmp"; mv /work/in "$R/work/in"; ln -s "$R/work/in" /work/in
T=$R/work/tessdata; mkdir -p $T
src=$(cd "$R" && ls -d usr/share/tesseract-ocr/*/tessdata usr/local/share/tessdata opt/tessdata opt/*/tessdata 2>/dev/null | head -1)
echo "tessdata in image: $src" > $O/tessdata.src
for t in "$R/$src"/*.traineddata "$R/$src"/*/*.traineddata; do [ -f "$t" ] && cp -n "$t" $T/; done
get() { [ -f "$T/$1.traineddata" ] && return; curl -fsSL -o "$T/$1.traineddata" "$2"; }
get deu_latf https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/e12c65a915945e4c28e237a9b52bc4a8f39a0cec/deu_latf.traineddata
get frak2021 https://zenodo.org/records/10125246/files/frak2021-0.905.traineddata
get eng https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/e12c65a915945e4c28e237a9b52bc4a8f39a0cec/eng.traineddata
get deu https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/e12c65a915945e4c28e237a9b52bc4a8f39a0cec/deu.traineddata
sha256sum $T/*.traineddata | sed "s#$T/##" > $O/tessdata.sha256 2>&1
chroot "$R" /usr/bin/env TESSDATA_PREFIX=/work/tessdata tesseract --version > $O/tesseract.version 2>&1
export R
one() { id=$1; rel=$2; lang=$3; psm=$4; name=$5
  t=$(timeout 900 chroot "$R" /usr/bin/env TESSDATA_PREFIX=/work/tessdata OMP_THREAD_LIMIT=1 tesseract "/work/in/$rel" - -l "$lang" --psm "$psm" --dpi 300 2>/dev/null | base64 -w0)
  printf '%s\t%s\n' "$id" "$t" >> "/work/out/$name.$$.part"; }
export -f one
P=$(nproc); while IFS=$'\t' read -r list name lang psm; do
  awk -F'\t' -v l="$lang" -v p="$psm" -v n="$name" '{printf "%s\t%s\t%s\t%s\t%s\n",$1,$2,l,p,n}' "$R/work/in/$list" |
    xargs -P "$P" -d '\n' -I{} bash -c 'IFS=$'"'"'\t'"'"' read -r a b c d e <<<"$1"; one "$a" "$b" "$c" "$d" "$e"' _ {}
  cat $O/$name.*.part > $O/$name.b64; rm -f $O/$name.*.part; echo "done $name $(date +%s)" >> $O/progress.txt
done < "$R/work/in/runs.tsv"
