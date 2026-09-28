#!/usr/bin/env bash
# Stop what scripts/local-azure/up.sh started in WORKDIR. The data (Azurite
# blobs, cosmos.json) stays, so up.sh on the same WORKDIR picks it back up.
#
#   scripts/local-azure/down.sh workdir
set -euo pipefail

work="${1:?usage: down.sh workdir}"
[ -f "$work/pids" ] || { echo "no $work/pids" >&2; exit 1; }
while read -r pid; do
  kill "$pid" 2> /dev/null || true
done < "$work/pids"
rm "$work/pids"
