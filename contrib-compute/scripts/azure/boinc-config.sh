#!/usr/bin/env bash
# Runs on a contributor's Azure VM or in an ACI init container. Fetches the
# BOINC account key from the contributor's own Key Vault (token from the managed
# identity, over the local metadata endpoint) and writes two files into the BOINC
# data directory: the account file that pre-attaches the client to the project,
# and global_prefs_override.xml. The key is never printed, never on a command
# line and never in an environment variable. Needs curl and jq.
#
#   boinc-config.sh --vault-name NAME --project-url URL --out-dir DIR \
#     [--secret-name boinc-account-key] [--tasks N] [--max-disk-gib N] [--run-if-idle]
#     [--identity-client-id ID] [--owner USER:GROUP]
#
# Defaults for the preferences are the dedicated-host column of docs/interface.md.
# (scripts/lib/boinc-common.sh is the canonical copy; scripts/check-prefs-consistent.sh keeps them equal.)
set -euo pipefail
die() { echo "boinc-config: $*" >&2; exit 1; }

VAULT='' PROJECT_URL='' OUT_DIR='' SECRET_NAME=boinc-account-key TASKS=1 MAX_DISK_GIB=5
RUN_IF_IDLE=0 CLIENT_ID='' OWNER=''
while [ $# -gt 0 ]; do
  case "$1" in
    --vault-name) VAULT=$2; shift 2 ;;
    --project-url) PROJECT_URL=$2; shift 2 ;;
    --out-dir) OUT_DIR=$2; shift 2 ;;
    --secret-name) SECRET_NAME=$2; shift 2 ;;
    --tasks) TASKS=$2; shift 2 ;;
    --max-disk-gib) MAX_DISK_GIB=$2; shift 2 ;;
    --run-if-idle) RUN_IF_IDLE=1; shift ;;
    --identity-client-id) CLIENT_ID=$2; shift 2 ;;
    --owner) OWNER=$2; shift 2 ;;
    --help) sed -n 2,14p "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $VAULT =~ ^[a-zA-Z][a-zA-Z0-9-]{1,22}[a-zA-Z0-9]$ ]] || die "--vault-name is not a valid Key Vault name"
[[ $PROJECT_URL =~ ^https://[a-z0-9.-]+(:[0-9]{1,5})?(/[a-z0-9._~-]+)*/$ ]] \
  || die "--project-url must be a lower-case https:// URL ending with a slash"
[ -d "$OUT_DIR" ] || die "--out-dir must be an existing directory"
[[ $SECRET_NAME =~ ^[a-zA-Z0-9-]{1,127}$ ]] || die "--secret-name is not a valid secret name"
[[ $TASKS =~ ^[1-9][0-9]*$ ]] || die "--tasks must be a whole number"
[[ $MAX_DISK_GIB =~ ^[1-9][0-9]*$ ]] || die "--max-disk-gib must be a whole number"

umask 077
WORK=$(mktemp -d /dev/shm/usnm.XXXXXX 2> /dev/null || mktemp -d)
trap 'rm -rf "$WORK"' EXIT

# Managed identity token: container groups set IDENTITY_ENDPOINT; VMs use the
# instance metadata service.
IMDS_HOST=169.254.169.254
suffix=''
[ -z "$CLIENT_ID" ] || suffix="&client_id=$CLIENT_ID"
if [ -n "${IDENTITY_ENDPOINT:-}" ]; then
  url="$IDENTITY_ENDPOINT?api-version=2019-08-01&resource=https://vault.azure.net$suffix"
  body=$(curl --fail --silent --show-error --max-time 20 -H "X-IDENTITY-HEADER: ${IDENTITY_HEADER:-}" "$url")
else
  url="http://$IMDS_HOST/metadata/identity/oauth2/token?api-version=2018-02-01&resource=https://vault.azure.net$suffix"
  body=$(curl --fail --silent --show-error --max-time 20 -H 'Metadata: true' "$url")
fi
# The token goes in a header file so it is not in the process list.
printf 'Authorization: Bearer %s\n' "$(jq -er '.access_token' <<< "$body")" > "$WORK/header"

code=$(curl --silent --show-error --max-time 20 -o "$WORK/secret.json" -w '%{http_code}' \
  -H "@$WORK/header" "https://$VAULT.vault.azure.net/secrets/$SECRET_NAME?api-version=7.4")
[ "$code" = 200 ] || die "reading secret $SECRET_NAME failed with HTTP $code (is it set? see docs/azure-contributors.md)"
KEY=$(jq -er '.value' "$WORK/secret.json" | tr -d '[:space:]')
[[ $KEY =~ ^[A-Za-z0-9]{8,128}$ ]] || die "the secret is not a BOINC account key"

# Account file name: lib/url.cpp and client/file_names.cpp in the BOINC source.
u=${PROJECT_URL#*://}; u=${u//[^A-Za-z0-9._-]/_}; u=${u%_}
acct="$OUT_DIR/account_${u}.xml"
printf '<account>\n    <master_url>%s</master_url>\n    <authenticator>%s</authenticator>\n</account>\n' \
  "$PROJECT_URL" "$KEY" > "$acct"

cpus=$(nproc)
pct=$(((TASKS * 100 + cpus - 1) / cpus)); [ "$pct" -le 100 ] || pct=100
active=1 idle_min=0
if [ "$RUN_IF_IDLE" = 1 ]; then active=0; idle_min=3; fi
cat > "$OUT_DIR/global_prefs_override.xml" << XML
<global_preferences>
    <max_ncpus_pct>$pct</max_ncpus_pct>
    <cpu_usage_limit>100</cpu_usage_limit>
    <run_if_user_active>$active</run_if_user_active>
    <idle_time_to_run>$idle_min</idle_time_to_run>
    <run_on_batteries>1</run_on_batteries>
    <ram_max_used_busy_pct>80</ram_max_used_busy_pct>
    <ram_max_used_idle_pct>90</ram_max_used_idle_pct>
    <disk_max_used_gb>$MAX_DISK_GIB</disk_max_used_gb>
    <work_buf_min_days>0.1</work_buf_min_days>
    <work_buf_additional_days>0.25</work_buf_additional_days>
    <suspend_cpu_usage>0</suspend_cpu_usage>
</global_preferences>
XML
chmod 0644 "$OUT_DIR/global_prefs_override.xml"
if [ -n "$OWNER" ]; then chown "$OWNER" "$acct" "$OUT_DIR/global_prefs_override.xml"; fi
echo "boinc-config: wrote $(basename "$acct") (mode 600) and global_prefs_override.xml in $OUT_DIR"
