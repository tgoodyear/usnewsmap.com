#!/usr/bin/env bash
# Keeps a host's feed monitor scripts (#1441) on the bundle CI attested from
# main, the way usnm-serving-update keeps the on-prem API on its image.
# install.sh installs it as /usr/local/sbin/usnm-feed-monitor-update, and
# usnm-feed-monitor-update.timer runs it 2 minutes after the last run ended.
#
#   usnm-feed-monitor-update [--version TAG | --rollback] [--keep 3]
#     [--idle-secs 120] [--dry-run]
#
# feed-monitor-release.yml builds bundle.sh's archive on every main commit
# that changes the scripts, attests it, and publishes it twice: as the
# release feed-monitor-<yyyymmdd>-<sha7>, which never changes, and as the
# assets of the release feed-monitor-latest, which it replaces each time.
# Each run reads feed-monitor-latest's SHA256SUMS from github.com's release
# download, which costs no API request; when that names the archive that's
# installed, the run ends there.
#
# A new archive is installed once:
#  - it matches SHA256SUMS (a mismatch is a publication in progress, tried
#    again next run);
#  - it has a build provenance attestation signed by this repository's
#    feed-monitor-release.yml on main. The attestation is read from the
#    repository's public API without a token, and `gh attestation verify
#    --bundle` checks it (the gh install.sh puts in
#    /usr/local/libexec/usnm-feed-monitor, never signed in). CI attests
#    before it publishes, so a missing attestation is asked for every run
#    for 10 minutes, then every 30, and once the API still answers that
#    there's none 3 hours on, the archive is rejected. An API error, or a gh
#    that can't load its trusted signing data, rejects nothing: the run
#    fails and the next one tries again;
#  - it holds exactly bundle.sh's files, as regular files, and its
#    VERSION.json names the attested commit and each file's sha256;
#  - its commit is the installed one's or a descendant of it, as the
#    public compare API says (no downgrade without --version);
#  - feed_collect.py and feed_status.py start (--help) from it.
# An archive that fails any of these is rejected: its digest goes into
# /var/lib/usnm-feed-monitor-update/rejected, and it is never tried again.
#
# Install: the archive is unpacked into a new directory beside the releases
# in /opt/usnm-feed-monitor-releases/, renamed to its version, and
# /opt/usnm-feed-monitor (the path the units run) becomes a symlink to it in
# one rename. Hand-copied files there before the first install are moved
# aside to /opt/usnm-feed-monitor.pre-ci-<time>, untouched. For the switch
# the active usnm-boinc-feed-* timers are stopped, so no run starts, and it
# waits up to --idle-secs for the running ones to end, so no run imports one
# version's module into another's; then the timers start again. If runs
# don't end in time, the next run switches (a --version or --rollback run
# fails instead). Nothing is restarted: the units are timer-started oneshots,
# and the next run uses the new files. The --keep newest releases stay, and
# always the live and the previous one. Each deploy logs, at Warning, the
# version before and after.
#
# --version TAG installs that release (feed-monitor-<yyyymmdd>-<sha7>),
# older ones included, and creates /etc/usnm-feed-monitor-update/hold, also
# when it's installed already. --rollback switches back to the previous
# release (/var/lib/usnm-feed-monitor-update/previous), checked against its
# VERSION.json, and holds too. While the hold file exists, the timer's runs
# do nothing; delete it to follow main again. --dry-run downloads and
# verifies what it would install, says so, and changes nothing.
#
# Runs take /run/lock/usnm-feed-monitor-update.lock and never overlap. Lines
# go to the journal under usnm-feed-monitor-update.
set -euo pipefail
usage() { sed -n '7,8s/^# \{0,1\}//p' "$0" >&2; exit 2; }
# Under systemd, a <N> prefix sets a line's syslog priority.
pri() { if [ -n "${JOURNAL_STREAM:-}" ]; then printf '<%s>%s\n' "$1" "$2"; else printf '%s\n' "$2"; fi; }
log() { pri 5 "$*"; }
warn() { pri 4 "$*"; }

VERSION="" ROLLBACK=false KEEP=3 IDLE_SECS=120 DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --version) VERSION=${2:-}; shift 2 || usage ;;
    --rollback) ROLLBACK=true; shift ;;
    --keep) KEEP=${2:-}; shift 2 || usage ;;
    --idle-secs) IDLE_SECS=${2:-}; shift 2 || usage ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
TAG_RE='^feed-monitor-[0-9]{8}-[0-9a-f]{7}$'
[ -z "$VERSION" ] || [[ $VERSION =~ $TAG_RE ]] || { echo "error: --version: a release tag, such as feed-monitor-20261108-0123abc" >&2; usage; }
[ -z "$VERSION" ] || ! $ROLLBACK || { echo "error: --version and --rollback don't go together" >&2; usage; }
[[ $KEEP =~ ^[2-9]$ ]] || { echo "error: --keep: 2 to 9" >&2; usage; }
[[ $IDLE_SECS =~ ^[0-9]{1,4}$ ]] || { echo "error: --idle-secs: 0 to 9999" >&2; usage; }

REPO=tgoodyear/usnewsmap.com
WORKFLOW="$REPO/.github/workflows/feed-monitor-release.yml"
API="https://api.github.com/repos/$REPO"
DOWNLOAD="https://github.com/$REPO/releases/download"
LATEST=feed-monitor-latest
ARCHIVE=usnm-feed-monitor.tar.gz
# bundle.sh's files; keep the two lists the same (test_feed_monitor.py).
FILES="VERSION.json acceptance_client.py client_account.py feed_collect.py feed_status.py"
WAIT_SECS=$((3 * 3600)) EAGER_SECS=600 BACKOFF_SECS=1800
# USNM_FEED_MONITOR_TEST_ROOT: the tests' stand-in for /. Never set on a host.
R=${USNM_FEED_MONITOR_TEST_ROOT:-}
[ "$DRY_RUN" = true ] || [ -n "$R" ] || [ "$(id -u)" -eq 0 ] || { echo "error: run as root" >&2; exit 1; }
LIVE=$R/opt/usnm-feed-monitor
RELEASES=$R/opt/usnm-feed-monitor-releases
STATE=$R/var/lib/usnm-feed-monitor-update
HOLD=$R/etc/usnm-feed-monitor-update/hold
LOCK=$R/run/lock/usnm-feed-monitor-update.lock
GH=$R/usr/local/libexec/usnm-feed-monitor/gh
REJECTED=$STATE/rejected
PENDING=$STATE/pending

# The release the live symlink names, or nothing (hand-copied files, or none).
live_tag() {
  local t
  [ -L "$LIVE" ] || return 0
  t=$(readlink "$LIVE")
  [ "$(dirname "$t")" = "$RELEASES" ] && [[ $(basename "$t") =~ $TAG_RE ]] && basename "$t"
  return 0
}
rejected() { [ -s "$REJECTED" ] && grep -qxF "$1" "$REJECTED"; }
# True when directory $1 holds exactly the bundle's files, as VERSION.json
# lists them.
intact() {
  local f want
  [ -f "$1/VERSION.json" ] || return 1
  [ "$(find "$1" -mindepth 1 -maxdepth 1 ! -name .bundle-sha256 -printf '%f\n' | LC_ALL=C sort | paste -sd' ')" = "$FILES" ] || return 1
  [ "$(jq -r '.files | keys | join(" ")' "$1/VERSION.json" 2> /dev/null)" = "${FILES#VERSION.json }" ] || return 1
  for f in ${FILES#VERSION.json }; do
    want=$(jq -r --arg f "$f" '.files[$f]' "$1/VERSION.json")
    [ -f "$1/$f" ] && [ ! -L "$1/$f" ] && [ "$(sha256sum "$1/$f" | cut -d' ' -f1)" = "$want" ] || return 1
  done
}
# Waits up to --idle-secs for no usnm-boinc-feed-* service to be running.
# False when they don't end in time or systemctl can't say.
idle() {
  local deadline=$((SECONDS + IDLE_SECS)) running
  while :; do
    running=$(systemctl list-units --plain --no-legend --state=activating,deactivating 'usnm-boinc-feed-*.service') ||
      { warn "warning: can't list the feed monitor runs"; return 1; }
    [ -n "$running" ] || return 0
    [ "$SECONDS" -lt "$deadline" ] || return 1
    sleep 2
  done
}
# The feed timers this run stopped, started again by resume (and on exit).
paused=()
resume() {
  [ ${#paused[@]} -eq 0 ] || systemctl start "${paused[@]}" || warn "warning: can't start ${paused[*]} again"
  paused=()
}
# Stops the active feed timers, so no run starts, then waits for the
# running ones to end (idle). False, with the timers running again, when
# any of that fails. Each step is checked here: set -e doesn't apply inside
# a function called as a condition.
drain() {
  local timers
  timers=$(systemctl list-units --plain --no-legend --state=active 'usnm-boinc-feed-*.timer') ||
    { warn "warning: can't list the feed monitor timers"; return 1; }
  mapfile -t paused < <(cut -d' ' -f1 <<< "$timers" | sed '/^$/d')
  if [ ${#paused[@]} -gt 0 ] && ! systemctl stop "${paused[@]}"; then
    warn "warning: can't stop ${paused[*]}"
    resume
    return 1
  fi
  idle || { resume; return 1; }
}
# GET $API/$1 into file $2; prints the HTTP status, 000 when nothing answered.
api_get() {
  curl -sSL --retry 2 --max-time 60 -H 'Accept: application/vnd.github+json' \
    -o "$2" -w '%{http_code}' "$API/$1" 2> /dev/null || true
}
# The pinned gh, never signed in.
ghx() {
  env -u GH_TOKEN -u GITHUB_TOKEN GH_CONFIG_DIR="$TMP/gh" GH_NO_UPDATE_NOTIFIER=1 GH_PROMPT_DISABLED=1 "$GH" "$@"
}
# Points the live path at release $1 in one rename; hand-copied files there
# are moved aside first.
switch_to() {
  ln -sfn "$RELEASES/$1" "$LIVE.new"
  if [ -d "$LIVE" ] && [ ! -L "$LIVE" ]; then
    mv "$LIVE" "$LIVE.pre-ci-$(date -u +%Y%m%dT%H%M%SZ)"
  fi
  mv -T "$LIVE.new" "$LIVE"
}

TMP=$(mktemp -d "${TMPDIR:-/var/tmp}/usnm-feed-monitor-update.XXXXXX")
trap 'resume; rm -rf "$TMP"' EXIT
if [ "$DRY_RUN" = false ]; then
  mkdir -p "$(dirname "$LOCK")" "$STATE" "$RELEASES"
  exec 9> "$LOCK"
  if ! flock -n 9; then
    log "another run holds $LOCK"
    exit 0
  fi
fi
# Read under the lock, so a pin or rollback that just ended is seen.
current=$(live_tag)
if [ -z "$VERSION" ] && ! $ROLLBACK && [ -e "$HOLD" ]; then
  log "held: $HOLD exists, so the feed monitor stays on ${current:-its hand-copied files}"
  exit 0
fi

if $ROLLBACK; then
  prev=$(cat "$STATE/previous" 2> /dev/null || true)
  [[ $prev =~ $TAG_RE ]] || { warn "warning: no previous release recorded in $STATE/previous"; exit 1; }
  intact "$RELEASES/$prev" || { warn "warning: $RELEASES/$prev doesn't match its VERSION.json; not rolling back to it"; exit 1; }
  if $DRY_RUN; then
    log "dry run: would roll back ${current:-the hand-copied files} -> $prev and hold"
    exit 0
  fi
  drain || { warn "warning: feed monitor runs didn't end within $IDLE_SECS s; not rolled back"; exit 1; }
  switch_to "$prev"
  resume
  [ -z "$current" ] || [ "$current" = "$prev" ] || echo "$current" > "$STATE/previous"
  mkdir -p "$(dirname "$HOLD")"
  touch "$HOLD"
  warn "usnm-feed-monitor rolled back: ${current:-hand-copied files} -> $prev; held until $HOLD is deleted"
  exit 0
fi

reject() {
  if $DRY_RUN; then
    warn "dry run: $*; would reject $ARCHIVE@sha256:$digest"
    exit 1
  fi
  echo "$digest" >> "$REJECTED"
  rm -f "$PENDING"
  warn "$*; $ARCHIVE@sha256:$digest is rejected"
  exit 1
}
fetch() { curl -fsSL --retry 2 --max-time 120 -o "$2" "$DOWNLOAD/$tag/$1" 2> "$TMP/curl.err"; }

tag=${VERSION:-$LATEST}
if ! fetch SHA256SUMS "$TMP/SHA256SUMS"; then
  warn "warning: can't read $tag's SHA256SUMS: $(tail -1 "$TMP/curl.err")"
  exit 1
fi
digest=$(sed -n 's/^\([0-9a-f]\{64\}\) [ *]usnm-feed-monitor\.tar\.gz$/\1/p' "$TMP/SHA256SUMS")
[[ $digest =~ ^[0-9a-f]{64}$ ]] || { warn "warning: $tag's SHA256SUMS names no $ARCHIVE"; exit 1; }
current_digest=""
[ -z "$current" ] || current_digest=$(cat "$RELEASES/$current/.bundle-sha256" 2> /dev/null || true)
if [ "$digest" = "$current_digest" ]; then
  if [ -n "$VERSION" ]; then
    $DRY_RUN || { mkdir -p "$(dirname "$HOLD")"; touch "$HOLD"; }
    log "$VERSION is installed already; held at it until $HOLD is deleted"
  fi
  exit 0
fi
if [ -z "$VERSION" ] && rejected "$digest"; then exit 0; fi
if ! fetch "$ARCHIVE" "$TMP/$ARCHIVE"; then
  warn "warning: can't download $tag's $ARCHIVE: $(tail -1 "$TMP/curl.err")"
  exit 1
fi
if [ "$(sha256sum "$TMP/$ARCHIVE" | cut -d' ' -f1)" != "$digest" ]; then
  # feed-monitor-latest's two files are replaced one after the other.
  [ -n "$VERSION" ] || { log "$tag's $ARCHIVE doesn't match its SHA256SUMS yet (being published?); trying next run"; exit 0; }
  warn "warning: $VERSION's $ARCHIVE doesn't match its SHA256SUMS"
  exit 1
fi

# The attestation, for feed-monitor-latest not asked for more often than the
# schedule above allows: GitHub allows 60 requests an hour from an address
# without a token, shared with every host behind it.
now=$(date +%s)
if [ -z "$VERSION" ]; then
  p_digest="" p_first=0 p_last=0
  [ ! -s "$PENDING" ] || read -r p_digest p_first p_last < "$PENDING" || true
  [[ $p_first =~ ^[0-9]+$ && $p_last =~ ^[0-9]+$ ]] || p_digest=""
  if [ "$p_digest" != "$digest" ]; then
    p_first=$now p_last=0
  elif [ $((now - p_first)) -gt "$EAGER_SECS" ] && [ $((now - p_last)) -lt "$BACKOFF_SECS" ]; then
    exit 0
  fi
  $DRY_RUN || echo "$digest $p_first $now" > "$PENDING"
fi
code=$(api_get "attestations/sha256:$digest" "$TMP/attestations.json")
case $code in
  200) jq -c '.attestations[]?.bundle' "$TMP/attestations.json" > "$TMP/bundles.jsonl" 2> /dev/null || : > "$TMP/bundles.jsonl" ;;
  404) : > "$TMP/bundles.jsonl" ;; # the API's answer for a digest it has no attestation of
  *) warn "warning: can't read the attestations of $ARCHIVE@sha256:$digest (HTTP $code); trying again next run"; exit 1 ;;
esac
if [ ! -s "$TMP/bundles.jsonl" ]; then
  if [ -n "$VERSION" ]; then
    warn "warning: $VERSION ($digest) has no attestation; not installing it"
    exit 1
  fi
  [ $((now - p_first)) -le "$WAIT_SECS" ] || reject "no attestation $((WAIT_SECS / 3600)) hours after $tag first showed it"
  log "waiting for the attestation of $ARCHIVE@sha256:$digest"
  exit 0
fi
mkdir -p "$TMP/gh"
verify() {
  ghx attestation verify "$TMP/$ARCHIVE" --bundle "$TMP/bundles.jsonl" --repo "$REPO" \
    --signer-workflow "$WORKFLOW" --source-ref refs/heads/main \
    --format json > "$TMP/verified.json" 2> "$TMP/verify.err"
}
if ! verify; then
  # gh fetches its trusted signing data even with --bundle; failing to is
  # no verdict on the archive.
  if ! ghx attestation trusted-root > /dev/null 2> "$TMP/root.err"; then
    warn "warning: gh can't load its trusted signing data ($(tail -1 "$TMP/root.err")); trying again next run"
    exit 1
  fi
  if ! verify; then
    warn "$(tail -5 "$TMP/verify.err")"
    reject "its attestation doesn't verify as built by $WORKFLOW on main"
  fi
fi
commit=$(jq -r '[.[].verificationResult.signature.certificate.sourceRepositoryDigest] | unique | .[]' "$TMP/verified.json" 2> /dev/null || true)
[[ $commit =~ ^[0-9a-f]{40}$ ]] || reject "its attestations name no single source commit"

# Its contents: exactly the bundle's files, regular, nothing else.
members=$(tar -tvzf "$TMP/$ARCHIVE" 2> /dev/null) || reject "it isn't a gzip tar archive"
! grep -qv '^-' <<< "$members" || reject "it holds something other than regular files"
[ "$(awk '{ print $NF }' <<< "$members" | LC_ALL=C sort | paste -sd' ')" = "$FILES" ] ||
  reject "it doesn't hold exactly $FILES"
stage=$TMP/stage
mkdir "$stage"
tar -xzf "$TMP/$ARCHIVE" -C "$stage" --no-same-owner --no-same-permissions --no-overwrite-dir
chmod 0644 "$stage"/*
intact "$stage" || reject "its files don't match its VERSION.json"
version=$(jq -r .version "$stage/VERSION.json")
[[ $version =~ $TAG_RE ]] || reject "its VERSION.json names no release (\"$version\")"
[ -z "$VERSION" ] || [ "$version" = "$VERSION" ] || reject "release $VERSION holds $version"
[ "$(jq -r .commit "$stage/VERSION.json")" = "$commit" ] || reject "its VERSION.json names another commit than its attestation ($commit)"
[ "${version: -7}" = "${commit:0:7}" ] || reject "$version isn't commit $commit"
at=$(jq -r .committed_at "$stage/VERSION.json")
[[ $at =~ ^[0-9]{9,11}$ ]] || reject "its VERSION.json has no commit time"
# Only forward along main: commit times don't order commits, ancestry does.
current_commit=""
[ -z "$current" ] || current_commit=$(jq -r .commit "$RELEASES/$current/VERSION.json" 2> /dev/null || true)
if [ -z "$VERSION" ] && [[ $current_commit =~ ^[0-9a-f]{40}$ ]] && [ "$current_commit" != "$commit" ]; then
  code=$(api_get "compare/$current_commit...$commit?per_page=1" "$TMP/compare.json")
  status=$(jq -r '.status // empty' "$TMP/compare.json" 2> /dev/null || true)
  if [ "$code" != 200 ] || [ -z "$status" ]; then
    warn "warning: can't compare $version with the installed $current (HTTP $code); trying again next run"
    exit 1
  fi
  case $status in
    ahead) ;;
    behind) reject "$version is older than the installed $current (pin it with --version to downgrade)" ;;
    *) reject "$version isn't a descendant of the installed $current ($status)" ;;
  esac
fi
for f in feed_collect.py feed_status.py; do
  (cd "$stage" && python3 -E -s -B "$f" --help > /dev/null 2> "$TMP/start.err") ||
    { warn "$(tail -3 "$TMP/start.err")"; reject "$f from $version doesn't start"; }
done
$DRY_RUN || rm -f "$PENDING"
log "$ARCHIVE@sha256:$digest: attestation verified for $commit ($version)"

if $DRY_RUN; then
  log "dry run: would install $version (now ${current:-hand-copied files})${VERSION:+ and hold}"
  exit 0
fi
# Unpacked beside the releases, so the rename into place stays on one filesystem.
if ! intact "$RELEASES/$version"; then
  new=$(mktemp -d "$RELEASES/.stage-XXXXXX")
  cp -p "$stage"/* "$new/"
  echo "$digest" > "$new/.bundle-sha256"
  chmod 0644 "$new"/* "$new/.bundle-sha256"
  chmod 0755 "$new"
  rm -rf "${RELEASES:?}/${version:?}"
  mv -T "$new" "$RELEASES/$version"
else
  echo "$digest" > "$RELEASES/$version/.bundle-sha256"
fi
touch "$RELEASES/$version"
if ! drain; then
  if [ -n "$VERSION" ]; then
    warn "warning: feed monitor runs didn't end within $IDLE_SECS s; $version is staged but not installed or held, run this again"
    exit 1
  fi
  log "feed monitor runs didn't end within $IDLE_SECS s; $version is staged and switches next run"
  exit 0
fi
switch_to "$version"
resume
[ -z "$current" ] || [ "$current" = "$version" ] || echo "$current" > "$STATE/previous"
if [ -n "$VERSION" ]; then
  mkdir -p "$(dirname "$HOLD")"
  touch "$HOLD"
fi
warn "usnm-feed-monitor deployed: ${current:-hand-copied files} -> $version ($commit)${VERSION:+; held at it until $HOLD is deleted}"

# The --keep newest releases, and always the live and the previous one.
previous=$(cat "$STATE/previous" 2> /dev/null || true)
find "$RELEASES" -mindepth 1 -maxdepth 1 -type d -name 'feed-monitor-*' -printf '%T@ %f\n' | sort -rn |
  awk -v keep="$KEEP" 'NR > keep { print $2 }' | while read -r old; do
  [ "$old" = "$version" ] || [ "$old" = "$previous" ] || rm -rf "${RELEASES:?}/$old"
done
find "$RELEASES" -mindepth 1 -maxdepth 1 -type d -name '.stage-*' -mmin +60 -exec rm -rf {} + 2> /dev/null || true
