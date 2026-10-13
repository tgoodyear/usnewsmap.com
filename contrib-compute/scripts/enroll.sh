#!/usr/bin/env bash
# Creates an unprivileged LXC from the stock Debian template on this Proxmox
# host, installs Debian's boinc-client package in it, and attaches it to the
# usnewsmap BOINC project. Nothing custom is built or downloaded from us.
# See docs/proxmox-contributors.md and docs/interface.md.
#
# Run as root on the Proxmox host:
#
#   enroll.sh --project-url https://<project-host>/<project>/ \
#     --account-key-file /root/usnm-account-key \
#     --ctid 9100 --storage local-lvm --bridge vmbr0 --cores 4 --memory-mib 10240 \
#     --block-cidr <your LAN range> [--block-cidr <another>] [--dry-run]
#
# Every setting is a named flag. --dry-run prints each command and every file
# it would write (the key is shown as <key>), and runs none of them.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/boinc-common.sh
. "$HERE/lib/boinc-common.sh"

die() { echo "enroll.sh: $*" >&2; exit 1; }
note() { echo "== $*"; }

usage() {
  cat <<'USAGE'
Usage: enroll.sh --project-url URL --account-key-file PATH --ctid N --storage NAME
                 --bridge NAME (--block-cidr CIDR ... | --no-lan-block) [options]

Required:
  --project-url URL        the BOINC project address the maintainer gave you
                           (https://, ends with a slash)
  --account-key-file PATH  file whose first line is your BOINC account key. The key
                           is copied into the container as a root-readable file and
                           never put on a command line
  --ctid N                 new container id, a whole number from 100 up; must not exist
  --storage NAME           Proxmox storage for the container disk
  --bridge NAME            bridge for the container's network, for example vmbr0
  --block-cidr CIDR        destination the container must not reach; repeat for each
                           of your LAN ranges. Or pass --no-lan-block (not advised).

Options:
  --hostname NAME          container hostname (default usnm-contrib-<ctid>)
  --cores N                CPU cores for the container, 1 to 64 (default 2)
  --memory-mib N           memory in MiB, 2560 to 262144 (default 6144). Each task needs
                           about 2.5 GiB, so tasks at once = memory / 2560, at most cores.
  --disk-gib N             root disk in GiB, 8 to 2048 (default 20)
  --max-disk-gib N         most disk BOINC may use for project files, 1 to 2048
                           (default 5; must be less than --disk-gib)
  --run-if-idle            compute only while the container is idle (default: always)
  --template-storage N     storage that holds container templates (default local)
  --template NAME          template file name (default: newest debian-12-standard)
  --dry-run                print every command and file; run nothing
  --help                   show this text
USAGE
}

DRY=0
PROJECT_URL="" KEY_FILE="" CTID="" HOSTNAME_="" STORAGE="" BRIDGE=""
CORES=2 MEMORY_MIB=6144 DISK_GIB=20 MAX_DISK_GIB=5 RUN_IF_IDLE=0
TEMPLATE_STORAGE=local TEMPLATE="" NO_LAN_BLOCK=0
BLOCK_CIDRS=()

need_arg() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a value"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --project-url) need_arg "$@"; PROJECT_URL=$2; shift 2 ;;
    --account-key-file) need_arg "$@"; KEY_FILE=$2; shift 2 ;;
    --ctid) need_arg "$@"; CTID=$2; shift 2 ;;
    --hostname) need_arg "$@"; HOSTNAME_=$2; shift 2 ;;
    --storage) need_arg "$@"; STORAGE=$2; shift 2 ;;
    --bridge) need_arg "$@"; BRIDGE=$2; shift 2 ;;
    --cores) need_arg "$@"; CORES=$2; shift 2 ;;
    --memory-mib) need_arg "$@"; MEMORY_MIB=$2; shift 2 ;;
    --disk-gib) need_arg "$@"; DISK_GIB=$2; shift 2 ;;
    --max-disk-gib) need_arg "$@"; MAX_DISK_GIB=$2; shift 2 ;;
    --run-if-idle) RUN_IF_IDLE=1; shift ;;
    --template-storage) need_arg "$@"; TEMPLATE_STORAGE=$2; shift 2 ;;
    --template) need_arg "$@"; TEMPLATE=$2; shift 2 ;;
    --block-cidr) need_arg "$@"; BLOCK_CIDRS+=("$2"); shift 2 ;;
    --no-lan-block) NO_LAN_BLOCK=1; shift ;;
    --dry-run) DRY=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; die "unknown or positional argument: $1 (every setting is a --flag)" ;;
  esac
done

# ---- validation -------------------------------------------------------------
[ -n "$PROJECT_URL" ] || die "--project-url is required"
boinc_valid_project_url "$PROJECT_URL" || die "--project-url must be https://host[:port]/[path/] and end with a slash"
[ -n "$KEY_FILE" ] || die "--account-key-file is required"
if [ "$DRY" -eq 1 ]; then
  KEY="<key>" # a dry run never prints the key
  [ ! -f "$KEY_FILE" ] || boinc_read_key_file "$KEY_FILE" > /dev/null || exit 1
else
  KEY=$(boinc_read_key_file "$KEY_FILE") || exit 1
  # Tell the contributor if the file is readable by others; it is the account's only secret.
  if [ -n "$(find "$KEY_FILE" -maxdepth 0 -perm /077 2> /dev/null)" ]; then
    echo "WARNING: $KEY_FILE is readable by other users; run: chmod 600 $KEY_FILE" >&2
  fi
fi
boinc_in_range "${CTID:-x}" 100 999999999 || die "--ctid must be a whole number from 100 up"
HOSTNAME_=${HOSTNAME_:-usnm-contrib-$CTID}
[[ $HOSTNAME_ =~ ^[a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?$ ]] || die "--hostname must be a lowercase DNS label"
[[ $STORAGE =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "--storage is required (a Proxmox storage name)"
[[ $TEMPLATE_STORAGE =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "--template-storage is not a storage name"
[[ $BRIDGE =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] || die "--bridge is required (for example vmbr0)"
boinc_in_range "$CORES" 1 64 || die "--cores must be a whole number from 1 to 64"
boinc_in_range "$MEMORY_MIB" "$BOINC_MEM_PER_TASK_MIB" 262144 \
  || die "--memory-mib must be a whole number from $BOINC_MEM_PER_TASK_MIB to 262144 (one task needs about 2.5 GiB)"
boinc_in_range "$DISK_GIB" 8 2048 || die "--disk-gib must be a whole number from 8 to 2048"
boinc_in_range "$MAX_DISK_GIB" 1 2048 || die "--max-disk-gib must be a whole number from 1 to 2048"
[ "$MAX_DISK_GIB" -lt "$DISK_GIB" ] || die "--max-disk-gib must be less than --disk-gib (the system needs room too)"
[ -z "$TEMPLATE" ] || [[ $TEMPLATE =~ ^[A-Za-z0-9._+-]+\.tar\.(zst|gz|xz)$ ]] || die "--template is not a template file name"
if [ "${#BLOCK_CIDRS[@]}" -eq 0 ] && [ "$NO_LAN_BLOCK" -eq 0 ]; then
  die "pass --block-cidr for each of your LAN ranges (the container must not reach them), or --no-lan-block"
fi
for c in "${BLOCK_CIDRS[@]+"${BLOCK_CIDRS[@]}"}"; do
  [[ $c =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}/[0-9]{1,2}$ || $c =~ ^[0-9a-fA-F:]+:[0-9a-fA-F:]*/[0-9]{1,3}$ ]] \
    || die "--block-cidr $c is not a CIDR such as <network>/<prefix>"
done

CONCURRENCY=$(boinc_concurrency "$CORES" "$MEMORY_MIB")

# ---- command runner ---------------------------------------------------------
run() {
  if [ "$DRY" -eq 1 ]; then
    local out="+" a
    for a in "$@"; do
      if [[ $a =~ ^[A-Za-z0-9_@%+=:,./-]+$ ]]; then out+=" $a"; else out+=" $(printf '%q' "$a")"; fi
    done
    echo "$out"
  else
    "$@"
  fi
}

if [ "$DRY" -eq 1 ]; then
  note "dry run: nothing below is executed, and checks that need Proxmox are skipped"
else
  [ "$(id -u)" -eq 0 ] || die "run as root on the Proxmox host"
  if ! command -v pct > /dev/null || ! command -v pveam > /dev/null; then
    die "pct and pveam not found; this is not a Proxmox host"
  fi
  if pct status "$CTID" > /dev/null 2>&1 || [ -e "/etc/pve/lxc/$CTID.conf" ] || [ -e "/etc/pve/qemu-server/$CTID.conf" ]; then
    die "id $CTID already exists; pick another --ctid"
  fi
fi

note "plan: container $CTID ($HOSTNAME_), $CORES cores, $MEMORY_MIB MiB, $DISK_GIB GiB on $STORAGE; $CONCURRENCY task(s) at once"

# ---- template ---------------------------------------------------------------
note "template (stock Debian 12 from Proxmox's own template list)"
run pveam update
if [ -z "$TEMPLATE" ]; then
  if [ "$DRY" -eq 1 ]; then
    echo "+ pveam available --section system   (newest debian-12-standard_*_amd64.tar.zst is chosen)"
    TEMPLATE="debian-12-standard_NEWEST_amd64.tar.zst"
  else
    TEMPLATE=$(pveam available --section system | awk '{print $2}' | grep -E '^debian-12-standard_.*_amd64\.tar\.(zst|gz)$' | sort -V | tail -n 1 || true)
    [ -n "$TEMPLATE" ] || die "no debian-12-standard template listed; pass --template"
  fi
fi
if [ "$DRY" -eq 1 ] || ! pveam list "$TEMPLATE_STORAGE" | grep -q "$TEMPLATE"; then
  run pveam download "$TEMPLATE_STORAGE" "$TEMPLATE"
fi

# ---- container --------------------------------------------------------------
note "container"
# No password, no ssh keys, no bind mounts, no nesting: the native app needs
# only the BOINC client, so none of podman's extra features are switched on.
# Enter the container with 'pct enter'.
run pct create "$CTID" "$TEMPLATE_STORAGE:vztmpl/$TEMPLATE" \
  --hostname "$HOSTNAME_" --unprivileged 1 \
  --cores "$CORES" --memory "$MEMORY_MIB" --swap 0 \
  --rootfs "$STORAGE:$DISK_GIB" \
  --net0 "name=eth0,bridge=$BRIDGE,ip=dhcp,firewall=1" \
  --ostype debian --onboot 1 --tags usnm-contrib --start 0

note "container firewall"
FW_FILE="/etc/pve/firewall/$CTID.fw"
FW_TEXT="[OPTIONS]
enable: 1
policy_in: DROP
policy_out: ACCEPT
log_level_in: nolog
log_level_out: nolog

[RULES]"
for c in "${BLOCK_CIDRS[@]+"${BLOCK_CIDRS[@]}"}"; do
  FW_TEXT+="
OUT DROP -dest $c -log nolog # no access to the contributor's LAN"
done
if [ "$DRY" -eq 1 ]; then
  echo "+ write $FW_FILE:"
  printf '    %s\n' "${FW_TEXT//$'\n'/$'\n    '}"
else
  printf '%s\n' "$FW_TEXT" > "$FW_FILE"
  # The container firewall only takes effect when the host firewall is on.
  if command -v pve-firewall > /dev/null && ! pve-firewall status 2> /dev/null | grep -q 'Status: enabled'; then
    echo "WARNING: the Proxmox firewall is not enabled at datacenter level; $FW_FILE has no effect until it is." >&2
  fi
fi

note "start"
run pct start "$CTID"
if [ "$DRY" -eq 0 ]; then
  for _ in $(seq 1 30); do
    pct exec "$CTID" -- getent hosts deb.debian.org > /dev/null 2>&1 && break
    sleep 2
  done
  pct exec "$CTID" -- getent hosts deb.debian.org > /dev/null 2>&1 \
    || die "the container has no working network or DNS; fix the bridge, then run: pct destroy $CTID"
fi

# ---- BOINC client from the Debian package -----------------------------------
note "install boinc-client (Debian package, no recommended extras)"
run pct exec "$CTID" -- env DEBIAN_FRONTEND=noninteractive apt-get update -q
run pct exec "$CTID" -- env DEBIAN_FRONTEND=noninteractive apt-get install -y -q --no-install-recommends boinc-client
# The package starts the service on install. Stop it so the account file and
# preferences are in place before the client's first run with them.
run pct exec "$CTID" -- systemctl stop boinc-client

note "settings and account file"
ACCT_NAME=$(boinc_account_filename "$PROJECT_URL")
if [ "$DRY" -eq 1 ]; then
  PCT_CPU="<$CONCURRENCY task(s) as a percentage of the CPUs the container shows>"
  PREFS=$(boinc_prefs_xml 100 "$RUN_IF_IDLE" "$MAX_DISK_GIB" | sed "s|<max_ncpus_pct>100<|<max_ncpus_pct>$PCT_CPU<|")
  echo "+ read the CPU count inside the container: nproc"
  echo "+ write <boinc data dir>/global_prefs_override.xml (0644, boinc:boinc):"
  printf '    %s\n' "${PREFS//$'\n'/$'\n    '}"
  echo "+ write <boinc data dir>/$ACCT_NAME (0600, boinc:boinc):"
  boinc_account_xml "$PROJECT_URL" "$KEY" | sed 's/^/    /'
  echo "+ (the BOINC data dir is the home directory of the container's 'boinc' user)"
else
  CPUS_SEEN=$(pct exec "$CTID" -- nproc)
  [[ $CPUS_SEEN =~ ^[1-9][0-9]*$ ]] || die "could not read the CPU count inside the container"
  PCT_CPU=$(boinc_ncpus_pct "$CONCURRENCY" "$CPUS_SEEN")
  DATA_DIR=$(pct exec "$CTID" -- getent passwd boinc | cut -d: -f6)
  [[ $DATA_DIR =~ ^/[A-Za-z0-9._/-]+$ ]] || die "could not find the boinc user's data directory"
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  chmod 700 "$TMP"
  boinc_prefs_xml "$PCT_CPU" "$RUN_IF_IDLE" "$MAX_DISK_GIB" > "$TMP/prefs.xml"
  (umask 077; boinc_account_xml "$PROJECT_URL" "$KEY" > "$TMP/account.xml")
  pct push "$CTID" "$TMP/prefs.xml" /root/usnm-prefs.xml --perms 0600 --user 0 --group 0
  pct push "$CTID" "$TMP/account.xml" /root/usnm-account.xml --perms 0600 --user 0 --group 0
  pct exec "$CTID" -- install -o boinc -g boinc -m 0644 /root/usnm-prefs.xml "$DATA_DIR/global_prefs_override.xml"
  pct exec "$CTID" -- install -o boinc -g boinc -m 0600 /root/usnm-account.xml "$DATA_DIR/$ACCT_NAME"
  pct exec "$CTID" -- rm -f /root/usnm-prefs.xml /root/usnm-account.xml
  rm -rf "$TMP"
fi

note "start the client"
run pct exec "$CTID" -- systemctl enable --now boinc-client

note "done"
cat <<EOF
The container is running the BOINC client, attached to $PROJECT_URL.
Check it:
  pct exec $CTID -- boinccmd --get_project_status
  pct exec $CTID -- boinccmd --get_tasks
Stop it for now:      pct stop $CTID
Remove everything:    pct stop $CTID; pct destroy $CTID --purge
(Then delete /etc/pve/firewall/$CTID.fw if it is still there.)
EOF
