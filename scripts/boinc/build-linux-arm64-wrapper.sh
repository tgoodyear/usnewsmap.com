#!/usr/bin/env bash
# Build the Linux arm64 docker_wrapper from the same upstream commit as the server image.
set -euo pipefail
usage() { echo 'usage: build-linux-arm64-wrapper.sh --out DIR' >&2; exit 2; }
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) [ $# -ge 2 ] || usage; out=$2; shift 2 ;;
    *) usage ;;
  esac
done
[ -n "$out" ] || usage
arch=$(uname -m)
case "$arch" in
  aarch64|arm64) ;;
  *) echo "build-linux-arm64-wrapper.sh must run on an arm64 Linux host (got $arch)" >&2; exit 1 ;;
esac
root=$(cd "$(dirname "$0")/../.." && pwd)
commit=$(sed -n 's/^ARG BOINC_COMMIT=//p' "$root/contrib-compute/images/base/Containerfile")
[[ $commit =~ ^[0-9a-f]{40}$ ]] || { echo 'invalid BOINC source pin' >&2; exit 1; }
mkdir "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
git -C "$work" init -q
git -C "$work" remote add origin https://github.com/BOINC/boinc.git
git -C "$work" fetch --depth 1 origin "$commit"
git -C "$work" checkout -q --detach FETCH_HEAD
[ "$(git -C "$work" rev-parse HEAD)" = "$commit" ]
(
  cd "$work"
  ./_autosetup
  ./configure --disable-client --disable-manager --disable-server
  make -C lib -j 2
  make -C api -j 2
  make -C samples/docker_wrapper -j 2
)
built=$(find "$work/samples/docker_wrapper" -maxdepth 1 -type f -perm -u+x -name 'docker_wrapper*' | head -n 1)
[ -n "$built" ] || { echo 'docker_wrapper binary was not produced' >&2; exit 1; }
cp "$built" "$out/docker_wrapper_aarch64-unknown-linux-gnu"
printf '%s\n' "$commit" > "$out/BOINC_COMMIT"
(cd "$out" && sha256sum docker_wrapper_aarch64-unknown-linux-gnu > SHA256SUMS)
sha=$(cut -d ' ' -f 1 "$out/SHA256SUMS")
python3 "$root/scripts/boinc/check-linux-wrapper.py" \
  --wrapper "$out/docker_wrapper_aarch64-unknown-linux-gnu" \
  --platform aarch64-unknown-linux-gnu \
  --sha256 "$sha" \
  --source-pin "$out/BOINC_COMMIT"
