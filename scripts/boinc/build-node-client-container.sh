#!/usr/bin/env bash
# Build away from the OCR host. Export only a pinned EL9-compatible client bundle.
set -euo pipefail
out=""
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out=${2:?}; shift 2 ;;
    *) echo "usage: $0 --out ABSOLUTE_NEW_DIRECTORY" >&2; exit 2 ;;
  esac
done
[[ $out =~ ^/[A-Za-z0-9_./-]+$ && $out != *..* && ! -e $out ]] || {
  echo "--out must be a new plain absolute directory" >&2; exit 2;
}
cd "$(dirname "${BASH_SOURCE[0]}")/../.."
name="usnm-node-client-$(python3 -c 'import uuid; print(uuid.uuid4().hex)')"
trap 'docker rm -f "$name" >/dev/null 2>&1 || true' EXIT
docker build --platform linux/amd64 --memory 1536m \
  -f scripts/boinc/NodeClientContainerfile -t "$name" .
docker create --name "$name" "$name" /bin/true >/dev/null
mkdir -p "$out"
docker cp "$name:/build/install" "$out/install"
docker cp "$name:/build/source-commit" "$out/source-commit"
docker cp "$name:/build/SHA256SUMS" "$out/build-SHA256SUMS"
docker image inspect "$name" --format '{{.Id}}' > "$out/build-image-id"
python3 - "$out" <<'PY'
import hashlib
from pathlib import Path
import sys
root = Path(sys.argv[1])
with (root / "SHA256SUMS").open("x") as stream:
    for relative in ("install/bin/boinc", "install/bin/boinccmd"):
        stream.write(hashlib.sha256((root / relative).read_bytes()).hexdigest() + "  " + relative + "\n")
PY
echo "Exported stock EL9-built 8.2.13 to $out; no host install/service changes."
