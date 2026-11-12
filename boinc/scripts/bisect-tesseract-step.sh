#!/bin/bash
# One step of a Tesseract bisect (#1835): builds an upstream commit in a persistent tree and reads a
# set of pages with it. Each call is one command; the tree keeps its object files, so a step
# recompiles only what the commit changed.
#
# usage: bisect-tesseract-step.sh --repo DIR --commit REF --base-image IMAGE --pages FILE \
#          --tessdata DIR --out DIR [--mount DIR]... [--network MODE] [--jobs N] \
#          [--overlay-ref REF --overlay-file PATH]...
#
# --repo is a clone of tesseract-ocr/tesseract. It is checked out at --commit and built in place,
# inside a container of --base-image (the default Containerfile's apt-native stage, so the
# compiler and libraries are the native build's) with Debian's flags (see Containerfile.upstream).
# The binary is the dynamically linked tesseract the tree builds; a plain upstream build gave the
# same output as the static build (docs/reports/2026-11-tesseract-5-5-4/report.md).
# --pages is a TSV: id, image path, profile. The outputs go to --out/<id>/page.{txt,xml}.
# --mount makes a directory visible at the same path in the container (the pages, the models).
# --overlay-ref and --overlay-file take the named files of the repository from REF instead of --commit,
# to hold one change constant while another is bisected. The repository is put back after the step.
# Exit status 125 if the commit doesn't build, as `git bisect run` expects.
set -uo pipefail
REPO='' COMMIT='' IMAGE='' PAGES='' TESSDATA='' OUT='' NET=host JOBS=3 MOUNTS=() OVERLAY_REF='' OVERLAY_FILES=()
while [ $# -gt 0 ]; do
  case $1 in
    --repo) REPO=$2;; --commit) COMMIT=$2;; --base-image) IMAGE=$2;; --pages) PAGES=$2;;
    --tessdata) TESSDATA=$2;; --out) OUT=$2;; --mount) MOUNTS+=(-v "$2:$2:ro,z");;
    --network) NET=$2;; --jobs) JOBS=$2;;
    --overlay-ref) OVERLAY_REF=$2;; --overlay-file) OVERLAY_FILES+=("$2");;
    *) echo "unknown flag $1" >&2; exit 2;;
  esac
  shift 2
done
for v in REPO COMMIT IMAGE PAGES TESSDATA OUT; do [ -n "${!v}" ] || { echo "--${v,,} is required" >&2; exit 2; }; done
REPO=$(realpath "$REPO") PAGES=$(realpath "$PAGES") TESSDATA=$(realpath "$TESSDATA")
mkdir -p "$OUT"; OUT=$(realpath "$OUT")
git -C "$REPO" checkout -q -f "$COMMIT" || exit 125
if [ -n "$OVERLAY_REF" ]; then
  git -C "$REPO" checkout -q "$OVERLAY_REF" -- "${OVERLAY_FILES[@]}" || exit 125
fi
rm -rf "${OUT:?}"/*
podman run --rm --network "$NET" -v "$REPO:/src/t:z" -v "$OUT:$OUT:z" -v "$PAGES:$PAGES:ro,z" -v "$TESSDATA:$TESSDATA:ro,z" \
  "${MOUNTS[@]}" -e JOBS="$JOBS" -e TESSDATA="$TESSDATA" -e PAGES="$PAGES" -e OUT="$OUT" "$IMAGE" bash -c '
  set -u
  cd /src/t
  rm -rf /usr/include/tesseract /usr/lib/x86_64-linux-gnu/libtesseract*
  export DEB_BUILD_MAINT_OPTIONS=hardening=+all
  if [ ! -f Makefile ]; then
    ./autogen.sh >/dev/null 2>&1 || exit 125
    CXXFLAGS="$(dpkg-buildflags --get CXXFLAGS) -Wall -g -fPIC"
    ./configure --disable-tessdata-prefix --prefix=/usr CXXFLAGS="$CXXFLAGS" \
      LDFLAGS="-lleptonica -Wl,-z,defs $(dpkg-buildflags --get LDFLAGS)" >/dev/null 2>&1 || exit 125
  fi
  # A commit that adds or removes source files needs the generated Makefile.in again.
  make -j"$JOBS" tesseract >/tmp/make.log 2>&1 || {
    ./autogen.sh >/dev/null 2>&1
    ./configure --disable-tessdata-prefix --prefix=/usr CXXFLAGS="$(dpkg-buildflags --get CXXFLAGS) -Wall -g -fPIC" \
      LDFLAGS="-lleptonica -Wl,-z,defs $(dpkg-buildflags --get LDFLAGS)" >/dev/null 2>&1
    make -j"$JOBS" tesseract >/tmp/make.log 2>&1 || { tail -20 /tmp/make.log >&2; exit 125; }
  }
  while IFS=$'"'"'\t'"'"' read -r id img prof; do
    d=$OUT/$id; mkdir -p "$d"; ext=${img##*.}; cp "$img" "$d/input.$ext"
    ( cd "$d" && OMP_THREAD_LIMIT=1 TESSDATA_PREFIX=$TESSDATA /src/t/tesseract "input.$ext" page -l "$prof" --psm 3 --dpi 300 txt alto >stdout.log 2>&1; rm -f input.* )
  done <"$PAGES"
'
status=$?
[ -z "$OVERLAY_REF" ] || git -C "$REPO" checkout -q -f "$COMMIT"
exit $status
