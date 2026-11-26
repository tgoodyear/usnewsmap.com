#!/usr/bin/env bash
# The daily held-page archive (#835) on the controller host (CT 211), as
# usnm-boinc-held-archive.timer runs it. While a trust scope is held, its pages' journals, claims
# and bridge journals pile up toward the controller's caps (10,000 each); this moves them out,
# as docs/boinc/server/batch-scheduler.md "Archive held pages (#835)" does by hand:
#
#   1. Stops the main plan's timer, usnm-boinc-priority-controller@main.timer (#1690), and
#      waits for a running tick to end. If the timer is already stopped, an operator has paused
#      the controller: the run does nothing. If the old shared unit
#      (usnm-boinc-priority-controller.timer) still runs instead, the run fails: the host hasn't
#      moved to the per-plan units, so set USNM_CONTROLLER_UNIT to the old name or migrate.
#   2. Bridges any batch dispatched earlier whose bridge receipt isn't here yet (a run that
#      stopped between the two steps).
#   3. Runs archive_history.py dispatch --include-held --apply, one batch at a time, under the
#      controller's own identity (its custom role deletes blobs in boinc-handoff, nothing else),
#      and after each batch sends its manifest and dispatch receipt to the BOINC host over the
#      controller's ssh step command, where `usnm-boinc-step --target archive --step bridge`
#      runs the bridge step under the bridge step locks. The bridge receipt it answers with is
#      kept as receipts/bridge.json. It stops when no page is eligible, or after --max-batches.
#   4. Starts the timer again, and a tick at once, whatever happened.
#
# On-prem hand-off (#1367): when the plan's flags file beside --config has --ssh-reads, the tool
# reads and changes the hand-off root on the BOINC host through its step runner, under the
# controller's local lock, not the Azure container (archive_history.py --handoff auto; see
# docs/boinc/server/batch-scheduler.md "On-prem hand-off"). Nothing here changes.
#
# No db step (#1587). The receipts are in <archive dir>/batches/<NNNN>/receipts/. Each run ends
# with one `boinc_held_archive` JSON line: the batches, pages and held pages it archived, and the
# journal and state counts after. A failure exits non-zero, so the unit fails; the controller's
# feed source reads this unit too (--maintenance-unit) and reports that as a controller failure.
#
#   held-archive.sh [--config FILE] [--state-dir DIR] [--archive-dir DIR] [--pages N]
#                   [--min-age-hours N] [--max-batches N] [--wait-mins N] [--tool FILE]
#                   [--check-state-dir DIR]... [--tick-lock FILE] [--gap-mins N]
#                   [--max-minutes N] [--batch-mins N] [--pre-batch-tick] [--lock-wait-mins N]
#                   [--max-pause-mins N]
#                   [--skip-if-busy]
#
# --lock-wait-mins N (#2052; default 30, 0 = fail at once) is how long the run waits for another
# archive's locks before it gives up. Six archive units (the main plan's and the native plans')
# share the two locks, so one starting while another runs queues behind it instead of failing and
# losing its slot: it logs boinc_held_archive_lock_wait when it starts waiting and again when it
# has the locks, and fails ("another held-page archive is running") only on the timeout. The
# --max-minutes budget starts once the locks are held, so a run that waited keeps its whole budget.
#
# --skip-if-busy (default off) is for units whose timers are staggered (RandomizedDelaySec with
# FixedRandomDelay, so each plan has its own slot in the cycle): if another archive holds the run
# lock when this one starts, it logs boinc_held_archive_busy ("busy, next slot") and exits 0
# without waiting, as the other archive's run is the lock's only holder. It skips at most one slot
# in a row: the skip leaves held-archive.skipped in --state-dir, and the next run waits as without
# the flag (and removes it once it holds the lock), so no plan can be starved. A wait for the archive
# lock that follows (a controller tick holds it too) still fails on --lock-wait-mins: a tick that
# holds it that long is a real problem.
#
# --gap-mins N (#2012; default 0, off) lets the controller tick between batches. The pause, the
# tick lock and the archive lock (/run/usnm-boinc-held-archive.lock, which other plans' ticks
# skip on) are needed only while a batch is dispatched and bridged: the controller's lease, its
# state and the journals must not change under it. With N set, the run lets all three go after
# each batch that another follows, starts the timer and a tick, waits up to N minutes for that
# tick to end (not at all if none starts within a minute), and pauses again for the next batch.
# Every batch still runs in the full pause, with the same checks. The archive's own run lock,
# /run/usnm-boinc-held-archive.lock.run, is held throughout, so two archives never overlap. A
# plan's tick sees /run/usnm-boinc-held-archive.lock.gap meanwhile and no longer skips for the
# archive's unit being active. Pair it with a small --pages, so a batch is short (a batch of
# 1,000 pages was 35 minutes on native-feed-4, 15 Nov 2026).
#
# --max-pause-mins N (#2156; default 0, off; needs --gap-mins) bounds how long the controller stays
# paused for one batch. The gap ticks the controller only between batches, and a batch runs in the
# full pause: a 1,000-page batch is 30-40 minutes, so on 26 Nov 2026 the main plan (whose timer
# the run stops) collected and refilled nothing for most of each hour and BOINC ran dry. With N
# set, each batch asks for the pages it can archive in N minutes at the pace measured so far:
# the last batch's pages times N minutes over the seconds it took (dispatch and bridge), at most
# --pages, at least MIN_BATCH_PAGES, and at most twice the last batch's. The first batch of a run
# takes the size the previous run settled on (<archive dir>/held-archive.pages), or
# FIRST_BATCH_PAGES when there is none. A batch that takes longer than N is never cut short; the
# next one is only smaller. The pages per hour stay about the same: more, shorter batches.
#
# --max-minutes N (default 0, no limit) is a budget for the whole run. No further batch starts
# once the run is N minutes old, or when what is left cannot cover a typical batch: the longest
# batch this run has taken, or --batch-mins N (default 10) until it has run one. A wait on the
# controller's tick or the tick lock is capped to what is left of the budget (at least 1 minute)
# instead of --wait-mins; one that runs out fails the run, which resumes on the next. The first
# batch is held to the same test, so a run whose pause, recovery bridges or tick used the budget
# archives nothing. The bridge step's own --wait-mins is not reduced: it is also the step's
# execution allowance. A batch under way is finished: it is never cut short.
#
# --pre-batch-tick (default off) runs one normal tick of the plan's controller unit before each
# batch takes the pause, so the cohort is refilled to its high watermark first and the BOINC
# host's buffer is full when the exclusive section starts. Before the first batch the run lets
# the archive lock and the tick lock go (as --gap-mins does) and runs `systemctl start
# UNIT.service`, the command the plan's timer runs, to its end, then takes the locks back and
# pauses. Before a later batch it is the gap's own tick (the one already running is joined, not
# doubled), or, without --gap-mins, the same release-and-tick as the first. The tick waits at most
# --wait-mins. A tick that fails, or finds the tick lock busy, is logged
# (boinc_held_archive_pre_batch_tick) and the batch goes on; one still running when the wait ends
# fails the run (the exit trap starts the timer at once). Every archive check is unchanged.
#
# --tick-lock (or USNM_HELD_ARCHIVE_TICK_LOCK) names a flock file that every plan's tick runs
# under (controller/controller-tick.sh; usnm-boinc-held-archive.service sets it). Once the
# controller tick it waited for has ended, the run takes that lock and holds it until it exits,
# so another plan's tick that was already queued on the lock can't take the controller's lease
# mid-run (CT 211, 11 Nov 2026: "the controller lease is leased" at dispatch). Without it,
# nothing changes.
#
# Other plans' state directories are passed to the dispatch step as --check-state-dir, so a page
# another plan has claimed or selected is never archived (#1713). Without --check-state-dir, every
# sibling `<state dir>.*` with a state.json is used (the drain plans, e.g.
# /var/lib/usnm-priority-controller.queue-feed-11).
#
# --tool is the archive_history.py to run (default: the one beside this script's parent, which
# imports the controller's own batch_scheduler.py beside it).
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() {
  printf '{"event": "boinc_held_archive_failure", "error": %s}\n' "$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$*")"
  echo "error: $*" >&2
  exit 1
}

HERE=$(cd "$(dirname "$0")/.." && pwd)
TOOL=$HERE/archive_history.py
CONFIG=/etc/usnm/boinc-priority-controller.json
STATE_DIR=/var/lib/usnm-priority-controller
ARCHIVE=/srv/usnm-archive
PAGES=1000 MIN_AGE=1 MAX_BATCHES=10 WAIT_MINS=60 GAP_MINS=0 MAX_MINUTES=0
PRE_TICK=false BATCH_MINS=10 LOCK_WAIT_MINS=30 SKIP_IF_BUSY=false MAX_PAUSE_MINS=0
MIN_BATCH_PAGES=25 FIRST_BATCH_PAGES=100
UNIT=${USNM_CONTROLLER_UNIT:-usnm-boinc-priority-controller@main}
# The shared unit every plan ticked in before #1690.
LEGACY_UNIT=usnm-boinc-priority-controller
LOCK=${USNM_HELD_ARCHIVE_LOCK:-/run/usnm-boinc-held-archive.lock}
TICK_LOCK=${USNM_HELD_ARCHIVE_TICK_LOCK:-}
CHECK_DIRS=()
while [ $# -gt 0 ]; do
  case $1 in
    --config) [ $# -ge 2 ] || usage; CONFIG=$2; shift 2 ;;
    --state-dir) [ $# -ge 2 ] || usage; STATE_DIR=$2; shift 2 ;;
    --archive-dir) [ $# -ge 2 ] || usage; ARCHIVE=$2; shift 2 ;;
    --pages) [ $# -ge 2 ] || usage; PAGES=$2; shift 2 ;;
    --min-age-hours) [ $# -ge 2 ] || usage; MIN_AGE=$2; shift 2 ;;
    --max-batches) [ $# -ge 2 ] || usage; MAX_BATCHES=$2; shift 2 ;;
    --wait-mins) [ $# -ge 2 ] || usage; WAIT_MINS=$2; shift 2 ;;
    --tool) [ $# -ge 2 ] || usage; TOOL=$2; shift 2 ;;
    --check-state-dir) [ $# -ge 2 ] || usage; CHECK_DIRS+=("$2"); shift 2 ;;
    --tick-lock) [ $# -ge 2 ] || usage; TICK_LOCK=$2; shift 2 ;;
    --gap-mins) [ $# -ge 2 ] || usage; GAP_MINS=$2; shift 2 ;;
    --max-minutes) [ $# -ge 2 ] || usage; MAX_MINUTES=$2; shift 2 ;;
    --pre-batch-tick) PRE_TICK=true; shift ;;
    --batch-mins) [ $# -ge 2 ] || usage; BATCH_MINS=$2; shift 2 ;;
    --skip-if-busy) SKIP_IF_BUSY=true; shift ;;
    --lock-wait-mins) [ $# -ge 2 ] || usage; LOCK_WAIT_MINS=$2; shift 2 ;;
    --max-pause-mins) [ $# -ge 2 ] || usage; MAX_PAUSE_MINS=$2; shift 2 ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
# Without trailing slashes, so "$STATE_DIR".* names siblings, not entries inside it.
while [ "${STATE_DIR%/}" != "$STATE_DIR" ] && [ "$STATE_DIR" != / ]; do STATE_DIR=${STATE_DIR%/}; done
if [ ${#CHECK_DIRS[@]} -eq 0 ]; then
  for dir in "$STATE_DIR".*; do
    [ -f "$dir/state.json" ] && CHECK_DIRS+=("$dir")
  done
fi
CHECKS=()
for dir in ${CHECK_DIRS[@]+"${CHECK_DIRS[@]}"}; do CHECKS+=(--check-state-dir "$dir"); done
for n in "$PAGES" "$MIN_AGE" "$MAX_BATCHES" "$WAIT_MINS" "$GAP_MINS" "$MAX_MINUTES" "$BATCH_MINS" "$LOCK_WAIT_MINS" "$MAX_PAUSE_MINS"; do
  [[ $n =~ ^[0-9]{1,5}$ ]] || die "--pages, --min-age-hours, --max-batches, --wait-mins, --gap-mins, --max-minutes, --batch-mins, --lock-wait-mins and --max-pause-mins are numbers"
done
# Without a gap the controller stays paused from one batch to the next, so a shorter batch frees nothing.
[ "$MAX_PAUSE_MINS" -eq 0 ] || [ "$GAP_MINS" -ge 1 ] || die "--max-pause-mins needs --gap-mins"
[ "$MAX_BATCHES" -ge 1 ] && [ "$WAIT_MINS" -ge 1 ] || die "--max-batches and --wait-mins are at least 1"
[ -f "$TOOL" ] || die "no $TOOL"
[ -d "$ARCHIVE/batches" ] || die "no $ARCHIVE/batches: the archive's own dataset isn't mounted"

# The run lock keeps two archives from overlapping for the whole run; the archive lock (fd 9)
# is what other plans' ticks skip on, and --gap-mins lets it go between batches.
GAP_FILE=$LOCK.gap
# Takes the lock open on fd $1, waiting up to what is left of --lock-wait-mins (#2052). flock
# runs as a waited-for child, so a stop during the wait is not deferred.
LOCK_WAIT_START=$SECONDS
# Seconds, for tests: USNM_HELD_ARCHIVE_LOCK_WAIT_SECS replaces --lock-wait-mins.
LOCK_WAIT_SECS=${USNM_HELD_ARCHIVE_LOCK_WAIT_SECS:-$((LOCK_WAIT_MINS * 60))}
LOCK_WAITED=false
LOCK_WAITER=""
# A stop while waiting (SIGTERM to this script only, KillMode=mixed) ends the waiter too: bash would
# otherwise die at once and leave the flock child waiting on, holding the lock's file and the output.
# Nothing is paused or held yet, so there is nothing to resume.
on_lock_wait_stop() {
  [ -z "$LOCK_WAITER" ] || kill -TERM "$LOCK_WAITER" 2> /dev/null || true
  [ -z "$LOCK_WAITER" ] || wait "$LOCK_WAITER" 2> /dev/null || true
  unmark_queued
  exit 143
}
trap on_lock_wait_stop TERM INT
# While the run waits, <lock>.queued.d/<invocation ID> tells the plans' ticks (controller-tick.sh)
# that this unit is only queued, so they don't skip for it (#2131). The ID is systemd's for this
# run, so a marker left by a run that was killed never matches a later one.
QUEUED_DIR=$LOCK.queued.d
QUEUED_MARKER=""
mark_queued() {
  [[ ${INVOCATION_ID:-} =~ ^[0-9a-f]{32}$ ]] || return 0
  mkdir -p "$QUEUED_DIR" && touch "$QUEUED_DIR/$INVOCATION_ID" && QUEUED_MARKER=$QUEUED_DIR/$INVOCATION_ID ||
    echo "warning: could not mark this run queued in $QUEUED_DIR; other plans skip while it waits" >&2
}
unmark_queued() {
  [ -z "$QUEUED_MARKER" ] || rm -f "$QUEUED_MARKER"
  QUEUED_MARKER=""
}
take_lock() {
  flock -n "$1" && return 0
  local left=$((LOCK_WAIT_SECS - (SECONDS - LOCK_WAIT_START))) waiter
  if [ "$left" -gt 0 ]; then
    if ! $LOCK_WAITED; then
      printf '{"event": "boinc_held_archive_lock_wait", "waiting": true, "wait_mins": %s}\n' "$LOCK_WAIT_MINS"
      LOCK_WAITED=true
      mark_queued
    fi
    flock -w "$left" "$1" &
    LOCK_WAITER=$!
    waiter=$LOCK_WAITER
    wait "$waiter" && { LOCK_WAITER=""; return 0; }
    LOCK_WAITER=""
  fi
  unmark_queued
  die "another held-page archive is running (waited $((SECONDS - LOCK_WAIT_START)) s, --lock-wait-mins $LOCK_WAIT_MINS)"
}
exec 6> "$LOCK.run"
# The run lock is held only by archives, so a busy one means another archive is running: skip this
# slot instead of queueing (#2052), as the staggered timers give the next run its own. A plan skips
# at most one slot in a row: the marker makes its next run wait for the lock as without the flag, so
# a plan whose slot always lands in a longer one's run can't be starved while every run exits 0.
SKIP_MARKER=$STATE_DIR/held-archive.skipped
if $SKIP_IF_BUSY && [ ! -e "$SKIP_MARKER" ] && ! flock -n 6; then
  touch "$SKIP_MARKER" || die "could not write $SKIP_MARKER"
  printf '{"event": "boinc_held_archive_busy", "skipped": true, "reason": "busy, next slot"}\n'
  echo "held-page archive busy, next slot"
  exit 0
fi
take_lock 6
rm -f "$SKIP_MARKER"
exec 9> "$LOCK"
take_lock 9
unmark_queued
trap - TERM INT
rm -f "$GAP_FILE"
if $LOCK_WAITED; then
  printf '{"event": "boinc_held_archive_lock_wait", "waiting": false, "waited_seconds": %s}\n' "$((SECONDS - LOCK_WAIT_START))"
fi
# The --max-minutes budget counts from here: time queued behind another archive is not the run's.
if $LOCK_WAITED; then SECONDS=$LOCK_WAIT_START; fi

# The controller's step command (ssh to the BOINC host with its forced command) and environment.
mapfile -t STEP < <(python3 -c '
import json, sys
c = json.load(open(sys.argv[1]))
print(c["env"])
for a in c["step_command"]:
    print(a)
' "$CONFIG") || die "no env or step_command in $CONFIG"
ENV_NAME=${STEP[0]:-}
STEP=("${STEP[@]:1}")
[ -n "$ENV_NAME" ] && [ ${#STEP[@]} -ge 1 ] || die "no env or step_command in $CONFIG"

timer=$(systemctl is-active "$UNIT.timer" || true)
if [ "$timer" != active ] && [ "$UNIT" != "$LEGACY_UNIT" ] &&
  [ "$(systemctl is-active "$LEGACY_UNIT.timer" || true)" = active ]; then
  # Not a pause: the controller still ticks in the shared unit, and skipping would let the
  # journals grow to the cap unnoticed.
  die "$UNIT.timer is $timer but the shared $LEGACY_UNIT.timer is active: migrate to the per-plan units (#1690) or set USNM_CONTROLLER_UNIT=$LEGACY_UNIT"
fi
if [ "$timer" != active ]; then
  printf '{"event": "boinc_held_archive", "skipped": "the controller timer is %s: an operator has paused the controller"}\n' "$timer"
  exit 0
fi
# Starts the timer and a tick (so the feed source sees one soon after the pause). Returns
# non-zero when either fails.
start_controller() {
  local failed=false
  systemctl start "$UNIT.timer" || { echo "error: could not start $UNIT.timer again; start it by hand" >&2; failed=true; }
  systemctl start --no-block "$UNIT.service" || { echo "error: could not start a $UNIT tick" >&2; failed=true; }
  ! $failed
}
# Either failing fails the run, whatever its status was, so the alert sees a controller left stopped.
resume() {
  local status=${1:-$?}
  rm -f "$GAP_FILE"
  if ! start_controller; then
    echo '{"event": "boinc_held_archive_failure", "error": "the controller was not started again"}'
    exit 1
  fi
  exit "$status"
}
# On a stop (systemd sends SIGTERM to this script only, KillMode=mixed), the archive tool gets
# SIGTERM, raised in it as KeyboardInterrupt, and is waited for: it then leaves its step resumable and releases the controller's
# blob lease (archive_history.py's leased()) before the controller starts again.
child=""
on_stop() {
  if [ -n "$child" ]; then
    # The job and what it runs (the tool's wrapper, or the bridge's tar and ssh).
    pkill -TERM -P "$child" 2> /dev/null || true
    kill -TERM "$child" 2> /dev/null || true
    wait "$child" 2> /dev/null || true
  fi
  exit 143
}
trap resume EXIT
trap on_stop TERM INT
# Stops the timer, waits for a running tick and takes the tick lock (when there is one). Held
# until the batch is done: resume() starts the controller again while it is still held, and that
# tick waits on the lock for the moment this run ends. flock runs as a waited-for child so a
# stop during the wait reaches on_stop at once (bash defers traps until a foreground command
# ends); the lock belongs to fd 8's open file, so it stays held after the child exits.
# The most a wait may take, in seconds: --wait-mins, or under --max-minutes what is left of the
# run's budget (never negative: once it is spent, nothing is waited for).
wait_secs() {
  local left=$((WAIT_MINS * 60))
  if [ "$MAX_MINUTES" -ne 0 ]; then
    local budget=$((MAX_MINUTES * 60 - SECONDS))
    [ "$budget" -ge 0 ] || budget=0
    [ "$budget" -ge "$left" ] || left=$budget
  fi
  echo "$left"
}
pause_controller() {
  systemctl stop "$UNIT.timer"
  local allowed
  allowed=$(wait_secs)
  deadline=$((SECONDS + allowed))
  until state=$(systemctl is-active "$UNIT.service" || true); [ "$state" = inactive ] || [ "$state" = failed ]; do
    [ $SECONDS -lt $deadline ] || die "a controller tick is still running after $allowed seconds"
    sleep $((deadline - SECONDS < 5 ? deadline - SECONDS : 5))
  done
  if [ -n "$TICK_LOCK" ]; then
    exec 8> "$TICK_LOCK"
    flock -w $((deadline > SECONDS ? deadline - SECONDS : 0)) 8 &
    child=$!
    wait "$child" || die "another tick still holds $TICK_LOCK after $allowed seconds"
    child=""
  fi
}
pause_controller

# Between two batches (--gap-mins): lets the controller tick, then pauses it again. The archive
# lock and the tick lock are let go first, and the controller is started with its lock free, so
# its tick, and any other plan's, runs for real.
TICK_START_SECS=${USNM_HELD_ARCHIVE_TICK_START_SECS:-60}
# The gap's length in seconds: --gap-mins, or under --max-minutes what is left of it if that is less.
gap_secs() {
  local left=$((GAP_MINS * 60))
  if [ "$MAX_MINUTES" -ne 0 ]; then
    local budget=$((MAX_MINUTES * 60 - SECONDS))
    [ "$budget" -ge 0 ] || budget=0
    [ "$budget" -ge "$left" ] || left=$budget
  fi
  echo "$left"
}
# --pre-batch-tick: one tick of the plan's unit, run to its end (at most wait_secs), with the
# archive and tick locks already let go. Starting the unit joins a tick that is already running.
# A tick that fails is logged and the batch goes on. One that is still running when the wait ends
# (the unit has no start timeout) fails the run instead: it would only get a second wait in
# pause_controller, and the exit trap starts the timer again at once without killing the tick.
pre_batch_tick() {
  local status=0 allowed
  allowed=$(wait_secs)
  if [ "$allowed" -le 0 ]; then
    printf '{"event": "boinc_held_archive_pre_batch_tick", "ok": false, "skipped": "no time is left in --max-minutes"}\n'
    return
  fi
  timeout "$allowed" systemctl start "$UNIT.service" &
  child=$!
  wait "$child" || status=$?
  child=""
  if [ "$status" -eq 0 ]; then
    printf '{"event": "boinc_held_archive_pre_batch_tick", "ok": true}\n'
  elif [ "$status" -eq 124 ]; then
    printf '{"event": "boinc_held_archive_pre_batch_tick", "ok": false, "status": 124, "timed_out": true}\n'
    die "the tick before the batch was still running after $allowed seconds"
  else
    printf '{"event": "boinc_held_archive_pre_batch_tick", "ok": false, "status": %s}\n' "$status"
    echo "warning: the tick before the batch failed (status $status); the batch goes on" >&2
  fi
}
# Before a batch that does not follow a gap: lets the controller's locks go as gap() does, ticks,
# and pauses again.
tick_before_batch() {
  : > "$GAP_FILE"
  flock -u 9
  if [ -n "$TICK_LOCK" ]; then flock -u 8; exec 8>&-; fi
  pre_batch_tick
  flock -w 60 9 || die "the archive lock is held by another process after the tick"
  rm -f "$GAP_FILE"
  pause_controller
}
gap() {
  : > "$GAP_FILE"
  flock -u 9
  if [ -n "$TICK_LOCK" ]; then flock -u 8; exec 8>&-; fi
  start_controller || die "the controller did not start between batches"
  local started=$SECONDS limit seen=false ticked=false st
  limit=$((SECONDS + $(gap_secs)))
  while [ $SECONDS -lt $limit ]; do
    st=$(systemctl is-active "$UNIT.service" || true)
    case $st in
      active | activating | reloading) seen=true ;;
      *)
        if $seen; then
          ticked=true
          # The tick the gap began is the one before the batch, so a failed one is logged here.
          if [ "$st" = failed ]; then
            echo '{"event": "boinc_held_archive_pre_batch_tick", "ok": false, "gap_tick": true}'
            echo "warning: the tick in the gap failed; the batch goes on" >&2
          fi
          break
        fi ;;
    esac
    # No tick started: it was skipped or is not due; the next pause won't wait for one either.
    $seen || [ $((SECONDS - started)) -lt "$TICK_START_SECS" ] || break
    sleep 1
  done
  # The budget ran out in the gap (a tick can fill it): no batch follows, so the run ends here with
  # the controller left running, not paused again (that wait is capped to nothing and would fail
  # the run), and the closing dry run, which needs the pause, is skipped.
  if over_budget; then BUDGET_ENDED=true; return 0; fi
  # The tick start_controller began is the one before the batch when it ended in the gap; one
  # still running, or none, is joined or run by pre_batch_tick (a second would be a whole extra run).
  ! $PRE_TICK || $ticked || pre_batch_tick
  flock -w 60 9 || die "the archive lock is held by another process after the gap"
  rm -f "$GAP_FILE"
  pause_controller
}

sha256() { sha256sum "$1" | cut -d' ' -f1; }
# The last JSON line of a step's output, with the field asked for (or "" when it lacks it).
field() {
  python3 -c '
import json, sys
lines = [l for l in open(sys.argv[1]) if l.startswith("{")]
d = json.loads(lines[-1]) if lines else {}
v = d
for k in sys.argv[2].split("."):
    v = v.get(k) if isinstance(v, dict) else None
print("null" if v is None else json.dumps(v) if not isinstance(v, str) else v)
' "$1" "$2"
}

# Sends one dispatched batch to the BOINC host's bridge step and keeps its receipt.
bridge() {
  local n=$1 dir=$ARCHIVE/batches/$1 out
  local status=0
  out=$(mktemp)
  # In the background too, so a stop ends the ssh at once (the BOINC host's bridge step
  # resumes on the next run) and the controller is started again within the stop grace.
  {
    tar -C "$dir" -cf - manifest.json receipts/dispatch.json |
      "${STEP[@]}" --env "$ENV_NAME" --target archive --step bridge --var "ARCHIVE_BATCH=$n" \
        --wait-mins "$WAIT_MINS" > "$out"
  } &
  child=$!
  wait "$child" || status=$?
  child=""
  [ "$status" -eq 0 ] || { cat "$out"; rm -f "$out"; die "batch $n's bridge step failed"; }
  cat "$out"
  [ "$(field "$out" event)" = boinc_archive_bridge ] && [ "$(field "$out" complete)" = true ] &&
    [ "$(field "$out" manifest_sha256)" = "$(sha256 "$dir/manifest.json")" ] ||
    { rm -f "$out"; die "batch $n's bridge step gave no complete receipt for its manifest"; }
  tail -n 1 "$out" > "$dir/receipts/.bridge.json.tmp"
  mv -f "$dir/receipts/.bridge.json.tmp" "$dir/receipts/bridge.json"
  rm -f "$out"
}

# Batches dispatched but not bridged yet (a run that stopped between the steps).
for dir in "$ARCHIVE"/batches/[0-9][0-9][0-9][0-9]; do
  [ -f "$dir/receipts/dispatch.json" ] && [ ! -f "$dir/receipts/bridge.json" ] || continue
  bridge "$(basename "$dir")"
done

# True once no batch should start: the budget is spent, or what is left can't cover a typical batch.
# --batch-mins is the estimate until a batch has run; then it is the longest measured one.
LONGEST=$((BATCH_MINS * 60)) MEASURED=false BUDGET_ENDED=false
over_budget() {
  [ "$MAX_MINUTES" -ne 0 ] || return 1
  # Spent whatever the estimate (even 0), or the rest cannot cover it.
  [ "$SECONDS" -lt $((MAX_MINUTES * 60)) ] || return 0
  [ $((SECONDS + LONGEST)) -gt $((MAX_MINUTES * 60)) ]
}

# --max-pause-mins: the pages the next batch asks for. Without it, always --pages.
PAGES_FILE=$ARCHIVE/held-archive.pages
# Seconds, for tests: USNM_HELD_ARCHIVE_MAX_PAUSE_SECS replaces --max-pause-mins' minutes.
MAX_PAUSE_SECS=${USNM_HELD_ARCHIVE_MAX_PAUSE_SECS:-$((MAX_PAUSE_MINS * 60))}
BATCH_PAGES=$PAGES
if [ "$MAX_PAUSE_MINS" -ne 0 ]; then
  BATCH_PAGES=$FIRST_BATCH_PAGES
  if [ -f "$PAGES_FILE" ]; then
    stored=$(cat "$PAGES_FILE" 2> /dev/null || true)
    [[ $stored =~ ^[0-9]{1,6}$ ]] && [ "$stored" -ge 1 ] && BATCH_PAGES=$stored
  fi
  [ "$BATCH_PAGES" -le "$PAGES" ] || BATCH_PAGES=$PAGES
  [ "$BATCH_PAGES" -ge "$MIN_BATCH_PAGES" ] || BATCH_PAGES=$MIN_BATCH_PAGES
  [ "$BATCH_PAGES" -le "$PAGES" ] || BATCH_PAGES=$PAGES
fi
# After a batch of $1 pages that took $2 seconds: the next batch's pages, kept for the next run too.
size_next_batch() {
  [ "$MAX_PAUSE_MINS" -ne 0 ] || return 0
  local done_pages=$1 secs=$2 next
  [ "$done_pages" -ge 1 ] || return 0
  [ "$secs" -ge 1 ] || secs=1
  next=$((done_pages * MAX_PAUSE_SECS / secs))
  [ "$next" -le $((done_pages * 2)) ] || next=$((done_pages * 2))
  [ "$next" -le "$PAGES" ] || next=$PAGES
  [ "$next" -ge "$MIN_BATCH_PAGES" ] || next=$MIN_BATCH_PAGES
  [ "$next" -le "$PAGES" ] || next=$PAGES
  BATCH_PAGES=$next
  printf '%s\n' "$next" > "$PAGES_FILE.tmp" && mv -f "$PAGES_FILE.tmp" "$PAGES_FILE" ||
    echo "warning: could not keep the batch size in $PAGES_FILE" >&2
  printf '{"event": "boinc_held_archive_batch_size", "pages": %s, "seconds": %s, "next_pages": %s, "max_pause_mins": %s}\n' \
    "$done_pages" "$secs" "$next" "$MAX_PAUSE_MINS"
}

batches=() pages=0 held=0
out=$(mktemp)
# shellcheck disable=SC2154 # st is set in the trap itself
trap 'st=$?; rm -f "$out"; resume "$st"' EXIT
for i in $(seq 1 "$MAX_BATCHES"); do
  if [ "$i" -eq 1 ]; then
    # Held to the same test as the later batches: the pause, recovery bridges and the tick before
    # it can use the budget up.
    over_budget && break
    if $PRE_TICK; then
      tick_before_batch
      over_budget && break
    fi
  else
    # A batch under way is never cut short; the budget only keeps another from starting. It is
    # read again after the gap, which can itself take its --gap-mins and a tick's end.
    over_budget && break
    if [ "$GAP_MINS" -ne 0 ]; then
      gap
      over_budget && break
    elif $PRE_TICK; then
      tick_before_batch
      over_budget && break
    fi
  fi
  batch_start=$SECONDS
  # In the background, so a stop can reach it. A background job starts with SIGINT ignored,
  # so the wrapper turns SIGTERM into KeyboardInterrupt: the tool's finally blocks then run.
  python3 -c '
import runpy, signal, sys
def stop(*_):
    raise KeyboardInterrupt
signal.signal(signal.SIGTERM, stop)
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")
' "$TOOL" dispatch --include-held --min-age-hours "$MIN_AGE" --pages "$BATCH_PAGES" \
    --config "$CONFIG" --state-dir "$STATE_DIR" --archive-dir "$ARCHIVE" --controller-unit "$UNIT" \
    ${CHECKS[@]+"${CHECKS[@]}"} --apply --confirm-controller-paused > "$out" &
  child=$!
  status=0
  wait "$child" || status=$?
  child=""
  [ "$status" -eq 0 ] || { cat "$out"; die "the dispatch step failed; it resumes when run again"; }
  cat "$out"
  n=$(field "$out" batch)
  [ "$n" != null ] || break
  [[ $n =~ ^[0-9]{4}$ ]] && [ "$(field "$out" complete)" = true ] || die "the dispatch step gave no complete receipt"
  batch_pages=$(field "$out" pages)
  pages=$((pages + batch_pages))
  held=$((held + $(python3 -c '
import json, sys
print(sum(1 for p in json.load(open(sys.argv[1]))["pages"] if p.get("held")))
' "$ARCHIVE/batches/$n/manifest.json")))
  batches+=("$n")
  bridge "$n"
  # The longest batch so far is what the next one is expected to take.
  if ! $MEASURED || [ $((SECONDS - batch_start)) -gt "$LONGEST" ]; then LONGEST=$((SECONDS - batch_start)); fi
  MEASURED=true
  size_next_batch "$batch_pages" $((SECONDS - batch_start))
done
if $BUDGET_ENDED; then
  printf '{"event": "boinc_held_archive", "batches": %s, "pages": %s, "held": %s, "budget_ended": true, "closing_dry_run": "skipped"}\n' \
    "$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1].split()))' "${batches[*]:-}")" "$pages" "$held"
  exit 0
fi
# The counts after: a dry run for the journals and what is left, and the state file.
# In the background as well, so a stop doesn't wait for it.
python3 "$TOOL" dispatch --include-held --min-age-hours "$MIN_AGE" --pages "$PAGES" \
  --config "$CONFIG" --state-dir "$STATE_DIR" --archive-dir "$ARCHIVE" --controller-unit "$UNIT" \
  ${CHECKS[@]+"${CHECKS[@]}"} > "$out" &
child=$!
status=0
wait "$child" || status=$?
child=""
[ "$status" -eq 0 ] || die "the closing dry run failed"
python3 -c '
import json, sys
d = [json.loads(l) for l in open(sys.argv[1]) if l.startswith("{")][-1]
state = json.load(open(sys.argv[2]))
batches = sys.argv[3].split()
print(json.dumps({"event": "boinc_held_archive", "batches": batches, "pages": int(sys.argv[4]),
                  "held": int(sys.argv[5]), "journals": d.get("journals"), "journal_cap": d.get("journal_cap"),
                  "eligible_left": (d.get("pages") or {}).get("eligible"),
                  "state": {k: len(state.get(k) or {}) for k in ("claims", "selected", "held_since", "archived_held")}},
                 sort_keys=True))
' "$out" "$STATE_DIR/state.json" "${batches[*]:-}" "$pages" "$held"
