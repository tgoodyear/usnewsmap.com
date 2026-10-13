#!/usr/bin/env bash
# First-boot setup for a BOINC VM in the contributor's Azure subscription.
# Started by cloud-init as root. Installs Debian's boinc-client package, writes
# the account file (key from the contributor's Key Vault) and the preferences,
# starts the client and the eviction watcher. Nothing is downloaded from the
# project's own servers; the only third-party inputs are Debian's package archive
# and the project URL the contributor gave.
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive

die() { echo "vm-setup: $*" >&2; exit 1; }
ENV_FILE=/etc/usnm/node.env
while [ $# -gt 0 ]; do
  case "$1" in
    --env-file) ENV_FILE=$2; shift 2 ;;
    *) die "unknown option $1" ;;
  esac
done

# KEY=VALUE lines, never sourced.
while IFS='=' read -r k v; do
  if [[ $k =~ ^[A-Z0-9_]+$ ]]; then printf -v "C_$k" '%s' "$v"; fi
done < "$ENV_FILE"
for k in PROJECT_URL VAULT_NAME TASKS MAX_DISK_GIB USNM_IDENTITY_CLIENT_ID; do
  n="C_$k"
  [ -n "${!n-}" ] || die "$k missing from $ENV_FILE"
done

apt-get update -qq
apt-get install -y -qq --no-install-recommends boinc-client curl jq ca-certificates
systemctl stop boinc-client.service || true

DATA_DIR=$(getent passwd boinc | cut -d: -f6)
[ -d "$DATA_DIR" ] || die "no data directory for the boinc user"
bash /opt/usnm/boinc-config.sh --vault-name "$C_VAULT_NAME" --project-url "$C_PROJECT_URL" \
  --out-dir "$DATA_DIR" --tasks "$C_TASKS" --max-disk-gib "$C_MAX_DISK_GIB" \
  --identity-client-id "$C_USNM_IDENTITY_CLIENT_ID" --owner boinc:boinc

systemctl daemon-reload
systemctl enable --now boinc-client.service usnm-boinc-evict.service
