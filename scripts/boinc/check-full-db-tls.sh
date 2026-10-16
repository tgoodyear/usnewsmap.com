#!/usr/bin/env bash
set -euo pipefail
image=""
local_image=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --server-image) image=${2:?}; shift 2 ;;
    --local-server-image) local_image=${2:?}; shift 2 ;;
    *) echo "usage: scripts/boinc/check-full-db-tls.sh --server-image IMAGE@sha256:DIGEST | --local-server-image LOCAL_TAG" >&2; exit 2 ;;
  esac
done
if [[ -n $local_image && -z $image ]]; then
  source_id=$(docker image inspect "$local_image" --format '{{.Id}}')
  [[ $source_id =~ ^sha256:[0-9a-f]{64}$ ]] || exit 2
elif [[ -z $local_image && $image =~ @sha256:[0-9a-f]{64}$ ]]; then
  source_id=$image
else
  echo "exactly one immutable published image or existing local image is required" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
tag="usnm-boinc-full-tls:$(git rev-parse --short HEAD)"
name="usnm-full-tls-$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
cleanup() {
  docker logs "$name-db" 2>/dev/null || true
  docker rm -f "$name-app" "$name-db" >/dev/null 2>&1 || true
  docker volume rm "$name-db" "$name-app" "$name-keys" >/dev/null 2>&1 || true
  if [[ -n $local_image ]]; then docker image rm "$name-source" >/dev/null 2>&1 || true; fi
}
trap cleanup EXIT
if [[ -n $local_image ]]; then
  docker tag "$source_id" "$name-source"
  image="$name-source"
fi
echo "full fixture server image: $source_id"
docker build --platform linux/amd64 --memory 1536m -f scripts/boinc/db-tls/FullContainerfile \
  --build-arg SERVER_IMAGE="$image" -t "$tag" .
# A native private DB avoids emulating InnoDB on ARM; neither container has
# ingress/egress. The reviewed BOINC runtime remains amd64 and unmodified.
docker build -f scripts/boinc/db-tls/Containerfile -t "$tag-db" .
docker volume create "$name-db" >/dev/null
docker volume create "$name-app" >/dev/null
docker volume create "$name-keys" >/dev/null
docker run -d --name "$name-db" --network none --read-only --cpus 1 --memory 512m --pids-limit 64 \
  --cap-drop ALL --security-opt no-new-privileges \
  --mount "type=volume,src=$name-db,dst=/fixture" \
  --tmpfs /usr/local/share/usnm:rw,nosuid,noexec,size=1m \
  --add-host tls-db.test:127.0.0.1 --add-host wrong-db.test:127.0.0.1 \
  --entrypoint python3 "$tag-db" /checks/dbservice.py >/dev/null
wait_db() {
  for _ in $(seq 1 120); do
    if docker exec "$name-db" test -f /fixture/ready; then return; fi
    [[ $(docker inspect "$name-db" --format '{{.State.Running}}') == true ]] || return 1
    sleep 1
  done
  echo "native private TLS DB readiness failed" >&2
  return 1
}
wait_db
for phase in init restart; do
  if [[ $phase == restart ]]; then
    docker exec "$name-db" rm /fixture/ready
    docker restart "$name-db" >/dev/null
    wait_db
  fi
  docker run --rm --name "$name-app" --platform linux/amd64 --network "container:$name-db" --read-only \
    --cpus 2 --memory 1024m --pids-limit 128 --shm-size 64m \
    --security-opt no-new-privileges \
    --env "ACCEPTANCE_SERVER_IMAGE=$source_id" \
    --mount "type=volume,src=$name-app,dst=/fixture" \
    --mount "type=volume,src=$name-keys,dst=/fixture-keys" \
    --mount "type=volume,src=$name-db,dst=/dbfixture,readonly" \
    --tmpfs /usr/local/share/usnm:rw,nosuid,noexec,size=1m "$tag" --phase "$phase"
done
