#!/usr/bin/env bash
# The daily BOINC page provenance run (#1956): resolves the published BOINC pages that the table
# does not hold as `resolved` yet (new pages, and pages that were unresolved) and upserts them.
# In the batch container (CT 215 on pve-laptop), as root, from usnm-quality-provenance.timer.
#
#   provenance-daily.sh [--env-file /etc/usnm/quality-provenance.env]
#
# Runs the resolver with the feed's virtualenv (pyarrow), /opt/usnm-quality-feed/venv.
#
# Needs, in the env file:
#   PAGES_DIR      the curated copy of ocr-local/pages (default /srv/curated/ocr-local/pages; not
#                  /srv/curated/pages, which is LoC's text). That mount is a stale mirror on the
#                  batch container, so set CURATED_URL too.
#   CURATED_URL    the lake's curated container (https://<account>.blob.core.windows.net/curated).
#                  When set, each run first brings PAGES_DIR up to date from the lake's
#                  ocr-local/pages/ with sync_curated_pages.py: read-only, as the host's Arc
#                  identity (needs IDENTITY_ENDPOINT and root or himds; the service sets them)
#   BOINC_EXPORT   a command that writes the BOINC database export into the directory given by
#                  --out-dir: wu.tsv (the workunit query of boinc_provenance.py), av.tsv
#                  and bundles/ (the bundle request files). It reads BOINC read-only, host to
#                  host; nothing passes through a workstation.
#   QUALITY_DSN    the quality_loader DSN file (default /etc/usnm/quality-db/dsn)
#
# Readings already resolved, or holding candidate hosts of a job that ran twice, are left alone: a
# later run, after BOINC purged one workunit, must not replace that evidence.
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
if [ -r "$ENV" ]; then set -a; . "$ENV"; set +a; fi
PAGES_DIR=${PAGES_DIR:-/srv/curated/ocr-local/pages}
QUALITY_DSN=${QUALITY_DSN:-/etc/usnm/quality-db/dsn}
HERE=$(cd "$(dirname "$0")" && pwd)
CURATED_URL=${CURATED_URL:-}
[ -n "${BOINC_EXPORT:-}" ] || die "BOINC_EXPORT is not set in $ENV"
if [ -n "$CURATED_URL" ]; then
  mkdir -p "$PAGES_DIR"
  /opt/usnm-quality-feed/venv/bin/python "$HERE/sync_curated_pages.py" --url "$CURATED_URL" --to "$PAGES_DIR"
fi
[ -d "$PAGES_DIR" ] || die "no curated pages at $PAGES_DIR"
work=$(mktemp -d /var/tmp/usnm-provenance.XXXXXX)
trap 'rm -rf "$work"' EXIT
psql "$(cat "$QUALITY_DSN")" -v ON_ERROR_STOP=1 -At -F "$(printf '\t')" \
  -c "SELECT doc_id, node FROM quality.boinc_page_provenance WHERE status = 'resolved' OR candidate_host_ids IS NOT NULL" > "$work/skip.tsv"
"$BOINC_EXPORT" --out-dir "$work"
[ -s "$work/wu.tsv" ] && [ -s "$work/av.tsv" ] || die "the BOINC export wrote no wu.tsv or av.tsv"
/opt/usnm-quality-feed/venv/bin/python "$HERE/boinc_provenance.py" --pages "$PAGES_DIR" --wu-tsv "$work/wu.tsv" --av-tsv "$work/av.tsv" \
  --bundles "$work/bundles" --skip "$work/skip.tsv" --out "$work/provenance.tsv"
if [ -s "$work/provenance.tsv" ]; then
  psql "$(cat "$QUALITY_DSN")" -v ON_ERROR_STOP=1 -q -f "$HERE/provenance_load.sql" < "$work/provenance.tsv"
else
  echo "nothing new to resolve"
fi
