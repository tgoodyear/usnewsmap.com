# Shared helpers for the three host routes (Proxmox container, docker/podman
# container, Azure). Source this file; it runs nothing by itself.
#
# This file is the one place the client preference values live. The doc
# docs/interface.md points here instead of repeating them.
# scripts/azure/boinc-config.sh is embedded into Bicep and cloud-init and cannot
# source this file; scripts/check-prefs-consistent.sh (run in CI) keeps its copy equal.
# shellcheck shell=bash

# One page in flight needs about 2.5 GiB (see docs/design.md, section 5.1).
BOINC_MEM_PER_TASK_MIB=2560

boinc_is_int() { [[ $1 =~ ^[1-9][0-9]*$ ]]; }
boinc_in_range() { boinc_is_int "$1" && [ "$1" -ge "$2" ] && [ "$1" -le "$3" ]; }

# https://host[:port][/path/] with a trailing slash, as BOINC wants for a master URL.
# Lower case only: the client lower-cases master URLs (lib/url.cpp,
# canonicalize_master_url), and the account file name must match.
boinc_valid_project_url() {
  [[ $1 =~ ^https://[a-z0-9.-]+(:[0-9]{1,5})?(/[a-z0-9._~-]+)*/$ ]]
}

# Tasks that run at once: memory / 2.5 GiB, at most the cores.
boinc_concurrency() { # cores memory_mib
  local n=$(($2 / BOINC_MEM_PER_TASK_MIB))
  [ "$n" -le "$1" ] || n=$1
  [ "$n" -ge 1 ] || n=1
  echo "$n"
}

# Name of the account file the BOINC client reads from its data directory.
# Rule from lib/url.cpp (escape_url_readable, escape_project_url) and
# client/file_names.cpp: drop "scheme://", turn every character that is not
# a letter, digit, '.', '-' or '_' into '_', drop one trailing '_'.
boinc_account_filename() { # master_url
  local u=${1#*://}
  u=${u//[^A-Za-z0-9._-]/_}
  u=${u%_}
  echo "account_${u}.xml"
}

# The account file. Pre-attaches the client to the project at its next start,
# so the key never appears on a command line (boinccmd --project_attach would
# put it in argv, visible to every process list).
boinc_account_xml() { # master_url account_key
  printf '<account>\n    <master_url>%s</master_url>\n    <authenticator>%s</authenticator>\n</account>\n' "$1" "$2"
}

# Read a key from a file: the first line, whitespace removed, 8 to 128
# letters and digits (BOINC authenticators are hex, 32 characters, but this
# does not insist on it). Prints it. Fails with a message on stderr.
boinc_read_key_file() { # path
  local f=$1 key=""
  [ -f "$f" ] || { echo "account key file not found: $f" >&2; return 1; }
  [ -r "$f" ] || { echo "account key file not readable: $f" >&2; return 1; }
  IFS= read -r key < "$f" || true
  key=${key//[[:space:]]/}
  [[ $key =~ ^[A-Za-z0-9]{8,128}$ ]] || { echo "account key file $f: first line is not a key" >&2; return 1; }
  printf '%s' "$key"
}

# global_prefs_override.xml. Defaults are for a dedicated host (container, CT, VM).
# A plain machine (scripts/boinc/attach.sh) passes lower RAM limits and run_on_batteries 0.
boinc_prefs_xml() { # ncpus_pct run_if_idle(0|1) max_disk_gib [ram_busy_pct=80 ram_idle_pct=90 run_on_batteries=1]
  local pct=$1 idle=$2 disk=$3 ram_busy=${4:-80} ram_idle=${5:-90} battery=${6:-1}
  local active=1 idle_min=0
  if [ "$idle" -eq 1 ]; then active=0; idle_min=3; fi
  cat <<XML
<global_preferences>
    <max_ncpus_pct>$pct</max_ncpus_pct>
    <cpu_usage_limit>100</cpu_usage_limit>
    <run_if_user_active>$active</run_if_user_active>
    <idle_time_to_run>$idle_min</idle_time_to_run>
    <run_on_batteries>$battery</run_on_batteries>
    <ram_max_used_busy_pct>$ram_busy</ram_max_used_busy_pct>
    <ram_max_used_idle_pct>$ram_idle</ram_max_used_idle_pct>
    <disk_max_used_gb>$disk</disk_max_used_gb>
    <work_buf_min_days>0.1</work_buf_min_days>
    <work_buf_additional_days>0.25</work_buf_additional_days>
    <suspend_cpu_usage>0</suspend_cpu_usage>
</global_preferences>
XML
}

# Percentage of the CPUs BOINC sees that gives `tasks` running at once (rounded up).
boinc_ncpus_pct() { # tasks cpus_seen
  local p=$((($1 * 100 + $2 - 1) / $2))
  [ "$p" -le 100 ] || p=100
  echo "$p"
}
