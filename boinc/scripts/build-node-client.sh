#!/usr/bin/env bash
# Build a stock stable client; never install, create users, or start a service.
# Run in a dev/build environment, NOT beside the production worker.
# Usage: boinc/scripts/build-node-client.sh --out ABSOLUTE_NEW_DIRECTORY
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) [ $# -ge 2 ] || die "--out needs a directory"; out=$2; shift 2 ;;
    *) die "usage: $0 --out ABSOLUTE_NEW_DIRECTORY" ;;
  esac
done
[ -n "$out" ] || die "usage: $0 --out ABSOLUTE_NEW_DIRECTORY"
[[ "$out" =~ ^/[A-Za-z0-9_./-]+$ ]] && [[ "$out" != *..* ]] || die "--out must be a plain absolute new directory"
[ ! -e "$out" ] || die "--out already exists; refusing to overwrite"
for cmd in git autoconf automake libtoolize make pkg-config c++; do
  command -v "$cmd" >/dev/null || die "missing build prerequisite: $cmd"
done
pkg-config --exists libcurl openssl || die "install curl/OpenSSL development packages in the build environment"
# Official stable client_release/8.2/8.2.13, peeled commit (not master or the server tag).
commit=ae68f3290ac82d08b4d9767acfe0daaccaeeb883
mkdir -p "$out"
git init -q "$out/source"
git -C "$out/source" fetch -q --depth 1 https://github.com/BOINC/boinc.git "$commit"
git -C "$out/source" checkout -q --detach FETCH_HEAD
[ "$(git -C "$out/source" rev-parse HEAD)" = "$commit" ] || die "source revision differs"
(
  cd "$out/source"
  ./_autosetup
  ./configure --disable-server --disable-manager --enable-client --prefix="$out/install"
  make -j 1 -C lib
  make -j 1 -C client
  make -j 1 -C lib install
  make -j 1 -C client install
)
"$out/install/bin/boinc" --version | grep -Eq '^8\.2\.13([[:space:]]|$)' || die "unexpected client version"
printf '%s\n' "$commit" > "$out/source-commit"
sha256sum "$out/install/bin/boinc" "$out/install/bin/boinccmd" > "$out/SHA256SUMS"
echo "Built stock 8.2.13 under $out/install; no host installation or service changes."
