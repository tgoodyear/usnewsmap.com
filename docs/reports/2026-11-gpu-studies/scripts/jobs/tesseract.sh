#!/usr/bin/env bash
# Runs in the usnm-boinc Tesseract worker image, on CPU. Lists hold id<TAB>relative image path.
# /work/in/runs.tsv lines: list<TAB>name<TAB>lang<TAB>psm. Output: /work/out/<name>.b64, id<TAB>base64 of the text.
set -u
O=/work/out; export TESSDATA_PREFIX=/work/tessdata; mkdir -p $TESSDATA_PREFIX
src=$(tesseract --print-parameters 2>/dev/null | head -0; ls -d /usr/share/tesseract-ocr/*/tessdata /usr/local/share/tessdata /opt/tessdata 2>/dev/null | head -1)
for t in "$src"/*.traineddata "$src"/*/*.traineddata; do [ -f "$t" ] && ln -sf "$t" $TESSDATA_PREFIX/; done
get() { [ -f "$TESSDATA_PREFIX/$1.traineddata" ] && return; curl -fsSL -o "$TESSDATA_PREFIX/$1.traineddata" "$2"; }
get deu_latf https://raw.githubusercontent.com/tesseract-ocr/tessdata_best/e12c65a915945e4c28e237a9b52bc4a8f39a0cec/deu_latf.traineddata
get frak2021 https://zenodo.org/records/10125246/files/frak2021-0.905.traineddata
sha256sum $TESSDATA_PREFIX/*.traineddata > $O/tessdata.sha256 2>&1
tesseract --version > $O/tesseract.version 2>&1
export OMP_THREAD_LIMIT=1
one() { id=$1; rel=$2; lang=$3; psm=$4; name=$5
  t=$(timeout 900 tesseract "/work/in/$rel" - -l "$lang" --psm "$psm" --dpi 300 2>/dev/null | base64 -w0)
  printf '%s\t%s\n' "$id" "$t" >> "/work/out/$name.$$.part"; }
export -f one
P=$(nproc); while IFS=$'\t' read -r list name lang psm; do
  awk -F'\t' -v l="$lang" -v p="$psm" -v n="$name" '{printf "%s\t%s\t%s\t%s\t%s\n",$1,$2,l,p,n}' "/work/in/$list" |
    xargs -P "$P" -d '\n' -I{} bash -c 'IFS=$'"'"'\t'"'"' read -r a b c d e <<<"$1"; one "$a" "$b" "$c" "$d" "$e"' _ {}
  cat $O/$name.*.part > $O/$name.b64; rm -f $O/$name.*.part; echo "done $name $(date +%s)" >> $O/progress.txt
done < /work/in/runs.tsv
