#!/usr/bin/env bash
# The quality retention step on the storage host (pve-big): lists finished usnm_quality
# workunits from the BOINC server (CT 230 on pve, read-only DB) and frees their download
# copies with quality_retention.py prune, as the share's owner, never root.
#
#   quality-retention.sh [--server-host pve.tjg.fyi] [--server-ct 230]
#     [--code /opt/usnm-quality-retention] [--state /var/lib/usnm-quality-retention]
#     [--share /tank/boinc-bulk]
#     [--owner 101000]
#     [--max-bytes 400000000000] [--unit-campaign NAME] [--unit-max-bytes 400000000000]
#     [--what-if]
#
# <code> is root's: quality_retention.py, this script, COMMIT (the commit they were installed
# from) and run/eligible.jsonl, the last list. Root writes nothing anywhere else. <state>
# belongs to the share's owner and holds only the journal, which prune writes as that owner.
# --what-if runs prune without --delete. --unit-campaign NAME also runs prune-units for that
# campaign (the units' own page.jpg, #1743) after prune, as the same owner. The partial loader's
# state (inventory.json, frontier.json) is on the server's own disk, so root copies it read-only
# from the server container's host into <code>/run/results/NAME first, as it does the list.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
SERVER=pve.tjg.fyi CT=230 CODE=/opt/usnm-quality-retention STATE=/var/lib/usnm-quality-retention SHARE=/tank/boinc-bulk
OWNER=101000
MAX_BYTES=400000000000 WHAT_IF=false UNIT_CAMPAIGN="" UNIT_MAX_BYTES=400000000000
while [ $# -gt 0 ]; do
  case $1 in
    --server-host) SERVER=${2:-}; shift 2 ;;
    --server-ct) CT=${2:-}; shift 2 ;;
    --code) CODE=${2:-}; shift 2 ;;
    --state) STATE=${2:-}; shift 2 ;;
    --share) SHARE=${2:-}; shift 2 ;;
    --owner) OWNER=${2:-}; shift 2 ;;
    --max-bytes) MAX_BYTES=${2:-}; shift 2 ;;
    --unit-campaign) UNIT_CAMPAIGN=${2:-}; shift 2 ;;
    --unit-max-bytes) UNIT_MAX_BYTES=${2:-}; shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $SERVER =~ ^[a-z0-9][a-z0-9.-]*$ ]] || die "--server-host: a host name"
[[ $CT =~ ^[0-9]+$ && $OWNER =~ ^[0-9]+$ && $MAX_BYTES =~ ^[0-9]+$ && $UNIT_MAX_BYTES =~ ^[0-9]+$ ]] ||
  die "--server-ct, --owner, --max-bytes and --unit-max-bytes: numbers"
[[ -z $UNIT_CAMPAIGN || $UNIT_CAMPAIGN =~ ^[A-Za-z0-9][A-Za-z0-9_.-]*$ ]] ||
  die "--unit-campaign: a campaign name"
script=$CODE/quality_retention.py
[ -f "$script" ] || die "$script is not installed"
[ "$(stat -c %u "$CODE" "$script")" = "$(printf '0\n0')" ] || die "$CODE and $script must be root's"
commit=$(cat "$CODE/COMMIT" 2>/dev/null || echo unknown)
install -d -m 0755 "$CODE/run"
list=$CODE/run/eligible.jsonl

# The list runs in the server container from this host's copy of the script, read-only.
ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$SERVER" \
  "pct exec $CT -- podman exec -i usnm-boinc-server python3 -I - list --out -" \
  < "$script" > "$list.tmp"
chmod 0644 "$list.tmp"
mv "$list.tmp" "$list"

args=(prune --eligible "$list" --download "$SHARE/download"
  --quality-root "$SHARE/quality" --journal "$STATE/journal.jsonl"
  --commit "$commit" --max-bytes "$MAX_BYTES")
[ "$WHAT_IF" = true ] || args+=(--delete)
cd "$STATE"
as_owner() { setpriv --reuid "$OWNER" --regid "$OWNER" --clear-groups python3 -I "$script" "$@"; }
as_owner "${args[@]}"
[ -n "$UNIT_CAMPAIGN" ] || exit 0
results=$CODE/run/results/$UNIT_CAMPAIGN
install -d -m 0755 "$results"
for f in inventory.json frontier.json; do
  ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$SERVER" \
    "pct exec $CT -- cat /srv/usnm-boinc/project/ops/quality-results/scoped/$UNIT_CAMPAIGN/$f" \
    > "$results/$f.tmp"
  chmod 0644 "$results/$f.tmp"
  mv "$results/$f.tmp" "$results/$f"
done
uargs=(prune-units --quality-root "$SHARE/quality" --campaign "$UNIT_CAMPAIGN"
  --results-root "$CODE/run/results" --journal "$STATE/journal.jsonl"
  --commit "$commit" --max-bytes "$UNIT_MAX_BYTES")
[ "$WHAT_IF" = true ] || uargs+=(--delete)
as_owner "${uargs[@]}"
