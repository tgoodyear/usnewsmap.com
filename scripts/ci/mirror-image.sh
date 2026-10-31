#!/usr/bin/env bash
# Copy an image CI just pushed to the environment's registry into the GitHub
# Container Registry, with its digest unchanged, so the same attestation
# subject digest holds in both.
#
#   scripts/ci/mirror-image.sh --source ACR/REPO --digest sha256:HEX \
#     --dest ghcr.io/OWNER/REPO --commit SHA [--tag main] [--attempts 3]
#
# The source is read by digest, so what lands in GHCR is the build that was
# pushed, or nothing. Tag COMMIT is written first and --tag last, and each is
# read back: a reader that sees the moving tag can count on the commit's own
# tag. GH_TOKEN signs in to GHCR and `az acr login` to the source, each from
# the standard input into a credentials file of its own (see quickwit-image.sh
# for why that isn't Docker's). A failure ends with a ::warning:: and exit 1:
# ci.yml runs this with continue-on-error, so a GHCR outage holds back only
# the mirror, never the Azure rollout.
set -euo pipefail
usage() { sed -n '6,7s/^# \{0,1\}//p' "$0" >&2; exit 2; }
SOURCE="" DIGEST="" DEST="" COMMIT="" TAG=main ATTEMPTS=3
while [ $# -gt 0 ]; do
  case "$1" in
    --source) SOURCE=${2:-}; shift 2 || usage ;;
    --digest) DIGEST=${2:-}; shift 2 || usage ;;
    --dest) DEST=${2:-}; shift 2 || usage ;;
    --commit) COMMIT=${2:-}; shift 2 || usage ;;
    --tag) TAG=${2:-}; shift 2 || usage ;;
    --attempts) ATTEMPTS=${2:-}; shift 2 || usage ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $SOURCE =~ ^[a-z0-9.-]+\.azurecr\.io/[a-z0-9/_-]+$ ]] || { echo "error: --source: REGISTRY.azurecr.io/REPO" >&2; usage; }
[[ $DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "error: --digest: sha256:HEX" >&2; usage; }
[[ $DEST =~ ^ghcr\.io/[a-z0-9._-]+/[a-z0-9/._-]+$ ]] || { echo "error: --dest: ghcr.io/OWNER/REPO" >&2; usage; }
[[ $COMMIT =~ ^[0-9a-f]{40}$ ]] || { echo "error: --commit: a full commit hash" >&2; usage; }
[[ $TAG =~ ^[a-z0-9][a-z0-9._-]{0,63}$ ]] || { echo "error: --tag: a registry tag, such as main" >&2; usage; }
[[ $ATTEMPTS =~ ^[1-9]$ ]] || { echo "error: --attempts: 1 to 9" >&2; usage; }
: "${GH_TOKEN:?}" "${RUNNER_TEMP:?}" "${GITHUB_ACTOR:?}"

fail() { echo "::warning::$DEST is not mirrored: $*"; exit 1; }
acr=${SOURCE%%/*}
auth=$(mktemp "$RUNNER_TEMP/mirror-auth.XXXXXX")
trap 'rm -f "$auth"' EXIT

command -v skopeo > /dev/null || { sudo apt-get update -q && sudo apt-get install -y -q skopeo; } ||
  fail "can't install skopeo"
az acr login --name "${acr%%.*}" --expose-token --query accessToken -o tsv |
  skopeo login --authfile "$auth" "$acr" -u 00000000-0000-0000-0000-000000000000 --password-stdin ||
  fail "can't sign in to $acr"
printf '%s' "$GH_TOKEN" | skopeo login --authfile "$auth" ghcr.io -u "$GITHUB_ACTOR" --password-stdin ||
  fail "can't sign in to ghcr.io"

# The digest of the manifest a reference names, as a registry would compute it.
digest_of() { echo "sha256:$(skopeo inspect --authfile "$auth" --raw "docker://$1" | sha256sum | cut -d' ' -f1)"; }

copy() {
  local tag=$1 try
  for ((try = 1; try <= ATTEMPTS; try++)); do
    if skopeo copy --all --preserve-digests --retry-times 2 --authfile "$auth" \
      "docker://$SOURCE@$DIGEST" "docker://$DEST:$tag" &&
      [ "$(digest_of "$DEST:$tag")" = "$DIGEST" ]; then
      echo "$DEST:$tag is $DIGEST"
      return 0
    fi
    [ "$try" -eq "$ATTEMPTS" ] || sleep $((try * 15))
  done
  return 1
}

copy "$COMMIT" || fail "can't copy $DIGEST to the tag $COMMIT"
copy "$TAG" || fail "can't copy $DIGEST to the tag $TAG"
