#!/usr/bin/env bash
set -euo pipefail
# make_project copied this tool once; refresh only it before the preserved startup.
if [ "${1:-}" = --serve ]; then
  # shellcheck source=lib.sh
  . /usr/local/lib/usnm-boinc/lib.sh
  python3 "$TEMPLATES/refresh-scheduler.py" \
    --source "$BOINC_SRC/tools/create_work" --target "$PROJ/bin/create_work"
fi
exec /usr/local/lib/usnm-boinc/entrypoint-before-amd-885.sh "$@"
