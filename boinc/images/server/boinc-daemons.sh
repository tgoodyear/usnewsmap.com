#!/usr/bin/env bash
# Run under supervisord as root; the project's own tools run as boincadm. bin/start daemonizes
# the project daemons and returns, so this stays in the foreground, runs the periodic tasks
# and restarts anything that stopped (what the project's cron line would do).
set -euo pipefail

run() { su -s /bin/bash boincadm -c "cd '$proj' && $1"; }

# bin/start opens pid_<host>/start.lock.<host>, marks it inheritable so every daemon it starts
# keeps it open, and unlinks it on the way out. On the Azure Files project share a file that is
# unlinked while still open stays delete-pending until the last handle closes, and opening it
# again fails with ENOENT, so every later `bin/start --cron` died at its lock before restarting
# any daemon that had stopped. The pid directory belongs to one host (bin/start names it after
# the short hostname, as the status page does), so it lives on this container's own disk and is
# linked into the project tree, where server_status.php reads it.
local_pid_dir() {
  local host link dir
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
}

# stop_maintenance in the project directory (boinc/docs/server-and-apps.md, "Moving the project share") holds
# the project stopped across container starts: bin/start would clear stop_sched and start the
# daemons, so neither it nor the periodic start runs until an operator removes the marker.
maintenance() {
  [ -e "$proj/stop_maintenance" ] || return 1
  echo "usnm-boinc: $proj/stop_maintenance is present; not starting the project" >&2
}

main() {
  : "${PROJECT_DIR:?}"
  proj="$PROJECT_DIR/${PROJECT:-usnmocr}"
  until [ -f "$proj/config.xml" ]; do sleep 5; done
  local_pid_dir
  until ! maintenance; do sleep 60; done
  run 'bin/start'
  while sleep 300; do maintenance || run 'bin/start --cron' || true; done
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  main
fi
