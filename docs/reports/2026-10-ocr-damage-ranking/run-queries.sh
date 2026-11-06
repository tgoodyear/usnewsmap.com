#!/usr/bin/env bash
# Run queries/*.sql one at a time against usnm_quality and write each result to --out-dir,
# named after the query without its number. Every query is read-only and sets that itself.
# Needs psql and a libpq service (pg_service.conf) for a role that can read schema quality:
# --service NAME, and --service-file FILE if the service isn't in the default service file.
# The connection settings stay out of the command line.
set -euo pipefail
service="" service_file="" out_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --service) service="$2"; shift 2 ;;
    --service-file) service_file="$2"; shift 2 ;;
    --out-dir) out_dir="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$service" ] && [ -n "$out_dir" ] || { echo "usage: run-queries.sh --service NAME [--service-file FILE] --out-dir DIR" >&2; exit 2; }
export PGSERVICE="$service"
[ -z "$service_file" ] || export PGSERVICEFILE="$service_file"
here="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$out_dir"
for sql in "$here"/queries/*.sql; do
  name="$(basename "$sql" .sql)"
  LC_ALL=C.UTF-8 psql -q -v ON_ERROR_STOP=1 -f "$sql" > "$out_dir/${name#*-}.csv"
done
