# shellcheck shell=bash
# Bounded az calls, for scripts that wait on Azure from an unattended host: a hung `az` has no
# timeout of its own, and macOS has no `timeout` command. Sourced. baz_ro and baz_mut need a
# `baz` function (az with the script's subscription), which can be defined after this file is
# sourced.
#
#   run_before_deadline DEADLINE COMMAND [ARG...]  runs COMMAND, a function included, and kills
#       its whole process group at DEADLINE (a value of bash's $SECONDS). Returns 124 when the
#       deadline passed, else the command's status. Output goes straight through.
#   baz_ro ARG...   a read-only baz call. Each attempt is killed after BOINC_AZ_READ_TIMEOUT
#       seconds (default 60), and a failed or timed-out attempt is retried, up to
#       BOINC_AZ_READ_ATTEMPTS attempts (default 3) with BOINC_AZ_READ_BACKOFF seconds (default
#       5) between them. Only the attempt that succeeds prints on stdout. With BAZ_RO_UNTIL set
#       to a $SECONDS value, no attempt runs past it, except that the first attempt always gets
#       at least 10 seconds (or the per-attempt timeout, if that is shorter).
#   baz_mut ARG...  a baz call that changes something. It is killed after BOINC_AZ_WRITE_TIMEOUT
#       seconds (default 300) and never retried, because a request that timed out may have taken
#       effect; the caller decides what an unknown outcome means. Returns 124 on a timeout.
#
# A caller whose baz call is meant to run long (a stack deployment, an ACR build) keeps calling
# plain baz: neither wrapper replaces it.

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

# bounded_az_seconds NAME DEFAULT MIN: the value of the variable NAME as a whole number of
# seconds (DEFAULT if unset), at least MIN.
bounded_az_seconds() {
  local value=${!1:-$2}
  if ! [[ $value =~ ^[0-9]+$ ]] || [ "$((10#$value))" -lt "$3" ]; then
    echo "error: $1 must be a whole number of seconds, at least $3 (got \"$value\")" >&2
    return 1
  fi
  echo "$((10#$value))"
}

# bounded_az_words ARG...: the az command's own words, without the flags, to say what timed out.
bounded_az_words() {
  local words="" arg
  for arg in "$@"; do
    case $arg in -*) break ;; esac
    words="$words${words:+ }$arg"
  done
  printf '%s' "$words"
}

baz_ro() {
  local attempts timeout backoff n=0 rc out why t left pause
  attempts=$(bounded_az_seconds BOINC_AZ_READ_ATTEMPTS 3 1) &&
    timeout=$(bounded_az_seconds BOINC_AZ_READ_TIMEOUT 60 1) &&
    backoff=$(bounded_az_seconds BOINC_AZ_READ_BACKOFF 5 0) || return 1
  while :; do
    t=$timeout
    if [ -n "${BAZ_RO_UNTIL:-}" ]; then
      left=$((BAZ_RO_UNTIL - SECONDS))
      if [ "$n" -eq 0 ]; then
        [ "$left" -ge 10 ] || left=10
      elif [ "$left" -le 0 ]; then
        break
      fi
      [ "$left" -ge "$t" ] || t=$left
    fi
    n=$((n + 1))
    rc=0
    # The output is held until the attempt succeeds, so a retry never repeats part of it.
    out=$(run_before_deadline $((SECONDS + t)) baz "$@") || rc=$?
    if [ "$rc" -eq 0 ]; then
      [ -z "$out" ] || printf '%s\n' "$out"
      return 0
    fi
    if [ "$rc" -eq 124 ]; then why="timed out after ${t}s and was killed"; else why="failed (exit $rc)"; fi
    [ "$n" -lt "$attempts" ] || break
    pause=$backoff
    if [ -n "${BAZ_RO_UNTIL:-}" ]; then
      left=$((BAZ_RO_UNTIL - SECONDS))
      [ "$left" -ge 0 ] || left=0
      [ "$pause" -le "$left" ] || pause=$left
    fi
    echo "warning: az $(bounded_az_words "$@") $why (attempt $n of $attempts); retrying in ${pause}s" >&2
    [ "$pause" -eq 0 ] || sleep "$pause"
  done
  echo "error: az $(bounded_az_words "$@") $why; gave up after $n of $attempts attempts" >&2
  return "$rc"
}

baz_mut() {
  local timeout rc=0
  timeout=$(bounded_az_seconds BOINC_AZ_WRITE_TIMEOUT 300 1) || return 1
  run_before_deadline $((SECONDS + timeout)) baz "$@" || rc=$?
  if [ "$rc" -eq 124 ]; then
    echo "error: az $(bounded_az_words "$@") timed out after ${timeout}s and was killed; it is not retried, because it may have taken effect" >&2
  fi
  return "$rc"
}
