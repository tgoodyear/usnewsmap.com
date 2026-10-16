#!/usr/bin/env bash
# Check the scan archive on the host holding the keep set (#442).
#   sudo scripts/ocr-node/check-scan-archive.sh [--limit 100] [--verify-only]
# Runs the bounded dry-run/upload/verify sequence as the service identity,
# holding the upload/verify units' lock throughout. It never starts a full
# upload or enables timers. --verify-only writes no blobs.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
LIMIT=100 VERIFY_ONLY=false
while [ $# -gt 0 ]; do
  case "$1" in
    --limit) LIMIT=${2:-}; shift 2 ;;
    --verify-only) VERIFY_ONLY=true; shift ;;
    -h|--help) sed -n '2,6s/^# \{0,1\}//p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $LIMIT =~ ^[1-9][0-9]{0,3}$ ]] && [ "$LIMIT" -le 1000 ] ||
  die "--limit must be 1 to 1000 scans"
[ "$(id -u)" = 0 ] || die "run as root on the host holding the scans"
for cmd in jq flock systemd-run systemd-tmpfiles; do
  command -v "$cmd" > /dev/null || die "needs $cmd"
done
ROOT=${USNM_OCR_TEST_ROOT:-}
[ -x "$ROOT/usr/local/bin/usnm-ocr-worker" ] || die "install the released worker binary first"
[ -r "$ROOT/etc/usnm/scan-archive.env" ] || die "missing /etc/usnm/scan-archive.env"
getent passwd usnm > /dev/null || die "missing service user usnm"
getent group himds > /dev/null || die "missing Arc token group himds"
[ -r "$ROOT/etc/tmpfiles.d/usnm-scan-archive.conf" ] || die "install the archive tmpfiles configuration first"
systemd-tmpfiles --create "$ROOT/etc/tmpfiles.d/usnm-scan-archive.conf"
exec 9<>"$ROOT/run/lock/usnm-scan-archive/lock"
flock --exclusive --nonblock 9 || die "an upload, verify or acceptance check holds the archive lock"

TMP=$(mktemp -d)
trap 'rm -f "$TMP/report.json"; rmdir "$TMP"' EXIT
SCANNED=""
check() {
  local phase=$1
  shift
  echo "== scan archive $phase (at most $LIMIT scans)"
  systemd-run --pipe --wait --collect --quiet \
    --uid=usnm --gid=usnm \
    -p SupplementaryGroups=himds \
    -p "EnvironmentFile=$ROOT/etc/usnm/scan-archive.env" \
    -p UnsetEnvironment=USNM_ARCHIVE_STATE \
    -p Environment=RUST_LOG=off \
    -p Environment=IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token \
    -p Environment=IMDS_ENDPOINT=http://localhost:40342 \
    "$ROOT/usr/local/bin/usnm-ocr-worker" archive-scans --limit "$LIMIT" "$@" > "$TMP/report.json" ||
    { cat "$TMP/report.json" >&2; die "$phase failed; timers and full upload were not started"; }
  cat "$TMP/report.json"
  jq -e -s --argjson limit "$LIMIT" '
    length == 1 and (.[0] |
      (.scanned | type == "number" and . > 0 and . <= $limit) and
      .bad_local == 0 and .conflicts == 0 and .missing == 0 and
      .mismatched == 0 and .errors == 0 and .skipped == 0)
  ' "$TMP/report.json" > /dev/null ||
    die "$phase returned an invalid, empty or unclean report"
  local count
  count=$(jq -r .scanned "$TMP/report.json")
  [ -z "$SCANNED" ] || [ "$count" = "$SCANNED" ] ||
    die "sample size changed between phases ($SCANNED to $count)"
  SCANNED=$count
  if [ "$phase" = dry-run ]; then
    jq -e '.present + .would_upload == .scanned and .uploaded == 0' \
      "$TMP/report.json" > /dev/null || die "dry-run did not account for every sampled scan"
  elif [ "$phase" = upload ]; then
    jq -e '.present + .uploaded == .scanned and .would_upload == 0' \
      "$TMP/report.json" > /dev/null || die "upload did not account for every sampled scan"
  fi
  if [ "$phase" = verify ]; then
    jq -e '.present == .scanned and .uploaded == 0 and .would_upload == 0' \
      "$TMP/report.json" > /dev/null || die "verify did not confirm every sampled scan"
  fi
}

if [ "$VERIFY_ONLY" = false ]; then
  check dry-run --dry-run
  check upload
fi
check verify --verify
echo "scan archive acceptance passed: $SCANNED scans verified; no timers or full upload started"
