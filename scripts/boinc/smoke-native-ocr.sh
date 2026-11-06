#!/usr/bin/env bash
# Runs the native OCR worker on one small page with no BOINC client, as a slot would have it, and
# checks that it refuses a job for another build. No container runtime is involved.
set -euo pipefail
build='' fixture=''
while [ $# -gt 0 ]; do
  case "$1" in
    --build) build=$(realpath "$2"); shift 2 ;;
    --fixture) fixture=$(realpath "$2"); shift 2 ;;
    *) echo "usage: smoke-native-ocr.sh --build DIR --fixture FILE" >&2; exit 2 ;;
  esac
done
[ -n "$build" ] && [ -n "$fixture" ] || { echo "--build and --fixture are required" >&2; exit 2; }
repo=$(cd "$(dirname "$0")/../.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
image="sha256:$(sha256sum "$build/usnm_native_build.json" | cut -d' ' -f1)"

slot() {
  mkdir "$work/$1"
  for file in "$build"/*; do
    [ "$(basename "$file")" = usnm_ocr ] ||
      printf '<soft_link>%s</soft_link>\n' "$file" > "$work/$1/$(basename "$file")"
  done
}

scan="$repo/web/public/og-image.png"
slot good
"$fixture" --scan "$scan" --language eng --image "$image" --dir "$work/good"
(cd "$work/good" && "$build/usnm_ocr")
for file in result.json text.txt layout.xml boinc_finish_called; do
  [ -s "$work/good/$file" ] || { echo "no $file" >&2; exit 1; }
done
grep -q '"text_sha256"' "$work/good/result.json"

slot wrong
"$fixture" --scan "$scan" --language eng --image "sha256:$(printf '2%.0s' {1..64})" --dir "$work/wrong"
if (cd "$work/wrong" && "$build/usnm_ocr" 2> "$work/wrong.log"); then
  echo "a job for another build ran" >&2; exit 1
fi
grep -q 'made for build' "$work/wrong.log"
[ ! -e "$work/wrong/boinc_finish_called" ]
echo "native worker read a page and refused another build's job"
