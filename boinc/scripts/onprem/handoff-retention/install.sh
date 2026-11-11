#!/usr/bin/env bash
# Installs the hand-off retention step (handoff_retention.py) on the BOINC host's CT from a
# checkout. Run from the operator's machine; it copies only this directory's code. Safe to
# run again. Without --enable it installs the code and units and leaves the timer off.
#
#   install.sh [--host pve.tjg.fyi] [--ctid 230] [--enable]
#
# Before --enable, dry-run it in the CT: /opt/usnm-handoff-retention/handoff-retention.sh --what-if
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
HOST=pve.tjg.fyi CTID=230 ENABLE=false
while [ $# -gt 0 ]; do
  case $1 in
    --host) HOST=${2:-}; shift 2 ;;
    --ctid) CTID=${2:-}; shift 2 ;;
    --enable) ENABLE=true; shift ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $HOST =~ ^[a-z0-9][a-z0-9.-]*$ ]] || die "--host: a host name"
[[ $CTID =~ ^[0-9]+$ ]] || die "--ctid: a number"
commit=$(git -C "$HERE" rev-parse HEAD 2> /dev/null || echo unknown)
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$HOST")
stage=/root/usnm-handoff-retention-install
"${SSH[@]}" "rm -rf $stage && install -d -m 700 $stage"
scp -q "$HERE/handoff_retention.py" "$HERE/handoff-retention.sh" "$HERE/usnm-handoff-retention.service" \
  "$HERE/usnm-handoff-retention.timer" "root@$HOST:$stage/"
"${SSH[@]}" bash -s -- "$CTID" "$stage" "$commit" "$ENABLE" <<'REMOTE'
set -euo pipefail
ct=$1 stage=$2 commit=$3 enable=$4
in_ct() { pct exec "$ct" -- "$@"; }
in_ct install -d -m 0755 /opt/usnm-handoff-retention
pct push "$ct" "$stage/handoff_retention.py" /opt/usnm-handoff-retention/handoff_retention.py --perms 644
pct push "$ct" "$stage/handoff-retention.sh" /opt/usnm-handoff-retention/handoff-retention.sh --perms 755
printf '%s\n' "$commit" | in_ct sh -c 'cat > /opt/usnm-handoff-retention/COMMIT'
pct push "$ct" "$stage/usnm-handoff-retention.service" /etc/systemd/system/usnm-handoff-retention.service
pct push "$ct" "$stage/usnm-handoff-retention.timer" /etc/systemd/system/usnm-handoff-retention.timer
in_ct systemctl daemon-reload
[ "$enable" != true ] || in_ct systemctl enable --now usnm-handoff-retention.timer
rm -rf "$stage"
echo "installed in CT $ct (timer enabled: $enable)"
REMOTE
