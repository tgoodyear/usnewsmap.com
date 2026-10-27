#!/usr/bin/env bash
set -euo pipefail
if [ "$#" -ne 0 ]; then
  echo "usage: boinc/scripts/check-db-tls.sh (local disposable fixture; no Azure)" >&2
  exit 2
fi
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
docker info >/dev/null
tag="usnm-boinc-db-tls:$(git rev-parse --short HEAD)"
docker build --memory 1536m -f boinc/scripts/db-tls/Containerfile -t "$tag" .
name="usnm-db-tls-$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
docker run --rm --name "$name" --network none --read-only --cpus 2 --memory 1g --pids-limit 128 \
  --cap-drop ALL --security-opt no-new-privileges \
  --tmpfs /fixture:rw,nosuid,size=512m \
  --tmpfs /usr/local/share/usnm:rw,nosuid,noexec,size=1m \
  --add-host tls-db.test:127.0.0.1 --add-host wrong-db.test:127.0.0.1 "$tag"
