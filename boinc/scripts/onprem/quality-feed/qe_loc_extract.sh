#!/bin/sh
# qe-loc-v1 extraction (usnewsmap.com#1989): one bulk OCR archive at a time, read once, as the
# unprivileged user qeloc on CT 211, nice 19 / ionice idle. Resumable per archive (a done/
# marker). The running extractor is stopped (SIGSTOP, by PID) only when pve-big is harmed:
# io full avg60 above 75 for 5 samples a minute apart; it continues when that clears. Also
# stopped from 18 Nov 9:00 AM ET (14:00 UTC) while STATE/rebuild-hold exists. STATE/stop ends it.
# Usage: qe_loc_extract.sh --state DIR --qe-dir DIR --bin PATH
set -u
state=; qe_dir=; bin=
while [ $# -gt 0 ]; do case "$1" in
  --state) state=$2; shift 2;; --qe-dir) qe_dir=$2; shift 2;; --bin) bin=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
archives=/tank/locbulk/ocr
mkdir -p "$state/done" "$state/work" "$state/units" "$state/langs"
log() { echo "$(date -u +%FT%TZ) $*" >> "$state/run.log"; }
io60() { sed -n 's/^full avg10=[0-9.]* avg60=\([0-9]*\).*/\1/p' /proc/pressure/io; }
held() {
  [ "$(date -u +%s)" -ge "$(date -u -d 2026-11-18T14:00:00Z +%s)" ] && [ -e "$state/rebuild-hold" ]
}
# Runs "$@" in the background and supervises it; returns its status.
supervise() {
  "$@" & pid=$!
  hot=0; stopped=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 60
    if [ "$(io60)" -gt 75 ] 2>/dev/null; then hot=$((hot+1)); else hot=0; fi
    if [ "$stopped" = 0 ] && { [ "$hot" -ge 5 ] || held; }; then
      kill -STOP "$pid"; stopped=1; log "stopped $pid (io60 $(io60), hold $(held && echo yes || echo no))"
    elif [ "$stopped" = 1 ] && [ "$hot" -eq 0 ] && ! held; then
      kill -CONT "$pid"; stopped=0; log "continued $pid"
    fi
  done
  wait "$pid"
}
for f in $(ls "$archives" | grep '\.tar\.bz2$'); do
  [ -e "$state/stop" ] && { log stop; exit 0; }
  [ -e "$state/done/$f" ] && continue
  while held; do sleep 600; done
  tmp="$state/work/$f"; rm -rf "$tmp"; mkdir -p "$tmp"
  if [ -s "$state/langs/$f.tsv" ] \
     && supervise nice -n 19 ionice -c3 "$bin" quality qe-text-extract --archive "$archives/$f" --out "$tmp/extract.jsonl" 2>> "$state/run.log" \
     && nice -n 19 ionice -c3 "$bin" quality qe-text-stage --extract "$tmp/extract.jsonl" --archive "$f" \
          --languages "$state/langs/$f.tsv" --title-languages "$state/titles.json" \
          --qe-dir "$qe_dir" --root "$state" 2>> "$state/run.log"; then
    : > "$state/done/$f"; log "done $f"
  else
    log "failed $f"
  fi
  rm -rf "$tmp"
done
log "all archives done"
