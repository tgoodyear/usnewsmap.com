#!/usr/bin/env bash
# Copies the BOINC project share into R2 (bucket usnm-boinc-project, prefix project/) for the
# move on-prem (#942). Runs inside the BOINC environment, as a bridge-job execution whose command
# is overridden to `bash -c <this file>`: only that job mounts the share, and the storage account
# allows no public access. Uses the job's managed identity for Key Vault (the R2 keys) and a
# pinned rclone, checked by its sha256.
#
# What is copied: everything the server needs. Skipped: the html cache (rebuilt), logs, pid
# files, the tls464-* backups, and every upload except those the database still needs. With
# RELAY_UPLOADS_FILTER=db/<name>.uploads-filter (written by db-dump-job.sh) the uploads pass
# copies exactly the uploads of results whose workunit is not yet assimilated. The final pass at
# cutover uses that. Without it, the pass copies uploads younger than 72 hours, a warm-up only.
# Each pass stops before the job's 30-minute replica timeout; the next run resumes, since rclone
# copies only what differs. The log goes to relay-logs/ in the bucket every two minutes and
# once more after the outcome line, because a job's console log is gone once its execution ends.
set -euo pipefail
w=$(mktemp -d /dev/shm/r2.XXXXXX); chmod 700 "$w"; trap 'rm -rf "$w"' EXIT
tok=$(curl -fsS -H @- "$IDENTITY_ENDPOINT?api-version=2019-08-01&resource=https://vault.azure.net&client_id=$AZURE_CLIENT_ID" <<< "X-IDENTITY-HEADER: $IDENTITY_HEADER" | jq -er .access_token)
(umask 077; printf 'Authorization: Bearer %s\n' "$tok" > "$w/auth"); unset tok
kv() { curl -fsS -H @"$w/auth" "https://$KEY_VAULT_NAME.vault.azure.net/secrets/$1?api-version=7.4" | jq -er .value; }
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
RCLONE_CONFIG_R2_ACCESS_KEY_ID=$(kv r2-access-key-id); RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=$(kv r2-secret-access-key); RCLONE_CONFIG_R2_ENDPOINT=$(kv r2-endpoint)
export RCLONE_CONFIG_R2_ACCESS_KEY_ID RCLONE_CONFIG_R2_SECRET_ACCESS_KEY RCLONE_CONFIG_R2_ENDPOINT
b=$(mktemp -d /tmp/rclone.XXXXXX); cd "$b"; curl -fsSL -o rc.zip https://downloads.rclone.org/v1.68.2/rclone-v1.68.2-linux-amd64.zip
echo "0e6fa18051e67fc600d803a2dcb10ddedb092247fc6eee61be97f64ec080a13c  rc.zip" | sha256sum -c --quiet
python3 -c 'import zipfile; open("rclone","wb").write(zipfile.ZipFile("rc.zip").read("rclone-v1.68.2-linux-amd64/rclone"))'; chmod 755 rclone; ./rclone version | head -1
run=$(date -u +%Y%m%dT%H%M%SZ); L=/tmp/relay-$run.log; exec > >(tee -a "$L") 2>&1
( while sleep 120; do ./rclone copyto "$L" "r2:usnm-boinc-project/relay-logs/$run.log" -q || true; done ) &
up=$!
echo "RELAY start $(date -u +%FT%TZ)"
echo "RELAY top $(cd /project && echo *)"; echo "RELAY proj $(cd /project/usnmocr && echo *)"
common=(--links --local-no-check-updated --transfers 48 --checkers 32 --cutoff-mode hard --timeout 60s --contimeout 15s --low-level-retries 5 --stats 120s --stats-one-line --stats-log-level NOTICE)
# RELAY_PART splits one full pass across executions run side by side, because listing the whole
# share takes longer than one execution may run: "download" is usnmocr/download, "uploads" the
# uploads pass, "rest" everything else. Unset, one execution does all three, resuming run by run.
part=${RELAY_PART:-all}
case $part in all | download | uploads | rest) ;; *) echo "RELAY_PART is download, uploads, rest or unset" >&2; exit 2 ;; esac
rc_up=0 rc_tree=0 rc_dl=0
extra=()
[ "$part" = rest ] && extra=(--exclude "/usnmocr/download/**")
if [ "$part" = all ] || [ "$part" = uploads ]; then
  if [ -n "${RELAY_UPLOADS_FILTER:-}" ]; then
    [[ $RELAY_UPLOADS_FILTER =~ ^db/[A-Za-z0-9._-]+\.uploads-filter$ ]] || { echo "RELAY_UPLOADS_FILTER is db/<name>.uploads-filter" >&2; exit 2; }
    ./rclone copyto "r2:usnm-boinc-project/$RELAY_UPLOADS_FILTER" "$w/uploads.filter"
    echo "RELAY uploads filter $RELAY_UPLOADS_FILTER: $(($(wc -l < "$w/uploads.filter") - 1)) results"
    timeout 600 ./rclone copy /project/usnmocr/upload r2:usnm-boinc-project/project/usnmocr/upload "${common[@]}" --filter-from "$w/uploads.filter" --max-duration 540s --transfers 96 || rc_up=$?
  else
    timeout 600 ./rclone copy /project/usnmocr/upload r2:usnm-boinc-project/project/usnmocr/upload "${common[@]}" --max-age 72h --max-duration 540s --transfers 96 || rc_up=$?
  fi
  echo "RELAY uploads rc=$rc_up $(date -u +%FT%TZ)"
fi
if [ "$part" = all ] || [ "$part" = rest ]; then
  rc_tree=0; timeout "$([ "$part" = rest ] && echo 1100 || echo 1000)" ./rclone copy /project r2:usnm-boinc-project/project "${common[@]}" --max-duration "$([ "$part" = rest ] && echo 1000s || echo 900s)" \
    --exclude "/tls464-*/**" --exclude "/usnmocr/upload/**" --exclude "/usnmocr/log_*/**" --exclude "/usnmocr/tmp_*/**" --exclude "/usnmocr/pid_*/**" --exclude "/usnmocr/html/cache/**" ${extra[@]+"${extra[@]}"} || rc_tree=$?
  echo "RELAY tree rc=$rc_tree $(date -u +%FT%TZ)"
fi
if [ "$part" = download ]; then
  rc_dl=0; timeout 1600 ./rclone copy /project/usnmocr/download r2:usnm-boinc-project/project/usnmocr/download "${common[@]}" --max-duration 1500s || rc_dl=$?
  echo "RELAY download rc=$rc_dl $(date -u +%FT%TZ)"
fi
# Nonzero unless every copy this run made completed: a duration cutoff (rclone 10, timeout 124)
# or a failed transfer means the next run has more to send, so the final pass must end 0.
if [ "$rc_up" = 0 ] && [ "$rc_tree" = 0 ] && [ "$rc_dl" = 0 ]; then
  status=0; echo "RELAY complete $(date -u +%FT%TZ)"
else
  status=1; echo "RELAY incomplete (uploads $rc_up, tree $rc_tree, download $rc_dl) $(date -u +%FT%TZ)"
fi
# Stop the periodic uploader, let tee write the outcome line, then upload the finished log.
kill "$up" 2>/dev/null || true; wait "$up" 2>/dev/null || true
exec >&- 2>&-; sleep 2
./rclone copyto "$L" "r2:usnm-boinc-project/relay-logs/$run.log" -q || true
exit "$status"
