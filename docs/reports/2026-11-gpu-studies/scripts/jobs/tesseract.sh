#!/usr/bin/env bash
# Runs on a plain base image, on CPU, in the unpacked usnm-boinc Tesseract worker image under chroot.
# /work/in/runs.tsv lines: list<TAB>name<TAB>lang<TAB>psm; each list holds id<TAB>relative image path.
# Successful reads go to <name>.b64; failed reads go to <name>.failures.tsv as id<TAB>exit status.
# An interrupted run retains <name>.<pid>.<serial>.*.part for recovery; do not merge different attempts.
set -uo pipefail
O=/work/out; I=/work/in; R=/work/rootfs
# shellcheck source=/dev/null
. "$I/tesseract-models.sh" || exit 6
LOCK="$I/models.lock"
tesseract_verify_lock "$LOCK" || exit 6
[ -r "$I/runs.tsv" ] || { echo "missing runs.tsv" >&2; exit 6; }
awk -F'\t' 'NF != 4 || $2 == "" || seen[$2]++ { exit 1 }' "$I/runs.tsv" || {
  echo "invalid or duplicate run name in runs.tsv" >&2; exit 6;
}
required=(deu_latf frak2021 eng deu)
while IFS=$'\t' read -r list name lang psm; do
  # Include both PIDs and the run serial within rent.sh's 121-character filename limit.
  [[ $name =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,84}$ ]] || { echo "invalid run name: $name" >&2; exit 6; }
  awk -F'\t' 'NF != 2 || $1 !~ /^[A-Za-z0-9._:-]+$/ || length($1) > 200 || $2 == "" || seen[$1]++ { exit 1 }' "$I/$list" || {
    echo "invalid or duplicate id in list: $list" >&2; exit 6;
  }
  models=$(tesseract_models_for_run "$LOCK" "$lang" "$psm") || exit 6
  while IFS= read -r model; do
    case " ${required[*]} " in *" $model "*) ;; *) required+=("$model");; esac
  done <<<"$models"
done <"$I/runs.tsv"
for model in "${required[@]}"; do tesseract_model_pin "$LOCK" "$model" >/dev/null || exit 6; done

apt-get update -qq >/dev/null 2>&1; apt-get install -y -qq curl ca-certificates jq >/dev/null 2>&1
bash "$I/rootfs.sh" --repo tgoodyear/usnm-boinc-worker --digest sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa || exit 6
# Verify the same files that the chroot will read.
[ ! -L "$R/work" ] && [ ! -L "$R/work/tessdata" ] || { echo "symlinked Tesseract directory" >&2; exit 6; }
mkdir -p "$R/work" "$R/tmp" || exit 6
mv "$I" "$R/work/in" && ln -s "$R/work/in" "$I" || exit 6
T="$R/work/tessdata"; mkdir -p "$T" || exit 6
src=""
for candidate in "$R"/opt/tessdata "$R"/opt/*/tessdata "$R"/usr/local/share/tessdata "$R"/usr/share/tesseract-ocr/*/tessdata; do
  [ -d "$candidate" ] || continue
  src=$candidate; break
done
printf 'tessdata in image: %s\n' "${src#"$R"/}" > "$O/tessdata.src" || exit 6
get() {
  local model=$1 url=$2 sum=$3 target="$T/$1.traineddata" part="$T/.$1.traineddata.part.$$"
  if [ -e "$target" ] || [ -L "$target" ]; then
    tesseract_model_checksum "$target" "$sum" && return 0
    echo "replacing mismatched Tesseract model: $model" >&2
    rm -f "$target" || return 1
  fi
  rm -f "$part" || return 1
  if ! curl -fsSL -o "$part" "$url"; then
    rm -f "$part"; echo "Tesseract model download failed: $model" >&2; return 1
  fi
  if ! tesseract_model_checksum "$part" "$sum"; then
    rm -f "$part"; echo "Tesseract model checksum mismatch: $model" >&2; return 1
  fi
  mv -f "$part" "$target"
}
: > "$O/tessdata.sha256" || exit 6
for model in "${required[@]}"; do
  if [ -n "$src" ] && [ ! -e "$T/$model.traineddata" ] && [ ! -L "$T/$model.traineddata" ]; then
    for candidate in "$src/$model.traineddata" "$src"/*/"$model.traineddata"; do
      [ -f "$candidate" ] || continue
      cp "$candidate" "$T/$model.traineddata" || exit 6
      break
    done
  fi
  pin=$(tesseract_model_pin "$LOCK" "$model") || exit 6
  IFS=$'\t' read -r url sum <<<"$pin"
  get "$model" "$url" "$sum" || exit 6
  (cd "$T" && sha256sum "$model.traineddata") >> "$O/tessdata.sha256" || exit 6
done
chroot "$R" /usr/bin/env TESSDATA_PREFIX=/work/tessdata tesseract --version > "$O/tesseract.version" 2>&1 || exit 6
export R O
# shellcheck disable=SC2329
one() {
  local id=$1 rel=$2 lang=$3 psm=$4 t rc
  set -o pipefail
  if t=$(timeout 900 chroot "$R" /usr/bin/env TESSDATA_PREFIX=/work/tessdata OMP_THREAD_LIMIT=1 tesseract "/work/in/$rel" - -l "$lang" --psm "$psm" --dpi 300 2>/dev/null | base64 -w0); then
    if printf '%s\t%s\n' "$id" "$t" > "$PARTS/$$.writing" && mv "$PARTS/$$.writing" "$O/$RUN_TAG.$$.part"; then return 0; fi
    rc=1
  else rc=$?; fi
  rm -f "$PARTS/$$.writing"
  printf '%s\t%s\n' "$id" "$rc" > "$O/$RUN_TAG.$$.failure" || return 1
  return 1
}
export -f one
P=$(nproc); failed=0
while IFS=$'\t' read -r list name lang psm; do
  PARTS="$O/.$name.parts.$$"; mkdir "$PARTS" || exit 1
  RUN_TAG=$(tesseract_run_tag "$O" "$name") || exit 1
  export PARTS RUN_TAG
  rm -f "$O/$name.b64" || exit 1
  : > "$O/$name.failures.tsv" || exit 1
  # shellcheck disable=SC2016
  awk -F'\t' -v l="$lang" -v p="$psm" '{printf "%s\t%s\t%s\t%s\n",$1,$2,l,p}' "$R/work/in/$list" |
    tr '\n' '\0' | xargs -0 -P "$P" -I{} bash -c 'IFS=$'"'"'\t'"'"' read -r a b c d <<<"$1"; one "$a" "$b" "$c" "$d"' _ '{}'
  rc=$?
  : > "$PARTS/output.b64" || exit 1
  for f in "$O/$RUN_TAG".*.part; do
    [ -f "$f" ] || continue
    cat "$f" >> "$PARTS/output.b64" || exit 1
  done
  for f in "$O/$RUN_TAG".*.failure; do
    [ -f "$f" ] || continue
    cat "$f" >> "$O/$name.failures.tsv" || exit 1
  done
  mv -f "$PARTS/output.b64" "$O/$name.b64" || exit 1
  rm -f "$O/$RUN_TAG".*.part "$O/$RUN_TAG".*.failure || exit 1
  rm -rf "$PARTS" || exit 1
  if [ "$rc" -eq 0 ]; then
    echo "done $name $(date +%s)" >> "$O/progress.txt"
  else
    echo "failed $name $(date +%s)" >> "$O/progress.txt"; failed=1
  fi
done < "$R/work/in/runs.tsv"
exit "$failed"
