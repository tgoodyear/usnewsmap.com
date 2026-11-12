#!/usr/bin/env bash
# One controller tick for one plan (#1690), as usnm-boinc-priority-controller@PLAN.service runs
# it on the controller host (CT 211). Each plan has its own unit and timer, so one plan's failed
# tick no longer stops the others, and the next tick of that plan runs on its own timer.
#
#   controller-tick.sh --plan PLAN [--dry-run] [--print-command] [--config-dir DIR]
#                      [--state-parent DIR] [--scheduler FILE] [--tick-lock FILE]
#                      [--lock-wait-secs N] [--held-archive-unit UNIT] [--add-flag ARG]...
#
# The plan named `main` is the live plan in the unsuffixed paths the shared service used:
#   /etc/usnm/boinc-priority-controller.json, /var/lib/usnm-priority-controller and
#   /etc/usnm/boinc-priority-controller.flags.
# Any other plan (a draining plan, a native plan) uses the suffixed paths:
#   /etc/usnm/boinc-priority-controller.PLAN.json, /var/lib/usnm-priority-controller.PLAN and
#   /etc/usnm/boinc-priority-controller.PLAN.flags.
#
# The flags file holds the plan's own batch_scheduler.py flags, one argument per line (a flag
# and its value go on two lines); blank lines and lines starting with # are skipped. No shell
# reads it, so nothing in it is expanded. --config, --state-dir and --apply are this script's and
# are refused there. A missing flags file fails the tick, so a mistyped instance never ticks with
# defaults. A draining plan's file has --drain.
#
# Every tick runs under the host's tick lock (default /run/usnm-boinc-controller-tick.lock,
# waited for up to --lock-wait-secs, default 1800), the lock held-archive.sh holds through its
# run. The hand-off container has one controller lease (scheduler/controller.lock) and one
# scheduler/inventory.json for every plan, so two plans' ticks can't run at once: without this
# lock the second would fail at once with "the controller lease is leased". See "Per-plan ticks
# (#1690)" in docs/boinc/server/batch-scheduler.md. A wait that runs out fails the tick with a
# boinc_batch_failure line feed_collect.py classes as lock_busy.
#
# A plan other than main skips its tick (exit 0, one boinc_batch_skipped line) while the
# held-page archive (--held-archive-unit) runs: the archive holds the tick lock for up to hours
# and its own pause covers main only. main is never skipped: the archive stops main's timer and
# starts a main tick itself when it ends, and that tick waits on the lock.
#
# --dry-run runs the plan's tick without --apply and without the lock (batch_scheduler.py then
# reads only the configuration). --print-command prints the command a tick would run, one
# argument per line, and runs nothing. --add-flag adds one argument after the flags file's for
# this run only, such as --resume-native for a tick run by hand; repeat it for a flag's value.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
json_string() { python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$1"; }
die() {
  printf '{"event": "boinc_batch_failure", "schema_version": 1, "at": %s, "error": %s}\n' \
    "$(date +%s)" "$(json_string "$*")" >&2
  exit 1
}

HERE=$(cd "$(dirname "$0")" && pwd)
PLAN=""
CONFIG_DIR=/etc/usnm
STATE_PARENT=/var/lib
SCHEDULER=$(cd "$HERE/../.." && pwd)/batch_scheduler.py
TICK_LOCK=/run/usnm-boinc-controller-tick.lock
LOCK_WAIT=1800
HELD_ARCHIVE_UNIT=usnm-boinc-held-archive.service
DRY_RUN=false PRINT=false
ADDED=()
while [ $# -gt 0 ]; do
  case $1 in
    --plan) [ $# -ge 2 ] || usage; PLAN=$2; shift 2 ;;
    --config-dir) [ $# -ge 2 ] || usage; CONFIG_DIR=$2; shift 2 ;;
    --state-parent) [ $# -ge 2 ] || usage; STATE_PARENT=$2; shift 2 ;;
    --scheduler) [ $# -ge 2 ] || usage; SCHEDULER=$2; shift 2 ;;
    --tick-lock) [ $# -ge 2 ] || usage; TICK_LOCK=$2; shift 2 ;;
    --lock-wait-secs) [ $# -ge 2 ] || usage; LOCK_WAIT=$2; shift 2 ;;
    --held-archive-unit) [ $# -ge 2 ] || usage; HELD_ARCHIVE_UNIT=$2; shift 2 ;;
    --add-flag) [ $# -ge 2 ] || usage; ADDED+=("$2"); shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    --print-command) PRINT=true; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
# The same names batch_scheduler.py accepts for a plan, lower case (a systemd instance name).
[[ $PLAN =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || die "--plan is a plan name: lower-case letters, digits and -"
[[ $LOCK_WAIT =~ ^[0-9]{1,5}$ ]] && [ "$LOCK_WAIT" -ge 1 ] || die "--lock-wait-secs is 1 to 99999"
[[ $HELD_ARCHIVE_UNIT =~ ^[a-zA-Z0-9@_.-]+\.service$ ]] || die "--held-archive-unit is a service name"

if [ "$PLAN" = main ]; then
  suffix=""
else
  suffix=".$PLAN"
fi
CONFIG=$CONFIG_DIR/boinc-priority-controller$suffix.json
FLAGS_FILE=$CONFIG_DIR/boinc-priority-controller$suffix.flags
STATE_DIR=$STATE_PARENT/usnm-priority-controller$suffix

[ -f "$SCHEDULER" ] || die "no $SCHEDULER"
[ -f "$CONFIG" ] || die "plan $PLAN has no configuration $CONFIG"
[ -f "$FLAGS_FILE" ] || die "plan $PLAN has no flags file $FLAGS_FILE (it may be empty)"

# True for --config, --state-dir or --apply, with or without =VALUE, and for any abbreviation
# of them: batch_scheduler.py's argparse takes a unique prefix (--appl) as the full option.
own_flag() {
  local name=${1%%=*} own
  [[ $name == --?* ]] || return 1
  for own in --config --state-dir --apply; do
    [ "${own#"$name"}" != "$own" ] && return 0
  done
  return 1
}
FLAGS=()
while IFS= read -r line || [ -n "$line" ]; do
  # Surrounding blanks don't count; a blank or # line is skipped.
  line=${line#"${line%%[![:space:]]*}"}
  line=${line%"${line##*[![:space:]]}"}
  case $line in
    "" | "#"*) continue ;;
  esac
  ! own_flag "$line" || die "$FLAGS_FILE sets $line, which controller-tick.sh sets itself"
  FLAGS+=("$line")
done < "$FLAGS_FILE"
for line in ${ADDED[@]+"${ADDED[@]}"}; do
  ! own_flag "$line" || die "--add-flag $line: controller-tick.sh sets it itself"
  FLAGS+=("$line")
done

COMMAND=(python3 "$SCHEDULER" --config "$CONFIG" --state-dir "$STATE_DIR")
$DRY_RUN || COMMAND+=(--apply)
COMMAND+=(${FLAGS[@]+"${FLAGS[@]}"})

if $PRINT; then
  printf '%s\n' "${COMMAND[@]}"
  exit 0
fi
if $DRY_RUN; then
  exec "${COMMAND[@]}"
fi

if [ "$PLAN" != main ]; then
  # A oneshot unit is "activating" while it runs, so is-active alone would miss it.
  archive=$(systemctl show "$HELD_ARCHIVE_UNIT" -p ActiveState --value 2> /dev/null || true)
  case $archive in
    active | activating | deactivating | reloading)
      printf '{"event": "boinc_batch_skipped", "schema_version": 1, "at": %s, "plan": %s, "reason": "held_archive_running"}\n' \
        "$(date +%s)" "$(json_string "$PLAN")"
      exit 0 ;;
  esac
fi

# The lock is fd 8's, taken here and inherited by the tick; it is released when the tick exits.
exec 8> "$TICK_LOCK"
flock -w "$LOCK_WAIT" 8 ||
  die "the tick lock is busy after $LOCK_WAIT seconds: another controller may be active"
exec "${COMMAND[@]}"
