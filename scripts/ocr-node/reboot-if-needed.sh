#!/usr/bin/env bash
# The weekly reboot window of an on-prem OCR node (#296). node-setup.sh
# installs this as /usr/local/sbin/usnm-ocr-reboot-if-needed and runs it
# from usnm-ocr-reboot.service, which usnm-ocr-reboot.timer starts once a
# week on the node's own day (node-setup.sh --reboot-day).
#
#   usnm-ocr-reboot-if-needed [--drain-timeout SECS] [--dnf-wait SECS] [--dry-run]
#
# dnf-automatic installs every update daily but never reboots
# (node-setup.sh sets reboot = never). This reboots only when
# `dnf needs-restarting -r` says an update needs it (the kernel, glibc,
# systemd and the like). It waits for a dnf-automatic run still going (up
# to --dnf-wait, default 1800 s; after that it skips this week), then stops
# usnm-ocr-worker.service and waits for it to exit, up to --drain-timeout
# (default 1260 s: the 1200 s page timeout and a minute), then reboots.
#
# On SIGTERM the worker stops taking pages, gives those in flight 60 s (its
# --shutdown-grace-secs) to finish, then abandons the rest without output;
# they reappear on the queue when their visibility runs out (30 minutes
# after they were taken) for either node, at the cost of one of their five
# receives. The unit's TimeoutStopSec (120 s) kills anything left, so
# `systemctl stop` normally returns within two minutes; --drain-timeout is
# the outer bound. Everything goes to the journal under usnm-ocr-reboot;
# a reboot and any problem are logged at Warning, which the Azure Monitor
# Agent collects from the node's syslog, the rest at Notice.
# --dry-run says what it would do and changes nothing.
set -euo pipefail
usage() { sed -n '7s/^# \{0,1\}//p' "$0" >&2; exit 2; }
# Under systemd, a <N> prefix sets a line's syslog priority.
pri() { if [ -n "${JOURNAL_STREAM:-}" ]; then printf '<%s>%s\n' "$1" "$2"; else printf '%s\n' "$2"; fi; }
log() { pri 5 "$*"; }
warn() { pri 4 "$*"; }

DRAIN_TIMEOUT=1260 DNF_WAIT=1800 DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --drain-timeout) DRAIN_TIMEOUT=${2:-}; shift 2 ;;
    --dnf-wait) DNF_WAIT=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $DRAIN_TIMEOUT =~ ^[1-9][0-9]*$ ]] || { echo "error: --drain-timeout: seconds" >&2; usage; }
[[ $DNF_WAIT =~ ^[0-9]+$ ]] || { echo "error: --dnf-wait: seconds" >&2; usage; }
[ "$(id -u)" -eq 0 ] || { echo "error: run as root" >&2; exit 1; }

WORKER=usnm-ocr-worker.service

# Exit status 1 means a reboot is needed, 0 not; anything else is a failure.
status=0
why=$(dnf -q needs-restarting -r 2>&1) || status=$?
case "$status" in
  0) log "no reboot needed"; exit 0 ;;
  1) warn "reboot needed: $(grep '^ *\*' <<< "$why" | tr -d ' *' | paste -sd, -)" ;;
  *) warn "error: dnf needs-restarting -r failed ($status): $why"; exit 1 ;;
esac

# Never reboot in the middle of a dnf transaction.
waited=0
while systemctl is-active --quiet dnf-automatic.service; do
  if [ "$waited" -ge "$DNF_WAIT" ]; then
    warn "warning: dnf-automatic is still running after $DNF_WAIT s; no reboot this week"
    exit 1
  fi
  [ "$waited" -gt 0 ] || log "waiting for dnf-automatic to finish"
  sleep 10
  waited=$((waited + 10))
done

if [ "$DRY_RUN" = true ]; then
  log "dry run: would stop $WORKER (up to $DRAIN_TIMEOUT s) and reboot"
  exit 0
fi

if systemctl is-active --quiet "$WORKER"; then
  log "stopping $WORKER (in-flight pages get the worker's grace; up to $DRAIN_TIMEOUT s in all)"
  start=$(date +%s)
  timeout "$DRAIN_TIMEOUT" systemctl stop "$WORKER" ||
    warn "warning: systemctl stop $WORKER returned $? after $(($(date +%s) - start)) s"
  if systemctl is-active --quiet "$WORKER"; then
    warn "warning: $WORKER still $(systemctl is-active "$WORKER" || true) after $DRAIN_TIMEOUT s; rebooting anyway"
  else
    log "$WORKER stopped after $(($(date +%s) - start)) s"
  fi
else
  log "$WORKER isn't running"
fi
warn "rebooting"
systemctl reboot
