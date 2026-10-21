#!/usr/bin/env bash
# Dumps the BOINC database into R2 (db/<name>.sql.gz in bucket usnm-boinc-project) for the move
# on-prem (#942). Runs as a bridge-job execution with its command overridden to
# `bash -c <this file>`, because the database is reachable only from the BOINC VNet. The admin
# password and the R2 keys come from Key Vault through the job's identity and stay in /dev/shm.
# DUMP_NAME names the dump (default trial-<UTC time>); db-import.sh loads it on the BOINC host.
set -euo pipefail
w=$(mktemp -d /dev/shm/dump.XXXXXX); chmod 700 "$w"; trap 'rm -rf "$w"' EXIT
tok=$(curl -fsS -H @- "$IDENTITY_ENDPOINT?api-version=2019-08-01&resource=https://vault.azure.net&client_id=$AZURE_CLIENT_ID" <<< "X-IDENTITY-HEADER: $IDENTITY_HEADER" | jq -er .access_token)
(umask 077; printf 'Authorization: Bearer %s\n' "$tok" > "$w/auth"); unset tok
kv() { curl -fsS -H @"$w/auth" "https://$KEY_VAULT_NAME.vault.azure.net/secrets/$1?api-version=7.4" | jq -j .value; }
(umask 077; printf '[client]\nhost=%s\nuser=usnmboincadmin\npassword=%s\n' "$DB_HOST" "$(kv mysql-admin-password)" > "$w/my.cnf")
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
RCLONE_CONFIG_R2_ACCESS_KEY_ID=$(kv r2-access-key-id); RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=$(kv r2-secret-access-key); RCLONE_CONFIG_R2_ENDPOINT=$(kv r2-endpoint)
export RCLONE_CONFIG_R2_ACCESS_KEY_ID RCLONE_CONFIG_R2_SECRET_ACCESS_KEY RCLONE_CONFIG_R2_ENDPOINT
b=$(mktemp -d /tmp/rclone.XXXXXX); cd "$b"; curl -fsSL -o rc.zip https://downloads.rclone.org/v1.68.2/rclone-v1.68.2-linux-amd64.zip
echo "0e6fa18051e67fc600d803a2dcb10ddedb092247fc6eee61be97f64ec080a13c  rc.zip" | sha256sum -c --quiet
python3 -c 'import zipfile; open("rclone","wb").write(zipfile.ZipFile("rc.zip").read("rclone-v1.68.2-linux-amd64/rclone"))'; chmod 755 rclone
name=${DUMP_NAME:-trial-$(date -u +%Y%m%dT%H%M%SZ)}
echo "DUMP start $name $(date -u +%FT%TZ)"
mysql --defaults-extra-file="$w/my.cnf" --ssl-ca=/usr/local/share/usnm/mysql-ca.pem --ssl-verify-server-cert -N -e \
  "SELECT table_name, table_rows, ROUND((data_length+index_length)/1048576) FROM information_schema.tables WHERE table_schema='$DB_NAME' ORDER BY 3 DESC LIMIT 12" | sed 's/^/DUMP table /'
set -o pipefail
mysqldump --defaults-extra-file="$w/my.cnf" --ssl-ca=/usr/local/share/usnm/mysql-ca.pem --ssl-verify-server-cert \
  --single-transaction --quick --routines --triggers --events --hex-blob --default-character-set=utf8mb4 --databases "$DB_NAME" |
  gzip -6 | ./rclone rcat "r2:usnm-boinc-project/db/$name.sql.gz" --s3-chunk-size 64M
echo "DUMP done rc=$? $(date -u +%FT%TZ)"
./rclone lsl "r2:usnm-boinc-project/db/" | sed 's/^/DUMP obj /'
