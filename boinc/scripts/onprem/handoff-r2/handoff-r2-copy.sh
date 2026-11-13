#!/usr/bin/env bash
# Optional offsite copy of the hand-off root to a Cloudflare R2 bucket (#1367). Runs as root on the
# BOINC host (CT 230) from usnm-handoff-r2-copy.timer, which is not enabled by default. It is its
# own unit, not a controller step: a failure here fails this unit only and never a tick, a claim or
# a collect, and nothing on-prem reads the copy back.
#
#   handoff-r2-copy.sh [--bucket NAME] [--r2-env FILE] [--root DIR] [--rclone FILE] [--status FILE]
#
# One way: `rclone copy` adds and updates objects and never deletes in the bucket, and the hand-off
# root is only read. The lock record and its sidecar, and half-written temporary files, are left out.
# R2 takes no Entra or managed identity, so the credential is a bucket-scoped R2 API token (ADR-0024's
# pattern, as for the db-backup bucket): never the account's keys. Kept in a root-only env file
# (R2_ACCESS_KEY_ID, R2_SECRET_ACCESS_KEY, R2_ENDPOINT), read here and not printed.
#
# Defaults are CT 230's:
#   --bucket NAME   usnm-boinc-handoff-copy
#   --r2-env FILE   /etc/usnm-boinc/secrets/r2-handoff-copy.env
#   --root DIR      /srv/usnm-boinc/handoff-state
#   --rclone FILE   /usr/local/lib/usnm-db-backup/rclone
#   --status FILE   /var/lib/usnm-handoff-r2/status.json   (one JSON line per run, latest only)
#
# Without the env file this exits 0 and does nothing: the copy is off until the token is installed.
# Each run ends with one JSON line (event boinc_handoff_r2_copy); a failed copy exits non-zero so the
# unit fails. Nothing watches that yet: feed-health does not read this unit or its status file. Do not
# enable the timer until something does, such as an alert on `systemctl is-failed
# usnm-handoff-r2-copy.service` or on the age of the status file, or a stale copy goes unnoticed.
# It copies the live root, not a snapshot with a completion manifest (the design note's recovery-grade
# copy): until that lands the copy is for inspection, and a recovery from it must verify first.
# rclone runs at error level with no progress, so the journal carries failures, not per-object paths.
set -euo pipefail
export PATH="${PATH:+$PATH:}/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"
die() { echo "error: $*" >&2; exit 1; }
BUCKET=usnm-boinc-handoff-copy
R2_ENV=/etc/usnm-boinc/secrets/r2-handoff-copy.env
ROOT=/srv/usnm-boinc/handoff-state
RCLONE=/usr/local/lib/usnm-db-backup/rclone
STATUS=/var/lib/usnm-handoff-r2/status.json
while [ $# -gt 0 ]; do
  case $1 in
    --bucket) BUCKET=${2:?}; shift 2 ;;
    --r2-env) R2_ENV=${2:?}; shift 2 ;;
    --root) ROOT=${2:?}; shift 2 ;;
    --rclone) RCLONE=${2:?}; shift 2 ;;
    --status) STATUS=${2:?}; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $BUCKET =~ ^[a-z0-9][a-z0-9-]{1,62}$ ]] || die "invalid bucket name"
if [ ! -e "$R2_ENV" ]; then
  echo '{"event":"boinc_handoff_r2_copy","result":"disabled"}'
  exit 0
fi
for f in "$R2_ENV" "$RCLONE"; do [ -r "$f" ] || die "missing $f"; done
[ -d "$ROOT" ] && [ ! -L "$ROOT" ] || die "no hand-off root $ROOT"
R2_ACCESS_KEY_ID="" R2_SECRET_ACCESS_KEY="" R2_ENDPOINT=""
# shellcheck disable=SC1090
. "$R2_ENV"
[ -n "$R2_ACCESS_KEY_ID" ] && [ -n "$R2_SECRET_ACCESS_KEY" ] && [ -n "$R2_ENDPOINT" ] || die "$R2_ENV lacks a value"
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
export RCLONE_CONFIG_R2_ACCESS_KEY_ID=$R2_ACCESS_KEY_ID RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=$R2_SECRET_ACCESS_KEY
export RCLONE_CONFIG_R2_ENDPOINT=$R2_ENDPOINT
unset R2_ACCESS_KEY_ID R2_SECRET_ACCESS_KEY R2_ENDPOINT
mkdir -p "$(dirname "$STATUS")"
# One copy at a time.
exec 9> "$(dirname "$STATUS")/copy.lock"
flock -n 9 || { echo '{"event":"boinc_handoff_r2_copy","result":"busy"}'; exit 0; }
started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
rc=0
"$RCLONE" --config /dev/null --retries 3 --low-level-retries 10 copy "$ROOT" "r2:$BUCKET/handoff" \
  --exclude 'scheduler/controller.lock.json' --exclude 'scheduler/.controller.lock.flock' \
  --exclude '.controller.lock' --exclude '**/.*tmp*' --exclude '**/.lock-*' \
  --transfers 8 --checkers 16 --log-level ERROR --stats 0 >&2 || rc=$?
result=ok; [ "$rc" -eq 0 ] || result=failed
line=$(printf '{"event":"boinc_handoff_r2_copy","result":"%s","rclone_exit":%d,"started":"%s","finished":"%s","bucket":"%s"}' \
  "$result" "$rc" "$started" "$(date -u +%Y-%m-%dT%H:%M:%SZ)" "$BUCKET")
echo "$line"
# Separate commands, so a failed status write fails the unit even when the copy succeeded.
echo "$line" > "$STATUS.tmp"
mv "$STATUS.tmp" "$STATUS"
exit "$rc"
