#!/usr/bin/env bash
# Make sure the registry ACR has the Quickwit image environment ENVIRONMENT
# runs, pinned in infra/quickwit-image.json (ADR-0013, 08 §8.6), and write it
# as `image=<registry>/<name>@<digest>` to GITHUB_OUTPUT, for the ingest
# image's base.
#
# - An environment the file lists runs our build, which only
#   scripts/build-quickwit.sh pushes: if the registry lacks that digest,
#   fail and say so.
# - Any other runs upstream's image, copied in with its digest unchanged. A
#   registry that already has the digest is left alone, so a deploy doesn't
#   pull from Docker Hub (whose rate limit failed publishes in October 2026).
#   A copy reads from Google's Docker Hub mirror first and from Docker Hub if
#   the mirror lacks it.
set -euo pipefail
: "${ACR:?}" "${ENVIRONMENT:?}"
pins=infra/quickwit-image.json
pin=$(jq -c --arg env "$ENVIRONMENT" '.environments[$env] // empty' "$pins")
ours=true
[ -n "$pin" ] || { ours=false; pin=$(jq -c .upstream "$pins"); }
name=$(jq -r .image <<< "$pin")
tag=$(jq -r .tag <<< "$pin")
want=$(jq -r .digest <<< "$pin")
[[ $want =~ ^sha256:[0-9a-f]{64}$ ]] || { echo "::error::bad digest in $pins: $want"; exit 1; }
echo "image=${ACR}/${name}@${want}" >> "${GITHUB_OUTPUT:-/dev/stdout}"

if [ "$ours" = true ]; then
  got=$(az acr repository show -n "${ACR%%.*}" --image "${name}@${want}" --query digest -o tsv 2> /dev/null || true)
  if [ "$got" != "$want" ]; then
    echo "::error::${ACR} has no ${name}@${want}, the Quickwit build $pins records for $ENVIRONMENT. Build it there (scripts/build-quickwit.sh --env $ENVIRONMENT, then commit the digest it records) or drop $ENVIRONMENT from the file for upstream's image."
    exit 1
  fi
  echo "The registry has our Quickwit build ${name}:${tag}@${want}."
  exit 0
fi

command -v skopeo > /dev/null || { sudo apt-get update -q && sudo apt-get install -y -q skopeo; }
# skopeo gets its own credentials file: `az acr login` leaves an identity token
# in Docker's, which `skopeo login` refuses to log in over.
auth="$RUNNER_TEMP/acr-auth.json"
az acr login --name "${ACR%%.*}" --expose-token --query accessToken -o tsv |
  skopeo login --authfile "$auth" "$ACR" -u 00000000-0000-0000-0000-000000000000 --password-stdin
dest="docker://${ACR}/${name}:${tag}"
src_ref="${name}@${want}"
digest() { echo "sha256:$(skopeo inspect --authfile "$auth" --raw "$1" | sha256sum | cut -d' ' -f1)"; }

if [ "$(digest "docker://${ACR}/${name}@${want}" 2> /dev/null || true)" = "$want" ]; then
  # The app and jobs pull it by digest, so the tag doesn't matter here.
  echo "The registry already has ${src_ref}; not copying."
  exit 0
fi

copied=
for src in "mirror.gcr.io/${src_ref}" "docker.io/${src_ref}"; do
  if skopeo copy --all --preserve-digests --dest-authfile "$auth" "docker://${src}" "$dest"; then
    copied=$src
    break
  fi
  echo "::warning::could not copy Quickwit from ${src}"
done
test -n "$copied" || { echo "::error::could not copy ${src_ref} from the mirror or Docker Hub"; exit 1; }
echo "Copied ${src_ref} from ${copied%%/*}."

got="$(digest "$dest")"
test "$got" = "$want" || { echo "::error::copied Quickwit digest $got != $want"; exit 1; }
