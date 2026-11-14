#!/usr/bin/env bash
# The daily BOINC page provenance run (#1956): resolves the published BOINC pages that the table
# does not hold as `resolved` yet (new pages, and pages that were unresolved) and upserts them.
# In the batch container (CT 215 on pve-laptop), as root, from usnm-quality-provenance.timer.
#
#   provenance-daily.sh [--env-file /etc/usnm/quality-provenance.env]
#
# Needs, in the env file:
#   PAGES_DIR      the curated copy of ocr-local/pages (default /srv/curated/pages)
#   BOINC_EXPORT   a command that writes the BOINC database export into the directory given as
#                  its first argument: wu.tsv (the workunit query of boinc_provenance.py), av.tsv
#                  and bundles/ (the bundle request files). It reads BOINC read-only, host to
#                  host; nothing passes through a workstation.
#   QUALITY_DSN    the quality_loader DSN file (default /etc/usnm/quality-db/dsn)
#
# Why daily: db_purge removes a workunit's rows 30 days after it is done, and the host of a page
# is only in those rows. Each run only looks at pages not yet `resolved`, so it is cheap. It can
# drop to weekly (the timer's OnCalendar) once #1959 is deployed and verified, as long as it runs
# at least every 20 days.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
ENV=/etc/usnm/quality-provenance.env
while [ $# -gt 0 ]; do
  case $1 in
    --env-file) ENV=${2:-}; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -r "$ENV" ] && . "$ENV"
PAGES_DIR=${PAGES_DIR:-/srv/curated/pages}
QUALITY_DSN=${QUALITY_DSN:-/etc/usnm/quality-db/dsn}
HERE=$(cd "$(dirname "$0")" && pwd)
[ -n "${BOINC_EXPORT:-}" ] || die "BOINC_EXPORT is not set in $ENV"
[ -d "$PAGES_DIR" ] || die "no curated pages at $PAGES_DIR"
work=$(mktemp -d /var/tmp/usnm-provenance.XXXXXX)
trap 'rm -rf "$work"' EXIT
psql "$(cat "$QUALITY_DSN")" -v ON_ERROR_STOP=1 -At -F "$(printf '\t')" \
  -c "SELECT doc_id, node FROM quality.boinc_page_provenance WHERE status = 'resolved'" > "$work/skip.tsv"
"$BOINC_EXPORT" "$work"
[ -s "$work/wu.tsv" ] && [ -s "$work/av.tsv" ] || die "the BOINC export wrote no wu.tsv or av.tsv"
python3 "$HERE/boinc_provenance.py" --pages "$PAGES_DIR" --wu-tsv "$work/wu.tsv" --av-tsv "$work/av.tsv" \
  --bundles "$work/bundles" --skip "$work/skip.tsv" --out "$work/provenance.tsv"
if [ -s "$work/provenance.tsv" ]; then
  psql "$(cat "$QUALITY_DSN")" -v ON_ERROR_STOP=1 -q -f "$HERE/provenance_load.sql" < "$work/provenance.tsv"
else
  echo "nothing new to resolve"
fi
