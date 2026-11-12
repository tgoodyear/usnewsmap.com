#!/usr/bin/env bash
# One controller tick for one plan (#1690), as usnm-boinc-priority-controller@PLAN.service runs
# it on the controller host (CT 211). Each plan has its own unit and timer, so one plan's failed
# tick no longer stops the others, and the next tick of that plan runs on its own timer.
#
#   controller-tick.sh --plan PLAN [--dry-run] [--print-command] [--print-timer]
#                      [--config-dir DIR] [--state-parent DIR] [--scheduler FILE]
#                      [--tick-lock FILE] [--lock-wait-secs N] [--lock-recheck-secs N]
#                      [--held-archive-unit UNIT] [--held-archive-lock FILE] [--add-flag ARG]...
#
# The plan named `main` is the live plan in the unsuffixed paths the shared service used:
#   /etc/usnm/boinc-priority-controller.json, /var/lib/usnm-priority-controller and
#   /etc/usnm/boinc-priority-controller.flags.
# Any other plan (a draining plan, a native plan) uses the suffixed paths:
#   /etc/usnm/boinc-priority-controller.PLAN.json, /var/lib/usnm-priority-controller.PLAN and
#   /etc/usnm/boinc-priority-controller.PLAN.flags.
#
# The flags file holds the plan's complete batch_scheduler.py flags. Each line is split on blanks
# into arguments, so a flag and its value may share a line or take one line each, and the
# one-line, space-separated files of the native tick script work as they are. Blank lines and
# lines starting with # are skipped. No shell reads it: quotes and backslashes are refused, and
# nothing is expanded. --config, --state-dir and --apply (or any abbreviation of them) are this
# script's, and --resume-native goes only in --add-flag for one run. A missing flags file fails
# the tick, so a mistyped instance never ticks with defaults.
#
# Lines starting with @ describe the plan rather than its tick:
#   @kind main|native|drain  required. Every kind needs --onprem-handoff claim, collect and
#                            export: without them the hand-off steps would go to the Azure
#                            job prod no longer has, so such a plan refuses to tick. A drain
#                            plan needs --drain, and only a drain plan may have it.
#   @every TIME              the timer's OnUnitInactiveSec (main and native 2min, drain 30min).
#   @boot TIME               the timer's OnBootSec (main and native 5min, drain 17min).
# TIME is a number with s, min or h. --print-timer prints the timer drop-in these give, which
# install.sh writes as usnm-boinc-priority-controller@PLAN.timer.d/cadence.conf.
#
# Every tick runs under the host's tick lock (default /run/usnm-boinc-controller-tick.lock,
# waited for up to --lock-wait-secs, default 1800), the lock held-archive.sh holds through its
# run. The hand-off container has one controller lease (scheduler/controller.lock) and one
# scheduler/inventory.json for every plan, so two plans' ticks can't run at once: without this
# lock the second would fail at once with "the controller lease is leased". See "Per-plan ticks
# (#1690)" in docs/boinc/server/batch-scheduler.md. A wait that runs out fails the tick with a
# boinc_batch_failure line feed_collect.py classes as lock_busy.
#
# A plan other than main skips its tick (exit 0, one boinc_batch_skipped line) while a held-page
# archive runs: the scheduled unit (--held-archive-unit), a hand-run usnm-held-archive-* unit, or
# any run holding held-archive.sh's lock (--held-archive-lock). A drain plan also skips while
# main's timer is stopped, as the shared drain service did: an operator's pause. This is checked
# before the lock, every --lock-recheck-secs (default 30) while waiting for it, and once it is
# taken. main is never skipped: the archive stops main's timer and starts a main tick itself
# when it ends, and that tick waits on the lock.
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
RECHECK=30
HELD_ARCHIVE_UNIT=usnm-boinc-held-archive.service
HELD_ARCHIVE_LOCK=/run/usnm-boinc-held-archive.lock
MAIN_TIMER=usnm-boinc-priority-controller@main.timer
DRY_RUN=false PRINT=false PRINT_TIMER=false
ADDED=()
while [ $# -gt 0 ]; do
  case $1 in
    --plan) [ $# -ge 2 ] || usage; PLAN=$2; shift 2 ;;
    --config-dir) [ $# -ge 2 ] || usage; CONFIG_DIR=$2; shift 2 ;;
    --state-parent) [ $# -ge 2 ] || usage; STATE_PARENT=$2; shift 2 ;;
    --scheduler) [ $# -ge 2 ] || usage; SCHEDULER=$2; shift 2 ;;
    --tick-lock) [ $# -ge 2 ] || usage; TICK_LOCK=$2; shift 2 ;;
    --lock-wait-secs) [ $# -ge 2 ] || usage; LOCK_WAIT=$2; shift 2 ;;
    --lock-recheck-secs) [ $# -ge 2 ] || usage; RECHECK=$2; shift 2 ;;
    --held-archive-unit) [ $# -ge 2 ] || usage; HELD_ARCHIVE_UNIT=$2; shift 2 ;;
    --held-archive-lock) [ $# -ge 2 ] || usage; HELD_ARCHIVE_LOCK=$2; shift 2 ;;
    --add-flag) [ $# -ge 2 ] || usage; ADDED+=("$2"); shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    --print-command) PRINT=true; shift ;;
    --print-timer) PRINT_TIMER=true; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
# The same names batch_scheduler.py accepts for a plan, lower case (a systemd instance name).
[[ $PLAN =~ ^[a-z0-9][a-z0-9-]{0,63}$ ]] || die "--plan is a plan name: lower-case letters, digits and -"
[[ $LOCK_WAIT =~ ^[0-9]{1,5}$ ]] && [ "$LOCK_WAIT" -ge 1 ] || die "--lock-wait-secs is 1 to 99999"
[[ $RECHECK =~ ^[0-9]{1,4}$ ]] && [ "$RECHECK" -ge 1 ] || die "--lock-recheck-secs is 1 to 9999"
[[ $HELD_ARCHIVE_UNIT =~ ^[a-zA-Z0-9@_.-]+\.service$ ]] || die "--held-archive-unit is a service name"

if [ "$PLAN" = main ]; then
  suffix=""
else
  suffix=".$PLAN"
fi
CONFIG=$CONFIG_DIR/boinc-priority-controller$suffix.json
FLAGS_FILE=$CONFIG_DIR/boinc-priority-controller$suffix.flags
STATE_DIR=$STATE_PARENT/usnm-priority-controller$suffix
[ -f "$FLAGS_FILE" ] || die "plan $PLAN has no flags file $FLAGS_FILE"

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
# The same for --resume-native, which clears a native plan's pause: one run at a time.
resume_flag() {
  local name=${1%%=*} own=--resume-native
  [[ $name == --r?* ]] && [ "${own#"$name"}" != "$own" ]
}
time_value() { [[ $1 =~ ^[1-9][0-9]{0,4}(s|min|h)$ ]]; }

KIND="" EVERY="" BOOT=""
FLAGS=()
while IFS= read -r line || [ -n "$line" ]; do
  # Surrounding blanks don't count; a blank or # line is skipped.
  line=${line#"${line%%[![:space:]]*}"}
  line=${line%"${line##*[![:space:]]}"}
  case $line in
    "" | "#"*) continue ;;
    *[\'\"\\]*) die "$FLAGS_FILE: quotes and backslashes aren't read as a shell would; write arguments plainly" ;;
  esac
  read -r -a words <<< "$line"
  if [[ $line == @* ]]; then
    [ ${#words[@]} -eq 2 ] || die "$FLAGS_FILE: $line: a setting takes one value"
    case ${words[0]} in
      @kind)
        [ -z "$KIND" ] || die "$FLAGS_FILE sets @kind twice"
        case ${words[1]} in main | native | drain) KIND=${words[1]} ;; *) die "$FLAGS_FILE: @kind is main, native or drain" ;; esac ;;
      @every)
        [ -z "$EVERY" ] || die "$FLAGS_FILE sets @every twice"
        time_value "${words[1]}" || die "$FLAGS_FILE: @every is a number with s, min or h"
        EVERY=${words[1]} ;;
      @boot)
        [ -z "$BOOT" ] || die "$FLAGS_FILE sets @boot twice"
        time_value "${words[1]}" || die "$FLAGS_FILE: @boot is a number with s, min or h"
        BOOT=${words[1]} ;;
      *) die "$FLAGS_FILE: unknown setting ${words[0]}" ;;
    esac
    continue
  fi
  for word in "${words[@]}"; do
    ! own_flag "$word" || die "$FLAGS_FILE sets $word, which controller-tick.sh sets itself"
    ! resume_flag "$word" || die "$FLAGS_FILE sets $word: pass it once with --add-flag instead"
    FLAGS+=("$word")
  done
done < "$FLAGS_FILE"
[ -n "$KIND" ] || die "$FLAGS_FILE has no @kind (main, native or drain)"
case $KIND in
  drain) EVERY=${EVERY:-30min} BOOT=${BOOT:-17min} ;;
  *) EVERY=${EVERY:-2min} BOOT=${BOOT:-5min} ;;
esac

# Checks the plan's own flags, before --add-flag: every kind runs its hand-off steps on-prem,
# and only a drain plan drains.
handoff=() drain=false previous=""
for word in ${FLAGS[@]+"${FLAGS[@]}"}; do
  case $previous:$word in
    --onprem-handoff:*) handoff+=("$word") ;;
    *:--onprem-handoff=*) handoff+=("${word#--onprem-handoff=}") ;;
  esac
  # argparse takes --dr, --dra and --drai as --drain too.
  case $word in --dr | --dra | --drai | --drain) drain=true ;; esac
  previous=$word
done
for step in claim collect export; do
  [[ " ${handoff[*]-} " == *" $step "* ]] ||
    die "$FLAGS_FILE lacks --onprem-handoff $step: its hand-off steps would go to the Azure job"
done
if [ "$KIND" = drain ]; then
  $drain || die "$FLAGS_FILE is @kind drain but has no --drain"
else
  ! $drain || die "$FLAGS_FILE has --drain but is @kind $KIND: make it @kind drain"
fi

if $PRINT_TIMER; then
  # An empty assignment clears every trigger of the timer (the template's), so one comes first.
  printf '# From %s (@kind %s), written by install.sh (#1690).\n[Timer]\nOnBootSec=\nOnBootSec=%s\nOnUnitInactiveSec=%s\n' \
    "$FLAGS_FILE" "$KIND" "$BOOT" "$EVERY"
  exit 0
fi

for word in ${ADDED[@]+"${ADDED[@]}"}; do
  ! own_flag "$word" || die "--add-flag $word: controller-tick.sh sets it itself"
  # Draining is the kind's: a plan that isn't @kind drain never drains for one run.
  case $KIND:$word in
    drain:*) ;;
    *:--dr | *:--dra | *:--drai | *:--drain) die "--add-flag $word: plan $PLAN is @kind $KIND" ;;
  esac
  FLAGS+=("$word")
done

[ -f "$SCHEDULER" ] || die "no $SCHEDULER"
[ -f "$CONFIG" ] || die "plan $PLAN has no configuration $CONFIG"
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

skipped() {
  printf '{"event": "boinc_batch_skipped", "schema_version": 1, "at": %s, "plan": %s, "reason": "%s"}\n' \
    "$(date +%s)" "$(json_string "$PLAN")" "$1"
}
# Why a plan other than main doesn't tick now, if it doesn't. A oneshot unit is "activating"
# while it runs, so is-active alone would miss it.
skip_reason() {
  [ "$PLAN" != main ] || return 1
  case $(systemctl show "$HELD_ARCHIVE_UNIT" -p ActiveState --value 2> /dev/null || true) in
    active | activating | deactivating | reloading) echo held_archive_running; return 0 ;;
  esac
  if [ -n "$(systemctl list-units --plain --no-legend --state=active,activating,deactivating \
    'usnm-held-archive-*' 2> /dev/null || true)" ]; then
    echo held_archive_running
    return 0
  fi
  if [ -e "$HELD_ARCHIVE_LOCK" ] && ! flock -n "$HELD_ARCHIVE_LOCK" true; then
    echo held_archive_running
    return 0
  fi
  if [ "$KIND" = drain ] && [ "$(systemctl is-active "$MAIN_TIMER" 2> /dev/null || true)" != active ]; then
    echo controller_paused
    return 0
  fi
  return 1
}
if reason=$(skip_reason); then skipped "$reason"; exit 0; fi

# The lock is fd 8's, taken here and inherited by the tick; it is released when the tick exits.
# It is waited for in steps of at most --lock-recheck-secs, so a tick queued behind another plan's
# skips as soon as an archive starts, instead of waiting out the whole wait and failing.
exec 8> "$TICK_LOCK"
deadline=$((SECONDS + LOCK_WAIT))
until flock -w "$(( deadline - SECONDS < RECHECK ? deadline - SECONDS : RECHECK ))" 8; do
  if reason=$(skip_reason); then skipped "$reason"; exit 0; fi
  [ "$SECONDS" -lt "$deadline" ] ||
    die "the tick lock is busy after $LOCK_WAIT seconds: another controller may be active"
done
# An archive that started while this tick waited goes first (exit releases the lock).
if reason=$(skip_reason); then skipped "$reason"; exit 0; fi
exec "${COMMAND[@]}"
