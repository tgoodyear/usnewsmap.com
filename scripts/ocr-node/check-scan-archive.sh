#!/usr/bin/env bash
# Check the scan archive on the host holding the keep set (#442, #895).
#   sudo scripts/ocr-node/check-scan-archive.sh [--limit 100] [--verify-only]
#     [--sample-from FILE] [--deadline 1800] [--progress 30] [--max-load LOAD]
# Runs the bounded dry-run/upload/verify sequence as the service identity on
# one fixed sample, holding the upload/verify units' lock throughout. It never
# walks the whole keep set, starts a full upload or enables timers.
# --verify-only writes no blobs.
set -euo pipefail
shopt -s nullglob
export LC_ALL=C
die() { echo "error: $*" >&2; exit 1; }
LIMIT=100 VERIFY_ONLY=false MANIFEST="" DEADLINE=1800 PROGRESS=30 MAX_LOAD=""
while [ $# -gt 0 ]; do
  case "$1" in
    --limit) LIMIT=${2:-}; shift 2 ;;
    --verify-only) VERIFY_ONLY=true; shift ;;
    --sample-from) MANIFEST=${2:-}; shift 2 ;;
    --deadline) DEADLINE=${2:-}; shift 2 ;;
    --progress) PROGRESS=${2:-}; shift 2 ;;
    --max-load) MAX_LOAD=${2:-}; shift 2 ;;
    -h|--help) sed -n '2,8s/^# \{0,1\}//p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $LIMIT =~ ^[1-9][0-9]{0,3}$ ]] && [ "$LIMIT" -le 1000 ] ||
  die "--limit must be 1 to 1000 scans"
[[ $DEADLINE =~ ^[1-9][0-9]{1,4}$ ]] && [ "$DEADLINE" -ge 60 ] && [ "$DEADLINE" -le 86400 ] ||
  die "--deadline must be 60 to 86400 seconds per phase"
[[ $PROGRESS =~ ^[1-9][0-9]{0,3}$ ]] || die "--progress must be 1 to 9999 seconds"
[ -z "$MAX_LOAD" ] || [[ $MAX_LOAD =~ ^[0-9]+([.][0-9]+)?$ && ! $MAX_LOAD =~ ^0+([.]0+)?$ ]] ||
  die "--max-load must be a positive load average"
[ -z "$MANIFEST" ] || { [ -f "$MANIFEST" ] && [ -r "$MANIFEST" ]; } ||
  die "--sample-from must name a readable file"
[ "$(id -u)" = 0 ] || die "run as root on the host holding the scans"
for cmd in jq flock systemd-run systemd-tmpfiles systemctl ionice renice stat sha256sum; do
  command -v "$cmd" > /dev/null || die "needs $cmd"
done
# Sampling and checking run at idle I/O and lowest CPU priority, like the
# transient units below, so they yield to the pool's other readers.
ionice -c 3 -p $$ > /dev/null
renice -n 19 -p $$ > /dev/null
ROOT=${USNM_OCR_TEST_ROOT:-}
ENVFILE=$ROOT/etc/usnm/scan-archive.env
[ -x "$ROOT/usr/local/bin/usnm-ocr-worker" ] || die "install the released worker binary first"
[ -r "$ENVFILE" ] || die "missing /etc/usnm/scan-archive.env"
getent passwd usnm > /dev/null || die "missing service user usnm"
getent group himds > /dev/null || die "missing Arc token group himds"
[ -r "$ROOT/etc/tmpfiles.d/usnm-scan-archive.conf" ] || die "install the archive tmpfiles configuration first"
SCANS=$(sed -n 's/^USNM_ARCHIVE_SCANS=//p' "$ENVFILE" | tail -n 1)
# Like systemd, take the value inside one pair of matching quotes.
if [[ $SCANS =~ ^\"(.*)\"$ || $SCANS =~ ^\'(.*)\'$ ]]; then SCANS=${BASH_REMATCH[1]}; fi
SCANS=$ROOT${SCANS:-/tank/ocr/scans}
[ -d "$SCANS/keep" ] || die "no keep set under $SCANS"
systemd-tmpfiles --create "$ROOT/etc/tmpfiles.d/usnm-scan-archive.conf"
exec 9<>"$ROOT/run/lock/usnm-scan-archive/lock"
flock --exclusive --nonblock 9 || die "an upload, verify or acceptance check holds the archive lock"

# The worker lists every scan under its --scans root before applying --limit,
# so it never sees the real root: each phase gets a read-only tmpfs holding
# bind mounts of the sampled scans and sidecars only. /run is root-only, so
# this directory cannot be swapped by another user.
WORK=$ROOT/run/usnm-scan-archive-check.$$
SAMPLE=$WORK/scans
mkdir -m 0755 "$WORK" || die "cannot create $WORK"
mkdir -m 0755 "$SAMPLE"
UNIT="" STOPPED=""
cleanup() {
  if [ -n "$UNIT" ] && systemctl is-active --quiet "$UNIT" 2> /dev/null; then
    systemctl stop "$UNIT" || true
  fi
  rm -f "$WORK/report.json"
  rmdir "$SAMPLE" "$WORK" 2> /dev/null || true
}
trap cleanup EXIT

NAMES=()
if [ -n "$MANIFEST" ]; then
  # A list of blob names, the first tab-separated field of each line (the
  # upload's archived.tsv as it is): read sequentially, sampled evenly from
  # first line to last.
  TOTAL=$(awk 'END { print NR }' "$MANIFEST")
  [ "$TOTAL" -gt 0 ] || die "$MANIFEST lists no scans"
  mapfile -t NAMES < <(awk -F'\t' -v n="$LIMIT" -v total="$TOTAL" '
    BEGIN { if (n > total) n = total; for (i = 0; i < n; i++) pick[int(i * total / n) + 1] = 1 }
    NR in pick { print $1 }' "$MANIFEST" | sort -u)
  FROM="spread over the $TOTAL lines of $MANIFEST"
else
  # The first scans in name order, depth first: the walk returns as soon as
  # the sample is full, so only the directories on the way are read.
  DIRS=0
  walk() {
    local entry
    DIRS=$((DIRS + 1))
    for entry in "$1"/*; do
      [ "${#NAMES[@]}" -lt "$LIMIT" ] || return 0
      [ ! -L "$entry" ] || continue
      case ${entry##*/} in
        *.json | *.pct*) ;;
        *.jp2 | *.jpg) if [ -f "$entry" ]; then NAMES+=("keep/${entry#"$SCANS/keep/"}"); fi ;;
        *) if [ -d "$entry" ]; then walk "$entry"; fi ;;
      esac
    done
  }
  walk "$SCANS/keep"
  FROM="the first in name order under $SCANS/keep ($DIRS directories read)"
fi
COUNT=${#NAMES[@]}
[ "$COUNT" -gt 0 ] || die "no scans to sample"
BINDS=()
for name in "${NAMES[@]}"; do
  # Plain segments only: no dot segments, spaces, colons or specifiers that
  # could change the bind mount syntax or leave the keep set.
  [[ $name =~ ^keep(/[A-Za-z0-9_][A-Za-z0-9._-]*)+[.](jp2|jpg)$ && ${name##*/} != *.pct* ]] ||
    die "not a keep-set scan name: $name"
  for f in "$name" "$name.json"; do
    [ -f "$SCANS/$f" ] && [ ! -L "$SCANS/$f" ] || die "sampled file is missing here: $f"
  done
  BINDS+=(-p "BindReadOnlyPaths=$SCANS/$name:$SAMPLE/$name $SCANS/$name.json:$SAMPLE/$name.json")
done
BYTES=$(cd "$SCANS" && stat -c %s -- "${NAMES[@]}" | awk '{ s += $1 } END { printf "%.0f", s }')
DIGEST=$(printf '%s\n' "${NAMES[@]}" | sha256sum | cut -c1-16)
mb() { awk -v b="$1" 'BEGIN { printf "%.1f", b / 1e6 }'; }
echo "== sample: $COUNT scans ($(mb "$BYTES") MB), $FROM; names sha256 $DIGEST"

# Progress while a phase runs: elapsed time, what the worker has read so far
# and the host load. Past --max-load it stops its own unit, nothing else.
watch_unit() {
  local phase=$1 run=$2 start=$3 next pid got load
  next=$((SECONDS + PROGRESS))
  while kill -0 "$run" 2> /dev/null; do
    sleep 0.2
    [ "$SECONDS" -ge "$next" ] || continue
    next=$((SECONDS + PROGRESS))
    load=$(cut -d' ' -f1 "$ROOT/proc/loadavg" 2> /dev/null || echo '?')
    pid=$(systemctl show --property=MainPID --value "$UNIT" 2> /dev/null || echo 0)
    got=$(awk '/^rchar:/ { print $2 }' "$ROOT/proc/${pid:-0}/io" 2> /dev/null || true)
    echo "== $phase: $((SECONDS - start))s of ${DEADLINE}s; worker read $(mb "${got:-0}") of $(mb "$BYTES") MB; load $load"
    if [ -n "$MAX_LOAD" ] && awk -v l="$load" -v m="$MAX_LOAD" 'BEGIN { exit !(l + 0 > m + 0) }'; then
      STOPPED="host load $load is above --max-load $MAX_LOAD"
      echo "== stopping $UNIT: $STOPPED" >&2
      systemctl stop "$UNIT" || true
      return
    fi
  done
}

check() {
  local phase=$1 run start status
  shift
  UNIT=usnm-scan-archive-check-$$-$phase
  echo "== scan archive $phase ($COUNT scans, idle I/O, deadline ${DEADLINE}s)"
  start=$SECONDS
  systemd-run --unit="$UNIT" --pipe --wait --collect --quiet \
    --uid=usnm --gid=usnm \
    -p SupplementaryGroups=himds \
    -p "EnvironmentFile=$ENVFILE" \
    -p UnsetEnvironment=USNM_ARCHIVE_STATE \
    -p Environment=RUST_LOG=off \
    -p Environment=IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token \
    -p Environment=IMDS_ENDPOINT=http://localhost:40342 \
    -p IOSchedulingClass=idle -p Nice=19 \
    -p "RuntimeMaxSec=$DEADLINE" -p TimeoutStopSec=30 \
    -p ProtectSystem=strict -p ProtectHome=yes -p NoNewPrivileges=yes \
    -p "TemporaryFileSystem=$SAMPLE:ro" "${BINDS[@]}" \
    "$ROOT/usr/local/bin/usnm-ocr-worker" archive-scans --scans "$SAMPLE" \
    --limit "$COUNT" --concurrency 1 "$@" > "$WORK/report.json" &
  run=$!
  watch_unit "$phase" "$run" "$start"
  if wait "$run"; then status=0; else status=$?; fi
  UNIT=""
  [ -z "$STOPPED" ] || die "$phase stopped: $STOPPED; not accepted"
  if [ "$status" -ne 0 ]; then
    cat "$WORK/report.json" >&2
    [ $((SECONDS - start)) -lt "$DEADLINE" ] ||
      die "$phase reached its ${DEADLINE}s deadline and was stopped; not accepted"
    die "$phase failed; timers and full upload were not started"
  fi
  cat "$WORK/report.json"
  jq -e -s --argjson count "$COUNT" '
    length == 1 and (.[0] |
      .scanned == $count and
      .bad_local == 0 and .conflicts == 0 and .missing == 0 and
      .mismatched == 0 and .errors == 0 and .skipped == 0)
  ' "$WORK/report.json" > /dev/null ||
    die "$phase returned an invalid, incomplete or unclean report"
  if [ "$phase" = dry-run ]; then
    jq -e '.present + .would_upload == .scanned and .uploaded == 0' \
      "$WORK/report.json" > /dev/null || die "dry-run did not account for every sampled scan"
  elif [ "$phase" = upload ]; then
    jq -e '.present + .uploaded == .scanned and .would_upload == 0' \
      "$WORK/report.json" > /dev/null || die "upload did not account for every sampled scan"
  fi
  if [ "$phase" = verify ]; then
    jq -e '.present == .scanned and .uploaded == 0 and .would_upload == 0' \
      "$WORK/report.json" > /dev/null || die "verify did not confirm every sampled scan"
  fi
  echo "== $phase done in $((SECONDS - start))s"
}

if [ "$VERIFY_ONLY" = false ]; then
  check dry-run --dry-run
  check upload
fi
check verify --verify
echo "scan archive acceptance passed: $COUNT scans verified (sample $DIGEST); no timers or full upload started"
