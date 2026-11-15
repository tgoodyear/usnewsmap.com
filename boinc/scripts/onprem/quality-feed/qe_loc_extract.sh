#!/bin/sh
# qe-loc-v1 extraction (usnewsmap.com#1989): bulk OCR archives read once each, two at a time,
# as the unprivileged user qeloc on CT 211, nice 19 / ionice best-effort class 7. Resumable per
# archive (a done/ marker). A running extractor is stopped (SIGSTOP, by PID) while
# CTL/harm exists, and continued when it is gone: qe_loc_harm_watch.sh on the host writes it
# when pve-big is harmed (io full avg60 above 75 for 5 minutes, a "blocked for more than"
# kernel message, or more than 30 D-state tasks). From 18 Nov 9:00 AM ET (14:00 UTC) the run
# is paused until STATE/rebuild-done exists. STATE/stop ends it.
# Usage: qe_loc_extract.sh --state DIR --ctl DIR --qe-dir DIR --bin PATH [--workers 2]
# CTL is the root-owned directory qe_loc_harm_watch.sh writes harm to.
# Environment: QE_MAX_BACKLOG_GB (default 300) of unsubmitted units under STATE/units.
set -u
state=; ctl=; qe_dir=; bin=; workers=2
while [ $# -gt 0 ]; do case "$1" in
  --state) state=$2; shift 2;; --qe-dir) qe_dir=$2; shift 2;; --bin) bin=$2; shift 2;;
  --ctl) ctl=$2; shift 2;; --workers) workers=$2; shift 2;; *) echo "unknown $1" >&2; exit 2;; esac; done
archives=/tank/locbulk/ocr
mkdir -p "$state/done" "$state/work" "$state/units" "$state/langs"
log() { echo "$(date -u +%FT%TZ) [$1] $2" >> "$state/run.log"; }
held() {
  [ "$(date -u +%s)" -ge "$(date -u -d 2026-11-18T14:00:00Z +%s)" ] && [ ! -e "$state/rebuild-done" ]
}
paused() { held || [ -e "$ctl/harm" ]; }
# Runs "$@" in the background and supervises it; returns its status.
supervise() {
  w=$1; shift
  "$@" & pid=$!
  stopped=0
  while kill -0 "$pid" 2>/dev/null; do
    sleep 20
    if [ -e "$state/stop" ]; then
      kill -CONT "$pid" 2>/dev/null; kill -TERM "$pid" 2>/dev/null; wait "$pid" 2>/dev/null
      log "$w" "stop: ended $pid"; return 143
    fi
    if [ "$stopped" = 0 ] && paused; then
      kill -STOP "$pid"; stopped=1; log "$w" "stopped $pid"
    elif [ "$stopped" = 1 ] && ! paused; then
      kill -CONT "$pid"; stopped=0; log "$w" "continued $pid"
    fi
  done
  wait "$pid"
}
backlog_gb() {
  for d in "$state"/units/*/; do [ -e "${d}submitted" ] || echo "$d"; done | xargs -r du -sc -B1G 2>/dev/null | tail -1 | cut -f1 | grep . || echo 0
}
worker() {
  w=$1; i=0
  for f in $(ls "$archives" | grep '\.tar\.bz2$'); do
    i=$((i+1))
    [ $((i % workers)) -eq "$w" ] || continue
    [ -e "$state/stop" ] && { log "$w" stop; return 0; }
    [ -e "$state/done/$f" ] && continue
    while paused; do
      [ -e "$state/stop" ] && { log "$w" stop; return 0; }
      sleep 60
    done
    # Stop making units while too many wait for submission (units without a submitted marker).
    while [ "$(backlog_gb)" -ge "${QE_MAX_BACKLOG_GB:-300}" ]; do
      [ -e "$state/stop" ] && { log "$w" stop; return 0; }
      sleep 300
    done
    tmp="$state/work/$f"; rm -rf "$tmp"; mkdir -p "$tmp"
    # The pages the corpus takes from this archive, with their detected language.
    if [ ! -s "$state/langs/$f.tsv" ]; then
      psql "$(cat /etc/usnm/quality-reader/dsn)" -X -At -F "$(printf '\t')" -c \
        "SELECT p.doc_id, p.lang FROM quality.loc_alto a JOIN quality.pages p USING (doc_id, batch, batch_version) WHERE a.archive = '$f' ORDER BY 1" \
        > "$state/langs/$f.tsv.part" 2>> "$state/run.log" && mv "$state/langs/$f.tsv.part" "$state/langs/$f.tsv"
    fi
    if [ -s "$state/langs/$f.tsv" ] \
       && supervise "$w" nice -n 19 ionice -c2 -n7 "$bin" quality qe-text-extract --archive "$archives/$f" --out "$tmp/extract.jsonl" 2>> "$state/run.log" \
       && [ ! -e "$state/stop" ] \
       && supervise "$w" nice -n 19 ionice -c2 -n7 "$bin" quality qe-text-stage --extract "$tmp/extract.jsonl" --archive "$f" \
            --languages "$state/langs/$f.tsv" --title-languages "$state/titles.json" \
            --qe-dir "$qe_dir" --root "$state" 2>> "$state/run.log"; then
      : > "$state/done/$f"; log "$w" "done $f"
    else
      log "$w" "failed $f"
    fi
    rm -rf "$tmp"
  done
  log "$w" "all archives done"
}
w=0
while [ "$w" -lt "$workers" ]; do worker "$w" & w=$((w+1)); done
wait
