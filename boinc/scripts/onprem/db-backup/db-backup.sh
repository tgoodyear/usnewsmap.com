#!/usr/bin/env bash
# Off-site backups of the on-prem BOINC MySQL to the R2 bucket usnm-boinc-db-backup. Runs as root
# on the BOINC host (CT 230), from usnm-boinc-db-dump.timer (nightly) and
# usnm-boinc-db-binlog-ship.timer (every 5 minutes); install.sh installs both.
#
#   db-backup.sh --dump [options]
#   db-backup.sh --ship-binlogs [--no-flush] [options]
#
# --dump streams `mysqldump --single-transaction` of the BOINC database through gzip to
#   dump/<year>/<UTC time>.sql.gz and then writes <same name>.json beside it: the server UUID,
#   gtid_mode, the binlog file and position the dump starts from, the dump's sha256 and size.
#   mysqldump takes FLUSH TABLES WITH READ LOCK for a moment to read those coordinates, so the
#   timer runs it at a quiet hour.
# --ship-binlogs runs FLUSH BINARY LOGS (unless --no-flush), so the newest changes are in a
#   closed file, and copies every closed binlog not in the bucket yet to
#   binlog/<server UUID>/<file>.gz. Nothing in the bucket is overwritten: the bucket's lock
#   refuses changes to objects younger than 7 days, and its lifecycle deletes dumps after 31
#   days and binlogs after 32, so every dump kept has the binlogs to roll it forward.
#
# Options (defaults are CT 230's):
#   --bucket NAME          R2 bucket (usnm-boinc-db-backup)
#   --r2-env FILE          R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ENDPOINT
#                          (/etc/usnm-boinc/secrets/r2-db-backup.env)
#   --password-file FILE   the usnmbackup MySQL user's password (/etc/usnm-boinc/secrets/db-backup)
#   --database NAME        (usnmocr)
#   --container NAME       the running MySQL container, whose image runs the clients
#                          (usnm-boinc-mysql)
#   --datadir DIR          the MySQL data directory on this host (/srv/usnm-boinc/mysql-data)
#   --ca FILE              the MySQL CA (/srv/usnm-boinc/mysql-tls/ca.pem)
#   --rclone FILE          (/usr/local/lib/usnm-db-backup/rclone)
#   --state-dir DIR        scratch space for compressed binlogs (/var/lib/usnm-db-backup)
#
# Each run ends with one JSON line (event boinc_db_backup) and exits non-zero on failure, so the
# unit fails. The restore and the monthly drill are in docs/boinc/server/db-backup.md.
set -euo pipefail
export PATH=/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin
die() { echo "error: $*" >&2; exit 1; }
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }

MODE="" FLUSH=true
BUCKET=usnm-boinc-db-backup
R2_ENV=/etc/usnm-boinc/secrets/r2-db-backup.env
PASSWORD_FILE=/etc/usnm-boinc/secrets/db-backup
DB=usnmocr CONTAINER=usnm-boinc-mysql
DATADIR=/srv/usnm-boinc/mysql-data
CA=/srv/usnm-boinc/mysql-tls/ca.pem
RCLONE=/usr/local/lib/usnm-db-backup/rclone
STATE=/var/lib/usnm-db-backup
while [ $# -gt 0 ]; do
  case $1 in
    --dump|--ship-binlogs) MODE=$1; shift ;;
    --no-flush) FLUSH=false; shift ;;
    --bucket) BUCKET=${2:?}; shift 2 ;;
    --r2-env) R2_ENV=${2:?}; shift 2 ;;
    --password-file) PASSWORD_FILE=${2:?}; shift 2 ;;
    --database) DB=${2:?}; shift 2 ;;
    --container) CONTAINER=${2:?}; shift 2 ;;
    --datadir) DATADIR=${2:?}; shift 2 ;;
    --ca) CA=${2:?}; shift 2 ;;
    --rclone) RCLONE=${2:?}; shift 2 ;;
    --state-dir) STATE=${2:?}; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$MODE" ] || usage
[[ $DB =~ ^[A-Za-z0-9_]+$ ]] || die "--database: a plain name"
for f in "$R2_ENV" "$PASSWORD_FILE" "$CA" "$RCLONE"; do [ -r "$f" ] || die "missing $f"; done

# One run at a time per mode; a dump and a ship can overlap.
exec 9> "/run/usnm-db-backup-${MODE#--}.lock"
flock -n 9 || die "another $MODE run holds the lock"

w=$(mktemp -d /dev/shm/usnm-db-backup.XXXXXX); chmod 700 "$w"; trap 'rm -rf "$w"' EXIT
R2_ACCESS_KEY_ID="" R2_SECRET_ACCESS_KEY="" R2_ENDPOINT=""
# shellcheck disable=SC1090
. "$R2_ENV"
[ -n "$R2_ACCESS_KEY_ID" ] && [ -n "$R2_SECRET_ACCESS_KEY" ] && [ -n "$R2_ENDPOINT" ] || die "$R2_ENV lacks a value"
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
export RCLONE_CONFIG_R2_ACCESS_KEY_ID=$R2_ACCESS_KEY_ID RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=$R2_SECRET_ACCESS_KEY
export RCLONE_CONFIG_R2_ENDPOINT=$R2_ENDPOINT
unset R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY
rclone() { "$RCLONE" --config /dev/null --retries 5 --low-level-retries 10 "$@"; }

# The MySQL clients come from the server's own image, so they always match it. --log-driver
# none: otherwise podman copies the container's stdout, the whole dump, into the journal.
IMG=$(podman inspect "$CONTAINER" --format '{{.Image}}') || die "no container $CONTAINER"
# 127.0.0.1 is in every certificate the database CA signs (the primary's and the replica's).
(umask 077; printf '[client]\nuser=usnmbackup\npassword=%s\nhost=127.0.0.1\nssl-mode=VERIFY_IDENTITY\nssl-ca=/tls/ca.pem\n' \
  "$(cat "$PASSWORD_FILE")" > "$w/my.cnf")
client() {
  podman run --rm -i --log-driver none --network host \
    -v "$w/my.cnf:/run/my.cnf:ro" -v "$CA:/tls/ca.pem:ro" "$IMG" "$1" --defaults-extra-file=/run/my.cnf "${@:2}"
}
sql() { client mysql -N -B -e "$1"; }
report() { printf '{"event": "boinc_db_backup", "mode": "%s", %s}\n' "${MODE#--}" "$1"; }

UUID=$(sql 'SELECT @@server_uuid')
[[ $UUID =~ ^[0-9a-f-]{36}$ ]] || die "unexpected server_uuid"

if [ "$MODE" = --dump ]; then
  ts=$(date -u +%Y%m%dT%H%M%SZ); key=dump/${ts:0:4}/$ts
  gtid=$(sql 'SELECT @@gtid_mode')
  started=$(date +%s)
  # Spooled to disk first (about 100 MB), so the checksum, size and coordinates are read from
  # the finished file before anything is uploaded.
  install -d -m 700 "$STATE"; spool=$STATE/dump.sql.gz
  rm -f "$spool"
  client mysqldump --single-transaction --quick --routines --triggers --events --hex-blob \
      --default-character-set=utf8mb4 --source-data=2 --set-gtid-purged=AUTO --databases "$DB" |
    gzip -6 > "$spool"
  gzip -t "$spool"
  { gzip -dc "$spool" || true; } | head -c 262144 > "$w/head"
  sha256sum < "$spool" | cut -d' ' -f1 > "$w/sha"; stat -c %s "$spool" > "$w/size"
  rclone copyto "$spool" "r2:$BUCKET/$key.sql.gz" --s3-chunk-size 64M
  rm -f "$spool"
  # mysqldump writes the coordinates as a comment near the top, e.g.
  # -- CHANGE REPLICATION SOURCE TO SOURCE_LOG_FILE='binlog.000011', SOURCE_LOG_POS=123;
  file=$(sed -n "s/^-- CHANGE REPLICATION SOURCE TO SOURCE_LOG_FILE='\([^']*\)', SOURCE_LOG_POS=\([0-9]*\);.*/\1/p" "$w/head" | head -1)
  pos=$(sed -n "s/^-- CHANGE REPLICATION SOURCE TO SOURCE_LOG_FILE='\([^']*\)', SOURCE_LOG_POS=\([0-9]*\);.*/\2/p" "$w/head" | head -1)
  [ -n "$file" ] && [ -n "$pos" ] || die "no binlog coordinates in the dump"
  grep -q '^-- MySQL dump' "$w/head" || die "the dump doesn't start like a mysqldump"
  sha=$(cat "$w/sha"); size=$(cat "$w/size")
  [[ $sha =~ ^[0-9a-f]{64}$ ]] && [[ $size =~ ^[0-9]+$ ]] || die "no checksum or size for the dump"
  version=$(sql 'SELECT VERSION()')
  # The name the server's container was started with (mysql@sha256:..., by mysql-setup.sh), so
  # a restore elsewhere pulls the same build.
  image=$(podman inspect "$CONTAINER" --format '{{.ImageName}}')
  [[ $image == *@sha256:* ]] || die "the server's image $image isn't pinned by digest"
  jq -n --arg key "$key.sql.gz" --arg uuid "$UUID" --arg gtid "$gtid" --arg file "$file" --argjson pos "$pos" \
    --arg sha "$sha" --argjson size "$size" --arg db "$DB" --arg version "$version" --arg ts "$ts" \
    --arg image "$image" \
    '{dump: $key, created: $ts, database: $db, server_uuid: $uuid, server_version: $version, image: $image,
      gtid_mode: $gtid, binlog_file: $file, binlog_pos: $pos, sha256: $sha, bytes: $size}' > "$w/manifest.json"
  rclone copyto "$w/manifest.json" "r2:$BUCKET/$key.json"
  report "$(printf '"dump": "%s.sql.gz", "bytes": %s, "binlog_file": "%s", "binlog_pos": %s, "gtid_mode": "%s", "seconds": %s' \
    "$key" "$size" "$file" "$pos" "$gtid" "$(( $(date +%s) - started ))")"
  exit 0
fi

# --ship-binlogs
$FLUSH && sql 'FLUSH BINARY LOGS'
binlogs=$(sql 'SHOW BINARY LOGS') || die "SHOW BINARY LOGS failed"
mapfile -t logs < <(cut -f1 <<< "$binlogs")
[ "${#logs[@]}" -ge 2 ] || { report '"shipped": 0, "note": "no closed binlog yet"'; exit 0; }
unset 'logs[-1]'   # the open one
for f in "${logs[@]}"; do [[ $f =~ ^[A-Za-z0-9_-]+\.[0-9]{6}$ ]] || die "odd binlog name"; done
# rclone exits 3 when the prefix has nothing in it yet; any other failure stops the run.
rc=0; listing=$(rclone lsf "r2:$BUCKET/binlog/$UUID/") || rc=$?
[ "$rc" = 0 ] || [ "$rc" = 3 ] || die "can't list binlog/$UUID/ (rclone exit $rc)"
declare -A have=()
while IFS= read -r o; do [ -z "$o" ] || have[$o]=1; done <<< "$listing"
install -d -m 700 "$STATE"
# A gap: the newest binlog in the bucket and the oldest one still here aren't consecutive, and
# nothing between them is anywhere (the primary purged them before they were shipped). Dumps
# from before the gap can't roll forward past it; the next dump starts a new unbroken run.
num() { local n=${1%.gz}; echo $((10#${n##*.})); }
newest=-1
for o in "${!have[@]}"; do [[ $o =~ ^[A-Za-z0-9_-]+\.[0-9]{6}\.gz$ ]] && [ "$(num "$o")" -gt "$newest" ] && newest=$(num "$o"); done
oldest=$(num "${logs[0]}")
gap=""
if [ "$newest" -ge 0 ] && [ "$oldest" -gt $((newest + 1)) ]; then
  gap="binlogs $((newest + 1)) to $((oldest - 1)) were purged before they were shipped"
fi
shipped=0 bytes=0
for f in "${logs[@]}"; do
  [[ $f =~ ^[A-Za-z0-9._-]+$ ]] || die "odd binlog name"
  [ -z "${have[$f.gz]:-}" ] || continue
  [ -r "$DATADIR/$f" ] || die "no $DATADIR/$f to ship (purged before it was shipped?)"
  gzip -6 -c "$DATADIR/$f" > "$STATE/$f.gz.part"
  mv "$STATE/$f.gz.part" "$STATE/$f.gz"
  rclone copyto "$STATE/$f.gz" "r2:$BUCKET/binlog/$UUID/$f.gz"
  bytes=$(( bytes + $(stat -c %s "$STATE/$f.gz") )); shipped=$(( shipped + 1 ))
  rm -f "$STATE/$f.gz"
done
report "$(printf '"shipped": %s, "bytes": %s, "newest_closed": "%s", "server_uuid": "%s", "gap": "%s"' "$shipped" "$bytes" "${logs[-1]}" "$UUID" "$gap")"
# Shipping what is here still helps the next dump; the run fails so the gap is seen.
[ -z "$gap" ] || die "$gap: dumps before binlog $oldest can't roll forward past it"
