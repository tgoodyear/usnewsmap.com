#!/usr/bin/env bash
# Study 10, Tesseract `yid` fine-tuned on the Yiddish OCR Corpus's training split (#1479): the steps
# as run, in order of dependency. Each step is a function; run one with --step NAME. Paths are flags
# so nothing here names a host. --work is a scratch directory; --study8 is Study 8's --work
# directory, whose test pages, line crops, newspaper crops, outputs and scores are reused unchanged;
# --tool is Study 8's gpu-study binary. --name is a model name for snap, a list for ocr and score.
# CPU only; needs podman, git, and python3 with Pillow. Only the corpus's training/ is checked out.
set -euo pipefail
STEP=""; WORK=""; S8=""; TOOL=""; NAMES=""; RUN=yidj2; CKPT=""; JOBS=1
while [ $# -gt 0 ]; do case "$1" in
  --step) STEP=$2; shift 2;; --work) WORK=$2; shift 2;; --study8) S8=$2; shift 2;; --tool) TOOL=$2; shift 2;;
  --name) NAMES=$2; shift 2;; --run) RUN=$2; shift 2;; --checkpoint) CKPT=$2; shift 2;; --jobs) JOBS=$2; shift 2;;
  *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$WORK" ] || { echo "--work is required" >&2; exit 2; }
mkdir -p "$WORK"; WORK=$(cd "$WORK" && pwd)
HERE=$(cd "$(dirname "$0")" && pwd)
ENGINE=localhost/usnm-tesseract:main-cf07f950  # Study 8's image, from ocr/engines/tesseract at cf07f950
TRAINER=localhost/usnm-tesstrain:cf07f950
STOCK=086ec92aea9c7016c74b503c3d24906381e7e57cc3f487f12e1784eb2530ba94  # tessdata_best yid, models.lock
TESSTRAIN=405346a3a67d8e4e049341d1da6a4b752e0b8351
CORPUS=bd69b412947e03cca18fdd5c72826207946d777f
LANGDATA=07930fd9f246622c26eb5de794d9212ceac432d3
trainer() { podman run --rm --network none -v "$WORK:/w" -w /w "$TRAINER" "$@"; }
need() { [ -n "$1" ] || { echo "--step $STEP needs $2" >&2; exit 2; }; }

# 1. The trainer: Study 8's Tesseract image plus make, Python, Pillow and python-bidi for tesstrain.
image() {
  mkdir -p "$WORK/img"; cp "$HERE/tesstrain.Containerfile" "$WORK/img/Containerfile"
  nice -n 19 podman build --network host -q -t "$TRAINER" "$WORK/img"
  trainer dpkg-query -W make python3 python3-pil python3-bidi tesseract-ocr
}

# 2. Inputs. The start model is the production pin, copied out of the engine image and checked.
#    tesstrain at the commit used. The corpus at Study 8's pin, training/ only. The stock word,
#    punctuation and number dictionaries as lists, which tesstrain rebuilds for a new model.
setup() {
  mkdir -p "$WORK/tessdata" "$WORK/stock"
  podman run --rm --network none --entrypoint /bin/cat "$ENGINE" /opt/tessdata/yid.traineddata > "$WORK/tessdata/yid.traineddata"
  echo "$STOCK  $WORK/tessdata/yid.traineddata" | sha256sum --check
  [ -d "$WORK/tesstrain/.git" ] || git clone -q https://github.com/tesseract-ocr/tesstrain.git "$WORK/tesstrain"
  git -C "$WORK/tesstrain" checkout -q --detach "$TESSTRAIN"
  if [ ! -d "$WORK/corpus/.git" ]; then
    git clone -q --filter=blob:none --no-checkout https://gitlab.com/jochre/corpora/jochre-yiddish-corpus.git "$WORK/corpus"
    git -C "$WORK/corpus" sparse-checkout set training
  fi
  git -C "$WORK/corpus" checkout -q --detach "$CORPUS"
  # shellcheck disable=SC2016 # $d expands in the container.
  trainer bash -c 'combine_tessdata -u tessdata/yid.traineddata stock/yid. > /dev/null
    for d in word punc number; do dawg2wordlist stock/yid.lstm-unicharset stock/yid.lstm-$d-dawg stock/yid.$d.list 2> /dev/null; done'
  wc -l "$WORK"/stock/yid.*.list
}

# 3. Line ground truth from the 554 training pages, read the way Study 8 reads ALTO lines, with a
#    hash list of the page images and ALTO files. One page, nybc213618_0068, has no text lines. The
#    training and test splits share all 60 books; no page may be in both.
lines() {
  python3 -I "$HERE/cut-lines.py" --corpus "$WORK/corpus" --split training --out "$WORK/gt"
  cut -d" " -f3 "$WORK/training-split.sha256.txt" | sed 's#.*/##; s#\.png$##' > "$WORK/train-ids.txt"
  if [ -n "$S8" ] && grep -qxF -f "$S8/test-ids.txt" "$WORK/train-ids.txt"; then
    echo "a test page is in the training split" >&2; exit 1
  fi
}

# 4. tesstrain's script data. tesstrain fetches langdata_lstm's main branch, unpinned; the files the
#    runs used match commit $LANGDATA, main since March 2024, so they are fetched from it here and
#    checked against langdata.sha256.
langdata() {
  mkdir -p "$WORK/data/langdata"
  cp "$HERE/langdata.sha256" "$WORK/data/langdata.sha256"
  # shellcheck disable=SC2016 # $f and $1 expand in the container.
  podman run --rm --network host -v "$WORK:/w" -w /w/data/langdata "$TRAINER" bash -c 'set -e
    for f in $(cut -d" " -f3 ../langdata.sha256); do
      wget -q -O "$f" "https://github.com/tesseract-ocr/langdata_lstm/raw/$1/$f"
    done
    sha256sum --check --quiet ../langdata.sha256' _ "$LANGDATA"
}

# 5. `yidrev`: the stock network with its dictionaries rebuilt right to left, as tesstrain builds them
#    for an RTL model. Stock `yid` stores its dictionaries in logical order, which the decoder, working
#    in visual order, does not match. Only the dictionaries change; the unichar ids must not.
yidrev() {
  mkdir -p "$WORK/dec/x" "$WORK/td"
  trainer bash -c 'set -e
    combine_lang_model --input_unicharset stock/yid.lstm-unicharset --script_dir data/langdata \
      --words stock/yid.word.list --puncs stock/yid.punc.list --numbers stock/yid.number.list \
      --output_dir dec --lang yidrev --pass_through_recoder --lang_is_rtl > dec/combine.log 2>&1
    combine_tessdata -u dec/yidrev/yidrev.traineddata dec/x/yidrev. > /dev/null
    cp tessdata/yid.traineddata td/yid.traineddata
    cp tessdata/yid.traineddata td/yidrev.traineddata
    combine_tessdata -o td/yidrev.traineddata dec/x/yidrev.lstm-word-dawg dec/x/yidrev.lstm-punc-dawg \
      dec/x/yidrev.lstm-number-dawg > /dev/null'
  cmp <(cut -d" " -f1 "$WORK/stock/yid.lstm-unicharset") <(cut -d" " -f1 "$WORK/dec/x/yidrev.lstm-unicharset")
  rm -f "$WORK/td/yidrev.traineddata.__tmp__"
  chmod 0644 "$WORK"/td/*.traineddata
}

# tesstrain from the stock model with the stock word lists: LANG_TYPE=RTL, 60,000 iterations, and
# tesstrain's defaults otherwise (learning rate 0.0001 from a start model, 90% of lines to training).
tesstrain() {
  local m=$1 gt=$2; shift 2
  mkdir -p "$WORK/data/$m"
  cp "$WORK/stock/yid.word.list" "$WORK/data/$m/$m.wordlist"
  cp "$WORK/stock/yid.punc.list" "$WORK/data/$m/$m.punc"
  cp "$WORK/stock/yid.number.list" "$WORK/data/$m/$m.numbers"
  podman run --rm --network none -v "$WORK:/w" "$TRAINER" nice -n 19 make -C /w/tesstrain MODEL_NAME="$m" START_MODEL=yid \
    TESSDATA=/w/tessdata GROUND_TRUTH_DIR="/w/$gt" DATA_DIR=/w/data LANG_TYPE=RTL MAX_ITERATIONS=60000 "$@"
}

# Tesseract treats a page as right to left only if RTL entries outnumber LTR ones in the model's
# unicharset (UNICHARSET::major_right_to_left, set per page in tessedit.cpp). Otherwise multi-column
# pages are read with their columns in the wrong order.
rtl_majority() {
  python3 -I - "$1" <<'PY'
import sys
rows = open(sys.argv[1], encoding="utf-8").read().split("\n")
fields = [r.split("\t")[0].split(" ") for r in rows[1:int(rows[0]) + 1]]
dirs = [int(f[5]) for f in fields if len(f) >= 8]
ltr, rtl = dirs.count(0), sum(dirs.count(d) for d in (1, 5, 13))
print("unicharset entries", int(rows[0]), "LTR", ltr, "RTL", rtl)
sys.exit(0 if rtl > ltr else "not right to left by majority")
PY
}

# 6. Run 1, stopped after 31,900 iterations. Its unicharset took the Latin, Cyrillic and Polish letters
#    in the corpus's lines and became left to right by majority.
train1() {
  tesstrain yidjochre gt -j3 lists unicharset proto-model
  tesstrain yidjochre gt training
}

# 7. Run 2, the result: run 1 without the lines that contain a letter of Unicode bidi class L. Run 1's
#    boxes and .lstmf files, which hold no unicharset, are linked rather than rebuilt where present.
train() {
  python3 -I - "$WORK" <<'PY'
import os, sys, unicodedata
gt, gt2 = os.path.join(sys.argv[1], "gt"), os.path.join(sys.argv[1], "gt2")
os.makedirs(gt2, exist_ok=True)
kept = dropped = 0
for f in sorted(os.listdir(gt)):
    if not f.endswith(".gt.txt"):
        continue
    stem = f[:-len(".gt.txt")]
    with open(os.path.join(gt, f), encoding="utf-8") as text:
        if any(unicodedata.bidirectional(c) == "L" for c in text.read()):
            dropped += 1
            continue
    kept += 1
    for ext in (".png", ".gt.txt", ".box", ".lstmf"):
        if os.path.exists(os.path.join(gt, stem + ext)) and not os.path.exists(os.path.join(gt2, stem + ext)):
            os.link(os.path.join(gt, stem + ext), os.path.join(gt2, stem + ext))
print("lines kept", kept, "dropped", dropped)
PY
  tesstrain yidj2 gt2 lists unicharset proto-model
  wc -l "$WORK/data/yidj2/list.train" "$WORK/data/yidj2/list.eval"
  rtl_majority "$WORK/data/yidj2/yidj2.unicharset"
  tesstrain yidj2 gt2 training
}

# 8. A model from --run's newest best checkpoint or --checkpoint, as td/NAME.traineddata with the
#    dictionaries tesstrain built, and td/NAME_sd.traineddata, the same network with stock `yid`'s
#    dictionaries, for a 2x2 with `yid` and `yidrev`. lstmtraining writes a best checkpoint only when
#    training error beats the last one written by more than 1/32, so the newest need not be the
#    lowest error reached. The stock unichar ids must come first, unchanged, for the stock
#    dictionaries to stay valid. Run 1's checkpoints fail the right-to-left check; its `yidj18k` was
#    converted before the check existed.
snap() {
  need "$NAMES" --name
  local n=$NAMES r=$RUN c=$CKPT
  [ -n "$c" ] || c=$(cd "$WORK/data/$r/checkpoints" && find . -name "${r}_*_*_*.checkpoint" | sed 's#^\./##' | sort -t_ -k4 -n | tail -1)
  mkdir -p "$WORK/snap/x" "$WORK/snap/stock" "$WORK/td"
  cp "$WORK/data/$r/checkpoints/$c" "$WORK/snap/$n.checkpoint"
  echo "checkpoint $c"
  trainer lstmtraining --stop_training --continue_from "snap/$n.checkpoint" \
    --traineddata "data/$r/$r.traineddata" --model_output "td/$n.traineddata"
  trainer combine_tessdata -u "td/$n.traineddata" "snap/x/$n." > /dev/null
  trainer combine_tessdata -u tessdata/yid.traineddata snap/stock/yid. > /dev/null
  rtl_majority "$WORK/snap/x/$n.lstm-unicharset"
  local k
  k=$(head -1 "$WORK/snap/stock/yid.lstm-unicharset")
  cmp <(sed -n "2,$((k + 1))p" "$WORK/snap/stock/yid.lstm-unicharset" | cut -d" " -f1) \
      <(sed -n "2,$((k + 1))p" "$WORK/snap/x/$n.lstm-unicharset" | cut -d" " -f1)
  cp "$WORK/td/$n.traineddata" "$WORK/td/${n}_sd.traineddata"
  trainer combine_tessdata -o "td/${n}_sd.traineddata" snap/stock/yid.lstm-word-dawg \
    snap/stock/yid.lstm-punc-dawg snap/stock/yid.lstm-number-dawg > /dev/null
  rm -f "$WORK/td/${n}_sd.traineddata.__tmp__"
  chmod 0644 "$WORK"/td/*.traineddata
  (cd "$WORK/td" && sha256sum "$n.traineddata" "${n}_sd.traineddata")
}

# 9. Reads, as Study 8 read stock `yid`: the 104 test pages at PSM 3, the 4,011 ALTO line crops at
#    PSM 7 and the 100 newspaper crops at PSM 3, in Study 8's engine image with td/ mounted.
ocr() {
  need "$NAMES" --name; need "$S8" --study8
  local n R="$WORK/eval/runs"
  for n in $NAMES; do
    "$HERE/tesseract-pages.sh" --dir "$S8/test-pages" --list "$S8/test-pngs.txt" --lang "$n" --psm 3 --image "$ENGINE" \
      --tessdata "$WORK/td" --out "$R/tess-$n-psm3" --jobs "$JOBS"
    "$HERE/tesseract-pages.sh" --dir "$S8/lines" --list "$S8/line-pngs.txt" --lang "$n" --psm 7 --image "$ENGINE" \
      --tessdata "$WORK/td" --out "$R/tess-$n-lines-psm7" --jobs "$JOBS"
    "$HERE/tesseract-pages.sh" --dir "$S8/silver/crops" --list "$S8/silver-pngs.txt" --lang "$n" --psm 3 --image "$ENGINE" \
      --tessdata "$WORK/td" --out "$R/silver-tess-$n-psm3" --jobs "$JOBS"
  done
}

# 10. Scores next to Study 8's, with its references, scorer, bootstrap and outputs for Jochre 3 and
#     stock `yid`. A reader's set is scored once its run is complete. Agreement with Jochre 3 on the
#     newspaper crops is agreement, not accuracy.
score() {
  need "$NAMES" --name; need "$S8" --study8; need "$TOOL" --tool
  local n R="$WORK/eval/runs" S="$WORK/eval/scores"
  local -a P=(--page-hyp jochre3="$S8/scores/jochre3.jsonl" --page-hyp tesseract-yid="$S8/scores/tess-yid.jsonl")
  local -a L=(--line-hyp tesseract-yid-psm7="$S8/scores/tess-yid-lines.jsonl")
  local -a H=(--hyp jochre3="$S8/scores/silver/jochre3.jsonl" --hyp tesseract-yid="$S8/scores/silver/tess-yid.jsonl")
  mkdir -p "$S/silver"
  for n in $NAMES; do
    if [ -f "$R/tess-$n-psm3/done" ]; then
      python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$R/tess-$n-psm3" --out "$S/tess-$n.jsonl"
      P+=(--page-hyp "tesseract-$n=$S/tess-$n.jsonl")
    fi
    if [ -f "$R/tess-$n-lines-psm7/done" ]; then
      python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$R/tess-$n-lines-psm7" --key id --idmap --out "$S/tess-$n-lines.jsonl"
      L+=(--line-hyp "tesseract-$n-psm7=$S/tess-$n-lines.jsonl")
    fi
    if [ -f "$R/silver-tess-$n-psm3/done" ]; then
      python3 -I "$HERE/to-jsonl.py" --kind tesseract --dir "$R/silver-tess-$n-psm3" --key id --idmap --out "$S/silver/tess-$n.jsonl"
      H+=(--hyp "tesseract-$n=$S/silver/tess-$n.jsonl")
    fi
  done
  "$TOOL" yid-score --pages "$S8/test-pages" --list "$S8/test-ids.txt" --refs "$S8/units256/v1/refs.jsonl" \
    "${P[@]}" "${L[@]}" --out "$S/book"
  "$TOOL" yid-agree --items "$S8/silver/units/v1/items.jsonl" "${H[@]}" --reference jochre3 --out "$S/silver/agree.jsonl"
}

case "$STEP" in
  image|setup|lines|langdata|yidrev|train1|train|snap|ocr|score) "$STEP";;
  *) echo "--step image|setup|lines|langdata|yidrev|train1|train|snap|ocr|score" >&2; exit 2;;
esac
