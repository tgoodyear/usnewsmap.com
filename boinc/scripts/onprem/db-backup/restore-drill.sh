#!/usr/bin/env bash
# The monthly restore drill for the BOINC MySQL backups (docs/boinc/server/db-backup.md). Run as
# root on a Proxmox host (pve-big), by usnm-boinc-db-restore-drill.timer or by hand:
#
#   restore-drill.sh --r2-env FILE [--ctid 9237] [--storage local-lvm] [--bridge vmbr0]
#     [--template local:vztmpl/debian-12-standard_12.12-1_amd64.tar.zst]
#     [--compare-ct 237] [--tolerance-rows 500] [--tolerance-pct 2] [--keep-ct]
#
# It makes a scratch unprivileged CT (--ctid, which must not exist), installs podman and the
# pinned rclone in it, runs restore.sh there with the bucket's reader key (--r2-env, a file on
# this host: R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ENDPOINT), and compares the restored
# row count of every table with the live replica's (--compare-ct, the CT that
# home-infra scripts/lab/boinc-db-replica-ct.sh made). The replica runs a few minutes ahead of
# the newest shipped binlog, so a table passes when the counts differ by at most
# --tolerance-rows or --tolerance-pct percent, whichever is larger; a table missing on either
# side fails. Then it destroys the scratch CT (not with --keep-ct).
#
# It leaves {"pass", "finished_at"} in /var/lib/usnm-db-backup/last-drill.json in the replica CT
# (--compare-ct), where the feed monitor's dbbackup source reads it for db_drill
# (boundaries-and-monitoring.md). A drill that stops early records a failure there too.
#
# Ends with one JSON line (event boinc_db_restore_drill, "pass": true or false) and exits 1
# when the drill fails, so the unit fails.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
R2_ENV="" CTID=9237 STORAGE=local-lvm BRIDGE=vmbr0 COMPARE=237 TROWS=500 TPCT=2 KEEP=false
TEMPLATE=local:vztmpl/debian-12-standard_12.12-1_amd64.tar.zst
while [ $# -gt 0 ]; do
  case $1 in
    --r2-env) R2_ENV=${2:?}; shift 2 ;;
    --ctid) CTID=${2:?}; shift 2 ;;
    --storage) STORAGE=${2:?}; shift 2 ;;
    --bridge) BRIDGE=${2:?}; shift 2 ;;
    --template) TEMPLATE=${2:?}; shift 2 ;;
    --compare-ct) COMPARE=${2:?}; shift 2 ;;
    --tolerance-rows) TROWS=${2:?}; shift 2 ;;
    --tolerance-pct) TPCT=${2:?}; shift 2 ;;
    --keep-ct) KEEP=true; shift ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -r "$R2_ENV" ] || usage
[[ $CTID =~ ^[0-9]+$ ]] && [[ $COMPARE =~ ^[0-9]+$ ]] && [[ $TROWS =~ ^[0-9]+$ ]] && [[ $TPCT =~ ^[0-9]+$ ]] || die "numbers only"
[ "$(id -u)" = 0 ] && command -v pct > /dev/null || die "run as root on the Proxmox host"
# One drill at a time (the timer and a run by hand), then a CT id nothing else holds.
exec 9> /run/usnm-boinc-db-restore-drill.lock
flock -n 9 || die "another drill is running"
pct status "$CTID" > /dev/null 2>&1 && die "CT $CTID exists: the drill only uses a CT it makes"
HERE=$(cd "$(dirname "$0")" && pwd)
pin() { sed -n "s/^$1=//p" "$HERE/install.sh"; }
RV=$(pin RCLONE_VERSION) RS=$(pin RCLONE_SHA256)
in_ct() { pct exec "$CTID" -- env -u XDG_RUNTIME_DIR LANG=C.UTF-8 LC_ALL=C.UTF-8 "$@"; }
w=$(mktemp -d)
recorded=false
finish() {
  local status=$?
  $recorded || record false
  [ "${1:-}" != teardown ] || teardown
  rm -rf "$w"
  exit "$status"
}
trap finish EXIT
started=$(date +%s)
# The outcome for the monitor; a drill that can't record it still reports on stdout. Any exit
# before a result was recorded (a failed pct create or push under set -e) records a failure.
record() {
  recorded=true
  printf '{"pass": %s, "finished_at": %s}\n' "$1" "$(date +%s)" > "$w/last-drill.json"
  pct exec "$COMPARE" -- mkdir -p /var/lib/usnm-db-backup &&
    pct push "$COMPARE" "$w/last-drill.json" /var/lib/usnm-db-backup/last-drill.json --perms 644 ||
    echo "warning: couldn't record the drill in CT $COMPARE" >&2
}
fail() {
  record false
  printf '{"event": "boinc_db_restore_drill", "pass": false, "error": %s}\n' "$(jq -Rn --arg e "$*" '$e')"
  exit 1
}
teardown() {
  $KEEP && { echo "kept CT $CTID" >&2; return 0; }
  pct status "$CTID" > /dev/null 2>&1 || return 0
  pct stop "$CTID" > /dev/null 2>&1 || true
  pct destroy "$CTID" --purge > /dev/null 2>&1 || echo "warning: couldn't destroy CT $CTID" >&2
}
pct create "$CTID" "$TEMPLATE" --hostname usnm-boinc-db-drill --unprivileged 1 --features nesting=1,keyctl=1 \
  --cores 2 --memory 4096 --swap 512 --rootfs "$STORAGE:30" --onboot 0 --ostype debian \
  --net0 "name=eth0,bridge=$BRIDGE,ip=dhcp,type=veth" \
  --description "Scratch CT for the BOINC MySQL restore drill; destroyed when it ends. usnewsmap.com boinc/scripts/onprem/db-backup/restore-drill.sh" > /dev/null
# Armed only now that this run made the CT, so a failed create never destroys another's.
trap 'finish teardown' EXIT
pct start "$CTID"
for _ in $(seq 30); do in_ct getent hosts deb.debian.org > /dev/null 2>&1 && break; sleep 2; done
in_ct bash -c 'DEBIAN_FRONTEND=noninteractive apt-get -qq update && DEBIAN_FRONTEND=noninteractive apt-get -qq install -y podman jq openssl curl python3 ca-certificates > /dev/null' ||
  fail "couldn't install podman in the scratch CT"
in_ct bash -c "set -e; d=\$(mktemp -d); curl -fsSL -o \$d/rc.zip https://downloads.rclone.org/v$RV/rclone-v$RV-linux-amd64.zip
  echo '$RS  '\$d/rc.zip | sha256sum -c --quiet
  python3 -c 'import sys, zipfile; open(sys.argv[2], \"wb\").write(zipfile.ZipFile(sys.argv[1]).read(sys.argv[3]))' \$d/rc.zip \$d/rclone rclone-v$RV-linux-amd64/rclone
  install -D -m 755 \$d/rclone /usr/local/lib/usnm-db-backup/rclone; rm -rf \$d" || fail "couldn't install rclone"
pct push "$CTID" "$HERE/restore.sh" /root/restore.sh --perms 700
pct push "$CTID" "$R2_ENV" /root/r2.env --perms 600
in_ct /root/restore.sh --r2-env /root/r2.env --workdir /root/restore --counts-out /root/counts.json > "$w/restore.log" 2> "$w/restore.err" ||
  fail "restore.sh failed: $(tail -3 "$w/restore.err" | tr '\n' ' ')"
pct pull "$CTID" /root/counts.json "$w/restored.json"
pct exec "$COMPARE" -- /root/boinc-db-replica-setup.sh --counts > "$w/replica.json" || fail "couldn't count rows on CT $COMPARE"

result=$(jq -n --slurpfile r "$w/restored.json" --slurpfile c "$w/replica.json" --argjson rows "$TROWS" --argjson pct "$TPCT" '
  ($r[0].tables) as $a | ($c[0].tables) as $b |
  [ ($a + $b | keys[]) as $t |
    { table: $t, restored: $a[$t], replica: $b[$t] } |
    .diff = (if .restored == null or .replica == null then null else (.restored - .replica | fabs) end) |
    .ok = (.diff != null and .diff <= ([$rows, ((.replica // 0) * $pct / 100)] | max)) ] as $rows_ |
  { tables: ($rows_ | length), failed: [ $rows_[] | select(.ok | not) ],
    largest: ($rows_ | map(select(.diff != null)) | max_by(.diff)), dump: $r[0].dump,
    binlogs: $r[0].binlogs, last_binlog: $r[0].last_binlog }')
pass=$(jq '.failed | length == 0' <<< "$result")
record "$pass"
jq -c --argjson pass "$pass" --argjson secs "$(( $(date +%s) - started ))" --arg restore "$(tail -1 "$w/restore.log")" \
  '{event: "boinc_db_restore_drill", pass: $pass, seconds: $secs} + . + {restore: ($restore | fromjson? // $restore)}' <<< "$result"
[ "$pass" = true ]
