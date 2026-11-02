#!/usr/bin/env bash
# Node-side wall-clock watchdog, run by a systemd timer so it survives the driving session.
# Instance labels are "usnm-gpu-<study>-d<unix deadline>". Past its deadline an instance is destroyed.
# Below --credit-floor, or past --final-deadline, every study instance is destroyed. Other labels are left alone.
set -uo pipefail
HERE=$(dirname "$(readlink -f "$0")"); FLOOR=9.89; FINAL=0; LOG=/var/log/usnm-gpu-studies-watchdog.log
while [ $# -gt 0 ]; do case "$1" in
  --credit-floor) FLOOR=$2; shift 2;; --final-deadline) FINAL=$2; shift 2;; --log) LOG=$2; shift 2;;
  *) echo "unknown flag: $1" >&2; exit 2;; esac; done
now=$(date +%s); credit=$("$HERE/vast.sh" --action credit 2>/dev/null || echo "")
list=$("$HERE/vast.sh" --action list 2>/dev/null) || { echo "$(date -Is) list failed" >>"$LOG"; exit 1; }
low=0; [ -n "$credit" ] && awk -v c="$credit" -v f="$FLOOR" 'BEGIN{exit !(c<f)}' && low=1
echo "$list" | jq -r 'select(.label!=null)|select(.label|startswith("usnm-gpu-"))|"\(.id) \(.label)"' |
while read -r id label; do
  deadline=${label##*-d}; case "$deadline" in ''|*[!0-9]*) deadline=0;; esac
  if [ "$now" -ge "$deadline" ] || [ "$low" = 1 ] || { [ "$FINAL" -gt 0 ] && [ "$now" -ge "$FINAL" ]; }; then
    "$HERE/vast.sh" --action destroy --instance "$id" && echo "$(date -Is) destroyed $id $label credit=$credit low=$low" >>"$LOG"
  fi
done
echo "$(date -Is) tick credit=$credit instances=$(echo -n "$list" | grep -c . )" >>"$LOG"
if [ "$FINAL" -gt 0 ] && [ "$now" -ge "$FINAL" ] && [ -z "$list" ]; then
  shred -u ${GPU_STUDY_SECRETS}/vast.key 2>/dev/null; systemctl stop usnm-gpu-studies-watchdog.timer
fi
exit 0
