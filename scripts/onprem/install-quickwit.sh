#!/usr/bin/env bash
# Install the Quickwit binary of an environment's pinned image
# (infra/quickwit-image.json) on an on-prem host, for an on-prem release
# (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem"). Run as a member of the himds
# group on an Arc machine with AcrPull on the environment's registry; needs
# curl, jq and skopeo.
#
#   scripts/onprem/install-quickwit.sh --registry crusnmprodffapzy [--env prod]
#     [--dest /srv/usnm/bin]
#
# The machine identity's Resource Manager token is exchanged for a registry
# refresh token, kept in a private auth file for the copy only. The image is
# copied by digest, so the bits are the ones the environment runs; the binary
# is taken from its layers to DEST/quickwit-<tag>-<digest prefix>, and
# DEST/quickwit points at it.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,15s/^# \{0,1\}//p' "$0" >&2; exit 2; }
REGISTRY="" ENV_NAME=prod DEST=/srv/usnm/bin
while [ $# -gt 0 ]; do
  case "$1" in
    --registry) REGISTRY=${2:-}; shift 2 ;;
    --env) ENV_NAME=${2:-}; shift 2 ;;
    --dest) DEST=${2:-}; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $REGISTRY =~ ^[a-z0-9]+$ ]] || usage
HERE=$(cd "$(dirname "$0")/../.." && pwd)
PIN=$HERE/infra/quickwit-image.json
IMAGE=$(jq -r --arg e "$ENV_NAME" '.environments[$e].image' "$PIN")
TAG=$(jq -r --arg e "$ENV_NAME" '.environments[$e].tag' "$PIN")
DIGEST=$(jq -r --arg e "$ENV_NAME" '.environments[$e].digest' "$PIN")
[[ $DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] || die "no digest for $ENV_NAME in $PIN"
HOST=$REGISTRY.azurecr.io

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
chmod 700 "$work"
u="http://localhost:40342/metadata/identity/oauth2/token?api-version=2020-06-01&resource=https%3A%2F%2Fmanagement.azure.com%2F"
# The agent's endpoint is on loopback: never through a proxy.
key=$(curl -s --noproxy '*' -D - -o /dev/null -H Metadata:true "$u" | tr -d '\r' | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: Basic realm=//p')
[ -n "$key" ] || die "no challenge from the Arc agent's identity endpoint"
# Only one of the agent's own challenge files, as usnm-store's ArcIdentity
# checks it: a .key file directly in its token directory, regular, at most
# 4 KiB. Anything else answering on the port could otherwise name any file.
tokens=$(realpath /var/opt/azcmagent/tokens)
file=$(realpath -e -- "$key" 2> /dev/null) || die "the challenge names a file that can't be read"
[[ $file == *.key && $(dirname "$file") == "$tokens" && -f $file && ! -L $key ]] ||
  die "the challenge file isn't one of the agent's"
[ "$(stat -c %s "$file")" -le 4096 ] || die "the challenge file is too large"
arm=$(curl -s --noproxy '*' -H Metadata:true -H "Authorization: Basic $(cat "$file")" "$u" | jq -r .access_token)
[ -n "$arm" ] && [ "$arm" != null ] || die "no token from the Arc identity (is this user in himds?)"
refresh=$(curl -s -X POST "https://$HOST/oauth2/exchange" \
  --data-urlencode grant_type=access_token --data-urlencode "service=$HOST" \
  --data-urlencode "access_token=$arm" | jq -r .refresh_token)
[ -n "$refresh" ] && [ "$refresh" != null ] || die "the registry refused the token (AcrPull?)"
(umask 077; jq -n --arg h "$HOST" --arg a "$(printf '00000000-0000-0000-0000-000000000000:%s' "$refresh" | base64 -w0)" \
  '{auths: {($h): {auth: $a}}}' > "$work/auth.json")
skopeo copy --authfile "$work/auth.json" "docker://$HOST/$IMAGE@$DIGEST" "dir:$work/image"
rm -f "$work/auth.json"
mkdir -p "$DEST"
out=$DEST/quickwit-$TAG-${DIGEST:7:8}
# The last layer holding the binary wins, as in the image.
found=false
for layer in $(jq -r '.layers[].digest | sub("sha256:"; "")' "$work/image/manifest.json"); do
  if tar -tzf "$work/image/$layer" 2> /dev/null | grep -qx 'usr/local/bin/quickwit'; then
    tar -xzf "$work/image/$layer" -C "$work" usr/local/bin/quickwit
    found=true
  fi
done
$found || die "no usr/local/bin/quickwit in $IMAGE@$DIGEST"
install -m 0755 "$work/usr/local/bin/quickwit" "$out"
ln -sfn "$(basename "$out")" "$DEST/quickwit"
"$DEST/quickwit" --version
