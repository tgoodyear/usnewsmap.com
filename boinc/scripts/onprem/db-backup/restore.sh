#!/usr/bin/env bash
# Restores the BOINC database from the R2 bucket usnm-boinc-db-backup: a dump that db-backup.sh
# --dump wrote, rolled forward with every binlog shipped after it. Runs as root on any host with
# podman (the monthly drill runs it in a scratch CT; docs/boinc/server/db-backup.md has the real
# restore). It starts its own MySQL container, from the image the dump's manifest names, with
# its data in --workdir, and never touches another MySQL.
#
#   restore.sh --r2-env FILE --workdir DIR [--dump KEY|latest] [--until BINLOG:POS]
#              [--bucket NAME] [--rclone FILE] [--port N] [--name NAME] [--image REF]
#              [--counts-out FILE] [--keep]
#
# --r2-env        R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ENDPOINT: the bucket's reader key
#                 (home Key Vault usnm-boinc-db-backup-r2-reader-*)
# --workdir       an empty directory with room for about 3x the database
# --dump          a dump key, dump/<year>/<time>.sql.gz, or latest (default): the newest dump
#                 whose first binlog has been shipped
# --until         stop the replay before binlog position POS of file BINLOG (e.g.
#                 binlog.000123:4567): for undoing a bad change. Find the position with --keep
#                 and SHOW RELAYLOG EVENTS on the restored server (db-backup.md). Without it,
#                 everything shipped is replayed.
# --port          the restored server's port on 127.0.0.1 (3306)
# --name          its container's name (usnm-boinc-mysql-restore)
# --image         the MySQL image (default: the one the manifest names)
# --counts-out    write {"tables": {name: rows}, ...} here after the restore
# --keep          leave the restored server running, with its relay logs and stopped replication
#                 channel; without it, it is stopped and its data removed at the end
#
# It checks the dump's sha256 against its manifest and loads it. Then it rolls forward with the
# server's own replication applier, as the image has no mysqlbinlog: every shipped binlog from
# the dump's file on goes into the data directory as a relay log, and the SQL thread applies
# them from the dump's position, with REPLICATE_DO_DB set to the database. Ends with one JSON
# line (event boinc_db_restore).
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
R2_ENV="" WD="" DUMP=latest UNTIL="" BUCKET=usnm-boinc-db-backup RCLONE=/usr/local/lib/usnm-db-backup/rclone
PORT=3306 NAME=usnm-boinc-mysql-restore IMAGE="" COUNTS="" KEEP=false
while [ $# -gt 0 ]; do
  case $1 in
    --r2-env) R2_ENV=${2:?}; shift 2 ;;
    --workdir) WD=${2:?}; shift 2 ;;
    --dump) DUMP=${2:?}; shift 2 ;;
    --until) UNTIL=${2:?}; shift 2 ;;
    --bucket) BUCKET=${2:?}; shift 2 ;;
    --rclone) RCLONE=${2:?}; shift 2 ;;
    --port) PORT=${2:?}; shift 2 ;;
    --name) NAME=${2:?}; shift 2 ;;
    --image) IMAGE=${2:?}; shift 2 ;;
    --counts-out) COUNTS=${2:?}; shift 2 ;;
    --keep) KEEP=true; shift ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$R2_ENV" ] && [ -n "$WD" ] || usage
[ -r "$R2_ENV" ] || die "no $R2_ENV"
[ -x "$RCLONE" ] || die "no rclone at $RCLONE"
[[ $PORT =~ ^[0-9]+$ ]] || die "--port: a number"
[ -z "$UNTIL" ] || [[ $UNTIL =~ ^[A-Za-z0-9_-]+\.[0-9]{6}:[0-9]+$ ]] || die "--until: BINLOG:POS, e.g. binlog.000123:4567"
install -d -m 700 "$WD"
[ -z "$(ls -A "$WD")" ] || die "$WD isn't empty"
WD=$(cd "$WD" && pwd)
# One restore per container name; cleanup removes only the container this run started.
exec 9> "/run/usnm-db-restore-$NAME.lock"
flock -n 9 || die "another restore uses the container name $NAME"
podman container exists "$NAME" && die "a container $NAME exists already"
created=false

R2_ACCESS_KEY_ID="" R2_SECRET_ACCESS_KEY="" R2_ENDPOINT=""
# shellcheck disable=SC1090
. "$R2_ENV"
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
export RCLONE_CONFIG_R2_ACCESS_KEY_ID=$R2_ACCESS_KEY_ID RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=$R2_SECRET_ACCESS_KEY
export RCLONE_CONFIG_R2_ENDPOINT=$R2_ENDPOINT
unset R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY
rclone() { "$RCLONE" --config /dev/null --retries 5 --low-level-retries 10 "$@"; }
started=$(date +%s)

cleanup() {
  $KEEP && return 0
  ! $created || podman rm -f "$NAME" > /dev/null 2>&1 || true
  rm -rf "$WD/data" "$WD/dl"
}
trap cleanup EXIT

if [ "$DUMP" = latest ]; then
  # The newest dump whose first binlog has been shipped: a dump taken minutes ago starts in a
  # binlog that is still open on the primary, so it can't be rolled forward yet.
  mkdir -p "$WD/dl"
  pick=""
  while IFS= read -r m; do
    t=${m##*/}; t=${t%.json}
    [[ $t =~ ^[0-9]{8}T[0-9]{6}Z$ ]] || continue
    # A manifest dated in the future can't be a real dump; skip it rather than let it win.
    [ "${t:0:8}" -le "$(date -u -d tomorrow +%Y%m%d)" ] || { echo "skipping future-dated $m" >&2; continue; }
    rclone copyto "r2:$BUCKET/dump/$m" "$WD/dl/candidate.json"
    u=$(jq -r .server_uuid "$WD/dl/candidate.json"); f=$(jq -r .binlog_file "$WD/dl/candidate.json")
    [[ $u =~ ^[0-9a-f-]{36}$ ]] && [[ $f =~ ^[A-Za-z0-9_-]+\.[0-9]{6}$ ]] || continue
    if rclone lsf --files-only "r2:$BUCKET/binlog/$u/$f.gz" 2> /dev/null | grep -qxF "$f.gz"; then pick=$m; break; fi
    echo "skipping $m: its binlog $f isn't shipped yet" >&2
  done < <(rclone lsf -R --files-only --include '*.json' "r2:$BUCKET/dump/" | sort -r)
  rm -f "$WD/dl/candidate.json"
  [ -n "$pick" ] || die "no dump in r2:$BUCKET/dump/ whose first binlog is shipped"
  DUMP=dump/${pick%.json}.sql.gz
fi
[[ $DUMP =~ ^dump/[0-9]{4}/[0-9]{8}T[0-9]{6}Z\.sql\.gz$ ]] || die "--dump: dump/<year>/<time>.sql.gz or latest"
mkdir -p "$WD/dl/binlog" "$WD/data"
rclone copyto "r2:$BUCKET/${DUMP%.sql.gz}.json" "$WD/dl/manifest.json"
rclone copyto "r2:$BUCKET/$DUMP" "$WD/dl/dump.sql.gz"
m() { jq -er ".$1" "$WD/dl/manifest.json"; }
[ "$(sha256sum < "$WD/dl/dump.sql.gz" | cut -d' ' -f1)" = "$(m sha256)" ] || die "the dump's sha256 doesn't match its manifest"
DB=$(m database); UUID=$(m server_uuid); FILE=$(m binlog_file); POS=$(m binlog_pos); GTID=$(m gtid_mode)
# The bucket is writable with the writer key, so every name and number from it is checked
# before it reaches arithmetic, a path or SQL.
[[ $DB =~ ^[A-Za-z0-9_]+$ ]] && [[ $UUID =~ ^[0-9a-f-]{36}$ ]] && [[ $FILE =~ ^[A-Za-z0-9_-]+\.[0-9]{6}$ ]] &&
  [[ $POS =~ ^[0-9]+$ ]] || die "odd manifest"
[ -n "$IMAGE" ] || IMAGE=$(m image)
# The manifest comes from the bucket, which the writer key can add to: run only the official
# MySQL image, by digest, whatever a manifest names.
[[ $IMAGE =~ ^docker\.io/library/mysql@sha256:[0-9a-f]{64}$ ]] ||
  die "image $IMAGE isn't docker.io/library/mysql@sha256:<digest>: pass --image"

# A dump made before GTIDs were on carries none, and binlogs after the switch carry them:
# ON_PERMISSIVE takes both. A dump made with GTIDs on sets gtid_purged, which needs ON.
mode=ON_PERMISSIVE; [ "$GTID" = ON ] && mode=ON
(umask 077; openssl rand -hex 24 | tr -d '\n' > "$WD/dl/root")
(umask 077; printf '[client]\nuser=root\npassword=%s\nhost=127.0.0.1\nport=%s\n' "$(cat "$WD/dl/root")" "$PORT" > "$WD/dl/my.cnf")
podman pull -q "$IMAGE" > /dev/null
# --server-id differs from the primary's, so the applier doesn't skip its events as its own.
podman run -d --name "$NAME" --log-driver none --network host -v "$WD/data:/var/lib/mysql:Z" -v "$WD/dl:/dl:ro" \
  -e MYSQL_ROOT_PASSWORD_FILE=/dl/root "$IMAGE" --port="$PORT" --bind-address=127.0.0.1 --mysqlx=OFF \
  --skip-log-bin --gtid-mode="$mode" --enforce-gtid-consistency=ON --innodb-buffer-pool-size=1G \
  --innodb-flush-log-at-trx-commit=2 --max-allowed-packet=1G --server-id=4242 --relay-log=restore-relay \
  --relay-log-purge=OFF --skip-replica-start > /dev/null
created=true
wait_up() {
  for _ in $(seq 1 120); do
    # The image's entrypoint runs a temporary server without networking first; wait for the real one.
    podman exec "$NAME" mysql --defaults-extra-file=/dl/my.cnf --protocol=TCP -N -e 'SELECT 1' > /dev/null 2>&1 && return 0
    sleep 3
  done
  podman logs --tail 30 "$NAME" >&2; die "the restore server didn't start"
}
wait_up
q() { podman exec -i "$NAME" mysql --defaults-extra-file=/dl/my.cnf --protocol=TCP "$@"; }

echo "loading $DUMP" >&2
# --skip-commands: the dump comes from a writable bucket, so the client runs no local commands
# (system, tee, pager...) from it; DELIMITER still works.
gzip -dc "$WD/dl/dump.sql.gz" | q --skip-commands
loaded=$(date +%s)

# A checked assignment, so a listing that fails part way stops the restore.
listing=$(rclone lsf --files-only "r2:$BUCKET/binlog/$UUID/") || die "can't list binlog/$UUID/"
mapfile -t logs < <(sed -n -E 's/^([A-Za-z0-9_-]+\.[0-9]{6})\.gz$/\1/p' <<< "$listing" | sort | awk -v f="$FILE" '$0 >= f')
[ "${#logs[@]}" -gt 0 ] && [ "${logs[0]}" = "$FILE" ] || die "binlog $FILE, where the dump starts, isn't in the bucket"
if [ -n "$UNTIL" ]; then
  # Only the binlogs up to the stop point, so a gap after it doesn't matter.
  ufile=${UNTIL%%:*}
  mapfile -t logs < <(printf '%s\n' "${logs[@]}" | awk -v u="$ufile" '$0 <= u')
  [ "${logs[-1]}" = "$ufile" ] || die "--until: $ufile isn't in the bucket at or after the dump's $FILE"
  # The dump already holds everything before its own position: a stop point before it needs an
  # older dump.
  [ "$ufile" != "$FILE" ] || [ "${UNTIL##*:}" -ge "$POS" ] || die "--until is before this dump's $FILE:$POS: pass an older --dump"
fi
prev=""
for f in "${logs[@]}"; do
  n=$((10#${f##*.}))
  [ -z "$prev" ] || [ "$n" = $((prev + 1)) ] || die "binlog $(( prev + 1 )) is missing from the bucket: can't roll past it"
  prev=$n
done
echo "replaying ${#logs[@]} binlogs from $FILE:$POS" >&2
# The relay logs go in while the server is stopped, so it reads the new index when it starts.
podman stop -t 110 "$NAME" > /dev/null
: > "$WD/data/restore-relay.index"
for f in "${logs[@]}"; do
  r=restore-relay.${f##*.}
  rclone cat "r2:$BUCKET/binlog/$UUID/$f.gz" | gzip -dc > "$WD/data/$r"
  echo "./$r" >> "$WD/data/restore-relay.index"
done
chown 999:999 "$WD/data"/restore-relay.*
podman start "$NAME" > /dev/null
wait_up
until_sql=""
if [ -n "$UNTIL" ]; then
  u=${UNTIL%%:*}
  until_sql=" UNTIL RELAY_LOG_FILE='restore-relay.${u##*.}', RELAY_LOG_POS=${UNTIL##*:}"
fi
q <<SQL
CHANGE REPLICATION SOURCE TO SOURCE_HOST='restore.invalid', RELAY_LOG_FILE='restore-relay.${FILE##*.}', RELAY_LOG_POS=$POS;
CHANGE REPLICATION FILTER REPLICATE_DO_DB = ($DB);
START REPLICA SQL_THREAD$until_sql;
SQL
last=restore-relay.${logs[-1]##*.}
st="" finished=false
field() { sed -n "s/^ *$1: //p" <<< "$st" | head -1; }
for _ in $(seq 1 2400); do
  st=$(q -e 'SHOW REPLICA STATUS\G')
  [ "$(field Last_SQL_Errno)" = 0 ] || die "the replay stopped: $(field Last_SQL_Error)"
  if [ -n "$UNTIL" ]; then
    [ "$(field Replica_SQL_Running)" = No ] && { finished=true; break; }
  else
    # The server opens a fresh relay log of its own after the last one it was given, so done is
    # "read everything" in that file or a later one.
    cur=$(field Relay_Log_File)
    [ "${cur##*.}" \> "${last##*.}" ] || [ "$cur" = "$last" ] || { sleep 3; continue; }
    [[ $(field Replica_SQL_Running_State) == *"read all relay log"* ]] && { finished=true; break; }
  fi
  sleep 3
done
[ "$(field Last_SQL_Errno)" = 0 ] || die "the replay stopped: $(field Last_SQL_Error)"
$finished || die "the replay didn't finish in 2 hours (at $(field Relay_Log_File):$(field Relay_Log_Pos))"
applied="$(field Relay_Log_File):$(field Relay_Log_Pos)"
# With --keep the relay logs stay for SHOW RELAYLOG EVENTS; RESET REPLICA ALL would delete them.
# Run it before the restored data serves anything (db-backup.md).
if $KEEP; then q -e 'STOP REPLICA'; else q -e 'STOP REPLICA; RESET REPLICA ALL'; fi

tables=$(q -N -B -e "SELECT table_name FROM information_schema.tables WHERE table_schema='$DB' AND table_type='BASE TABLE' ORDER BY 1")
{
  printf '{"dump": "%s", "database": "%s", "binlogs": %s, "last_binlog": "%s", "applied_to": "%s", "tables": {' "$DUMP" "$DB" "${#logs[@]}" "${logs[-1]}" "$applied"
  sep=""
  for t in $tables; do
    [[ $t =~ ^[A-Za-z0-9_]+$ ]] || die "odd table name"
    n=$(q -N -B -e "SELECT COUNT(*) FROM \`$DB\`.\`$t\`")
    [[ $n =~ ^[0-9]+$ ]] || die "no count for $t"
    printf '%s"%s": %s' "$sep" "$t" "$n"; sep=", "
  done
  printf '}}\n'
} > "$WD/counts.json"
jq -e . "$WD/counts.json" > /dev/null
[ -z "$COUNTS" ] || cp "$WD/counts.json" "$COUNTS"
printf '{"event": "boinc_db_restore", "dump": "%s", "binlogs": %s, "last_binlog": "%s", "applied_to": "%s", "tables": %s, "load_seconds": %s, "total_seconds": %s, "kept": %s}\n' \
  "$DUMP" "${#logs[@]}" "${logs[-1]}" "$applied" "$(jq '.tables | length' "$WD/counts.json")" "$(( loaded - started ))" "$(( $(date +%s) - started ))" "$KEEP"
