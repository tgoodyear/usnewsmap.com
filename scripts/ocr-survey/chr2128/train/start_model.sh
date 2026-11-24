#!/usr/bin/env bash
# The #2128 start model with Ꮐ (U+13C0) added to the character set.
#   start_model.sh --base chr.traineddata --langdata DIR --out DIR
# --base is round two's start model, stock chr with the 1860 NT word list (model B of #1621, sha256
# 7af4310b...). Its unicharset gets Ꮐ (unicharset_extractor + merge_unicharsets), and combine_lang_model
# builds a new starter traineddata from it with the base's own word, punctuation and number lists
# (dawg2wordlist), so the dictionary is unchanged. --langdata holds langdata_lstm's radical-stroke.txt,
# Cherokee.unicharset and Latin.unicharset, which combine_lang_model reads. Training then runs with
# --continue_from <out>/base.lstm --old_traineddata <base> --traineddata <out>/chr/chr.traineddata, and
# lstmtraining maps the base network's output layer onto the new character set (one new output for Ꮐ).
set -euo pipefail
while [ $# -gt 0 ]; do case "$1" in
  --base) BASE=$2; shift 2;; --langdata) LD=$2; shift 2;; --out) OUT=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
[ "$(sha256sum "$BASE" | cut -d' ' -f1)" = 7af4310bbe332f86eea16b7e93c88b185660fd3712bbf2601f97aab39c576144 ] || { echo "base is not 7af4310b" >&2; exit 1; }
mkdir -p "$OUT/lang"
cp "$BASE" "$OUT/base.traineddata"
combine_tessdata -u "$OUT/base.traineddata" "$OUT/base." >/dev/null
for d in punc number word; do dawg2wordlist "$OUT/base.lstm-unicharset" "$OUT/base.lstm-$d-dawg" "$OUT/$d.txt" 2>/dev/null; done
printf 'Ꮐ\n' > "$OUT/nah.txt"
unicharset_extractor --output_unicharset "$OUT/nah.unicharset" --norm_mode 1 "$OUT/nah.txt" >/dev/null 2>&1
merge_unicharsets "$OUT/base.lstm-unicharset" "$OUT/nah.unicharset" "$OUT/merged.unicharset" >/dev/null 2>&1
combine_lang_model --input_unicharset "$OUT/merged.unicharset" --script_dir "$LD" --words "$OUT/word.txt" \
  --numbers "$OUT/number.txt" --puncs "$OUT/punc.txt" --output_dir "$OUT/lang" --lang chr >/dev/null 2>&1
T="$OUT/lang/chr/chr.traineddata"
[ "$(head -1 "$OUT/merged.unicharset")" -eq $(( $(head -1 "$OUT/base.lstm-unicharset") + 1 )) ] || { echo "unicharset did not grow by one" >&2; exit 1; }
grep -q "^Ꮐ " "$OUT/lang/chr/chr.unicharset" || { echo "no Ꮐ in the new unicharset" >&2; exit 1; }
echo "base $(head -1 "$OUT/base.lstm-unicharset") chars, new $(head -1 "$OUT/merged.unicharset"); words $(wc -l < "$OUT/word.txt")"
sha256sum "$T"
