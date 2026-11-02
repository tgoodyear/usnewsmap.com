#!/usr/bin/env bash
# Runs on a plain Python image. American Stories at the commit the survey used, models from the hash-pinned archive.
# Reads /work/in/list.tsv, one process per image, as the pipeline's own script does.
set -u
O=/work/out; I=/work/in; A=/work/as; mkdir -p $A/models
apt-get update -qq >/dev/null 2>&1; apt-get install -y -qq git unzip libgl1 libglib2.0-0 >/dev/null 2>&1
git clone -q https://github.com/dell-research-harvard/AmericanStories.git $A/AmericanStories 2>> $O/setup.log
c=$(git -C $A/AmericanStories rev-parse HEAD); echo "head $c" >> $O/setup.log
git -C $A/AmericanStories checkout -q f307e5c 2>> $O/setup.log
# The Dropbox link is mutable. This is the sha256 in scripts/ocr-survey/models.lock and of the archive the local batch read.
MODELS_SHA256=53d46ba7173c06e907d222b1bebbd22689df15b7064b51f7a79cc962a18c9933
curl -fsSL -o $A/models.zip 'https://www.dropbox.com/sh/sfaf1nmuji9yhu6/AAAj1UGrPmCWFJUiTSP41ihpa?dl=1'
sha256sum $A/models.zip | tee -a $O/setup.log
echo "$MODELS_SHA256  $A/models.zip" | sha256sum -c --quiet - >> $O/setup.log 2>&1 || { echo "models.zip does not match the pinned sha256" >> $O/setup.log; exit 6; }
unzip -q -o $A/models.zip -d $A/models 2>> $O/setup.log
ls $A/models | head -30 >> $O/setup.log
cd $A/AmericanStories; python -m pip -q install -r requirements.txt 2>&1 | tail -3 >> $O/setup.log
python -m pip -q install 'git+https://github.com/dell-research-harvard/efficient_ocr.git@e38da3cd0b614cf13ba4c148bab5b37aeec3db7d' onnxruntime-gpu 2>&1 | tail -3 >> $O/setup.log
m=$A/models; n=0; total=$(wc -l < $I/list.tsv)
while IFS=$'\t' read -r id rel; do
  d=$(mktemp -d); mkdir $d/scans $d/out; cp "$I/$rel" $d/scans/page.jpg; s=$(date +%s)
  (cd $A && timeout 300 python AmericanStories/src/run_img2txt_yolo_pipeline.py --manifest_path $d/scans \
    --checkpoint_path_layout $m/layout_model_new.onnx --checkpoint_path_line $m/line_model_new.onnx \
    --label_map_path_layout AmericanStories/src/label_maps/label_map_layout.json --label_map_path_line AmericanStories/src/label_maps/label_map_line.json \
    --effocr_recognizer_dir $m/word_recognizer_new --effocr_char_recognizer_dir $m/char_recognizer --effocr_localizer_dir $m/ \
    --output_save_path $d/out --legibility-classifier $m/legibility_model_new.onnx --localizer_model_backend yolov8 \
    --line_model_backend yolov8 --layout_model_backend yolov8 --recognizer_word_thresh 0.85 --word-level-effocr) > $d/log 2>&1
  if [ -f $d/out/page.json ]; then printf '%s\t%s\t%s\n' "$id" "$(( $(date +%s)-s ))" "$(base64 -w0 $d/out/page.json)" >> $O/as.b64
  else tail -n 5 $d/log >> $O/as-errors.log; fi
  rm -rf $d; n=$((n+1)); [ $((n%10)) = 0 ] && echo "$n/$total" >> $O/progress.txt
done < $I/list.tsv
