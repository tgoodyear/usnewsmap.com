#!/usr/bin/env bash
# The nightly archive report (#2008) on the controller host (CT 211), as
# usnm-boinc-archive-report.timer runs it: a dry run of one archive batch and a report of it.
#
# It never applies. docs/boinc/server/batch-scheduler.md "Archive drained history (#1483)" has
# the maintainer approve each live run (pause every plan, an operator's blob token, the bridge
# and db steps on other hosts), so a timer must not start one. This run only reads what
# `archive_history.py dispatch` reads, without --apply: it takes no lock and lease, stops no
# timer and writes nothing but its report. The maintainer runs the batch it names.
#
# Stop conditions, each one a line of JSON and an exit status:
#   skipped (exit 0)  no page is drained and old enough (pages.eligible is 0), or a batch is
#                     planned already and waiting for its steps (resume): nothing to report.
#   failure (exit 1)  the dry run fails or prints no result; its journals and state don't agree
#                     (outstanding_pages.error); archiving the batch would leave pages needing
#                     work again (outstanding_pages.unchanged is not true); the batch is larger
#                     than --pages; the archive dataset lacks the space the apply needs; or the
#                     archive dataset isn't mounted. No check is weakened or skipped: the tool's
#                     own are the ones that decide.
#   ready (exit 0)    a `boinc_archive_report` line with "ready": true, the batch's pages and
#                     bytes, and the journal counts before and after, kept as
#                     <report dir>/latest.json.
#
#   archive-report.sh [--config FILE] [--state-dir DIR] [--archive-dir DIR] [--pages N]
#                     [--min-age-hours N] [--controller-unit UNIT] [--report-dir DIR]
#                     [--tool FILE] [--check-state-dir DIR]... [--allow-root-device]
#
# Other plans' state directories are passed as --check-state-dir, as held-archive.sh does (#1713):
# a page another plan has claimed or selected is never eligible. Without --check-state-dir,
# every sibling `<state dir>.*` with a state.json is used. The command the report names for the
# maintainer carries the same --check-state-dir list, shell-quoted, because `dispatch --apply`
# finds no other plan's directory itself.
#
# The archive directory must be on another device than /, so an unmounted dataset's empty
# directory on the root filesystem doesn't pass; --allow-root-device (tests) turns that off.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() {
  printf '{"event": "boinc_archive_report_failure", "error": %s}\n' "$(python3 -c 'import json, sys; print(json.dumps(sys.argv[1]))' "$*")"
  echo "error: $*" >&2
  exit 1
}

HERE=$(cd "$(dirname "$0")/.." && pwd)
TOOL=$HERE/archive_history.py
CONFIG=/etc/usnm/boinc-priority-controller.json
STATE_DIR=/var/lib/usnm-priority-controller
ARCHIVE=/srv/usnm-archive
REPORT_DIR=/var/lib/usnm-archive-report
PAGES=1000 MIN_AGE=24
UNIT=${USNM_CONTROLLER_UNIT:-usnm-boinc-priority-controller@main}
ROOT_DEVICE_OK=false
CHECK_DIRS=()
while [ $# -gt 0 ]; do
  case $1 in
    --config) [ $# -ge 2 ] || usage; CONFIG=$2; shift 2 ;;
    --state-dir) [ $# -ge 2 ] || usage; STATE_DIR=$2; shift 2 ;;
    --archive-dir) [ $# -ge 2 ] || usage; ARCHIVE=$2; shift 2 ;;
    --report-dir) [ $# -ge 2 ] || usage; REPORT_DIR=$2; shift 2 ;;
    --pages) [ $# -ge 2 ] || usage; PAGES=$2; shift 2 ;;
    --min-age-hours) [ $# -ge 2 ] || usage; MIN_AGE=$2; shift 2 ;;
    --controller-unit) [ $# -ge 2 ] || usage; UNIT=$2; shift 2 ;;
    --tool) [ $# -ge 2 ] || usage; TOOL=$2; shift 2 ;;
    --check-state-dir) [ $# -ge 2 ] || usage; CHECK_DIRS+=("$2"); shift 2 ;;
    --allow-root-device) ROOT_DEVICE_OK=true; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
while [ "${STATE_DIR%/}" != "$STATE_DIR" ] && [ "$STATE_DIR" != / ]; do STATE_DIR=${STATE_DIR%/}; done
if [ ${#CHECK_DIRS[@]} -eq 0 ]; then
  for dir in "$STATE_DIR".*; do
    [ -f "$dir/state.json" ] && CHECK_DIRS+=("$dir")
  done
fi
CHECKS=()
for dir in ${CHECK_DIRS[@]+"${CHECK_DIRS[@]}"}; do CHECKS+=(--check-state-dir "$dir"); done
if ! [[ $PAGES =~ ^[0-9]{1,4}$ && $MIN_AGE =~ ^[0-9]{1,5}$ ]]; then die "--pages and --min-age-hours are numbers"; fi
if [ "$PAGES" -lt 1 ] || [ "$PAGES" -gt 5000 ]; then die "--pages is 1 to 5000"; fi
[ -f "$TOOL" ] || die "no $TOOL"
[ -d "$ARCHIVE/batches" ] || die "no $ARCHIVE/batches: the archive's own dataset isn't mounted"
if ! $ROOT_DEVICE_OK && [ "$(stat -c %d "$ARCHIVE")" = "$(stat -c %d /)" ]; then
  die "$ARCHIVE is on the root filesystem: the archive's own dataset isn't mounted"
fi
mkdir -p "$REPORT_DIR"

out=$(mktemp)
trap 'rm -f "$out"' EXIT
status=0
python3 "$TOOL" dispatch --min-age-hours "$MIN_AGE" --pages "$PAGES" \
  --config "$CONFIG" --state-dir "$STATE_DIR" --archive-dir "$ARCHIVE" --controller-unit "$UNIT" \
  ${CHECKS[@]+"${CHECKS[@]}"} > "$out" || status=$?
[ "$status" -eq 0 ] || { cat "$out"; die "the dry run failed (status $status)"; }

python3 - "$out" "$ARCHIVE" "$PAGES" "$REPORT_DIR" "$UNIT" "$MIN_AGE" "$STATE_DIR" "$CONFIG" "$TOOL" \
  ${CHECK_DIRS[@]+"${CHECK_DIRS[@]}"} << 'PY'
import json, shlex, shutil, sys, time
from pathlib import Path

out, archive, pages, report_dir, unit, min_age, state_dir, config, tool, *check_dirs = sys.argv[1:]
pages = int(pages)


def fail(message):
    print(json.dumps({"event": "boinc_archive_report_failure", "error": message}))
    print(f"error: {message}", file=sys.stderr)
    sys.exit(1)


lines = [json.loads(line) for line in open(out) if line.startswith("{")]
result = lines[-1] if lines else {}
if result.get("event") != "boinc_archive_dispatch" or result.get("apply") is not False:
    fail("the dry run printed no boinc_archive_dispatch result")
batch = result.get("batch") or {}
eligible = (result.get("pages") or {}).get("eligible")
summary = {"event": "boinc_archive_report", "schema_version": 1, "at": int(time.time()),
           "plan": result.get("plan"), "pages_limit": pages, "eligible": eligible,
           "journals": result.get("journals"), "journal_cap": result.get("journal_cap")}
if result.get("resume"):
    # A batch an earlier run stopped half way: its stores can disagree with the journals until
    # its steps are finished, so the consistency checks below don't apply yet.
    summary.update(ready=False, skipped=f"batch {result['resume']['batch']} is planned already: finish its steps first",
                   resume=result["resume"])
else:
    checks = result.get("outstanding_pages") or {}
    if checks.get("error") is not None:
        fail(f"the journals and state don't agree already: {checks['error']}")
    if checks.get("unchanged") is not True:
        fail("archiving this batch would leave pages needing work again")
    if type(eligible) is not int or type(batch.get("pages")) is not int or batch["pages"] > pages:
        fail("the dry run's page counts are missing or larger than --pages")
    if eligible == 0 or batch["pages"] == 0:
        summary.update(ready=False, skipped="no page is drained and old enough to archive")
    else:
        free = shutil.disk_usage(archive).free
        need = batch["bytes"] * 1.1 + (64 << 20)
        if free <= need:
            fail(f"not enough free space in {archive} for the batch: {free} bytes free, {int(need)} needed")
        summary.update(
            ready=True, batch=batch, journals_after=result["after_batch"]["journals"], archive_free_bytes=free,
            next_step=("the maintainer approves the run (docs/boinc/server/batch-scheduler.md, Archive drained "
                       "history): " + " ".join(shlex.quote(word) for word in [
                           "python3", tool, "dispatch", "--apply", "--confirm-controller-paused",
                           "--pages", str(pages), "--min-age-hours", min_age, "--controller-unit", unit,
                           "--config", config, "--state-dir", state_dir, "--archive-dir", archive,
                           *[word for directory in check_dirs for word in ("--check-state-dir", directory)]])))
text = json.dumps(summary, sort_keys=True)
target = Path(report_dir) / "latest.json"
temp = target.with_name(".latest.json.tmp")
temp.write_text(text + "\n")
temp.replace(target)
print(text)
PY
