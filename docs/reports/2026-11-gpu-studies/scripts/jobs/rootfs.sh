#!/usr/bin/env bash
# Apply a public GHCR image under an empty $ROOTFS (default /work/rootfs), checking each layer's sha256.
# The plain base image runs the engine under chroot when the worker image itself gives no SSH on Vast.
# Named flags: --repo owner/name --digest sha256:...
set -euo pipefail
REPO=""; DIGEST=""
while [ $# -gt 0 ]; do case "$1" in --repo) REPO=$2; shift 2;; --digest) DIGEST=$2; shift 2;; *) exit 2;; esac; done
[[ $REPO =~ ^[a-z0-9]+([._][a-z0-9]+|__[a-z0-9]+|-+[a-z0-9]+)*/[a-z0-9]+([._][a-z0-9]+|__[a-z0-9]+|-+[a-z0-9]+)*$ ]] &&
  [[ $DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "valid repo and sha256 image digest required" >&2; exit 2; }
HERE=$(CDPATH='' cd -- "$(dirname -- "$0")" && pwd -P)
R=${ROOTFS:-/work/rootfs}
[[ $R != *$'\n'* ]] || { echo "ROOTFS cannot contain a newline" >&2; exit 2; }
mkdir -p -- "$R"
R=$(CDPATH='' cd -- "$R" && pwd -P)
python3 -I "$HERE/rootfs-layer.py" --action check --root "$R"
T=$(curl -fsS "https://ghcr.io/token?scope=repository:$REPO:pull" | jq -r .token)
# The token goes to curl as a config on stdin, so it never appears in argv.
ghcr_config() { printf 'header = "Authorization: Bearer %s"\n' "$T"; }
ghcr() { ghcr_config | curl -K - "$@"; }
A='Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json'
manifest() {
  local expected=$1 actual
  # A sentinel preserves trailing newlines in the exact bytes covered by the digest.
  M=$(ghcr -fsS -H "$A" "https://ghcr.io/v2/$REPO/manifests/$expected" && printf '\001') || return 1
  M=${M%$'\001'}
  actual=$(printf '%s' "$M" | sha256sum) || return 1
  [ "${actual%% *}" = "${expected#sha256:}" ] || { echo "manifest sha256 mismatch: $expected" >&2; return 1; }
}
manifest "$DIGEST"
if [ "$(echo "$M" | jq 'has("manifests")')" = true ]; then
  D=$(echo "$M" | jq -r '[.manifests[]|select(.platform.os=="linux" and .platform.architecture=="amd64")|.digest][0] // empty')
  [[ $D =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "no valid linux/amd64 manifest digest" >&2; exit 1; }
  manifest "$D"
fi
echo "$M" | jq -e '.layers | type == "array"' >/dev/null ||
  { echo "invalid image layers" >&2; exit 1; }
LAYERS=$(echo "$M" | jq -r '.layers[].digest')
if [ -n "$LAYERS" ]; then
  while IFS= read -r l; do
    [[ $l =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "invalid layer digest: $l" >&2; exit 1; }
    ghcr_config | python3 -I "$HERE/rootfs-layer.py" --action fetch --root "$R" --digest "$l" --repo "$REPO"
  done <<<"$LAYERS"
fi
