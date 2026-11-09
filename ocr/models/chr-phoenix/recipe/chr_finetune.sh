#!/usr/bin/env bash
# Fine-tune Tesseract's Cherokee model on Phoenix lines (#1621). Runs in the usnm-chr-bank image.
# Named arguments only:
#   chr_finetune.sh --base DIR_WITH_chr.traineddata --real DIR --synth DIR --work DIR [--iterations N] [--jobs N]
# Each training dir holds <name>.png + <name>.gt.txt. A twentieth of the real lines, by page, is the
# evaluation list lstmtraining reports on (pages chosen by hash); the frozen test set is scored separately afterwards.
set -euo pipefail
ITER=20000 JOBS=8
while [ $# -gt 0 ]; do case "$1" in
  --base) BASE=$2; shift 2;; --real) REAL=$2; shift 2;; --synth) SYNTH=$2; shift 2;;
  --work) WORK=$2; shift 2;; --iterations) ITER=$2; shift 2;; --jobs) JOBS=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
[ "$(basename "$REAL")" != "$(basename "$SYNTH")" ] || { echo "--real and --synth need different directory names" >&2; exit 2; }
mkdir -p "$WORK/lstmf" "$WORK/out"
# 1. A WordStr box file per line image (tesstrain's line format), then an .lstmf.
make_one() {
  png=$1; base=${png%.png}; name=$(basename "$base"); dir=$(dirname "$png")
  out="$WORK/lstmf/$(basename "$dir")__$name"
  [ -s "$out.lstmf" ] && return 0
  read -r w h < <(python3 -c "from PIL import Image;im=Image.open('$png');print(*im.size)")
  text=$(head -1 "$base.gt.txt")
  printf 'WordStr 0 0 %d %d 0 #%s\n\t %d %d %d %d 0\n' "$w" "$h" "$text" "$w" "$h" "$((w+1))" "$((h+1))" > "$out.box"
  cp "$png" "$out.png"
  tesseract "$out.png" "$out" --psm 13 lstm.train >/dev/null 2>&1 || echo "failed $png" >&2
  rm -f "$out.png"
}
export -f make_one; export WORK
find "$REAL" "$SYNTH" -name '*.png' | sort | xargs -P "$JOBS" -I{} bash -c 'make_one {}'
# 2. Lists: evaluation = real lines of about one page in twenty, chosen by a hash of the page.
python3 - "$WORK" "$(basename "$REAL")" <<'PY'
import glob, os, sys, hashlib
work, real = sys.argv[1], sys.argv[2] + "__"  # make_one names each file <its dir>__<name>
files = sorted(glob.glob(os.path.join(work, "lstmf", "*.lstmf")))
train, ev = [], []
for f in files:
    name = os.path.basename(f)
    if name.startswith(real):
        page = "_".join(name.split("__", 1)[1].split("_")[:4])
        (ev if int(hashlib.sha256(page.encode()).hexdigest(), 16) % 20 == 0 else train).append(f)
    else:
        train.append(f)
open(os.path.join(work, "train.list"), "w").write("\n".join(train) + "\n")
open(os.path.join(work, "eval.list"), "w").write("\n".join(ev) + "\n")
print(len(train), "train,", len(ev), "eval")
PY
# 3. Fine-tune from the base model's network, same character set.
cp "$BASE/chr.traineddata" "$WORK/chr.traineddata"
combine_tessdata -e "$WORK/chr.traineddata" "$WORK/chr.lstm" >/dev/null
lstmtraining --continue_from "$WORK/chr.lstm" --traineddata "$WORK/chr.traineddata" \
  --model_output "$WORK/out/chr_phoenix" --train_listfile "$WORK/train.list" --eval_listfile "$WORK/eval.list" \
  --max_iterations "$ITER" --target_error_rate 0.005 2>&1 | tee "$WORK/train.log" | grep -E "At iteration|Finished|best" | tail -40
lstmtraining --stop_training --continue_from "$WORK/out/chr_phoenix_checkpoint" --traineddata "$WORK/chr.traineddata" \
  --model_output "$WORK/chr_phoenix.traineddata"
sha256sum "$WORK/chr_phoenix.traineddata"
