#!/usr/bin/env bash
# Install the on-prem Japanese OCR (#917) on an OCR node, as root: build the
# usnewsmap-ja-ocr image from a pinned commit of the public repository
# (docker/Dockerfile.ja-ocr; no registry), write /etc/usnm/ja-ocr.env and install usnm-ja-ocr@.service.
#
#   ja-ocr-onprem.sh --commit SHA --curated-url URL --reference-url URL
#     [--replicas 2] [--start]
#
# The host's Arc machine identity must be in the on-prem hosts' group
# (USNM_ONPREM_GROUP) and the environment provisioned with USNM_JA_OCR_ONPREM, which
# grants that group read on curated and reference and create-only writes under
# curated/ocr-ja/ (infra/modules/jaocr-runner.bicep). --start enables and starts one
# instance per replica (usnm-ja-ocr@1 ... @N). Safe to run again: a new --commit
# rebuilds the image and restarts running instances on it.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '6,7s/^# \{0,1\}//p' "$0" >&2; exit 2; }
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=tgoodyear/usnewsmap.com

COMMIT="" CURATED="" REFERENCE="" REPLICAS=2 START=false
while [ $# -gt 0 ]; do
  case "$1" in
    --commit) COMMIT=${2:-}; shift 2 ;;
    --curated-url) CURATED=${2:-}; shift 2 ;;
    --reference-url) REFERENCE=${2:-}; shift 2 ;;
    --replicas) REPLICAS=${2:-}; shift 2 ;;
    --start) START=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $COMMIT =~ ^[0-9a-f]{40}$ ]] || die "--commit: a full 40-character commit sha"
[[ $CURATED =~ ^https://[a-z0-9]+\.blob\.core\.windows\.net/curated$ ]] || die "--curated-url: https://<account>.blob.core.windows.net/curated"
[[ $REFERENCE =~ ^https://[a-z0-9]+\.blob\.core\.windows\.net/reference$ ]] || die "--reference-url: https://<account>.blob.core.windows.net/reference"
[[ $REPLICAS =~ ^[1-8]$ ]] || die "--replicas: 1 to 8"
[ "$(id -u)" = 0 ] || die "run as root"
command -v podman > /dev/null || die "podman missing"
gid=$(getent group himds | cut -d: -f3) || die "no himds group: is the Arc agent installed?"
[ -d /var/opt/azcmagent/tokens ] || die "no /var/opt/azcmagent/tokens: is the Arc agent installed?"

# The image, from the commit's source tarball (GitHub's archive of a public repository).
image="localhost/usnewsmap-ja-ocr:${COMMIT:0:12}"
if ! podman image exists "$image"; then
  work=$(mktemp -d /var/tmp/usnm-ja-ocr-build.XXXXXX)
  trap 'rm -rf "$work"' EXIT
  curl -fsSL "https://codeload.github.com/$REPO/tar.gz/$COMMIT" | tar -xz -C "$work" --strip-components=1
  # Commits before the Dockerfiles moved to docker/ keep it at the root.
  dockerfile="$work/docker/Dockerfile.ja-ocr"
  [ -f "$dockerfile" ] || dockerfile="$work/Dockerfile.ja-ocr"
  podman build --pull=missing -f "$dockerfile" -t "$image" "$work"
fi

install -d -m 0755 /etc/usnm
umask 022
cat > /etc/usnm/ja-ocr.env << EOF
# Written by scripts/ocr-node/ja-ocr-onprem.sh (commit $COMMIT).
JAOCR_IMAGE=$image
USNM_CURATED_URL=$CURATED
USNM_REFERENCE_URL=$REFERENCE
JAOCR_REPLICAS=$REPLICAS
HIMDS_GID=$gid
EOF
install -m 0644 "$HERE/usnm-ja-ocr@.service" /etc/systemd/system/
systemctl daemon-reload
echo "image $image; /etc/usnm/ja-ocr.env written"

if $START; then
  # Instances above --replicas (from an earlier, larger setting) stop first, so the
  # old and new replica counts never pace loc.gov at the same time.
  for unit in $(systemctl list-units --all --plain --no-legend 'usnm-ja-ocr@*' | awk '{print $1}'); do
    n=${unit#usnm-ja-ocr@}; n=${n%.service}
    [ "$n" -le "$REPLICAS" ] || { systemctl disable --now "$unit" > /dev/null 2>&1; echo "stopped $unit"; }
  done
  for i in $(seq 1 "$REPLICAS"); do
    systemctl enable "usnm-ja-ocr@$i" > /dev/null 2>&1
    systemctl restart "usnm-ja-ocr@$i"
  done
  echo "started $REPLICAS replica(s): journalctl -fu 'usnm-ja-ocr@*'"
fi
