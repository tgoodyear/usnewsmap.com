#!/usr/bin/env bash
# Tesseract on a local node's CPU in the pinned worker rootfs, with the rented run's flags.
# --list holds id<TAB>path under --root; successful output is id<TAB>base64, failures are id<TAB>exit status.
# An interrupted run retains <name>.<pid>.<serial>.*.part for recovery; do not merge different attempts.
# Retrying an ID replaces its prior result or failure receipt.
# --models-lock (or TESSERACT_MODELS_LOCK) may locate the exact repository lock when scripts are copied elsewhere.
set -uo pipefail
LIST=""; ROOT=""; NAME=""; LANGS=""; PSM=3; RF=""; OUT=""; JOBS=2; LOCK=${TESSERACT_MODELS_LOCK:-}
while [ $# -gt 0 ]; do case "$1" in --list) LIST=$2; shift 2;; --root) ROOT=$2; shift 2;; --name) NAME=$2; shift 2;; --langs) LANGS=$2; shift 2;; --psm) PSM=$2; shift 2;; --rootfs) RF=$2; shift 2;; --out) OUT=$2; shift 2;; --jobs) JOBS=$2; shift 2;; --models-lock) LOCK=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$LIST" ] && [ -n "$ROOT" ] && [ -n "$RF" ] && [ -n "$OUT" ] || { echo "missing required argument" >&2; exit 2; }
[[ $NAME =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,120}$ && $JOBS =~ ^[1-9][0-9]*$ ]] || { echo "invalid name or job count" >&2; exit 2; }
HERE=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
# shellcheck source=/dev/null
. "$HERE/tesseract-models.sh" || exit 6
if [ -z "$LOCK" ]; then
  LOCK="$HERE/../../../../ocr/engines/tesseract/models.lock"
  if [ ! -f "$LOCK" ] && [ -n "${GPU_STUDY_ROOT:-}" ]; then LOCK="$GPU_STUDY_ROOT/repo/ocr/engines/tesseract/models.lock"; fi
fi
tesseract_verify_lock "$LOCK" || exit 6
models=$(tesseract_models_for_run "$LOCK" "$LANGS" "$PSM") || exit 6
# Verify the same files that the chroot will read.
[ ! -L "$RF/work" ] && [ ! -L "$RF/work/tessdata" ] || { echo "symlinked Tesseract directory" >&2; exit 6; }
while IFS= read -r model; do
  pin=$(tesseract_model_pin "$LOCK" "$model") || exit 6
  IFS=$'\t' read -r _ sum <<<"$pin"
  tesseract_model_checksum "$RF/work/tessdata/$model.traineddata" "$sum" || {
    echo "missing or mismatched Tesseract model: $model" >&2; exit 6;
  }
done <<<"$models"
awk -F'\t' 'NF != 2 || $1 !~ /^[A-Za-z0-9._:-]+$/ || length($1) > 200 || $2 == "" || seen[$1]++ { exit 1 }' "$LIST" || {
  echo "invalid or duplicate id in list: $LIST" >&2; exit 2;
}
mkdir -p "$OUT" "$RF/work/in" || exit 1
PARTS="$OUT/.$NAME.parts.$$"; mkdir "$PARTS" || exit 1
trap 'rm -rf "$PARTS"' EXIT
# Retain unrelated items but invalidate earlier readings and failure receipts for every retry.
for suffix in b64 failures.tsv; do
  if [ -f "$OUT/$NAME.$suffix" ]; then
    awk -F'\t' 'FILENAME == ARGV[1] { attempted[$1]=1; next } !($1 in attempted)' "$LIST" "$OUT/$NAME.$suffix" > "$PARTS/retained.$suffix" || exit 1
  else
    : > "$PARTS/retained.$suffix" || exit 1
  fi
  mv -f "$PARTS/retained.$suffix" "$OUT/$NAME.$suffix" || exit 1
done
RUN_TAG=$(tesseract_run_tag "$OUT" "$NAME") || exit 1
export RF PARTS OUT RUN_TAG
one() {
  local id rel lang psm t rc img
  IFS=$'\t' read -r id rel lang psm <<<"$1"
  img="$RF/work/in/$id.$$.img"
  set -o pipefail
  if cp "$2/$rel" "$img"; then
    if t=$(timeout 1800 chroot "$RF" /usr/bin/env TESSDATA_PREFIX=/work/tessdata OMP_THREAD_LIMIT=1 tesseract "/work/in/$id.$$.img" - -l "$lang" --psm "$psm" --dpi 300 2>/dev/null | base64 -w0); then
      if rm -f "$img" && printf '%s\t%s\n' "$id" "$t" > "$PARTS/$$.writing" && mv "$PARTS/$$.writing" "$OUT/$RUN_TAG.$$.part"; then return 0; fi
      rc=1
    else rc=$?; fi
  else rc=$?; fi
  rm -f "$img" "$PARTS/$$.writing"
  printf '%s\t%s\n' "$id" "$rc" > "$OUT/$RUN_TAG.$$.failure" || return 1
  return 1
}
export -f one
# shellcheck disable=SC2016
awk -F'\t' -v l="$LANGS" -v p="$PSM" '{printf "%s\t%s\t%s\t%s\n",$1,$2,l,p}' "$LIST" |
  tr '\n' '\0' | xargs -0 -P "$JOBS" -I{} nice -n 19 bash -c 'one "$1" "$2"' _ '{}' "$ROOT"
rc=$?
cp "$OUT/$NAME.b64" "$PARTS/output.b64" || exit 1
for f in "$OUT/$RUN_TAG".*.part; do
  [ -f "$f" ] || continue
  cat "$f" >> "$PARTS/output.b64" || exit 1
done
for f in "$OUT/$RUN_TAG".*.failure; do
  [ -f "$f" ] || continue
  cat "$f" >> "$OUT/$NAME.failures.tsv" || exit 1
done
mv -f "$PARTS/output.b64" "$OUT/$NAME.b64" || exit 1
rm -f "$OUT/$RUN_TAG".*.part "$OUT/$RUN_TAG".*.failure || exit 1
if [ "$rc" -ne 0 ]; then
  echo "failed $NAME" >>"$OUT/progress.txt"
  echo "one or more Tesseract reads failed: $NAME" >&2
  exit "$rc"
fi
echo "done $NAME" >>"$OUT/progress.txt"
