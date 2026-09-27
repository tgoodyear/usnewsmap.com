#!/usr/bin/env bash
# Copy the pinned Quickwit image (QUICKWIT, name@digest) into the registry ACR
# with its digest unchanged (08 §8.6), and check the digest.
set -euo pipefail
: "${ACR:?}" "${QUICKWIT:?}"
command -v skopeo > /dev/null || { sudo apt-get update -q && sudo apt-get install -y -q skopeo; }
# skopeo gets its own credentials file: `az acr login` leaves an identity token
# in Docker's, which `skopeo login` refuses to log in over.
auth="$RUNNER_TEMP/acr-auth.json"
az acr login --name "${ACR%%.*}" --expose-token --query accessToken -o tsv |
  skopeo login --authfile "$auth" "$ACR" -u 00000000-0000-0000-0000-000000000000 --password-stdin
dest="docker://${ACR}/quickwit/quickwit:v0.9.1"
skopeo copy --all --preserve-digests --dest-authfile "$auth" "docker://docker.io/${QUICKWIT}" "$dest"
want="${QUICKWIT#*@}"
got="sha256:$(skopeo inspect --authfile "$auth" --raw "$dest" | sha256sum | cut -d' ' -f1)"
test "$got" = "$want" || { echo "::error::copied Quickwit digest $got != $want"; exit 1; }
