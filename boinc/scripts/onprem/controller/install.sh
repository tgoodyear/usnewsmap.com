#!/usr/bin/env bash
# Installs the controller host's systemd units (#1690): the per-plan controller template
# (usnm-boinc-priority-controller@.service and .timer) and the held-page archive's units. Run it
# as root on the controller host (CT 211) from the pinned checkout the units run
# (/opt/usnm), after the checkout has moved to a commit with these files. Safe to run again.
#
#   install.sh [--root DIR]
#
# It enables, starts and stops nothing, and never touches /etc/usnm: moving the plans onto the
# template is the migration in docs/boinc/server/batch-scheduler.md, "Per-plan ticks (#1690)".
# With --root other than /, the files go under that directory and systemd isn't reloaded (for
# tests and image builds). The units run /opt/usnm (under --root), which must already hold
# controller-tick.sh, so they never point at a missing script.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
HERE=$(cd "$(dirname "$0")" && pwd)
ONPREM=$(cd "$HERE/.." && pwd)
ROOT=/
while [ $# -gt 0 ]; do
  case $1 in
    --root) [ $# -ge 2 ] || die "--root needs a directory"; ROOT=$2; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -d "$ROOT" ] || die "--root $ROOT is not a directory"
# systemd is reloaded only for the real root; under any other root the files are only placed.
RELOAD=false
[ "$(cd "$ROOT" && pwd -P)" != / ] || RELOAD=true
# Without a trailing slash, so $ROOT/etc names one path ("" for the real root).
ROOT=${ROOT%/}
tick=$ROOT/opt/usnm/boinc/scripts/onprem/controller/controller-tick.sh
[ -f "$tick" ] || die "no $tick: move the checkout to a commit with the per-plan units first"
units=$ROOT/etc/systemd/system
install -d -m 0755 "$units"
for file in "$HERE/usnm-boinc-priority-controller@.service" "$HERE/usnm-boinc-priority-controller@.timer" \
  "$ONPREM/usnm-boinc-held-archive.service" "$ONPREM/usnm-boinc-held-archive.timer"; do
  install -m 0644 "$file" "$units/$(basename "$file")"
  echo "installed $units/$(basename "$file")"
done
if $RELOAD; then
  systemctl daemon-reload
  echo "systemd reloaded; nothing enabled or started"
fi
