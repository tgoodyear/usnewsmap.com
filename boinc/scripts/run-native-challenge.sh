#!/usr/bin/env bash
# Runs the calibrated English challenge through a native build with no BOINC client, as a
# usnm_gold_native slot would have it (#1418, docs/boinc/apps/native-ocr.md, "The challenge gate").
#
#   run-native-challenge.sh --build DIR --fixture FILE --prepared FILE --mirror FILE --out DIR
#
# --build     the build directory of the boinc-ocr-native artifact (usnm_ocr, usnm_tesseract,
#             usnm_native_build.json, tessdata_*)
# --fixture   the native_fixture example binary built from the same checkout
# --prepared  the challenge's prepared.json (boinc/project/challenges/eng-d8cd7279.prepared.json)
# --mirror    the project download path of each input (boinc/project/challenges/eng-d8cd7279.mirror.json)
# --out       where gold-result.json, the build's identity and the unit's timing are written
#
# The bundle tar and the BLN600 zip come from the project's public download URL (anonymous GET),
# both checked against the sizes and sha256 values in prepared.json before use.
# Nothing here is a secret, and no credential is read.
set -euo pipefail
build='' fixture='' prepared='' mirror='' out=''
base_url="${PROJECT_DOWNLOAD_URL:-https://boinc.usnewsmap.com/usnmocr_download}"
while [ $# -gt 0 ]; do
  case "$1" in
    --build) build=$(realpath "$2"); shift 2 ;;
    --fixture) fixture=$(realpath "$2"); shift 2 ;;
    --prepared) prepared=$(realpath "$2"); shift 2 ;;
    --mirror) mirror=$(realpath "$2"); shift 2 ;;
    --out) mkdir -p "$2"; out=$(realpath "$2"); shift 2 ;;
    *) echo "usage: run-native-challenge.sh --build DIR --fixture FILE --prepared FILE --mirror FILE --out DIR" >&2; exit 2 ;;
  esac
done
for v in build fixture prepared mirror out; do
  [ -n "${!v}" ] || { echo "--$v is required" >&2; exit 2; }
done
sha256() { if command -v sha256sum >/dev/null; then sha256sum "$1"; else shasum -a 256 "$1"; fi | cut -d' ' -f1; }
# The download hierarchy's fanout directory is a hash of the physical name that only the server's
# dir_hier_path computes, so the mirror file records each input's path.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
root="$work/campaign"
mkdir -p "$root" "$work/slot" "$work/inputs"

python3 -I - "$prepared" "$mirror" "$root" "$work/inputs" "$base_url" <<'PY'
import hashlib, json, os, shutil, subprocess, sys
prepared, mirror, root, inputs, base = sys.argv[1:]
p = json.load(open(prepared))
m = json.load(open(mirror))
shutil.copy(prepared, f"{root}/prepared.json")
def fetch(url, staged, dest):
    subprocess.run(["curl", "-fsSL", "--retry", "5", "--retry-all-errors", "--retry-delay", "5",
                    "-o", dest, url], check=True)
    h = hashlib.sha256()
    with open(dest, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    if h.hexdigest() != staged["sha256"] or os.path.getsize(dest) != staged["bytes"]:
        sys.exit(f"{staged['name']} does not match prepared.json")
fetch(f"{base}/{m['bundle']}", p["bundle"], f"{root}/bundle.tar")
for r in p["remotes"]:
    fetch(f"{base}/{m['remotes'][r['name']]}", r, f"{inputs}/{r['name']}")
PY

image="sha256:$(sha256 "$build/usnm_native_build.json")"
engine_base="sha256:$(sha256 "$build/usnm_tesseract")"
for file in "$build"/*; do
  [ "$(basename "$file")" = usnm_ocr ] ||
    printf '<soft_link>%s</soft_link>\n' "$file" > "$work/slot/$(basename "$file")"
done
"$fixture" --gold-root "$root" --gold-base "$engine_base" --image "$image" --dir "$work/slot"
for f in "$work"/inputs/*; do
  printf '<soft_link>%s</soft_link>\n' "$f" > "$work/slot/$(basename "$f")"
done
start=$(date +%s)
(cd "$work/slot" && "$build/usnm_ocr")
elapsed=$(( $(date +%s) - start ))
[ -s "$work/slot/gold-result.json" ] || { echo "no gold-result.json" >&2; exit 1; }
[ -e "$work/slot/boinc_finish_called" ] || { echo "the worker did not finish" >&2; exit 1; }
cp "$work/slot/gold-result.json" "$out/gold-result.json"
printf '{"elapsed_secs": %s, "engine_base": "%s", "engine_image": "%s"}\n' \
  "$elapsed" "$engine_base" "$image" > "$out/identity.json"
echo "challenge ran in ${elapsed}s for $image"
