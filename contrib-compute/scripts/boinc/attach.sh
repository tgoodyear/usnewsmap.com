#!/usr/bin/env bash
# Volunteer script: installs or detects the BOINC client, attaches it to the usnewsmap OCR
# project, and sets limits so it does not take over the machine. See docs/volunteers.md.
#
# Run as root (or with sudo) on Debian, Ubuntu or a RHEL-family Linux:
#
#   attach.sh --project-url URL --account-key-file FILE [--max-cores N] [--max-memory-mib N] \
#             [--run-if-idle] [--dry-run]
#   attach.sh --project-url URL --email ADDRESS --account-name NAME --password-file FILE ...
#   attach.sh --project-url URL --detach
#
# Every setting is a named flag. --dry-run prints each command and each file it would
# write, and runs nothing.
set -euo pipefail

SCRIPT_VERSION="0.1.0"
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/boinc-common.sh
. "$HERE/../lib/boinc-common.sh"
DATA_DIR_DEFAULT="/var/lib/boinc-client"

die() { echo "attach.sh: $*" >&2; exit 1; }
note() { echo "== $*"; }

usage() {
  cat <<'USAGE'
Usage: attach.sh --project-url URL (--account-key-file FILE |
                                    --email ADDRESS --account-name NAME --password-file FILE |
                                    --detach) [options]

Required:
  --project-url URL      project address, https:// with a trailing slash (from the maintainer)

Pick one way to identify yourself:
  --account-key-file F   file holding the account key (authenticator) from the maintainer.
                         The key is never a command-line argument or an environment variable.
  --email ADDRESS        create a new account. Needs --account-name and --password-file.
                         Works only if the project allows open sign-up.
  --detach               leave the project and stop

Options:
  --max-cores N          most CPU cores BOINC may use (default: half of this machine's cores)
  --max-memory-mib N     most memory BOINC may use, in MiB (default: 40 percent of RAM).
                         Each page in flight needs about 2.5 GiB; a work unit uses up to 2.5 GiB.
  --run-if-idle          compute only when you have not used the machine for 3 minutes
                         (default: compute whenever the machine is on)
  --data-dir DIR         BOINC data directory (default /var/lib/boinc-client)
  --dry-run              print what would happen, change nothing
  --help                 this text
  --version              print the script version
USAGE
}

project_url="" account_key="" account_key_file="" email="" account_name="" password_file=""
max_cores="" max_memory_mib="" run_if_idle=0 data_dir="$DATA_DIR_DEFAULT" dry_run=0 detach=0

need_value() { [ "$2" -ge 2 ] || die "$1 needs a value"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --project-url) need_value "$1" $#; project_url="$2"; shift 2 ;;
    --account-key-file) need_value "$1" $#; account_key_file="$2"; shift 2 ;;
    --email) need_value "$1" $#; email="$2"; shift 2 ;;
    --account-name) need_value "$1" $#; account_name="$2"; shift 2 ;;
    --password-file) need_value "$1" $#; password_file="$2"; shift 2 ;;
    --max-cores) need_value "$1" $#; max_cores="$2"; shift 2 ;;
    --max-memory-mib) need_value "$1" $#; max_memory_mib="$2"; shift 2 ;;
    --data-dir) need_value "$1" $#; data_dir="$2"; shift 2 ;;
    --run-if-idle) run_if_idle=1; shift ;;
    --detach) detach=1; shift ;;
    --dry-run) dry_run=1; shift ;;
    --help|-h) usage; exit 0 ;;
    --version) echo "$SCRIPT_VERSION"; exit 0 ;;
    *) usage >&2; die "unknown argument: $1" ;;
  esac
done

[ -n "$project_url" ] || die "--project-url is required"
case "$project_url" in https://*/) ;; *) die "--project-url must start with https:// and end with /" ;; esac

# run prints the command in dry-run mode and runs it otherwise.
run() {
  if [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] $*"
  else
    "$@"
  fi
}

# boinccmd runs boinccmd against the local client, from the data directory so it finds
# gui_rpc_auth.cfg. The key goes in as an argument; it is not echoed in dry-run mode.
boinccmd() { run command boinccmd --host localhost "$@"; }

uint() { case "$2" in ''|*[!0-9]*) die "$1 must be a whole number" ;; esac; }

if [ "$dry_run" -eq 0 ] && [ "$(id -u)" -ne 0 ]; then
  die "run as root (use sudo), or pass --dry-run to see the plan"
fi

# --- identity choice -------------------------------------------------------------
if [ "$detach" -eq 0 ]; then
  modes=0
  [ -n "$account_key_file" ] && modes=$((modes + 1))
  [ -n "$email" ] && modes=$((modes + 1))
  [ "$modes" -eq 1 ] || die "give exactly one of --account-key-file, --email"
  if [ -n "$email" ]; then
    [ -n "$account_name" ] || die "--email needs --account-name"
    [ -n "$password_file" ] || die "--email needs --password-file"
    if [ "$dry_run" -eq 0 ]; then [ -r "$password_file" ] || die "cannot read $password_file"; fi
  fi
  if [ -n "$account_key_file" ] && [ "$dry_run" -eq 0 ]; then
    [ -r "$account_key_file" ] || die "cannot read $account_key_file"
    account_key="$(tr -d '[:space:]' < "$account_key_file")"
  fi
fi

# --- limits ---------------------------------------------------------------------
total_cores=1
if command -v nproc >/dev/null 2>&1; then total_cores="$(nproc)"; fi
total_mem_mib=4096
if [ -r /proc/meminfo ]; then
  mem_kb=0
  while read -r name kb _; do
    if [ "$name" = "MemTotal:" ]; then mem_kb=$kb; break; fi
  done < /proc/meminfo
  [ "$mem_kb" -gt 0 ] && total_mem_mib=$((mem_kb / 1024))
fi
[ -n "$max_cores" ] || max_cores=$(( (total_cores + 1) / 2 ))
uint --max-cores "$max_cores"
[ "$max_cores" -ge 1 ] || die "--max-cores must be at least 1"
[ "$max_cores" -le "$total_cores" ] || max_cores="$total_cores"
cpu_pct=$(( max_cores * 100 / total_cores ))

if [ -n "$max_memory_mib" ]; then
  uint --max-memory-mib "$max_memory_mib"
  [ "$max_memory_mib" -ge 2560 ] || die "--max-memory-mib below 2560 cannot run a work unit"
  mem_pct=$(( max_memory_mib * 100 / total_mem_mib ))
else
  mem_pct=40
  max_memory_mib=$(( total_mem_mib * mem_pct / 100 ))
fi
[ "$mem_pct" -le 100 ] || mem_pct=100
[ "$mem_pct" -ge 1 ] || mem_pct=1
if [ "$max_memory_mib" -lt 2560 ] && [ "$detach" -eq 0 ]; then
  echo "attach.sh: warning: $max_memory_mib MiB is under the 2560 MiB a work unit needs;" >&2
  echo "attach.sh: BOINC will fetch no work until you raise --max-memory-mib." >&2
fi

# --- client install -------------------------------------------------------------
install_client() {
  if command -v boinccmd >/dev/null 2>&1 && command -v boinc >/dev/null 2>&1; then
    note "BOINC client already installed"
    return
  fi
  if [ ! -r /etc/os-release ]; then
    [ "$dry_run" -eq 1 ] && { echo "[dry-run] no /etc/os-release here; would pick apt or dnf on the target"; return; }
    die "cannot detect the distribution: no /etc/os-release"
  fi
  # shellcheck disable=SC1091
  . /etc/os-release
  case " ${ID:-} ${ID_LIKE:-} " in
    *" debian "*|*" ubuntu "*)
      note "installing boinc-client with apt"
      run env DEBIAN_FRONTEND=noninteractive apt-get update
      run env DEBIAN_FRONTEND=noninteractive apt-get install -y boinc-client ;;
    *" rhel "*|*" fedora "*|*" centos "*)
      # Fedora packages boinc-client. Whether Rocky, Alma or RHEL carry it (possibly via EPEL)
      # is not confirmed in docs/verified-facts.md, so the script says so if it fails.
      note "installing boinc-client with dnf"
      if ! run dnf install -y boinc-client; then
        die "dnf could not install boinc-client. Enable EPEL, or install the BOINC client yourself and run this script again"
      fi ;;
    *) die "unsupported distribution (${ID:-unknown}). Install the BOINC client yourself and run this script again" ;;
  esac
}

service_name="boinc-client"
start_client() {
  note "starting $service_name"
  run systemctl enable --now "$service_name"
}

# --- preferences ----------------------------------------------------------------
write_prefs() {
  local file="$data_dir/global_prefs_override.xml"
  note "writing limits to $file: $max_cores core(s), $mem_pct percent of RAM (about $max_memory_mib MiB)"
  local body
  body="$(boinc_prefs_xml "$cpu_pct" "$run_if_idle" 2 "$mem_pct" "$mem_pct" 0)"
  if [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] write $file:"
    echo "$body"
  else
    umask 022
    printf '%s\n' "$body" > "$file"
  fi
  boinccmd --read_global_prefs_override
}

# --- attach ---------------------------------------------------------------------
attach() {
  if [ -n "$email" ]; then
    note "creating an account for $account_name"
    local pw=""
    if [ "$dry_run" -eq 0 ]; then pw="$(tr -d '\n' < "$password_file")"; else pw="<password from file>"; fi
    boinccmd --create_account "$project_url" "$email" "$pw" "$account_name"
    note "now run: boinccmd --lookup_account $project_url $email <password>, then attach with the key it prints"
    note "(or pass --account-key-file next time). Not attaching in this run."
    return
  fi
  note "attaching to $project_url"
  # The key goes in as a file the client reads at start, so it is never in a process list:
  # account_<name from boinc_account_filename>.xml in the data directory.
  local acct
  acct="$data_dir/$(boinc_account_filename "$project_url")"
  if [ "$dry_run" -eq 1 ]; then
    echo "[dry-run] write $acct (mode 600, owner boinc) with the account key from $account_key_file"
    echo "[dry-run] systemctl restart $service_name"
    return
  fi
  ( umask 077
    boinc_account_xml "$project_url" "$account_key" > "$acct" )
  chown boinc:boinc "$acct" 2>/dev/null || true
  systemctl restart "$service_name"
  sleep 5
  if ! boinccmd --host localhost --get_project_status 2>/dev/null | grep -q "$project_url"; then
    die "the client did not pick up $acct. The file name rule is UNVERIFIED (docs/verified-facts.md); check the BOINC data directory for how it names account files"
  fi
}

print_detach() {
  cat <<DETACH

To stop and leave the project later:
  sudo boinccmd --project $project_url detach
or run this script again with:  --project-url $project_url --detach
To stop BOINC entirely:  sudo systemctl disable --now $service_name
DETACH
}

if [ "$detach" -eq 1 ]; then
  note "detaching from $project_url"
  if [ "$dry_run" -eq 0 ]; then cd "$data_dir"; fi
  boinccmd --project "$project_url" detach
  exit 0
fi

install_client
start_client
# boinccmd finds gui_rpc_auth.cfg in its working directory.
if [ "$dry_run" -eq 0 ]; then cd "$data_dir"; fi
write_prefs
attach
note "check progress with:  boinccmd --get_state   or   boinccmd --get_tasks"
print_detach
