#!/usr/bin/env bash
# Copies a quality campaign's collected results from the BOINC server CT230 into the batch
# container on pve-laptop, for scripts/onprem/quality/load.py (#962). Runs on pve-laptop as
# root over the host trust; host to host, nothing passes through a workstation.
#
#   quality-results-pull.sh --campaign NAME [--server-host pve.tjg.fyi] [--server-ct 230]
#     [--ctid N|local] [--snapshot-only] [--snapshot-id <32 lowercase hex>]
#
# Takes <share>/quality/<campaign>/out/pages.parquet (quality collect's output) and
# <campaign>/collected/*.parquet (the wave runner's parts), whichever exist, into
# /var/lib/usnm-quality-feed/results/<campaign>/ in the container, and prints the files.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
# Runs a command in the batch container, or directly when CTID is `local` (already inside it).
ct_run() {
  if [ "$CTID" = local ]; then "$@"; else pct exec "$CTID" -- "$@"; fi
}
CAMPAIGN="" SERVER=${SERVER_HOST:-} SCT=${SERVER_CT:-} CTID=${CTID:-} SNAPSHOT=false SNAPSHOT_ID=""
while [ $# -gt 0 ]; do
  case $1 in
    --campaign) CAMPAIGN=${2:-}; shift 2 ;;
    --server-host) SERVER=${2:-}; shift 2 ;;
    --server-ct) SCT=${2:-}; shift 2 ;;
    --ctid) CTID=${2:-}; shift 2 ;;
    --snapshot-only) SNAPSHOT=true; shift ;;
    --snapshot-id) SNAPSHOT_ID=${2:-}; shift 2 ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -n "$SERVER" ] || die "set --server-host or SERVER_HOST"
[ -n "$SCT" ] || die "set --server-ct or SERVER_CT"
[ -n "$CTID" ] || die "set --ctid or CTID"
[[ $CAMPAIGN =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || die "--campaign: 1-41 of [a-z0-9-]"
[[ $SERVER =~ ^[a-z0-9][a-z0-9.-]*$ && $SCT =~ ^[0-9]+$ && ( $CTID =~ ^[0-9]+$ || $CTID = local ) ]] || die "bad --server-host, --server-ct or --ctid"
dest=/var/lib/usnm-quality-feed/results/$CAMPAIGN
if [ -n "$SNAPSHOT_ID" ]; then
  [[ $SNAPSHOT_ID =~ ^[0-9a-f]{32}$ ]] || die "--snapshot-id: 32 lowercase hex"
  dest=$dest/snapshots/$SNAPSHOT_ID
fi
ct_run install -d "$dest"
if $SNAPSHOT; then
  list='test -f out/pages.parquet; printf "%s\n" out/pages.parquet'
else
  list='for p in out/pages.parquet collected/*.parquet; do if [ -f "$p" ]; then printf "%s\n" "$p"; fi; done'
fi
# shellcheck disable=SC2029 # validated campaign, ids and fixed commands expand remotely
ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$SERVER" "pct exec $SCT -- bash -c 'set -euo pipefail; cd /srv/usnm-boinc/project/quality/$CAMPAIGN; \
  { $list; } | tar -cf - -T -'" |
  ct_run timeout --signal=TERM --kill-after=10s 4m tar -x --no-same-owner -C "$dest"
ct_run find "$dest" -name '*.parquet' | sort
