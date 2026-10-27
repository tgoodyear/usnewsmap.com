#!/usr/bin/env bash
# Provisions an unprivileged Debian 13 (trixie) LXC container on a Proxmox VE
# host and attaches it as a BOINC volunteer client for https://boinc.usnewsmap.com/.
#
# Run on the Proxmox host as root:
#   boinc/scripts/pve-boinc-ct.sh --ctid 101 --name boinc-volunteer-2
#
# The container receives nesting=1,keyctl=1 so Docker works unprivileged inside
# the LXC, installs BOINC >= 8.2.13 from Berkeley's official Debian trixie
# repository, configures global_prefs_override.xml, and attaches to the project
# using either --authenticator-file or a fresh account created via boinccmd.
set -euo pipefail

CTID=101
NAME=boinc-volunteer
CORES=4
MEMORY=8192
SWAP=2048
DISK_GB=64
STORAGE=local-lvm
BRIDGE=vmbr0
IP=dhcp
TEMPLATE=""
PROJECT_URL="https://boinc.usnewsmap.com/"
AUTH_FILE=""
ACCOUNT_NAME=""
MAX_NCPUS_PCT=100
CPU_USAGE_LIMIT=100
WHAT_IF=false

die() { echo "error: $*" >&2; exit 1; }

usage() {
  cat <<'EOF'
Usage: boinc/scripts/pve-boinc-ct.sh [options]

Options:
  --ctid <id>                  Container ID (default: 101)
  --name <hostname>            Container hostname (default: boinc-volunteer)
  --cores <n>                  CPU cores (default: 4)
  --memory <mb>                RAM in MiB (default: 8192)
  --swap <mb>                  Swap in MiB (default: 2048)
  --disk-gb <gb>               Rootfs size in GiB (default: 64)
  --storage <pool>             Proxmox storage pool (default: local-lvm)
  --bridge <iface>             Network bridge (default: vmbr0)
  --ip <cidr|dhcp>             IPv4 config or dhcp (default: dhcp)
  --template <volid>           Explicit Debian 13 LXC template volid
  --project-url <url>          BOINC project URL (default: https://boinc.usnewsmap.com/)
  --authenticator-file <path>  File containing an existing BOINC account key
  --account-name <name>        Account name when creating a new BOINC account (default: --name)
  --max-ncpus-pct <1-100>      BOINC max_ncpus_pct override (default: 100)
  --cpu-usage-limit <1-100>    BOINC cpu_usage_limit override (default: 100)
  --what-if                    Print the provisioning plan without changing the host
EOF
}

while [ $# -gt 0 ]; do
  case "$1" in
    --ctid) CTID=${2:?}; shift 2 ;;
    --name) NAME=${2:?}; shift 2 ;;
    --cores) CORES=${2:?}; shift 2 ;;
    --memory) MEMORY=${2:?}; shift 2 ;;
    --swap) SWAP=${2:?}; shift 2 ;;
    --disk-gb) DISK_GB=${2:?}; shift 2 ;;
    --storage) STORAGE=${2:?}; shift 2 ;;
    --bridge) BRIDGE=${2:?}; shift 2 ;;
    --ip) IP=${2:?}; shift 2 ;;
    --template) TEMPLATE=${2:?}; shift 2 ;;
    --project-url) PROJECT_URL=${2:?}; shift 2 ;;
    --authenticator-file) AUTH_FILE=${2:?}; shift 2 ;;
    --account-name) ACCOUNT_NAME=${2:?}; shift 2 ;;
    --max-ncpus-pct) MAX_NCPUS_PCT=${2:?}; shift 2 ;;
    --cpu-usage-limit) CPU_USAGE_LIMIT=${2:?}; shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown option: $1" ;;
  esac
done

[[ "$CTID" =~ ^[0-9]+$ ]] && [ "$CTID" -ge 100 ] || die "--ctid must be an integer >= 100"
[[ "$NAME" =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] || die "--name must be a valid DNS hostname label"
for num_var in CORES MEMORY SWAP DISK_GB MAX_NCPUS_PCT CPU_USAGE_LIMIT; do
  [[ "${!num_var}" =~ ^[0-9]+$ ]] && [ "${!num_var}" -ge 1 ] ||
    die "--$(printf '%s' "$num_var" | tr 'A-Z_' 'a-z-') must be a positive integer"
done
[ "$MAX_NCPUS_PCT" -le 100 ] || die "--max-ncpus-pct must be between 1 and 100"
[ "$CPU_USAGE_LIMIT" -le 100 ] || die "--cpu-usage-limit must be between 1 and 100"
[[ "$STORAGE" =~ ^[A-Za-z0-9._-]+$ ]] || die "--storage has invalid characters"
[[ "$BRIDGE" =~ ^[A-Za-z0-9._-]+$ ]] || die "--bridge has invalid characters"
[[ "$PROJECT_URL" =~ ^https://[A-Za-z0-9.-]+(/.*)?$ ]] || die "--project-url must be an https:// URL"

if [ -z "$ACCOUNT_NAME" ]; then
  ACCOUNT_NAME="$NAME"
fi
[[ "$ACCOUNT_NAME" =~ ^[A-Za-z0-9._-]+$ ]] || die "--account-name must match [A-Za-z0-9._-]+"

AUTHENTICATOR=""
if [ -n "$AUTH_FILE" ]; then
  [ -f "$AUTH_FILE" ] || die "authenticator file not found: $AUTH_FILE"
  AUTHENTICATOR=$(tr -d '[:space:]' < "$AUTH_FILE")
  [[ "$AUTHENTICATOR" =~ ^[0-9a-f]{32,64}$ ]] ||
    die "$AUTH_FILE must contain a 32-to-64-character lowercase hex BOINC authenticator"
fi

if $WHAT_IF; then
  echo "would provision PVE BOINC client CT $CTID ($NAME):"
  echo "  pct create $CTID ${TEMPLATE:-local:vztmpl/debian-13-standard_<latest>_amd64.tar.zst} \\"
  echo "    --hostname $NAME --unprivileged 1 --features nesting=1,keyctl=1 \\"
  echo "    --cores $CORES --memory $MEMORY --swap $SWAP --rootfs ${STORAGE}:${DISK_GB} \\"
  echo "    --onboot 1 --ostype debian --net0 name=eth0,bridge=${BRIDGE},ip=${IP},firewall=0"
  echo "  project: $PROJECT_URL (account: $ACCOUNT_NAME, max_ncpus_pct=$MAX_NCPUS_PCT, cpu_usage_limit=$CPU_USAGE_LIMIT)"
  exit 0
fi

command -v pct >/dev/null || die "pct not found (run this script on the Proxmox VE host)"
command -v pveam >/dev/null || die "pveam not found (run this script on the Proxmox VE host)"

if [ -z "$TEMPLATE" ]; then
  TEMPLATE=$(pveam list local | awk '/debian-13-standard_/ {print $1}' | sort -V | tail -n 1)
  if [ -z "$TEMPLATE" ]; then
    pveam update >/dev/null
    tmpl_name=$(pveam available --section system | awk '/debian-13-standard_/ {print $2}' | sort -V | tail -n 1)
    [ -n "$tmpl_name" ] || die "no debian-13-standard template found in pveam"
    pveam download local "$tmpl_name"
    TEMPLATE="local:vztmpl/$tmpl_name"
  fi
fi

if pct status "$CTID" >/dev/null 2>&1; then
  echo "CT $CTID already exists"
else
  net="name=eth0,bridge=${BRIDGE},ip=${IP},firewall=0"
  pct create "$CTID" "$TEMPLATE" \
    --hostname "$NAME" \
    --unprivileged 1 \
    --features nesting=1,keyctl=1 \
    --cores "$CORES" \
    --memory "$MEMORY" \
    --swap "$SWAP" \
    --rootfs "${STORAGE}:${DISK_GB}" \
    --onboot 1 \
    --ostype debian \
    --net0 "$net"
fi

if ! pct status "$CTID" | grep -q running; then
  pct start "$CTID"
fi

echo "waiting for network inside CT $CTID..."
for _ in $(seq 1 30); do
  if pct exec "$CTID" -- getent hosts deb.debian.org >/dev/null 2>&1; then
    break
  fi
  sleep 2
done

pct exec "$CTID" -- env \
  PROJECT_URL="$PROJECT_URL" \
  AUTHENTICATOR="$AUTHENTICATOR" \
  ACCOUNT_NAME="$ACCOUNT_NAME" \
  MAX_NCPUS_PCT="$MAX_NCPUS_PCT" \
  CPU_USAGE_LIMIT="$CPU_USAGE_LIMIT" \
  bash -s <<'GUEST'
set -euo pipefail
export DEBIAN_FRONTEND=noninteractive
export LC_ALL=C.UTF-8
export PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin"

apt-get update -qq
apt-get install -y -qq ca-certificates curl gnupg openssl docker.io

install -d -m 0755 /usr/share/keyrings
if [ ! -s /usr/share/keyrings/boinc.gpg ]; then
  curl -fsSL https://boinc.berkeley.edu/dl/linux/stable/trixie/boinc.gpg \
    | gpg --dearmor -o /usr/share/keyrings/boinc.gpg
  chmod 0644 /usr/share/keyrings/boinc.gpg
fi
arch="$(dpkg --print-architecture)"
cat >/etc/apt/sources.list.d/boinc.list <<EOF
deb [arch=${arch} signed-by=/usr/share/keyrings/boinc.gpg] https://boinc.berkeley.edu/dl/linux/stable/trixie trixie main
EOF

apt-get update -qq
apt-get install -y -qq boinc-client

ver="$(boinc --version | awk '{print $1}')"
dpkg --compare-versions "$ver" ge "8.2.13" || {
  echo "error: boinc-client $ver is older than 8.2.13" >&2
  exit 1
}

usermod -aG docker boinc
systemctl enable --now docker

install -d -m 0755 /etc/boinc-client
install -d -o boinc -g boinc /var/lib/boinc
cat >/etc/boinc-client/global_prefs_override.xml <<EOF
<global_preferences>
   <run_on_batteries>1</run_on_batteries>
   <run_if_user_active>1</run_if_user_active>
   <suspend_cpu_usage>0.000000</suspend_cpu_usage>
   <max_ncpus_pct>${MAX_NCPUS_PCT}.000000</max_ncpus_pct>
   <cpu_usage_limit>${CPU_USAGE_LIMIT}.000000</cpu_usage_limit>
   <ram_max_used_busy_pct>90.000000</ram_max_used_busy_pct>
   <ram_max_used_idle_pct>90.000000</ram_max_used_idle_pct>
</global_preferences>
EOF
chown boinc:boinc /etc/boinc-client/global_prefs_override.xml
ln -sf /etc/boinc-client/global_prefs_override.xml /var/lib/boinc/global_prefs_override.xml

if [ ! -f /etc/boinc-client/cc_config.xml ]; then
  cat >/etc/boinc-client/cc_config.xml <<'EOF'
<cc_config>
  <log_flags>
    <task>1</task>
    <sched_ops>1</sched_ops>
  </log_flags>
</cc_config>
EOF
  chown boinc:boinc /etc/boinc-client/cc_config.xml
  ln -sf /etc/boinc-client/cc_config.xml /var/lib/boinc/cc_config.xml
fi

systemctl enable --now boinc-client
systemctl restart boinc-client

cd /var/lib/boinc
for _ in $(seq 1 30); do
  boinccmd --get_state >/dev/null 2>&1 && break
  sleep 1
done

boinccmd --read_global_prefs_override

if boinccmd --get_project_status | grep -Fq "master URL: $PROJECT_URL"; then
  echo "already attached to $PROJECT_URL"
  boinccmd --project "$PROJECT_URL" update
else
  auth="$AUTHENTICATOR"
  if [ -z "$auth" ]; then
    email="boinc-pve-${ACCOUNT_NAME}-$(openssl rand -hex 6)@usnewsmap.invalid"
    passwd="$(openssl rand -hex 24)"
    out="$(boinccmd --create_account "$PROJECT_URL" "$email" "$passwd" "$ACCOUNT_NAME" 2>&1)"
    auth="$(printf '%s\n' "$out" | awk -F': ' '/account key/ {print $2}' | tr -d '[:space:]')"
    [[ "$auth" =~ ^[0-9a-f]{32,64}$ ]] || {
      echo "error: failed to obtain account key from boinccmd --create_account" >&2
      exit 1
    }
  fi
  boinccmd --project_attach "$PROJECT_URL" "$auth"
fi

boinccmd --get_project_status | grep -E 'name:|master URL:|user_name:|hostid:|tasks completed:' || true
GUEST
