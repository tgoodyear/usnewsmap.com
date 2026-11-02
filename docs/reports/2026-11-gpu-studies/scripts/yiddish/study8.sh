#!/usr/bin/env bash
# Study 8, the Yiddish test set: the steps as run, in order. Each step is a function; run one with
# --step NAME. Paths are flags so nothing here names a host. --corpus is a checkout of
# gitlab.com/jochre/corpora/jochre-yiddish-corpus at bd69b412947e03cca18fdd5c72826207946d777f,
# --work a scratch directory, --tool the gpu-study binary built from ../../tools against main's
# crates at cf07f950 (the report branch's crates predate request versions 3 and 4).
set -euo pipefail
STEP=""; CORPUS=""; WORK=""; TOOL=""; ENGINE=""; SILVER_SRC=""
while [ $# -gt 0 ]; do case "$1" in
  --step) STEP=$2; shift 2;; --corpus) CORPUS=$2; shift 2;; --work) WORK=$2; shift 2;;
  --tool) TOOL=$2; shift 2;; --engine) ENGINE=$2; shift 2;; --silver-src) SILVER_SRC=$2; shift 2;;
  *) echo "unknown flag $1" >&2; exit 2;; esac; done
HERE=$(cd "$(dirname "$0")" && pwd)

# 1. The test split's file list and hashes, before any engine runs. Only test/ is read.
split() {
  (cd "$CORPUS/test" && find . -name "*.png" | sort | while read -r p; do b=${p%.png}
    echo "$(sha256sum < "$p" | cut -c1-64) $(sha256sum < "${b}_alto4.xml" | cut -c1-64) $(sha256sum < "${b}_alto4_simplified.txt" | cut -c1-64) ${p#./}"; done) > "$WORK/test-split.sha256.txt"
  mkdir -p "$WORK/test-pages"
  cut -d" " -f4 "$WORK/test-split.sha256.txt" | while read -r p; do b=$(basename "$p" .png)
    for s in .png _alto4.xml _alto4.txt _alto4_simplified.txt; do cp "$CORPUS/test/${p%/*}/$b$s" "$WORK/test-pages/"; done; done
  (cd "$WORK/test-pages" && ls *.png | sed "s/\.png$//") > "$WORK/test-ids.txt"
  "$TOOL" yid-yivo --pages "$WORK/test-pages" --list "$WORK/test-ids.txt" --out "$WORK/yivo-check.jsonl"
}

# 2. Line units for the native app. The pilot read 4 pages at version 4 in 56-crop units; the rest
#    are 256-crop units, so model load and the first request's warm-up are paid less often.
units() {
  printf "nybc200008_0025\nnybc200008_0096\nnybc200016_0030\nnybc200089_0171\n" > "$WORK/pilot-ids.txt"
  grep -vxF -f "$WORK/pilot-ids.txt" "$WORK/test-ids.txt" > "$WORK/rest-ids.txt"
  "$TOOL" yid-units --pages "$WORK/test-pages" --list "$WORK/test-ids.txt" --engine "$ENGINE" --prompt-version 4 --out "$WORK/units/v4"
  for v in 1 3; do "$TOOL" yid-units --pages "$WORK/test-pages" --list "$WORK/test-ids.txt" --engine "$ENGINE" --prompt-version $v --max-crops 256 --out "$WORK/units256/v$v"; done
  "$TOOL" yid-units --pages "$WORK/test-pages" --list "$WORK/rest-ids.txt" --engine "$ENGINE" --prompt-version 4 --max-crops 256 --out "$WORK/units256/v4"
  "$TOOL" yid-cut --pages "$WORK/test-pages" --refs "$WORK/units256/v1/refs.jsonl" --out "$WORK/lines"
}

# 3. CPU readers. Tesseract from the image built from ocr/engines/tesseract; Jochre 3 1.1.4 with
#    its layout server 1.0.5, served locally on 127.0.0.1:3434 (AGPL, for scoring only).
cpu() {
  img=localhost/usnm-tesseract:main-cf07f950
  sed "s/$/.png/" "$WORK/test-ids.txt" > "$WORK/test-pngs.txt"
  "$HERE/jochre-pages.sh" --pages "$WORK/test-pages" --list "$WORK/test-ids.txt" --out "$WORK/runs/jochre3"
  "$HERE/tesseract-pages.sh" --dir "$WORK/test-pages" --list "$WORK/test-pngs.txt" --lang yid --psm 3 --image $img --out "$WORK/runs/tess-yid-psm3"
  "$HERE/tesseract-pages.sh" --dir "$WORK/test-pages" --list "$WORK/test-pngs.txt" --lang yid+eng --psm 3 --image $img --out "$WORK/runs/tess-yideng-psm3"
  (cd "$WORK/lines" && ls *.png) > "$WORK/line-pngs.txt"
  "$HERE/tesseract-pages.sh" --dir "$WORK/lines" --list "$WORK/line-pngs.txt" --lang yid --psm 7 --image $img --out "$WORK/runs/tess-yid-lines-psm7"
}

# 4. GPU, on the BOINC host, in bounded windows (gpu-windows.sh calls gpu-window.sh, which calls
#    qwen-native-units.sh with the installed 1.17 binary). Run there, with the units copied over:
#    gpu-windows.sh --gap 600 --max-windows 14 --seconds 2400 --units .../units256/v4 \
#      --units .../units256/v1 --units .../units256/v3 --unit-seconds 800 --log windows.log

# 5. Newspaper silver set: selection on the BOINC server's qwen-nonenglish-v1 units (silver-select.py),
#    the 100 units copied with their page.jpg, then units for each version and the crops as PNG.
silver() {
  for v in 1 3 4; do
    extra=(); [ $v = 1 ] && extra=(--crops "$WORK/silver/crops")
    "$TOOL" yid-silver --selection "$WORK/silver/selection.jsonl" --src "$SILVER_SRC" --engine "$ENGINE" --prompt-version $v --out "$WORK/silver/units/v$v" "${extra[@]}"
  done
  (cd "$WORK/silver/crops" && ls *.png | sed "s/\.png$//") > "$WORK/silver-ids.txt"
  (cd "$WORK/silver/crops" && ls *.png) > "$WORK/silver-pngs.txt"
  "$HERE/jochre-pages.sh" --pages "$WORK/silver/crops" --list "$WORK/silver-ids.txt" --out "$WORK/runs/silver-jochre3"
  "$HERE/tesseract-pages.sh" --dir "$WORK/silver/crops" --list "$WORK/silver-pngs.txt" --lang yid --psm 3 --image localhost/usnm-tesseract:main-cf07f950 --out "$WORK/runs/silver-tess-yid-psm3"
}

# 6. Scoring, once per reader per page.
score() {
  S="$WORK/scores"; mkdir -p "$S/silver"
  for v in 1 3 4; do "$TOOL" yid-collect --units "$WORK/res/units256/v$v" --out "$S/v$v-main.jsonl" --summary "$S/v$v-units.jsonl"; done
  "$TOOL" yid-collect --units "$WORK/res/units/v4" --out "$S/v4-pilot.jsonl" --summary "$S/v4-pilot-units.jsonl"
  cat "$S/v4-main.jsonl" "$S/v4-pilot.jsonl" | grep -v '"missing":true' > "$S/v4.jsonl"
  for v in 1 3; do cp "$S/v$v-main.jsonl" "$S/v$v.jsonl"; done
  python3 -I "$HERE/to-jsonl.py" --kind jochre --dir "$WORK/runs/jochre3" --out "$S/jochre3.jsonl"
  python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$WORK/runs/tess-yid-psm3" --out "$S/tess-yid.jsonl"
  python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$WORK/runs/tess-yideng-psm3" --out "$S/tess-yideng.jsonl"
  python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$WORK/runs/tess-yid-lines-psm7" --key id --idmap --out "$S/tess-yid-lines.jsonl"
  "$TOOL" yid-score --pages "$WORK/test-pages" --list "$WORK/test-ids.txt" --refs "$WORK/units256/v1/refs.jsonl" \
    --page-hyp jochre3="$S/jochre3.jsonl" --page-hyp tesseract-yid="$S/tess-yid.jsonl" --page-hyp tesseract-yid-eng="$S/tess-yideng.jsonl" \
    --line-hyp tesseract-yid-psm7="$S/tess-yid-lines.jsonl" --line-hyp qwen-v1="$S/v1.jsonl" --line-hyp qwen-v3="$S/v3.jsonl" --line-hyp qwen-v4="$S/v4.jsonl" \
    --out "$S/book"
  for v in 1 3 4; do "$TOOL" yid-collect --units "$WORK/res/silver/v$v" --out "$S/silver/v$v.jsonl" --summary "$S/silver/v$v-units.jsonl"; done
  python3 -I "$HERE/to-jsonl.py" --kind jochre --dir "$WORK/runs/silver-jochre3" --key id --idmap --out "$S/silver/jochre3.jsonl"
  python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$WORK/runs/silver-tess-yid-psm3" --key id --idmap --out "$S/silver/tess-yid.jsonl"
  python3 -I -c 'import json,sys
for l in open(sys.argv[1]):
    r = json.loads(l); print(json.dumps({"id": r["id"], "text": r["loc_text"]}, ensure_ascii=False))' "$WORK/silver/units/v1/items.jsonl" > "$S/silver/loc.jsonl"
  "$TOOL" yid-agree --items "$WORK/silver/units/v1/items.jsonl" --hyp jochre3="$S/silver/jochre3.jsonl" --hyp tesseract-yid="$S/silver/tess-yid.jsonl" \
    --hyp qwen-v1="$S/silver/v1.jsonl" --hyp qwen-v3="$S/silver/v3.jsonl" --hyp qwen-v4="$S/silver/v4.jsonl" --hyp loc="$S/silver/loc.jsonl" \
    --reference jochre3 --out "$S/silver/agree.jsonl"
}

case "$STEP" in split|units|cpu|silver|score) "$STEP";; *) echo "--step split|units|cpu|silver|score" >&2; exit 2;; esac
