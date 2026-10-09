#!/usr/bin/env bash
# Copy the pinned Quickwit image (QUICKWIT, name@digest) into the registry ACR
# with its digest unchanged (08 §8.6), and check the digest. A registry that
# already has the digest is left alone, so a deploy doesn't pull from Docker
# Hub (whose rate limit failed publishes in October 2026). A copy reads from
# Google's Docker Hub mirror first and from Docker Hub if the mirror lacks it.
set -euo pipefail
: "${ACR:?}" "${QUICKWIT:?}"
command -v skopeo > /dev/null || { sudo apt-get update -q && sudo apt-get install -y -q skopeo; }
# skopeo gets its own credentials file: `az acr login` leaves an identity token
# in Docker's, which `skopeo login` refuses to log in over.
auth="$RUNNER_TEMP/acr-auth.json"
az acr login --name "${ACR%%.*}" --expose-token --query accessToken -o tsv |
  skopeo login --authfile "$auth" "$ACR" -u 00000000-0000-0000-0000-000000000000 --password-stdin
dest="docker://${ACR}/quickwit/quickwit:v0.9.1"
want="${QUICKWIT#*@}"
digest() { echo "sha256:$(skopeo inspect --authfile "$auth" --raw "$1" | sha256sum | cut -d' ' -f1)"; }

if [ "$(digest "docker://${ACR}/quickwit/quickwit@${want}" 2> /dev/null || true)" = "$want" ]; then
  # The app and jobs pull it by digest, so the tag doesn't matter here.
  echo "The registry already has ${QUICKWIT}; not copying."
  exit 0
fi

copied=
for src in "mirror.gcr.io/${QUICKWIT}" "docker.io/${QUICKWIT}"; do
  if skopeo copy --all --preserve-digests --dest-authfile "$auth" "docker://${src}" "$dest"; then
    copied=$src
    break
  fi
  echo "::warning::could not copy Quickwit from ${src}"
done
test -n "$copied" || { echo "::error::could not copy ${QUICKWIT} from the mirror or Docker Hub"; exit 1; }
echo "Copied ${QUICKWIT} from ${copied%%/*}."

got="$(digest "$dest")"
test "$got" = "$want" || { echo "::error::copied Quickwit digest $got != $want"; exit 1; }
