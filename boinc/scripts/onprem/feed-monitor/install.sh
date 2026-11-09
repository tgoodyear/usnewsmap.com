#!/usr/bin/env bash
# Sets up a feed monitor host (#1441) to follow the bundle CI attests from
# main: every host that runs a usnm-boinc-feed-source@ instance or the
# usnm-boinc-feed-health evaluator. Run as root on the host, from a checkout
# of reviewed main. Safe to run again.
#
#   boinc/scripts/onprem/feed-monitor/install.sh [--dry-run]
#
# --dry-run: say what would change, run the updater's own dry run when gh is
#   in place already, and change nothing.
#
# It installs:
#   /usr/local/sbin/usnm-feed-monitor-update (update.sh)
#   /etc/systemd/system/usnm-feed-monitor-update.service and .timer
#   /usr/local/libexec/usnm-feed-monitor/gh: the GitHub CLI release pinned
#     below, checked against its sha256, for `gh attestation verify` only
#     (Debian's gh is too old for it). Nothing else uses it; it never signs in.
#   jq, from the distribution, if it's missing.
# Then it runs the updater once, which installs the newest attested bundle
# and makes /opt/usnm-feed-monitor a symlink to it (hand-copied files there
# move to /opt/usnm-feed-monitor.pre-ci-<time>), and enables the timer.
# It doesn't touch the feed source or evaluator units, their environment
# files or configs, and restarts nothing.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '7s/^# \{0,1\}//p' "$0" >&2; exit 2; }

DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
HERE=$(cd "$(dirname "$0")" && pwd)
# USNM_FEED_MONITOR_TEST_ROOT: the tests' stand-in for /. Never set on a host.
R=${USNM_FEED_MONITOR_TEST_ROOT:-}
[ "$DRY_RUN" = true ] || [ -n "$R" ] || [ "$(id -u)" -eq 0 ] || die "run as root"
GH_VERSION=2.103.3
GH_SHA256=4c806978ffce1cec70853302a56d70ac6da4f19bae7adca4e1a8ab00633dd91d
GH_TARBALL="gh_${GH_VERSION}_linux_amd64.tar.gz"
GH=$R/usr/local/libexec/usnm-feed-monitor/gh
UNITS=$R/etc/systemd/system
[ "$(uname -m)" = x86_64 ] || die "the pinned gh is for x86_64; this host is $(uname -m)"
for tool in python3 curl flock tar gzip sha256sum systemctl find; do
  command -v "$tool" > /dev/null || die "no $tool here"
done

# Installs $1 as $2 with mode $3 when it differs.
put() {
  if [ -f "$2" ] && cmp -s "$1" "$2" && [ "$(stat -c %a "$2")" = "${3#0}" ]; then
    return 0
  fi
  if $DRY_RUN; then
    echo "would install $2 (mode $3)"
  else
    mkdir -p "$(dirname "$2")"
    install -m "$3" "$1" "$2.new"
    mv "$2.new" "$2"
    echo "installed $2"
  fi
}
run() { if $DRY_RUN; then echo "would run: $*"; else "$@"; fi; }

if ! command -v jq > /dev/null; then
  run apt-get update -qq
  run apt-get install -y -qq --no-install-recommends jq
fi

if [ -x "$GH" ] && "$GH" --version 2> /dev/null | grep -q "^gh version $GH_VERSION "; then
  :
elif $DRY_RUN; then
  echo "would install gh $GH_VERSION as $GH (sha256 $GH_SHA256)"
else
  tmp=$(mktemp -d)
  trap 'rm -rf "$tmp"' EXIT
  curl -fsSL --retry 3 --max-time 300 -o "$tmp/$GH_TARBALL" \
    "https://github.com/cli/cli/releases/download/v$GH_VERSION/$GH_TARBALL"
  echo "$GH_SHA256  $tmp/$GH_TARBALL" | sha256sum --check --quiet - || die "$GH_TARBALL doesn't match its pinned sha256"
  tar -xzf "$tmp/$GH_TARBALL" -C "$tmp" "gh_${GH_VERSION}_linux_amd64/bin/gh"
  mkdir -p "$(dirname "$GH")"
  install -m 0755 "$tmp/gh_${GH_VERSION}_linux_amd64/bin/gh" "$GH.new"
  mv "$GH.new" "$GH"
  echo "installed gh $GH_VERSION as $GH"
fi

put "$HERE/update.sh" "$R/usr/local/sbin/usnm-feed-monitor-update" 0755
put "$HERE/usnm-feed-monitor-update.service" "$UNITS/usnm-feed-monitor-update.service" 0644
put "$HERE/usnm-feed-monitor-update.timer" "$UNITS/usnm-feed-monitor-update.timer" 0644

if $DRY_RUN; then
  if [ -x "$GH" ]; then
    echo "the updater's dry run:"
    bash "$HERE/update.sh" --dry-run || true
  fi
  echo "would run: systemctl daemon-reload"
  echo "would run: systemctl start usnm-feed-monitor-update.service"
  echo "would run: systemctl enable --now usnm-feed-monitor-update.timer"
  exit 0
fi
systemctl daemon-reload
# The first run installs the bundle; journalctl -u usnm-feed-monitor-update
# says what it did. A failure leaves the timer to try again.
systemctl start usnm-feed-monitor-update.service ||
  echo "warning: the first update failed; see journalctl -u usnm-feed-monitor-update" >&2
systemctl enable --now usnm-feed-monitor-update.timer
live=$(readlink "$R/opt/usnm-feed-monitor" 2> /dev/null || true)
echo "feed monitor: ${live:-not installed yet} (usnm-feed-monitor-update.timer enabled)"
