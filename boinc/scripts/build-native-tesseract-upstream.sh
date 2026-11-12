#!/bin/bash
# Builds the native usnm_tesseract from an upstream tag listed in
# boinc/apps/native/tesseract-upstream.lock (#1835), beside the default Debian-source build.
# usage: build-native-tesseract-upstream.sh --version 5.5.4 --out FILE [--native-base IMAGE] [--network MODE] [--main-std STD] [--keep-images]
# The default build, boinc/apps/native/Containerfile, is not changed or used for its binary.
set -euo pipefail
VERSION='' OUT='' BASE='' NET='' STD=c++17 KEEP=0
while [ $# -gt 0 ]; do
  case $1 in
    --version) VERSION=$2; shift 2;;
    --out) OUT=$2; shift 2;;
    --native-base) BASE=$2; shift 2;;
    --network) NET=$2; shift 2;;
    --main-std) STD=$2; shift 2;;
    --keep-images) KEEP=1; shift;;
    *) echo "unknown flag $1" >&2; exit 2;;
  esac
done
[ -n "$VERSION" ] && [ -n "$OUT" ] || { echo "--version and --out are required" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")/../apps/native" && pwd)
read -r _ URL COMMIT SHA < <(awk -v v="$VERSION" '$1 == v' "$HERE/tesseract-upstream.lock")
[ -n "${SHA:-}" ] || { echo "$VERSION is not in tesseract-upstream.lock" >&2; exit 1; }
TAG=$$
NETARG=(); [ -z "$NET" ] || NETARG=(--network "$NET")
if [ -z "$BASE" ]; then
  BASE=localhost/usnm-native-apt:$TAG
  podman build "${NETARG[@]}" -f "$HERE/Containerfile" --target apt-native -t "$BASE" "$HERE"
fi
IMG=localhost/usnm-tesseract-upstream:$VERSION-$TAG
podman build "${NETARG[@]}" -f "$HERE/Containerfile.upstream" -t "$IMG" "$HERE" \
  --build-arg NATIVE_BASE="$BASE" --build-arg TESSERACT_VERSION="$VERSION" \
  --build-arg MAIN_STD="$STD" --build-arg TESSERACT_URL="$URL" --build-arg TESSERACT_COMMIT="$COMMIT" --build-arg TESSERACT_SHA256="$SHA"
podman run --rm --network=none "$IMG" cat /out/usnm_tesseract > "$OUT"
chmod +x "$OUT"
podman run --rm --network=none "$IMG" cat /out/usnm_tesseract.packages > "$OUT.packages"
sha256sum "$OUT"
if [ "$KEEP" = 0 ]; then podman rmi -f "$IMG" >/dev/null; [ "$BASE" != "localhost/usnm-native-apt:$TAG" ] || podman rmi -f "$BASE" >/dev/null; fi
