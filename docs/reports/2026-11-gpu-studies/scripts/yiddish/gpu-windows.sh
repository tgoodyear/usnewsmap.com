#!/usr/bin/env bash
# Repeats bounded GPU windows until every unit under the given directories has a result:
# --gap N seconds of production time between windows, --max-windows N, --seconds N a window,
# --units DIR (repeatable), --unit-seconds N, --log FILE, --after UNIT (a systemd unit to wait for first).
set -uo pipefail
GAP=600; MAXW=14; SECS=2400; UNIT_S=800; LOG=""; AFTER=""; UNITS=()
while [ $# -gt 0 ]; do case "$1" in --gap) GAP=$2; shift 2;; --max-windows) MAXW=$2; shift 2;; --seconds) SECS=$2; shift 2;; --units) UNITS+=("$2"); shift 2;; --unit-seconds) UNIT_S=$2; shift 2;; --log) LOG=$2; shift 2;; --after) AFTER=$2; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
D=$(cd "$(dirname "$0")/.." && pwd)
[ -n "$AFTER" ] && while systemctl is-active -q "$AFTER"; do sleep 30; done
left() { for u in "${UNITS[@]}"; do for b in "$u"/b*; do [ -f "$b/result.json" ] || echo "$b"; done; done | wc -l; }
args=(); for u in "${UNITS[@]}"; do args+=(--units "$u"); done
for w in $(seq "$MAXW"); do
  [ "$(left)" -eq 0 ] && { echo "$(date -Is) all units done" >> "$LOG"; exit 0; }
  sleep "$GAP"
  "$D/scripts/gpu-window.sh" --seconds "$SECS" "${args[@]}" --unit-seconds "$UNIT_S" --log "$LOG" --wait-max 5400
done
echo "$(date -Is) stopped after $MAXW windows, $(left) units left" >> "$LOG"
