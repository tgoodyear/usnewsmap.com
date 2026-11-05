#!/usr/bin/env bash
# Run under supervisord as root; the project's own tools run as boincadm. bin/start daemonizes
# the project daemons and returns, so this stays in the foreground, runs the periodic tasks
# and restarts anything that stopped (what the project's cron line would do), within a bound on
# feeder restarts.
set -euo pipefail

run() { su -s /bin/bash boincadm -c "cd '$proj' && $1"; }
epoch() { date +%s; }

proc=/proc
# The loop restarts a feeder it finds stopped at most restart_limit times in any restart_window
# seconds; the next time, it holds (docs/boinc/feeder-refresh.md, "The loop's restart bound").
restart_limit=3
restart_window=3600
restarts=""

# bin/start opens pid_<host>/start.lock.<host>, marks it inheritable so every daemon it starts
# keeps it open, and unlinks it on the way out. On the Azure Files project share a file that is
# unlinked while still open stays delete-pending until the last handle closes, and opening it
# again fails with ENOENT, so every later `bin/start --cron` died at its lock before restarting
# any daemon that had stopped. The pid directory belongs to one host (bin/start names it after
# the short hostname, as the status page does), so it lives on this container's own disk and is
# linked into the project tree, where server_status.php reads it.
local_pid_dir() {
  local link dir
  host=$(python3 -c 'import socket; print(socket.gethostname().split(".")[0])')
  link="$proj/pid_$host"
  dir="${USNM_PID_ROOT:-/var/lib/usnm-boinc}/pid_$host"
  mkdir -p "$dir"
  chown "${USNM_BOINC_USER:-boincadm}:${USNM_BOINC_GROUP:-boincadm}" "$dir"
  chmod 0775 "$dir"
  if [ -L "$link" ]; then
    [ "$(readlink "$link")" = "$dir" ] || ln -sfn "$dir" "$link"
  elif [ -e "$link" ]; then
    # A directory left by an earlier start under this hostname (a project volume reused with a
    # fixed hostname, outside Container Apps). Leave it: its pid files may name live daemons.
    echo "usnm-boinc: $link is not a link; keeping it as bin/start's pid directory" >&2
  else
    ln -s "$dir" "$link"
  fi
  hold="$dir/usnm-feeder-restart-hold"
}

# stop_maintenance in the project directory (docs/boinc/server-and-apps.md, "Moving the project share") holds
# the project stopped across container starts: bin/start would clear stop_sched and start the
# daemons, so neither it nor the periodic start runs until an operator removes the marker.
maintenance() {
  [ -e "$proj/stop_maintenance" ] || return 1
  echo "usnm-boinc: $proj/stop_maintenance is present; not starting the project" >&2
}

# USNM_FEEDER_RESTART_LIMIT (0 to 9999; 0 never restarts) and USNM_FEEDER_RESTART_WINDOW
# (1 to 999999 seconds) override the bound. Any other value is reported and the default kept.
restart_bound() {
  local limit=${USNM_FEEDER_RESTART_LIMIT-} window=${USNM_FEEDER_RESTART_WINDOW-}
  if [[ $limit =~ ^[0-9]{1,4}$ ]]; then
    restart_limit=$((10#$limit))
  elif [ -n "$limit" ]; then
    echo "usnm-boinc: ignoring USNM_FEEDER_RESTART_LIMIT=$limit; using $restart_limit" >&2
  fi
  if [[ $window =~ ^[0-9]{1,6}$ ]] && [ $((10#$window)) -gt 0 ]; then
    restart_window=$((10#$window))
  elif [ -n "$window" ]; then
    echo "usnm-boinc: ignoring USNM_FEEDER_RESTART_WINDOW=$window; using $restart_window" >&2
  fi
}

# The feeder bin/start would see as running: feeder.pid names a live process called feeder.
feeder_running() {
  local pid state
  pid=$(head -c 16 "$proj/pid_$host/feeder.pid" 2>/dev/null | tr -d '[:space:]') || return 1
  [[ $pid =~ ^[0-9]+$ ]] || return 1
  [ "$(cat "$proc/$pid/comm" 2>/dev/null)" = feeder ] || return 1
  state=$(cat "$proc/$pid/stat" 2>/dev/null) || return 1
  state=${state##*) }
  [ "${state%% *}" != Z ]
}

# The hold marker is on the container's own disk: a container start clears it, and only an
# operator removes it otherwise.
on_hold() {
  [ -e "$hold" ] || return 1
  echo "usnm-boinc: $hold is present; not starting the project" >&2
}

# Succeeds when this run of bin/start --cron must not happen. A disabled project (bin/stop
# leaves stop_daemons) or a running feeder needs no restart and doesn't count.
held() {
  local now t kept="" count=0
  on_hold && return 0
  if [ -e "$proj/stop_daemons" ] || feeder_running; then
    return 1
  fi
  now=$(epoch)
  for t in $restarts; do
    if [ $((now - t)) -lt "$restart_window" ]; then
      kept="$kept $t"
      count=$((count + 1))
    fi
  done
  if [ "$count" -ge "$restart_limit" ]; then
    printf 'held_at=%s restarts=%s window_s=%s\n' "$now" "$count" "$restart_window" > "$hold"
    echo "usnm-boinc: the feeder is not running after $count restarts in ${restart_window}s; holding the project until $hold is removed" >&2
    restarts=""
    return 0
  fi
  restarts="$kept $now"
  echo "usnm-boinc: the feeder is not running; restart $((count + 1)) of $restart_limit in ${restart_window}s" >&2
  return 1
}

main() {
  : "${PROJECT_DIR:?}"
  proj="$PROJECT_DIR/${PROJECT:-usnmocr}"
  until [ -f "$proj/config.xml" ]; do sleep 5; done
  local_pid_dir
  restart_bound
  while maintenance || on_hold; do sleep 60; done
  run 'bin/start'
  while sleep 300; do maintenance || held || run 'bin/start --cron' || true; done
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main
fi
