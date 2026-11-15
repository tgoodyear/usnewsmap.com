#!/bin/bash
# The qe-loc-v1 pipeline, once an hour (usnm-qe-loc-pipeline.timer), on pve-big as root, with no
# agent involved. Per run, in order:
#   1. stop when paused: from 18 Nov 9:00 AM ET (14:00 UTC) until STATE/rebuild-done exists;
#   2. collect returned units into quality.qe_loc_scores (3% spot comparisons and the resends
#      after a failed host happen inside qe-text-collect), prune old collected units;
#   3. ship newly extracted units (and any pruned unit a resend needs again) from STATE/units
#      to CT 230, node to node over the host trust pve-big -> pve; a shipped unit gets a marker
#      in STATE/shipped;
#   4. submit at a paced rate.
# One summary line goes to the journal (shipped, submitted, collected, rows, spot pass/fail,
# invalid hosts). The extraction runner (qe_loc_extract.sh) is separate and keeps running.
#
# Pacing defaults, sized to CT 230 (a NUC; its uplink serves every app):
#   --max-inflight 200      results unsent or in progress at once. A unit is up to 5000 pages,
#                           about 60 to 125 MB, sent once (quorum 1) plus 3% to a second host:
#                           200 in flight is about 25 GB at most outstanding.
#   --submit-per-run 60     new units an hour: about 7.5 GB an hour, 17 Mbit/s on average.
#   --max-staged 120        shipped units waiting to be submitted, so the share holds about two
#                           runs' worth ahead and no more.
#   --min-free-gb 150       ships and submits nothing while CT 230's project share has less.
#   --prune-days 14         a collected unit's files leave CT 230 after this (pve-big keeps them).
# Usage: qe_loc_pipeline.sh [--state DIR] [--ssh-host H] [--ct N] [--max-inflight N]
#   [--submit-per-run N] [--max-staged N] [--min-free-gb N] [--prune-days N]
# For tests: QE_LOC_SSH (the ssh command) and QE_LOC_NOW (epoch seconds) replace ssh and date.
set -uo pipefail
state=/tank/locstats/qeloc/state; host=pve.tjg.fyi; ct=230
max_inflight=200; per_run=60; max_staged=120; min_free=150; prune_days=14
while [ $# -gt 0 ]; do case "$1" in
  --state) state=$2; shift 2;; --ssh-host) host=$2; shift 2;; --ct) ct=$2; shift 2;;
  --max-inflight) max_inflight=$2; shift 2;; --submit-per-run) per_run=$2; shift 2;;
  --max-staged) max_staged=$2; shift 2;; --min-free-gb) min_free=$2; shift 2;;
  --prune-days) prune_days=$2; shift 2;; *) echo "unknown $1" >&2; exit 2;; esac; done
ssh_cmd=${QE_LOC_SSH:-ssh}
now=${QE_LOC_NOW:-$(date -u +%s)}
say() { echo "qe-loc-pipeline: $*"; logger -t qe-loc-pipeline "$*" 2>/dev/null || true; }
srv() { $ssh_cmd -o BatchMode=yes -o ConnectTimeout=20 "$host" "pct exec $ct -- /usr/local/sbin/qe-loc-server $*"; }
jget() { python3 -c 'import json,sys; d=json.loads(sys.stdin.read().strip().splitlines()[-1]); print(d.get(sys.argv[1], 0) if not isinstance(d.get(sys.argv[1], 0), list) else len(d[sys.argv[1]]))' "$1"; }

mkdir -p "$state/shipped"
exec 9> "$state/pipeline.lock"
flock -n 9 || { say "another run is going; nothing done"; exit 0; }
if [ "$now" -ge "$(date -u -d 2026-11-18T14:00:00Z +%s)" ] && [ ! -e "$state/rebuild-done" ]; then
  say "paused for the rebuild (until $state/rebuild-done exists)"; exit 0
fi

status=$(srv --command status 2>&1) || { say "failed: status: $(echo "$status" | tail -1)"; exit 1; }
val() { echo "$status" | sed -n "s/^$1=//p" | head -1; }
inflight=$(val inflight); staged=$(val staged); free=$(val free_gb); need=$(val need)
inflight=${inflight:-0}; staged=${staged:-0}; free=${free:-0}

collected=0; rows=0; spot_pass=0; spot_fail=0; bad=0; resent=0
out=$(srv --command collect 2>&1) && {
  collected=$(echo "$out" | jget units_collected); rows=$(echo "$out" | jget rows_new)
  spot_pass=$(echo "$out" | jget spot_checked); bad=$(echo "$out" | jget hosts_failed)
  resent=$(echo "$out" | jget units_resent); spot_fail=$((bad / 2))
} || say "collect failed: $(echo "$out" | tail -1)"
srv --command prune --days "$prune_days" > /dev/null 2>&1 || true

shipped=0; submitted=0
if [ "$free" -lt "$min_free" ]; then
  say "CT $ct has $free GB free (under $min_free); nothing shipped or submitted"
else
  # Units a resend needs again come first, then new ones in name order.
  want=$((max_staged - staged)); [ "$want" -lt 0 ] && want=0
  list=""; n=0
  for id in $need; do
    [ "$n" -ge "$want" ] && break
    [ -s "$state/units/$id/qe_text.json" ] && { list="$list $id"; n=$((n+1)); }
  done
  for d in "$state"/units/*/; do
    [ "$n" -ge "$want" ] && break
    id=$(basename "$d")
    [ -s "$d/qe_text.json" ] && [ ! -e "$state/shipped/$id" ] && { list="$list $id"; n=$((n+1)); }
  done
  if [ -n "$list" ]; then
    # --no-same-owner: the units belong to the extraction user on this side.
    if tar -C "$state/units" -c $list | $ssh_cmd -o BatchMode=yes "$host" "pct exec $ct -- tar -x --no-same-owner -C /srv/usnm-boinc/project/qetext/units"; then
      for id in $list; do : > "$state/shipped/$id"; shipped=$((shipped+1)); done
    else
      say "shipping failed; the units stay unshipped"
    fi
  fi
  room=$((max_inflight - inflight)); [ "$room" -lt 0 ] && room=0
  take=$per_run; [ "$room" -lt "$take" ] && take=$room
  if [ "$take" -gt 0 ]; then
    out=$(srv --command submit --count "$take" 2>&1) && submitted=$(echo "$out" | jget submitted) \
      || say "submit failed: $(echo "$out" | tail -1)"
  fi
fi
say "shipped=$shipped submitted=$submitted collected=$collected rows=$rows spot_pass=$spot_pass spot_fail=$spot_fail invalid_hosts=$bad resent=$resent inflight=$inflight staged=$staged free_gb=$free"
