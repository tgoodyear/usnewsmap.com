#!/usr/bin/env bash
# Builds the Tesseract of the native macOS `usnm_ocr` and `usnm_gold` app versions (#1418) from
# the sources pinned in static-sources.lock: one Mach-O executable, statically linked against
# those libraries, that needs nothing but the system's libc++, libSystem and frameworks.
#
#   boinc/apps/native/build-tesseract-macos.sh --out build --work /tmp/tesseract-macos
#
# It builds for the machine's own architecture (arm64 or x86_64), so CI runs it once on each.
# Writes <out>/usnm_tesseract, ad hoc signed, and <out>/usnm_tesseract.sources, which records the
# sources, the compiler and the libraries the executable loads. Differences from the Linux build
# (Containerfile) are in docs/boinc/apps/native-ocr.md: upstream sources without Debian's
# patches, no GIF, no OpenMP, libjpeg-turbo without SIMD, and libtiff with only zlib and JPEG.
#
# Tesseract gets the upstream patches of tesseract-patches.lock (#1835), each checked against its
# sha256 there, as the Linux build does. --no-patches builds the stock 5.5.0 instead, which CI
# does to compare the two on one runner.
set -Eeuo pipefail

out='' work='' patches=1
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --work) work="$2"; shift 2 ;;
    --no-patches) patches=0; shift ;;
    *) echo "usage: build-tesseract-macos.sh --out DIR --work DIR [--no-patches]" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] && [ -n "$work" ] || { echo "--out and --work are required" >&2; exit 2; }
[ "$(uname -s)" = Darwin ] || { echo "this builds on macOS only" >&2; exit 2; }
arch=$(uname -m)
case "$arch" in arm64|x86_64) ;; *) echo "unsupported architecture $arch" >&2; exit 2 ;; esac

here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$out" "$work"
out=$(cd "$out" && pwd)
work=$(cd "$work" && pwd)
src="$work/src" prefix="$work/prefix"
rm -rf "$src" "$prefix" "$work/build"
mkdir -p "$src" "$prefix" "$work/build"
jobs=$(sysctl -n hw.ncpu)

# The oldest macOS the build runs on, for every library and the engine alike.
export MACOSX_DEPLOYMENT_TARGET=11.0
# Only this prefix's libraries: never Homebrew's, which a CI runner has installed.
export PKG_CONFIG_LIBDIR="$prefix/lib/pkgconfig" PKG_CONFIG_PATH=''
common=(
  -DCMAKE_BUILD_TYPE=Release
  -DCMAKE_INSTALL_PREFIX="$prefix"
  -DCMAKE_INSTALL_LIBDIR=lib
  -DCMAKE_PREFIX_PATH="$prefix"
  -DCMAKE_IGNORE_PREFIX_PATH="/opt/homebrew;/usr/local"
  # Apple's compilers and make, whatever else is on PATH.
  -DCMAKE_C_COMPILER=/usr/bin/clang
  -DCMAKE_CXX_COMPILER=/usr/bin/clang++
  -DCMAKE_MAKE_PROGRAM=/usr/bin/make
  -DCMAKE_FIND_USE_PACKAGE_REGISTRY=OFF
  -DCMAKE_FIND_FRAMEWORK=LAST
  -DCMAKE_OSX_ARCHITECTURES="$arch"
  -DCMAKE_OSX_DEPLOYMENT_TARGET="$MACOSX_DEPLOYMENT_TARGET"
  -DCMAKE_POSITION_INDEPENDENT_CODE=ON
  -DBUILD_SHARED_LIBS=OFF
  # zlib and libjpeg-turbo declare minimums CMake 4 no longer accepts.
  -DCMAKE_POLICY_VERSION_MINIMUM=3.5
)

fetch() { # name url tag commit
  git -c init.defaultBranch=pinned init -q "$src/$1"
  git -C "$src/$1" fetch -q --depth 1 "$2" "refs/tags/$3"
  local got
  got=$(git -C "$src/$1" rev-parse 'FETCH_HEAD^{commit}')
  [ "$got" = "$4" ] || { echo "$1 $3 is $got, the lock says $4" >&2; exit 1; }
  git -C "$src/$1" -c advice.detachedHead=false checkout -q "$4"
}

build() { # name, then its own CMake options
  local name=$1; shift
  current="$work/build/$name.log"
  cmake -S "$src/$name" -B "$work/build/$name" "${common[@]}" "$@" >"$work/build/$name.log"
  cmake --build "$work/build/$name" --parallel "$jobs" >>"$work/build/$name.log"
  cmake --install "$work/build/$name" >>"$work/build/$name.log"
  # A static build: no shared copy of a library may stay where the next one could link it.
  rm -f "$prefix"/lib/*.dylib
}

# A failed step shows the end of its log; the logs are quiet otherwise.
current=""
trap '[ -n "$current" ] && [ -f "$current" ] && tail -n 60 "$current" >&2' ERR

grep -v '^#' "$here/static-sources.lock" | while read -r name url tag commit; do
  [ -n "$name" ] && fetch "$name" "$url" "$tag" "$commit"
done

# The upstream patches, in the lock's order: every file in patches/ must be in the lock with its
# sha256, and every line of the lock must have its file.
if [ "$patches" = 1 ]; then
  [ "$(grep -v '^#' "$here/tesseract-patches.lock" | awk 'NF {print $1}')" = "$(ls "$here/patches")" ] ||
    { echo "patches/ and tesseract-patches.lock list different files" >&2; exit 1; }
  grep -v '^#' "$here/tesseract-patches.lock" | while read -r file commit sum; do
    [ -n "$file" ] || continue
    [ "$(shasum -a 256 "$here/patches/$file" | cut -d' ' -f1)" = "$sum" ] ||
      { echo "$file does not have the sha256 in tesseract-patches.lock" >&2; exit 1; }
    git -C "$src/tesseract" apply --ignore-whitespace "$here/patches/$file" ||
      { echo "patch $file does not apply" >&2; exit 1; }
  done
fi

build zlib -DZLIB_BUILD_EXAMPLES=OFF
# libjpeg-turbo resolves a relative CMAKE_INSTALL_LIBDIR against the working directory, not the prefix.
build libjpeg-turbo -DCMAKE_INSTALL_LIBDIR="$prefix/lib" -DENABLE_SHARED=OFF -DENABLE_STATIC=ON -DWITH_SIMD=OFF -DWITH_TURBOJPEG=OFF
build libpng -DPNG_SHARED=OFF -DPNG_STATIC=ON -DPNG_TESTS=OFF -DPNG_TOOLS=OFF -DPNG_FRAMEWORK=OFF
build libtiff -Dtiff-tools=OFF -Dtiff-tests=OFF -Dtiff-contrib=OFF -Dtiff-docs=OFF -Dcxx=OFF \
  -Dzlib=ON -Djpeg=ON -Djpeg12=OFF -Djbig=OFF -Dlerc=OFF -Dlibdeflate=OFF -Dlzma=OFF -Dwebp=OFF -Dzstd=OFF
build openjpeg -DBUILD_STATIC_LIBS=ON -DBUILD_CODEC=OFF -DBUILD_TESTING=OFF -DBUILD_DOC=OFF
build libwebp -DWEBP_BUILD_ANIM_UTILS=OFF -DWEBP_BUILD_CWEBP=OFF -DWEBP_BUILD_DWEBP=OFF \
  -DWEBP_BUILD_GIF2WEBP=OFF -DWEBP_BUILD_IMG2WEBP=OFF -DWEBP_BUILD_VWEBP=OFF -DWEBP_BUILD_WEBPINFO=OFF \
  -DWEBP_BUILD_WEBPMUX=OFF -DWEBP_BUILD_EXTRAS=OFF -DWEBP_BUILD_LIBWEBPMUX=ON
# The static libraries' installed configs don't import their own dependencies. libtiff's says
# "TODO: import dependencies", so TIFF::tiff links JPEG::JPEG and a CMath::CMath no package
# defines, and Leptonica's links WebP::webp, WebP::libwebpmux and ZLIB::ZLIB without loading
# them. Leptonica and Tesseract both load this first (Tesseract's TIFF check recreates the
# targets it finds). On macOS the maths library is part of libSystem, so an empty imported
# target is the right CMath.
cat >"$work/static-deps.cmake" <<'CMAKE'
if(NOT TARGET CMath::CMath)
  add_library(CMath::CMath INTERFACE IMPORTED)
endif()
find_package(ZLIB REQUIRED)
find_package(JPEG REQUIRED)
find_package(WebP CONFIG REQUIRED)
CMAKE
build leptonica -DSW_BUILD=OFF -DBUILD_PROG=OFF -DENABLE_GIF=OFF -DCMAKE_DISABLE_FIND_PACKAGE_GIF=ON \
  -DCMAKE_PROJECT_INCLUDE="$work/static-deps.cmake"

# Apple's linker resolves static archives in any order, so naming every library here covers
# whatever Leptonica's exported target leaves out.
libs="-L$prefix/lib -lleptonica -ltiff -lopenjp2 -lwebpmux -lwebp -lsharpyuv -lpng16 -ljpeg -lz"
current="$work/build/tesseract.log"
cmake -S "$src/tesseract" -B "$work/build/tesseract" "${common[@]}" \
  -DSW_BUILD=OFF -DOPENMP_BUILD=OFF -DBUILD_TRAINING_TOOLS=OFF -DBUILD_TESTS=OFF \
  -DDISABLE_ARCHIVE=ON -DDISABLE_CURL=ON -DENABLE_LTO=OFF -DENABLE_NATIVE=OFF -DINSTALL_CONFIGS=OFF \
  -DCMAKE_PROJECT_INCLUDE="$work/static-deps.cmake" -DCMAKE_EXE_LINKER_FLAGS="$libs" >"$work/build/tesseract.log"
cmake --build "$work/build/tesseract" --parallel "$jobs" --target tesseract >>"$work/build/tesseract.log"
engine=$(find "$work/build/tesseract" -type f -name tesseract -perm -u+x | head -n 1)
[ -n "$engine" ] || { echo "the Tesseract build made no executable" >&2; exit 1; }

cp "$engine" "$out/usnm_tesseract"
strip -x "$out/usnm_tesseract"
codesign --force --sign - "$out/usnm_tesseract"

# Nothing outside the system: every library the engine loads is in /usr/lib or /System.
loads=$(otool -L "$out/usnm_tesseract" | tail -n +2 | awk '{print $1}')
if printf '%s\n' "$loads" | grep -Ev '^(/usr/lib/|/System/Library/)'; then
  echo "usnm_tesseract loads a library from outside the system" >&2
  exit 1
fi
file "$out/usnm_tesseract" | grep -q "Mach-O 64-bit executable $arch" ||
  { echo "usnm_tesseract is not a $arch Mach-O executable" >&2; exit 1; }
"$out/usnm_tesseract" --version | head -n 1 | grep -qx 'tesseract 5.5.0' ||
  { echo "usnm_tesseract is not Tesseract 5.5.0" >&2; exit 1; }

{
  grep -v '^#' "$here/static-sources.lock" | awk 'NF {print "source", $1, $3, $4}'
  if [ "$patches" = 1 ]; then
    grep -v '^#' "$here/tesseract-patches.lock" | awk 'NF {print "tesseract-patch", $1, $2, $3}'
  else
    echo "tesseract-patches none"
  fi
  echo "compiler $(clang --version | head -n 1)"
  echo "sdk $(xcrun --show-sdk-version)"
  echo "deployment-target $MACOSX_DEPLOYMENT_TARGET"
  printf 'loads %s\n' $loads
  "$out/usnm_tesseract" --version 2>&1 | sed 's/^ */version /'
} > "$out/usnm_tesseract.sources"
shasum -a 256 "$out/usnm_tesseract"
