#!/usr/bin/env bash
# BOINC_EXPORT for provenance-daily.sh (#1956): unpacks the BOINC export that
# provenance-export-fetch.sh left on the batch container's outbox mount into the directory given
# as the first argument (wu.tsv, av.tsv, bundles/). It reads that file only: no BOINC access.
#
#   provenance-export-unpack.sh DIR
#
# EXPORT (default /var/lib/usnm-quality-feed/outbox/provenance/export.tar.gz) must be younger than
# MAX_AGE_HOURS (default 36), so a fetch that stopped is an error here, not a silent stale run.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
[ $# -eq 1 ] && [ -d "$1" ] || die "usage: provenance-export-unpack.sh DIR"
EXPORT=${EXPORT:-/var/lib/usnm-quality-feed/outbox/provenance/export.tar.gz}
MAX_AGE_HOURS=${MAX_AGE_HOURS:-36}
[ -s "$EXPORT" ] || die "no BOINC export at $EXPORT"
age=$(( ($(date +%s) - $(stat -c %Y "$EXPORT")) / 3600 ))
[ "$age" -lt "$MAX_AGE_HOURS" ] || die "the BOINC export is ${age} h old (limit ${MAX_AGE_HOURS} h): is provenance-export-fetch.sh failing on pve-laptop?"
tar -xzf "$EXPORT" -C "$1"
