#!/usr/bin/env bash
# Unpack a public GHCR image by digest under $ROOTFS (default /work/rootfs), checking each layer's sha256. Used for an
# image whose own container gives no SSH on Vast: a plain base image runs instead, and the engine runs under chroot.
# Named flags: --repo owner/name --digest sha256:...
set -eu
REPO=""; DIGEST=""
while [ $# -gt 0 ]; do case "$1" in --repo) REPO=$2; shift 2;; --digest) DIGEST=$2; shift 2;; *) exit 2;; esac; done
R=${ROOTFS:-/work/rootfs}; mkdir -p "$R"; L=$(mktemp -d)
T=$(curl -fsS "https://ghcr.io/token?scope=repository:$REPO:pull" | jq -r .token)
# The token goes to curl as a config on stdin, so it never appears in argv.
ghcr() { printf 'header = "Authorization: Bearer %s"\n' "$T" | curl -K - "$@"; }
A='Accept: application/vnd.oci.image.index.v1+json, application/vnd.docker.distribution.manifest.v2+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json'
M=$(ghcr -fsS -H "$A" "https://ghcr.io/v2/$REPO/manifests/$DIGEST")
if [ "$(echo "$M" | jq 'has("manifests")')" = true ]; then
  D=$(echo "$M" | jq -r '.manifests[]|select(.platform.architecture=="amd64")|.digest' | head -1)
  M=$(ghcr -fsS -H "$A" "https://ghcr.io/v2/$REPO/manifests/$D")
fi
for l in $(echo "$M" | jq -r '.layers[].digest'); do
  f=$L/${l#sha256:}
  ghcr -fsSL -o "$f" "https://ghcr.io/v2/$REPO/blobs/$l"
  echo "${l#sha256:}  $f" | sha256sum -c --quiet -
  tar -xzf "$f" -C "$R" --exclude='.wh.*' 2>/dev/null || tar -xf "$f" -C "$R" --exclude='.wh.*'
  rm -f "$f"
done
rm -rf "$L"
