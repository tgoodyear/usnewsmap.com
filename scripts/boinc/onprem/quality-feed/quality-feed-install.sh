#!/usr/bin/env bash
# Installs the on-prem quality feeder (#962) from a checkout: the feeder in the batch
# container on pve-laptop and the shipper on pve-laptop itself. Run from the operator's
# machine; it copies only this directory's code, never data. Safe to run again.
#
#   quality-feed-install.sh --campaign NAME --engine-image sha256:<worker digest>
#     [--host pve-laptop.tjg.fyi] [--ctid 215] [--scan-server http://usnm-scanfetch.tjg.fyi:8380]
#     [--chunk-pages 200] [--max-pending 5] [--workers 4]
#     [--max-unsent 300] [--max-chunks 1] [--bundle-size 200] [--enable]
#
# Before it: home-infra's scripts/host-ssh-trust.sh --from pve-laptop.tjg.fyi --to pve.tjg.fyi
# and scripts/lab/batch-ct-quality.sh --mounts on pve-laptop (the curated copy at /srv/curated
# and the outbox in the container). In the container it installs python3-venv and
# postgresql-client, a venv at /opt/usnm-quality-feed/venv from requirements.txt (hashes
# required), quality_feed.py, the unit usnm-quality-feed.service and
# /etc/usnm/quality-feed.env. On the host it installs quality-ship.sh, quality_receive.py,
# usnm-quality-ship.{service,timer} and /etc/usnm/quality-ship.env. The campaign's page list
# (/var/lib/usnm-quality-feed/<campaign>/pages.jsonl in the container, from
# quality_feed.py select) must be there before --enable starts the feeder and the timer.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)

CAMPAIGN="" ENGINE="" HOST=pve-laptop.tjg.fyi CTID=215 SCAN_SERVER=http://usnm-scanfetch.tjg.fyi:8380
CHUNK=200 PENDING=5 WORKERS=4 MAX_UNSENT=300 MAX_CHUNKS=1 BUNDLE=200 ENABLE=false
while [ $# -gt 0 ]; do
  case $1 in
    --campaign) CAMPAIGN=${2:-}; shift 2 ;;
    --engine-image) ENGINE=${2:-}; shift 2 ;;
    --host) HOST=${2:-}; shift 2 ;;
    --ctid) CTID=${2:-}; shift 2 ;;
    --scan-server) SCAN_SERVER=${2:-}; shift 2 ;;
    --chunk-pages) CHUNK=${2:-}; shift 2 ;;
    --max-pending) PENDING=${2:-}; shift 2 ;;
    --workers) WORKERS=${2:-}; shift 2 ;;
    --max-unsent) MAX_UNSENT=${2:-}; shift 2 ;;
    --max-chunks) MAX_CHUNKS=${2:-}; shift 2 ;;
    --bundle-size) BUNDLE=${2:-}; shift 2 ;;
    --enable) ENABLE=true; shift ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $CAMPAIGN =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || die "--campaign: 1-41 of [a-z0-9-]"
[[ $ENGINE =~ ^sha256:[0-9a-f]{64}$ ]] || die "--engine-image: sha256:<64 hex>"
[[ $HOST =~ ^[a-z0-9][a-z0-9.-]*$ ]] || die "--host: a host name"
[[ $SCAN_SERVER =~ ^http://[a-z0-9][a-z0-9.-]*:[0-9]+$ ]] || die "--scan-server: http://<host>:<port>"
for n in "$CTID" "$CHUNK" "$PENDING" "$WORKERS" "$MAX_UNSENT" "$MAX_CHUNKS" "$BUNDLE"; do
  [[ $n =~ ^[0-9]+$ ]] || die "numbers only: $n"
done
SSH=(ssh -o BatchMode=yes -o ConnectTimeout=15 "root@$HOST")
stage=/root/usnm-quality-feed-install

# The files, to a root-only directory on the host, then into the container.
"${SSH[@]}" "rm -rf $stage && install -d -m 700 $stage"
scp -q "$HERE/quality_feed.py" "$HERE/requirements.txt" "$HERE/usnm-quality-feed.service" \
  "$HERE/quality-ship.sh" "$HERE/quality_receive.py" "$HERE/usnm-quality-ship.service" \
  "$HERE/usnm-quality-ship.timer" "$HERE/../../../onprem/quality/load.py" "root@$HOST:$stage/"

"${SSH[@]}" bash -s -- "$CTID" "$stage" "$CAMPAIGN" "$SCAN_SERVER" "$CHUNK" "$PENDING" "$WORKERS" \
  "$ENGINE" "$MAX_UNSENT" "$MAX_CHUNKS" "$BUNDLE" "$ENABLE" <<'REMOTE'
set -euo pipefail
ct=$1 stage=$2 campaign=$3 scan=$4 chunk=$5 pending=$6 workers=$7 engine=$8 unsent=$9 chunks=${10} bundle=${11} enable=${12}
in_ct() { pct exec "$ct" -- "$@"; }
in_ct test -d /srv/curated/pages || { echo "error: no curated copy in CT $ct: run batch-ct-quality.sh --mounts" >&2; exit 1; }
in_ct bash -c 'export DEBIAN_FRONTEND=noninteractive; apt-get install -y -q python3-venv postgresql-client > /dev/null'
in_ct install -d /opt/usnm-quality-feed /etc/usnm /var/lib/usnm-quality-feed
pct push "$ct" "$stage/requirements.txt" /opt/usnm-quality-feed/requirements.txt
pct push "$ct" "$stage/quality_feed.py" /opt/usnm-quality-feed/quality_feed.py --perms 755
pct push "$ct" "$stage/load.py" /opt/usnm-quality-feed/load.py --perms 755
pct push "$ct" "$stage/usnm-quality-feed.service" /etc/systemd/system/usnm-quality-feed.service
in_ct bash -c '[ -x /opt/usnm-quality-feed/venv/bin/python ] || python3 -m venv /opt/usnm-quality-feed/venv
  /opt/usnm-quality-feed/venv/bin/pip install -q --require-hashes --no-deps --only-binary :all: -r /opt/usnm-quality-feed/requirements.txt'
printf 'CAMPAIGN=%s\nSCAN_SERVER=%s\nCHUNK_PAGES=%s\nMAX_PENDING=%s\nWORKERS=%s\n' \
  "$campaign" "$scan" "$chunk" "$pending" "$workers" | in_ct sh -c 'cat > /etc/usnm/quality-feed.env'
in_ct systemctl daemon-reload

install -d /opt/usnm-quality-feed /etc/usnm
install -m 0755 "$stage/quality-ship.sh" /opt/usnm-quality-feed/quality-ship.sh
install -m 0644 "$stage/quality_receive.py" /opt/usnm-quality-feed/quality_receive.py
install -m 0644 "$stage/usnm-quality-ship.service" "$stage/usnm-quality-ship.timer" /etc/systemd/system/
printf 'CAMPAIGN=%s\nENGINE_IMAGE=%s\nMAX_UNSENT=%s\nMAX_CHUNKS=%s\nBUNDLE_SIZE=%s\n' \
  "$campaign" "$engine" "$unsent" "$chunks" "$bundle" > /etc/usnm/quality-ship.env
systemctl daemon-reload
rm -rf "$stage"
if [ "$enable" = true ]; then
  in_ct test -s "/var/lib/usnm-quality-feed/$campaign/pages.jsonl" ||
    { echo "error: no page list for $campaign in CT $ct" >&2; exit 1; }
  in_ct systemctl restart usnm-quality-feed.service
  systemctl enable --now usnm-quality-ship.timer
fi
echo "installed: feeder in CT $ct, shipper on $(hostname) (campaign $campaign, enabled: $enable)"
REMOTE
