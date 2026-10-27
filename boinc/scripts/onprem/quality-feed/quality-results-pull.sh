#!/usr/bin/env bash
# Copies a quality campaign's collected results from the BOINC server CT230 into the batch
# container on pve-laptop, for scripts/onprem/quality/load.py (#962). Runs on pve-laptop as
# root over the host trust; host to host, nothing passes through a workstation.
#
#   quality-results-pull.sh --campaign NAME [--server-host pve.tjg.fyi] [--server-ct 230]
#     [--ctid 215]
#
# Takes <share>/quality/<campaign>/out/pages.parquet (quality collect's output) and
# <campaign>/collected/*.parquet (the wave runner's parts), whichever exist, into
# /var/lib/usnm-quality-feed/results/<campaign>/ in the container, and prints the files.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
CAMPAIGN="" SERVER=pve.tjg.fyi SCT=230 CTID=215
while [ $# -gt 0 ]; do
  case $1 in
    --campaign) CAMPAIGN=${2:-}; shift 2 ;;
    --server-host) SERVER=${2:-}; shift 2 ;;
    --server-ct) SCT=${2:-}; shift 2 ;;
    --ctid) CTID=${2:-}; shift 2 ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $CAMPAIGN =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || die "--campaign: 1-41 of [a-z0-9-]"
[[ $SERVER =~ ^[a-z0-9][a-z0-9.-]*$ && $SCT =~ ^[0-9]+$ && $CTID =~ ^[0-9]+$ ]] || die "bad --server-host, --server-ct or --ctid"
dest=/var/lib/usnm-quality-feed/results/$CAMPAIGN
pct exec "$CTID" -- install -d "$dest"
# shellcheck disable=SC2029 # the paths are meant to expand on the far side
ssh -o BatchMode=yes "root@$SERVER" "pct exec $SCT -- bash -c 'cd /srv/usnm-boinc/project/quality/$CAMPAIGN && \
  ls out/pages.parquet collected/*.parquet 2> /dev/null | tar -cf - -T -'" |
  pct exec "$CTID" -- tar -x --no-same-owner -C "$dest"
pct exec "$CTID" -- find "$dest" -name '*.parquet' | sort
