#!/usr/bin/env bash
# Runs the official BOINC client container (boinc/client) on any machine with
# Docker or Podman, attached to the usnewsmap BOINC project. No image of ours:
# the script only adds limits, the account file, a restart policy and a
# reminder of how to block your LAN. See docs/container-contributors.md.
#
#   run-container.sh --project-url https://<project-host>/<project>/ \
#     --account-key-file ~/usnm-account-key --cores 4 --memory-mib 10240 \
#     --block-cidr <your LAN range> [--engine docker|podman] [--dry-run]
#   run-container.sh --status | --stop | --remove [--purge]
#
# Every setting is a named flag. --dry-run prints each command and file (the key
# is shown as <key>) and runs nothing.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# shellcheck source=scripts/lib/boinc-common.sh
. "$HERE/lib/boinc-common.sh"

die() { echo "run-container.sh: $*" >&2; exit 1; }
note() { echo "== $*"; }

usage() {
  cat <<'USAGE'
Usage: run-container.sh --project-url URL --account-key-file PATH
                        (--block-cidr CIDR ... | --no-lan-block) [options]
       run-container.sh (--status | --stop | --remove [--purge]) [--name NAME] [--engine E]

Required to start:
  --project-url URL        BOINC project address from the maintainer (https://, ends with /)
  --account-key-file PATH  file whose first line is your BOINC account key
  --block-cidr CIDR        LAN range the container must not reach; repeat. This script only
                           prints the firewall rules for it (see the doc). Or --no-lan-block.

Options:
  --engine E               docker, podman or auto (default auto: docker, then podman)
  --name NAME              container name (default usnm-contrib)
  --cores N                CPU limit, 1 to 256 (default 2)
  --memory-mib N           memory limit in MiB, 2560 to 1048576 (default 6144). Tasks at
                           once = memory / 2560, at most cores
  --max-disk-gib N         most disk BOINC may use for project files (default 5)
  --run-if-idle            compute only when the machine is idle (default: always)
  --image REF              image (default boinc/client:latest)
  --image-digest sha256:H  run exactly this digest of the image repository; the pull fails
                           if it does not match
  --image-archive PATH     load the image from a docker/podman save archive instead of
                           pulling; needs --archive-sha256
  --archive-sha256 HEX     sha256 of that archive
  --quadlet                podman only: run as a systemd service (Quadlet) so it starts at
                           boot; otherwise a restart policy is used
  --no-hardening           do not drop capabilities / use a read-only root file system
  --dry-run                print every command and file; run nothing
Actions:
  --status                 show the container and the client's project status
  --stop                   stop the container (keeps data)
  --remove                 stop and remove the container (keeps data); --purge also deletes
                           the data volume and the network
  --help                   show this text
USAGE
}

DRY=0 ACTION=start ENGINE=auto NAME=usnm-contrib
PROJECT_URL="" KEY_FILE="" CORES=2 MEMORY_MIB=6144 MAX_DISK_GIB=5 RUN_IF_IDLE=0
IMAGE="boinc/client:latest" IMAGE_DIGEST="" ARCHIVE="" ARCHIVE_SHA=""
QUADLET=0 HARDEN=1 PURGE=0 NO_LAN_BLOCK=0
BLOCK_CIDRS=()

need_arg() { [ "$#" -ge 2 ] && [ -n "$2" ] || die "$1 needs a value"; }

while [ $# -gt 0 ]; do
  case "$1" in
    --project-url) need_arg "$@"; PROJECT_URL=$2; shift 2 ;;
    --account-key-file) need_arg "$@"; KEY_FILE=$2; shift 2 ;;
    --engine) need_arg "$@"; ENGINE=$2; shift 2 ;;
    --name) need_arg "$@"; NAME=$2; shift 2 ;;
    --cores) need_arg "$@"; CORES=$2; shift 2 ;;
    --memory-mib) need_arg "$@"; MEMORY_MIB=$2; shift 2 ;;
    --max-disk-gib) need_arg "$@"; MAX_DISK_GIB=$2; shift 2 ;;
    --run-if-idle) RUN_IF_IDLE=1; shift ;;
    --image) need_arg "$@"; IMAGE=$2; shift 2 ;;
    --image-digest) need_arg "$@"; IMAGE_DIGEST=$2; shift 2 ;;
    --image-archive) need_arg "$@"; ARCHIVE=$2; shift 2 ;;
    --archive-sha256) need_arg "$@"; ARCHIVE_SHA=$2; shift 2 ;;
    --block-cidr) need_arg "$@"; BLOCK_CIDRS+=("$2"); shift 2 ;;
    --no-lan-block) NO_LAN_BLOCK=1; shift ;;
    --quadlet) QUADLET=1; shift ;;
    --no-hardening) HARDEN=0; shift ;;
    --purge) PURGE=1; shift ;;
    --status) ACTION=status; shift ;;
    --stop) ACTION=stop; shift ;;
    --remove) ACTION=remove; shift ;;
    --dry-run) DRY=1; shift ;;
    --help|-h) usage; exit 0 ;;
    *) usage >&2; die "unknown or positional argument: $1 (every setting is a --flag)" ;;
  esac
done

[[ $NAME =~ ^[a-zA-Z0-9][a-zA-Z0-9_.-]{0,62}$ ]] || die "--name is not a valid container name"
VOLUME="$NAME-data"
NETWORK="$NAME-net"

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

# ---- engine -----------------------------------------------------------------
case "$ENGINE" in
  auto)
    if command -v docker > /dev/null; then ENGINE=docker
    elif command -v podman > /dev/null; then ENGINE=podman
    elif [ "$DRY" -eq 1 ]; then ENGINE=docker
    else die "neither docker nor podman found; install one or pass --engine"; fi ;;
  docker|podman) ;;
  *) die "--engine must be docker, podman or auto" ;;
esac
if [ "$DRY" -eq 0 ] && ! command -v "$ENGINE" > /dev/null; then die "$ENGINE not found"; fi
[ "$QUADLET" -eq 0 ] || [ "$ENGINE" = podman ] || die "--quadlet needs --engine podman"

# ---- actions that do not start anything -------------------------------------
quadlet_file() {
  if [ "$(id -u)" -eq 0 ]; then echo "/etc/containers/systemd/$NAME.container"
  else echo "${XDG_CONFIG_HOME:-$HOME/.config}/containers/systemd/$NAME.container"; fi
}
sysctl_user() { if [ "$(id -u)" -eq 0 ]; then echo systemctl; else echo "systemctl --user"; fi; }

case "$ACTION" in
  status)
    run "$ENGINE" ps -a --filter "name=^$NAME\$"
    run "$ENGINE" exec "$NAME" boinccmd --get_project_status
    run "$ENGINE" exec "$NAME" boinccmd --get_tasks
    exit 0 ;;
  stop)
    if [ -f "$(quadlet_file)" ]; then
      # shellcheck disable=SC2046
      run $(sysctl_user) stop "$NAME.service"
    else
      run "$ENGINE" stop "$NAME"
    fi
    exit 0 ;;
  remove)
    if [ -f "$(quadlet_file)" ]; then
      # shellcheck disable=SC2046
      run $(sysctl_user) stop "$NAME.service"
      run rm -f "$(quadlet_file)"
      # shellcheck disable=SC2046
      run $(sysctl_user) daemon-reload
    else
      run "$ENGINE" rm -f "$NAME"
    fi
    if [ "$PURGE" -eq 1 ]; then
      run "$ENGINE" volume rm "$VOLUME"
      run "$ENGINE" network rm "$NETWORK"
    else
      echo "Data volume $VOLUME kept. Add --purge to delete it and the network."
    fi
    exit 0 ;;
esac

# ---- validation for start ---------------------------------------------------
[ -n "$PROJECT_URL" ] || die "--project-url is required"
boinc_valid_project_url "$PROJECT_URL" || die "--project-url must be https://host[:port]/[path/] in lower case and end with a slash"
[ -n "$KEY_FILE" ] || die "--account-key-file is required"
if [ "$DRY" -eq 1 ]; then
  KEY="<key>"
  [ ! -f "$KEY_FILE" ] || boinc_read_key_file "$KEY_FILE" > /dev/null || exit 1
else
  KEY=$(boinc_read_key_file "$KEY_FILE") || exit 1
  if [ -n "$(find "$KEY_FILE" -maxdepth 0 -perm /077 2> /dev/null)" ]; then
    echo "WARNING: $KEY_FILE is readable by other users; run: chmod 600 $KEY_FILE" >&2
  fi
fi
boinc_in_range "$CORES" 1 256 || die "--cores must be a whole number from 1 to 256"
boinc_in_range "$MEMORY_MIB" "$BOINC_MEM_PER_TASK_MIB" 1048576 \
  || die "--memory-mib must be a whole number from $BOINC_MEM_PER_TASK_MIB to 1048576"
boinc_in_range "$MAX_DISK_GIB" 1 2048 || die "--max-disk-gib must be a whole number from 1 to 2048"
[[ $IMAGE =~ ^[a-z0-9][a-z0-9._/:-]*$ ]] || die "--image is not an image reference"
[ -z "$IMAGE_DIGEST" ] || [[ $IMAGE_DIGEST =~ ^sha256:[0-9a-f]{64}$ ]] || die "--image-digest must be sha256: and 64 hex digits"
if [ -n "$ARCHIVE" ]; then
  [[ $ARCHIVE_SHA =~ ^[0-9a-f]{64}$ ]] || die "--image-archive needs --archive-sha256 (64 lowercase hex digits)"
  [ "$DRY" -eq 1 ] || [ -f "$ARCHIVE" ] || die "archive not found: $ARCHIVE"
fi
if [ "${#BLOCK_CIDRS[@]}" -eq 0 ] && [ "$NO_LAN_BLOCK" -eq 0 ]; then
  die "pass --block-cidr for each of your LAN ranges, or --no-lan-block"
fi
for c in "${BLOCK_CIDRS[@]+"${BLOCK_CIDRS[@]}"}"; do
  [[ $c =~ ^[0-9]{1,3}(\.[0-9]{1,3}){3}/[0-9]{1,2}$ || $c =~ ^[0-9a-fA-F:]+:[0-9a-fA-F:]*/[0-9]{1,3}$ ]] \
    || die "--block-cidr $c is not a CIDR such as <network>/<prefix>"
done
CONCURRENCY=$(boinc_concurrency "$CORES" "$MEMORY_MIB")

REF=$IMAGE
if [ -n "$IMAGE_DIGEST" ]; then
  REPO=${IMAGE%%@*}
  [[ ${REPO##*/} != *:* ]] || REPO=${REPO%:*}
  REF="$REPO@$IMAGE_DIGEST"
fi

if [ "$DRY" -eq 0 ]; then
  if "$ENGINE" container inspect "$NAME" > /dev/null 2>&1; then
    die "container $NAME exists; run --remove first (or pick another --name)"
  fi
fi
note "plan: $ENGINE container $NAME from $REF, $CORES cpu(s), $MEMORY_MIB MiB, $CONCURRENCY task(s) at once"
[ -n "$IMAGE_DIGEST" ] || echo "NOTE: image is not pinned by digest; $REF is whatever the registry serves today. Ask the maintainer for a digest." >&2

# ---- image ------------------------------------------------------------------
note "image"
if [ -n "$ARCHIVE" ]; then
  if [ "$DRY" -eq 1 ]; then
    echo "+ check sha256 of $ARCHIVE equals $ARCHIVE_SHA"
  else
    if command -v sha256sum > /dev/null; then got=$(sha256sum "$ARCHIVE" | cut -d' ' -f1); else got=$(shasum -a 256 "$ARCHIVE" | cut -d' ' -f1); fi
    [ "$got" = "$ARCHIVE_SHA" ] || die "archive sha256 mismatch (got $got)"
  fi
  run "$ENGINE" load -i "$ARCHIVE"
else
  run "$ENGINE" pull "$REF"
  if [ -n "$IMAGE_DIGEST" ]; then
    if [ "$DRY" -eq 1 ]; then
      echo "+ check that the pulled image's repo digests include $IMAGE_DIGEST"
    else
      "$ENGINE" image inspect --format '{{json .RepoDigests}}' "$REF" | grep -q "$IMAGE_DIGEST" \
        || die "pulled image does not carry digest $IMAGE_DIGEST"
    fi
  fi
fi

# ---- settings ---------------------------------------------------------------
note "settings (client preferences and the account file)"
ACCT_NAME=$(boinc_account_filename "$PROJECT_URL")
if [ "$DRY" -eq 1 ]; then
  CPUS_SEEN=0
else
  if [ "$ENGINE" = docker ]; then CPUS_SEEN=$(docker info --format '{{.NCPU}}')
  else CPUS_SEEN=$(podman info --format '{{.Host.CPUs}}'); fi
  [[ $CPUS_SEEN =~ ^[1-9][0-9]*$ ]] || die "could not read the CPU count from $ENGINE info"
fi
# BOINC inside the container sees the engine host's CPUs, not the --cpus limit,
# so the percentage is worked out from the host count.
if [ "$DRY" -eq 1 ]; then
  PREFS=$(boinc_prefs_xml "<$CONCURRENCY task(s) as % of the CPUs $ENGINE reports>" "$RUN_IF_IDLE" "$MAX_DISK_GIB")
else
  PREFS=$(boinc_prefs_xml "$(boinc_ncpus_pct "$CONCURRENCY" "$CPUS_SEEN")" "$RUN_IF_IDLE" "$MAX_DISK_GIB")
fi
echo "+ files copied into volume $VOLUME at /var/lib/boinc:"
echo "    global_prefs_override.xml (0644)"
printf '    %s\n' "${PREFS//$'\n'/$'\n    '}"
echo "    $ACCT_NAME (0600)"
boinc_account_xml "$PROJECT_URL" "$KEY" | sed 's/^/        /'

# random password for the local RPC port, which is not published anywhere
if [ "$DRY" -eq 1 ]; then RPC_PW="<random>"; else RPC_PW=$(LC_ALL=C tr -dc 'A-Za-z0-9' < /dev/urandom | head -c 24); fi

note "network and volume"
run "$ENGINE" network create "$NETWORK"
run "$ENGINE" volume create "$VOLUME"

if [ "$DRY" -eq 1 ]; then
  echo "+ create a throwaway container, copy the two files into $VOLUME, remove it"
else
  TMP=$(mktemp -d)
  trap 'rm -rf "$TMP"' EXIT
  chmod 700 "$TMP"
  printf '%s\n' "$PREFS" > "$TMP/global_prefs_override.xml"
  (umask 077; boinc_account_xml "$PROJECT_URL" "$KEY" > "$TMP/$ACCT_NAME")
  chmod 644 "$TMP/global_prefs_override.xml"
  "$ENGINE" create --name "$NAME-seed" -v "$VOLUME:/var/lib/boinc" --entrypoint true "$REF" > /dev/null
  "$ENGINE" cp "$TMP/global_prefs_override.xml" "$NAME-seed:/var/lib/boinc/global_prefs_override.xml"
  "$ENGINE" cp "$TMP/$ACCT_NAME" "$NAME-seed:/var/lib/boinc/$ACCT_NAME"
  "$ENGINE" rm "$NAME-seed" > /dev/null
  rm -rf "$TMP"
fi

# ---- run --------------------------------------------------------------------
note "start"
LIMITS=(--cpus "$CORES" --memory "${MEMORY_MIB}m" --memory-swap "${MEMORY_MIB}m")
HARD=()
if [ "$HARDEN" -eq 1 ]; then
  HARD=(--cap-drop ALL --security-opt no-new-privileges --read-only --tmpfs /tmp)
fi
if [ "$QUADLET" -eq 1 ]; then
  QF=$(quadlet_file)
  UNIT="[Unit]
Description=usnewsmap BOINC client ($NAME)
After=network-online.target

[Container]
Image=$REF
ContainerName=$NAME
Network=$NETWORK
Volume=$VOLUME:/var/lib/boinc
Environment=BOINC_GUI_RPC_PASSWORD=$RPC_PW
PodmanArgs=--cpus=$CORES --memory=${MEMORY_MIB}m --memory-swap=${MEMORY_MIB}m"
  if [ "$HARDEN" -eq 1 ]; then
    UNIT+="
DropCapability=ALL
NoNewPrivileges=true
ReadOnly=true
Tmpfs=/tmp"
  fi
  UNIT+="

[Service]
Restart=always

[Install]
WantedBy=default.target"
  if [ "$DRY" -eq 1 ]; then
    echo "+ write $QF (0600):"
    printf '    %s\n' "${UNIT//$'\n'/$'\n    '}"
  else
    mkdir -p "$(dirname "$QF")"
    (umask 077; printf '%s\n' "$UNIT" > "$QF")
  fi
  # shellcheck disable=SC2046
  run $(sysctl_user) daemon-reload
  # shellcheck disable=SC2046
  run $(sysctl_user) start "$NAME.service"
  echo "For a rootless user to start at boot without logging in: loginctl enable-linger \$USER"
else
  run "$ENGINE" run -d --name "$NAME" --restart unless-stopped \
    --network "$NETWORK" -v "$VOLUME:/var/lib/boinc" \
    -e "BOINC_GUI_RPC_PASSWORD=$RPC_PW" \
    "${LIMITS[@]}" "${HARD[@]+"${HARD[@]}"}" "$REF"
  if [ "$ENGINE" = podman ]; then
    echo "NOTE: with podman, --restart unless-stopped only applies after a reboot if podman-restart.service is enabled (systemctl enable podman-restart.service), or use --quadlet."
  fi
fi

# ---- LAN block recipe -------------------------------------------------------
if [ "${#BLOCK_CIDRS[@]}" -gt 0 ]; then
  note "LAN block: this script does NOT enforce it. Rules to apply yourself (docs/container-contributors.md)"
  if [ "$DRY" -eq 1 ]; then SUBNET="<subnet of $NETWORK>"
  else SUBNET=$("$ENGINE" network inspect --format '{{range .Subnets}}{{.Subnet}} {{end}}' "$NETWORK" 2> /dev/null | awk '{print $1}' || true)
       [ -n "$SUBNET" ] || SUBNET=$("$ENGINE" network inspect --format '{{range .IPAM.Config}}{{.Subnet}} {{end}}' "$NETWORK" | awk '{print $1}'); fi
  if [ "$ENGINE" = docker ]; then
    for c in "${BLOCK_CIDRS[@]}"; do
      echo "  iptables -I DOCKER-USER -s $SUBNET -d $c -j DROP"
    done
    echo "  (run as root on the Docker host; they vanish at reboot unless you persist them)"
  else
    for c in "${BLOCK_CIDRS[@]}"; do
      echo "  nft insert rule inet filter forward ip saddr $SUBNET ip daddr $c drop   # table/chain names depend on your setup"
    done
    echo "  Rootless podman: container traffic leaves from your user's network stack, so use host firewall rules that match your user (UNVERIFIED), or run the Proxmox route instead."
  fi
  echo "  Then test from inside: $ENGINE exec $NAME sh -c 'timeout 3 bash -c \"</dev/tcp/<a LAN host>/80\"; echo exit=\$?'  (expect it not to connect)"
fi

note "done"
cat <<EOF2
Check it:   $0 --status --engine $ENGINE --name $NAME
Stop it:    $0 --stop --engine $ENGINE --name $NAME
Remove it:  $0 --remove --purge --engine $ENGINE --name $NAME
EOF2
