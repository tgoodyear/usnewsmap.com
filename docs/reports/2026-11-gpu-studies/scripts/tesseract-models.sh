#!/usr/bin/env bash
# The study uses this exact ocr/engines/tesseract/models.lock, not a lock supplied by a model host.
TESSERACT_MODEL_LOCK_SHA256=acfbd62d99fe4ff321a6f5d189445a984b90cb995f51070c72fdc931d7bfa4ad

tesseract_checksum() {
  printf '%s  %s\n' "$2" "$1" | sha256sum -c - >/dev/null 2>&1
}

tesseract_model_checksum() {
  # A symlink can resolve to different bytes on the host and inside the chroot.
  [ -f "$1" ] && [ ! -L "$1" ] && tesseract_checksum "$1" "$2"
}

tesseract_verify_lock() {
  if [ -f "$1" ] && tesseract_checksum "$1" "$TESSERACT_MODEL_LOCK_SHA256"; then return 0; fi
  echo "Tesseract model lock missing or does not match the pinned repository lock: $1" >&2
  return 1
}

tesseract_run_tag() {
  local dir=$1 name=$2 tag serial=0 f used
  while [ "$serial" -le 99999 ]; do
    tag="$name.$$.$serial"; used=0
    for f in "$dir/$tag".*.part "$dir/$tag".*.failure; do
      if [ -e "$f" ] || [ -L "$f" ]; then used=1; break; fi
    done
    if [ "$used" -eq 0 ]; then printf '%s\n' "$tag"; return 0; fi
    serial=$((serial + 1))
  done
  echo "cannot allocate a Tesseract run tag: $name" >&2
  return 1
}

tesseract_model_pin() {
  local lock=$1 model=$2 pin url sum
  [[ $model =~ ^[A-Za-z0-9_]+$ ]] || return 1
  pin=$(awk -v name="tessdata/$model.traineddata" '
    $1 == name { count++; line=$2 " " $3 }
    END { if (count != 1) exit 1; print line }
  ' "$lock") || { echo "no unique pinned Tesseract model: $model" >&2; return 1; }
  read -r url sum <<<"$pin"
  [[ $url == https://* && $sum =~ ^[0-9a-f]{64}$ ]] || {
    echo "invalid Tesseract model pin: $model" >&2; return 1;
  }
  printf '%s\t%s\n' "$url" "$sum"
}

tesseract_models_for_run() {
  local lock=$1 langs=$2 psm=$3 model
  local -a models
  [[ $langs =~ ^[A-Za-z0-9_]+(\+[A-Za-z0-9_]+)*$ && $psm =~ ^[0-9]+$ ]] || {
    echo "invalid Tesseract languages or PSM: $langs / $psm" >&2; return 1;
  }
  # These PSMs implicitly load osd.traineddata, which the worker lock does not pin.
  if [[ $psm =~ ^0*(0|1|12)$ ]]; then
    echo "Tesseract PSM $psm requires an unpinned osd model" >&2; return 1
  fi
  IFS=+ read -r -a models <<<"$langs"
  for model in "${models[@]}"; do
    tesseract_model_pin "$lock" "$model" >/dev/null || return 1
    printf '%s\n' "$model"
  done
}
