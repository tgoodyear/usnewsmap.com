#!/usr/bin/env bash
# Run queries/*.sql one at a time against usnm_quality and write each result to --out-dir,
# named after the query without its number. Every query is read-only and sets that itself.
# Needs psql, and a libpq DSN in --dsn-file for a role that can read schema quality.
set -euo pipefail
dsn_file="" out_dir=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dsn-file) dsn_file="$2"; shift 2 ;;
    --out-dir) out_dir="$2"; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$dsn_file" ] && [ -n "$out_dir" ] || { echo "usage: run-queries.sh --dsn-file FILE --out-dir DIR" >&2; exit 2; }
here="$(cd "$(dirname "$0")" && pwd)"
dsn="$(cat "$dsn_file")"
for sql in "$here"/queries/*.sql; do
  name="$(basename "$sql" .sql)"
  LC_ALL=C.UTF-8 psql "$dsn" -q -v ON_ERROR_STOP=1 -f "$sql" > "$out_dir/${name#*-}.csv"
done
