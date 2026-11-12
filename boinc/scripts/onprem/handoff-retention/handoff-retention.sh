#!/usr/bin/env bash
# The hand-off retention step on the BOINC host (CT 230 on pve): frees the held `pages` copy
# of each OCR bundle that nothing reads any more, with handoff_retention.py prune-bundles.
# Journalled, bounded, and a dry run with --what-if.
#
#   handoff-retention.sh [--root /srv/usnm-boinc/project/handoff]
#     [--code /opt/usnm-handoff-retention] [--state /var/lib/usnm-handoff-retention]
#     [--min-age-hours 2] [--max-bundles 5000] [--max-bytes 200000000000] [--what-if]
#     [--confirmed /srv/usnm-boinc/project/handoff-scans/scan-confirmed/confirmed-sha256.txt]
#
# <code> is root's: handoff_retention.py, this script and COMMIT (the commit they were
# installed from). <state> holds the journal. --confirmed is the list scan_confirm.py writes on the
# storage host (usnm-scan-confirm.timer) of the scans that still have another copy: a bundle's
# pages are freed only when every scan in them is on it.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
ROOT=/srv/usnm-boinc/project/handoff CODE=/opt/usnm-handoff-retention STATE=/var/lib/usnm-handoff-retention
MIN_AGE=2 MAX_BUNDLES=5000 MAX_BYTES=200000000000 WHAT_IF=false
CONFIRMED=/srv/usnm-boinc/project/handoff-scans/scan-confirmed/confirmed-sha256.txt
while [ $# -gt 0 ]; do
  case $1 in
    --root) ROOT=${2:-}; shift 2 ;;
    --code) CODE=${2:-}; shift 2 ;;
    --state) STATE=${2:-}; shift 2 ;;
    --min-age-hours) MIN_AGE=${2:-}; shift 2 ;;
    --max-bundles) MAX_BUNDLES=${2:-}; shift 2 ;;
    --max-bytes) MAX_BYTES=${2:-}; shift 2 ;;
    --confirmed) CONFIRMED=${2:-}; shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $MIN_AGE =~ ^[0-9]+(\.[0-9]+)?$ && $MAX_BUNDLES =~ ^[0-9]+$ && $MAX_BYTES =~ ^[0-9]+$ ]] ||
  die "--min-age-hours, --max-bundles and --max-bytes: numbers"
script=$CODE/handoff_retention.py
[ -f "$script" ] || die "$script is not installed"
[ "$(stat -c %u "$CODE" "$script")" = "$(printf '0\n0')" ] || die "$CODE and $script must be root's"
commit=$(cat "$CODE/COMMIT" 2> /dev/null || echo unknown)
install -d -m 0700 "$STATE"
args=(prune-bundles --root "$ROOT" --journal "$STATE/journal.jsonl" --commit "$commit" --confirmed "$CONFIRMED"
  --min-age-hours "$MIN_AGE" --max-bundles "$MAX_BUNDLES" --max-bytes "$MAX_BYTES")
[ "$WHAT_IF" = true ] || args+=(--delete)
exec python3 -I "$script" "${args[@]}"
