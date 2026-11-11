#!/usr/bin/env bash
# Sets up the off-site BOINC MySQL backups (db-backup.sh) on the BOINC host, CT 230. Run as root
# there from a checkout of reviewed main. Safe to run again; it restarts nothing and changes no
# MySQL setting.
#
#   install.sh [--r2-env-stdin] [--no-enable]
#
# --r2-env-stdin  read the bucket-scoped R2 writer key from stdin, as three lines
#                 R2_ACCESS_KEY_ID=..., R2_SECRET_ACCESS_KEY=..., R2_ENDPOINT=...,
#                 into /etc/usnm-boinc/secrets/r2-db-backup.env (mode 600). The key is in the
#                 home Key Vault as usnm-boinc-db-backup-r2-writer-access-key-id,
#                 usnm-boinc-db-backup-r2-writer-secret-access-key and
#                 usnm-boinc-db-backup-r2-endpoint (docs/boinc/server/db-backup.md shows the
#                 pipe). Without it, the file must exist already.
# --no-enable     install everything but leave the timers off.
#
# It installs:
#   /usr/local/lib/usnm-db-backup/db-backup.sh, and rclone (the release pinned below, checked
#     against its sha256)
#   /etc/systemd/system/usnm-boinc-db-dump.{service,timer} and
#     usnm-boinc-db-binlog-ship.{service,timer}
#   the MySQL user usnmbackup@127.0.0.1 (TLS only) with what mysqldump and the binlog
#     shipping need, its password in /etc/usnm-boinc/secrets/db-backup (made here once)
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
die() { echo "error: $*" >&2; exit 1; }
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
R2_STDIN=false ENABLE=true
while [ $# -gt 0 ]; do
  case $1 in
    --r2-env-stdin) R2_STDIN=true; shift ;;
    --no-enable) ENABLE=false; shift ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ "$(id -u)" = 0 ] || die "run as root"
HERE=$(cd "$(dirname "$0")" && pwd)
SEC=/etc/usnm-boinc/secrets LIB=/usr/local/lib/usnm-db-backup
NAME=usnm-boinc-mysql TLS=/srv/usnm-boinc/mysql-tls
RCLONE_VERSION=1.68.2
RCLONE_SHA256=0e6fa18051e67fc600d803a2dcb10ddedb092247fc6eee61be97f64ec080a13c
for tool in podman jq gzip flock python3 curl sha256sum; do command -v "$tool" > /dev/null || die "no $tool here"; done
[ -s "$SEC/db-root" ] || die "no $SEC/db-root: is this the BOINC host?"

install -d -m 755 "$LIB"; install -d -m 700 "$SEC" /var/lib/usnm-db-backup
if ! "$LIB/rclone" version 2> /dev/null | grep -q "^rclone v$RCLONE_VERSION\$"; then
  t=$(mktemp -d); trap 'rm -rf "$t"' EXIT
  curl -fsSL -o "$t/rc.zip" "https://downloads.rclone.org/v$RCLONE_VERSION/rclone-v$RCLONE_VERSION-linux-amd64.zip"
  echo "$RCLONE_SHA256  $t/rc.zip" | sha256sum -c --quiet
  python3 -c 'import sys, zipfile; open(sys.argv[2], "wb").write(zipfile.ZipFile(sys.argv[1]).read(sys.argv[3]))' \
    "$t/rc.zip" "$t/rclone" "rclone-v$RCLONE_VERSION-linux-amd64/rclone"
  install -m 755 "$t/rclone" "$LIB/rclone"
fi
install -m 755 "$HERE/db-backup.sh" "$LIB/db-backup.sh"

if $R2_STDIN; then
  (umask 077; grep -E '^R2_(ACCESS_KEY_ID|SECRET_ACCESS_KEY|ENDPOINT)=[^[:space:]]+$' > "$SEC/r2-db-backup.env.new")
  [ "$(wc -l < "$SEC/r2-db-backup.env.new")" = 3 ] || { rm -f "$SEC/r2-db-backup.env.new"; die "stdin: want the three R2_ lines"; }
  mv "$SEC/r2-db-backup.env.new" "$SEC/r2-db-backup.env"
fi
[ -s "$SEC/r2-db-backup.env" ] || die "no $SEC/r2-db-backup.env: run with --r2-env-stdin"
chmod 600 "$SEC/r2-db-backup.env"
[ -s "$SEC/db-backup" ] || (umask 077; openssl rand -hex 24 | tr -d '\n' > "$SEC/db-backup")

# The user, as root over TLS, with the server's own image (mysql-setup.sh does the same).
IMG=$(podman inspect "$NAME" --format '{{.Image}}')
cnf=$(mktemp /dev/shm/my.XXXXXX); trap 'rm -f "$cnf"; rm -rf "${t:-}"' EXIT
printf '[client]\nuser=root\npassword=%s\nhost=%s\nssl-mode=VERIFY_IDENTITY\nssl-ca=/tls/ca.pem\n' "$(cat "$SEC/db-root")" "$NAME" > "$cnf"
podman run -i --rm --log-driver none --network host --add-host "$NAME:127.0.0.1" -v "$cnf:/run/my.cnf:ro" -v "$TLS/ca.pem:/tls/ca.pem:ro" "$IMG" \
  mysql --defaults-extra-file=/run/my.cnf <<SQL
CREATE USER IF NOT EXISTS 'usnmbackup'@'127.0.0.1' IDENTIFIED BY '$(cat "$SEC/db-backup")' REQUIRE SSL;
ALTER USER 'usnmbackup'@'127.0.0.1' IDENTIFIED BY '$(cat "$SEC/db-backup")' REQUIRE SSL;
GRANT SELECT, SHOW VIEW, TRIGGER, EVENT, LOCK TABLES ON usnmocr.* TO 'usnmbackup'@'127.0.0.1';
GRANT RELOAD, PROCESS, REPLICATION CLIENT, BACKUP_ADMIN ON *.* TO 'usnmbackup'@'127.0.0.1';
SQL

for u in usnm-boinc-db-dump.service usnm-boinc-db-dump.timer usnm-boinc-db-binlog-ship.service usnm-boinc-db-binlog-ship.timer; do
  install -m 644 "$HERE/$u" "/etc/systemd/system/$u"
done
systemctl daemon-reload
if $ENABLE; then
  systemctl enable -q --now usnm-boinc-db-dump.timer usnm-boinc-db-binlog-ship.timer
fi
echo "installed; timers $($ENABLE && echo enabled || echo left off)"
