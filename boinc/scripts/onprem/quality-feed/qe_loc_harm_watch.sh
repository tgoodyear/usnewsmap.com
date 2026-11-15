#!/bin/sh
# Host-side companion of qe_loc_extract.sh (runs as root on pve-big, systemd service): writes
# STATE/harm while pve-big is harmed and removes it when it clears. Harmed means io full
# avg60 above 75 for 5 samples a minute apart, a new "blocked for more than" kernel message,
# or more than 30 tasks in D state. Usage: qe_loc_harm_watch.sh STATE
set -u
state=$1; hot=0; msgs=$(dmesg | grep -c 'blocked for more than'); clear=0
while :; do
  a=$(sed -n 's/^full avg10=[0-9.]* avg60=\([0-9]*\).*/\1/p' /proc/pressure/io)
  if [ "${a:-0}" -gt 75 ]; then hot=$((hot+1)); else hot=0; fi
  m=$(dmesg | grep -c 'blocked for more than')
  d=$(ps -eo stat= | grep -c '^D')
  if [ "$hot" -ge 5 ] || [ "$m" -gt "$msgs" ] || [ "$d" -gt 30 ]; then
    [ -e "$state/harm" ] || echo "$(date -u +%FT%TZ) harm: io60=$a hung_msgs=$((m-msgs)) dstate=$d" >> "$state/harm.log"
    : > "$state/harm"; clear=0
  else
    clear=$((clear+1))
    # Cleared for 5 minutes (20 samples of 15 s), and the kernel count is the new baseline.
    if [ "$clear" -ge 20 ] && [ -e "$state/harm" ]; then rm -f "$state/harm"; msgs=$m; echo "$(date -u +%FT%TZ) clear" >> "$state/harm.log"; fi
  fi
  sleep 15
done
