#!/usr/bin/env bash
# Study 11 Tesseract runs, Study 3 settings: eng or frak2021, PSM 6 on regions, PSM 3 on whole pages.
# --jobs N (parallel reads). Outputs under /tank/usnm-dev/s11/tesseract.
set -uo pipefail
JOBS=2
while [ $# -gt 0 ]; do case "$1" in --jobs) JOBS=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
S=/tank/usnm-dev/s11; O=$S/tesseract; mkdir -p "$O"
run() { # set lang psm list name
  local s=$(date +%s)
  bash $S/scripts/tesseract-local.sh --list "$S/inputs/$1/$4" --root "$S/inputs/$1" --name "$5" --langs "$2" --psm "$3" \
    --rootfs $S/tess-rootfs --out "$O" --jobs "$JOBS" --models-lock $S/scripts/models.lock --survey-models-lock $S/scripts/survey-models.lock
  echo "$(date -Is) $5 exit $? $(( $(date +%s) - s ))s" >> "$O/timings.txt"
}
run de-rz frak2021 6 list.tsv de-rz-regions
run de-cg frak2021 6 list.tsv de-cg-regions
run en-asgold eng 6 list.tsv en-asgold-regions
run de-rz frak2021 3 pages.tsv de-rz-pages
run de-cg frak2021 3 pages.tsv de-cg-pages
run en-asgold eng 3 pages.tsv en-asgold-pages
echo "$(date -Is) all done" >> "$O/timings.txt"
