# shellcheck shell=bash
# Bounded runs of a command, for scripts that call az from an unattended host: a hung `az` has
# no timeout of its own, and macOS has no `timeout` command. Sourced.
#
#   run_before_deadline DEADLINE COMMAND [ARG...]  runs COMMAND, a function included, and kills
#       its whole process group at DEADLINE (a value of bash's $SECONDS). Returns 124 when the
#       deadline passed, else the command's status. Output goes straight through.

# Job control gives each request/timer its own process group so the watchdog and
# cleanup kill their children too, without leaving Azure CLI or sleep processes.
run_before_deadline() (
  local deadline=$1 remaining request timer result=0
  shift
  remaining=$((deadline - SECONDS))
  [ "$remaining" -gt 0 ] || return 124
  set -m
  "$@" &
  request=$!
  (sleep "$remaining"; kill -KILL -- "-$request" 2>/dev/null) >/dev/null 2>&1 &
  timer=$!
  trap 'kill -KILL -- "-$request" "-$timer" 2>/dev/null || true' EXIT
  wait "$request" 2>/dev/null || result=$?
  kill -KILL -- "-$timer" 2>/dev/null || true
  wait "$timer" 2>/dev/null || true
  trap - EXIT
  [ "$SECONDS" -lt "$deadline" ] || return 124
  return "$result"
)
