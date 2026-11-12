#!/usr/bin/env bash
# Installs the controller host's systemd units (#1690): the per-plan controller template
# (usnm-boinc-priority-controller@.service and .timer), each plan's timer cadence, and the
# held-page archive's units. Run it
# as root on the controller host (CT 211) from the pinned checkout the units run
# (/opt/usnm), after the checkout has moved to a commit with these files. Safe to run again.
#
#   install.sh [--root DIR]
#
# Each plan with a flags file in /etc/usnm (boinc-priority-controller.flags for main,
# boinc-priority-controller.PLAN.flags for the others) gets
# usnm-boinc-priority-controller@PLAN.timer.d/cadence.conf from its @kind, @every and @boot
# (controller-tick.sh --print-timer). Every flags file is checked first; one that
# controller-tick.sh refuses stops the install before anything is written. Run it again after
# changing a plan's cadence or adding a plan.
#
# It enables, starts and stops nothing, and never writes /etc/usnm: moving the plans onto the
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
# Each plan's cadence drop-in, made before anything is written.
staged=$(mktemp -d)
trap 'rm -rf "$staged"' EXIT
plans=()
for flags in "$ROOT"/etc/usnm/boinc-priority-controller.flags "$ROOT"/etc/usnm/boinc-priority-controller.*.flags; do
  [ -f "$flags" ] || continue
  name=$(basename "$flags" .flags)
  plan=${name#boinc-priority-controller}
  plan=${plan#.}
  # Only the unsuffixed file is main's; .main. or an empty name would be read as that file.
  if [ -z "$plan" ]; then
    [ "$name" = boinc-priority-controller ] || die "$flags: not a plan's flags file name; nothing installed"
    plan=main
  elif [ "$plan" = main ]; then
    die "$flags: main's flags file is boinc-priority-controller.flags; nothing installed"
  fi
  bash "$HERE/controller-tick.sh" --plan "$plan" --config-dir "$ROOT/etc/usnm" --print-timer \
    > "$staged/$plan.conf" || die "plan $plan's flags file $flags is refused (above); nothing installed"
  plans+=("$plan")
done
install -d -m 0755 "$units"
for file in "$HERE/usnm-boinc-priority-controller@.service" "$HERE/usnm-boinc-priority-controller@.timer" \
  "$ONPREM/usnm-boinc-held-archive.service" "$ONPREM/usnm-boinc-held-archive.timer"; do
  install -m 0644 "$file" "$units/$(basename "$file")"
  echo "installed $units/$(basename "$file")"
done
for plan in ${plans[@]+"${plans[@]}"}; do
  install -d -m 0755 "$units/usnm-boinc-priority-controller@$plan.timer.d"
  install -m 0644 "$staged/$plan.conf" "$units/usnm-boinc-priority-controller@$plan.timer.d/cadence.conf"
  echo "installed $units/usnm-boinc-priority-controller@$plan.timer.d/cadence.conf"
done
if $RELOAD; then
  systemctl daemon-reload
  echo "systemd reloaded; nothing enabled or started"
fi
