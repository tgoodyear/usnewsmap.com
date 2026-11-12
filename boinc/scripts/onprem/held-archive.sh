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
# No db step (#1587). The receipts are in <archive dir>/batches/<NNNN>/receipts/. Each run ends
# with one `boinc_held_archive` JSON line: the batches, pages and held pages it archived, and the
# journal and state counts after. A failure exits non-zero, so the unit fails; the controller's
# feed source reads this unit too (--maintenance-unit) and reports that as a controller failure.
#
#   held-archive.sh [--config FILE] [--state-dir DIR] [--archive-dir DIR] [--pages N]
#                   [--min-age-hours N] [--max-batches N] [--wait-mins N] [--tool FILE]
#                   [--check-state-dir DIR]... [--tick-lock FILE]
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
PAGES=1000 MIN_AGE=1 MAX_BATCHES=10 WAIT_MINS=60
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
for n in "$PAGES" "$MIN_AGE" "$MAX_BATCHES" "$WAIT_MINS"; do
  [[ $n =~ ^[0-9]{1,5}$ ]] || die "--pages, --min-age-hours, --max-batches and --wait-mins are numbers"
done
[ "$MAX_BATCHES" -ge 1 ] && [ "$WAIT_MINS" -ge 1 ] || die "--max-batches and --wait-mins are at least 1"
[ -f "$TOOL" ] || die "no $TOOL"
[ -d "$ARCHIVE/batches" ] || die "no $ARCHIVE/batches: the archive's own dataset isn't mounted"

exec 9> "$LOCK"
flock -n 9 || die "another held-page archive is running"

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
# Starts the timer and a tick (so the feed source sees one soon after the pause). Either
# failing fails the run, whatever its status was, so the alert sees a controller left stopped.
resume() {
  local status=${1:-$?} failed=false
  systemctl start "$UNIT.timer" || { echo "error: could not start $UNIT.timer again; start it by hand" >&2; failed=true; }
  systemctl start --no-block "$UNIT.service" || { echo "error: could not start a $UNIT tick" >&2; failed=true; }
  if $failed; then
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
systemctl stop "$UNIT.timer"
deadline=$((SECONDS + WAIT_MINS * 60))
until state=$(systemctl is-active "$UNIT.service" || true); [ "$state" = inactive ] || [ "$state" = failed ]; do
  [ $SECONDS -lt $deadline ] || die "a controller tick is still running after $WAIT_MINS minutes"
  sleep 5
done
# Held until exit: resume() starts the controller again while it is still held, and that tick
# waits on the lock for the moment this run ends. flock runs as a waited-for child so a stop
# during the wait reaches on_stop at once (bash defers traps until a foreground command ends);
# the lock belongs to fd 8's open file, so it stays held after the child exits.
if [ -n "$TICK_LOCK" ]; then
  exec 8> "$TICK_LOCK"
  flock -w $((deadline > SECONDS ? deadline - SECONDS : 1)) 8 &
  child=$!
  wait "$child" || die "another tick still holds $TICK_LOCK after $WAIT_MINS minutes"
  child=""
fi

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

batches=() pages=0 held=0
out=$(mktemp)
# shellcheck disable=SC2154 # st is set in the trap itself
trap 'st=$?; rm -f "$out"; resume "$st"' EXIT
for _ in $(seq 1 "$MAX_BATCHES"); do
  # In the background, so a stop can reach it. A background job starts with SIGINT ignored,
  # so the wrapper turns SIGTERM into KeyboardInterrupt: the tool's finally blocks then run.
  python3 -c '
import runpy, signal, sys
def stop(*_):
    raise KeyboardInterrupt
signal.signal(signal.SIGTERM, stop)
sys.argv = sys.argv[1:]
runpy.run_path(sys.argv[0], run_name="__main__")
' "$TOOL" dispatch --include-held --min-age-hours "$MIN_AGE" --pages "$PAGES" \
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
  pages=$((pages + $(field "$out" pages)))
  held=$((held + $(python3 -c '
import json, sys
print(sum(1 for p in json.load(open(sys.argv[1]))["pages"] if p.get("held")))
' "$ARCHIVE/batches/$n/manifest.json")))
  batches+=("$n")
  bridge "$n"
done
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
