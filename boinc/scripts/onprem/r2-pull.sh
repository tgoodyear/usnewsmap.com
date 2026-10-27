#!/usr/bin/env bash
# Pulls the BOINC project tree from R2 (bucket usnm-boinc-project, prefix project/) into
# /srv/usnm-boinc/project. R2 keys come from the BOINC Key Vault through this machine's Arc
# identity; they live only in this process's environment.
set -euo pipefail
KV=usnmboinckvprodv7n7u6 EP=http://localhost:40342/metadata/identity/oauth2/token
DEST=${DEST:-/srv/usnm-boinc/project}
url="$EP?api-version=2020-06-01&resource=https://vault.azure.net"
realm=$(curl -sS -o /dev/null -D - -H "Metadata: true" "$url" | tr -d '\r' | sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *Basic realm=\([^[:space:]]*\).*/\1/p')
case "$realm" in /var/opt/azcmagent/tokens/*.key) [[ $realm != *..* ]] && [ -f "$realm" ] ;; *) echo "no Arc challenge" >&2; exit 1 ;; esac
tok=$(curl -fsS -H "Metadata: true" -H @<(printf 'Authorization: Basic %s\n' "$(cat "$realm")") "$url" | jq -er .access_token)
kv() { curl -fsS -H @<(printf 'Authorization: Bearer %s\n' "$tok") "https://$KV.vault.azure.net/secrets/$1?api-version=7.4" | jq -er .value; }
export RCLONE_CONFIG_R2_TYPE=s3 RCLONE_CONFIG_R2_PROVIDER=Cloudflare RCLONE_CONFIG_R2_NO_CHECK_BUCKET=true
RCLONE_CONFIG_R2_ACCESS_KEY_ID=$(kv r2-access-key-id); RCLONE_CONFIG_R2_SECRET_ACCESS_KEY=$(kv r2-secret-access-key); RCLONE_CONFIG_R2_ENDPOINT=$(kv r2-endpoint)
export RCLONE_CONFIG_R2_ACCESS_KEY_ID RCLONE_CONFIG_R2_SECRET_ACCESS_KEY RCLONE_CONFIG_R2_ENDPOINT; unset tok
mkdir -p "$DEST"
exec rclone copy r2:usnm-boinc-project/project "$DEST" --links --transfers 32 --checkers 32 --stats 120s --stats-one-line --stats-log-level NOTICE "$@"
