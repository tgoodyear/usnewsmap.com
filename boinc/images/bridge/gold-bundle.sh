#!/usr/bin/env bash
# Build a locked gold bundle (docs/ocr-gold-set.md#volunteer-bundle) inside the VNet, onto the
# project share, and install it for a gold campaign (boinc/docs/gold-check.md#build-the-gold-bundle):
#
#   gold-bundle.sh --share /project --campaign v2-tess550 --version 2
#   Add --source-cache /sources to rebuild offline from a read-only cache of all six datasets.
#
# 1. gold.py public-fetch for bln600, overproof-ca, newseye-bnf, newseye-nlf-fi,
#    newseye-nlf-sv and nzz into <share>/gold/build-v1/data, with gold.py's own pacing (one
#    request at a time, a second apart; loc.gov as locfetch paces it). A file already there is
#    not fetched again, so a rerun resumes.
# 2. gold.py public-convert for each, with the flags of the 13 October 2026 runs the lock was
#    built from (docs/ocr-scoring-guide.md), into <share>/gold/build-v1/out.
# 3. usnm-ocr-survey bundle-build with the selected version, --seed 457 --per-source 30, writing its lock
#    beside the bundle, then bundle-verify on the tar.
# 4. The new lock must equal the committed one (--lock), the tar's sha256 must be --tar-sha256
#    and the manifest's --manifest-sha256. Anything else stops here and installs nothing.
# 5. <share>/gold/<campaign>/bundle.tar: written beside, then renamed. An existing one is kept
#    if it has the same sha256 and refused otherwise.
#
# No database, no secrets: it talks only to the datasets' publishers and writes the share.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
SHARE="" CAMPAIGN="" VERSION=${GOLD_BUNDLE_VERSION:-1} SOURCE_CACHE=""
LIB=/usr/local/lib/usnm-boinc
GOLD_PY="$LIB/gold/scripts/ocr-survey/gold.py"
SURVEY=/usr/local/bin/usnm-ocr-survey
LOCK="" TAR_SHA="" MANIFEST_SHA=""
while [ $# -gt 0 ]; do
  case $1 in
    --share) [ $# -ge 2 ] || usage; SHARE=$2; shift 2 ;;
    --campaign) [ $# -ge 2 ] || usage; CAMPAIGN=$2; shift 2 ;;
    --version) [ $# -ge 2 ] || usage; VERSION=$2; shift 2 ;;
    --source-cache) [ $# -ge 2 ] || usage; SOURCE_CACHE=$2; shift 2 ;;
    --gold-py) [ $# -ge 2 ] || usage; GOLD_PY=$2; shift 2 ;;
    --survey) [ $# -ge 2 ] || usage; SURVEY=$2; shift 2 ;;
    --lock) [ $# -ge 2 ] || usage; LOCK=$2; shift 2 ;;
    --tar-sha256) [ $# -ge 2 ] || usage; TAR_SHA=$2; shift 2 ;;
    --manifest-sha256) [ $# -ge 2 ] || usage; MANIFEST_SHA=$2; shift 2 ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$SHARE" ] && [ -n "$CAMPAIGN" ] || usage
log() { echo "gold-bundle: $*" >&2; }
die() { log "$*"; exit 1; }
case $VERSION in 1 | 2) ;; *) die "--version must be 1 or 2" ;; esac
LOCK=${LOCK:-"$LIB/gold/bundle-v$VERSION.lock.json"}
[ -f "$LOCK" ] || die "no lock at $LOCK"
[ "$(jq -er .bundle_version "$LOCK")" = "$VERSION" ] || die "lock does not match --version $VERSION"
TAR_SHA=${TAR_SHA:-$(jq -er .tar_sha256 "$LOCK")}
MANIFEST_SHA=${MANIFEST_SHA:-$(jq -er .manifest_sha256 "$LOCK")}
[[ $CAMPAIGN =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || die "--campaign must be 1-41 of [a-z0-9-]"
[[ $TAR_SHA =~ ^[0-9a-f]{64}$ && $MANIFEST_SHA =~ ^[0-9a-f]{64}$ ]] || die "the expected checksums must be 64 hex digits"
sha256() { sha256sum "$1" | cut -d' ' -f1; }

# One build at a time across versions, including older bridge images using this lock name.
# Executions of a manual job can overlap, and they share the
# destination. mkdir is atomic on the share. A lock older than the job's six-hour timeout is
# from a killed run: of two runs that see it, only the one whose rename succeeds takes over.
mkdir -p "$SHARE/gold"
lock="$SHARE/gold/.build-v1.lock"
if ! mkdir "$lock" 2> /dev/null; then
  [ -n "$(find "$lock" -maxdepth 0 -mmin +420 2> /dev/null)" ] || die "another gold-bundle run holds $lock; wait for it to end"
  stale="$lock.stale.$$.$(date +%s)"
  mv "$lock" "$stale" 2> /dev/null || die "another run took over $lock first"
  rm -rf "$stale"
  mkdir "$lock" 2> /dev/null || die "another run took $lock first"
  log "took over $lock, left by a run more than 7 hours ago"
fi
trap 'rm -rf "$lock"' EXIT

dest="$SHARE/gold/$CAMPAIGN/bundle.tar"
if [ -e "$dest" ]; then
  [ "$(sha256 "$dest")" = "$TAR_SHA" ] || die "$dest exists with another sha256; move it away first"
  log "$dest is already the bundle ($TAR_SHA)"
  exit 0
fi

work="$SHARE/gold/build-v$VERSION"
data=${SOURCE_CACHE:-"$work/data"} out="$work/out"
if [ -n "$SOURCE_CACHE" ]; then
  [ -d "$data" ] || die "no source cache at $data"
else
  mkdir -p "$data"
fi
mkdir -p "$out"
# Dataset, then its public-fetch flags, then its public-convert flags.
datasets=(
  "bln600||--count 600 --per-page 1"
  "overproof-ca|--pages 20|--count 600 --per-page 50"
  "newseye-bnf|--subset validation|--count 60"
  "newseye-nlf-fi|--subset validation|--count 60"
  "newseye-nlf-sv|--subset validation|--count 60"
  "nzz||--count 60"
)
sources=()
for spec in "${datasets[@]}"; do
  IFS='|' read -r ds fetch convert <<< "$spec"
  if [ -n "$SOURCE_CACHE" ]; then
    [ -d "$data/$ds" ] || die "source cache is missing $ds"
  else
    log "fetching $ds"
    # shellcheck disable=SC2086 # the flags are words
    python3 "$GOLD_PY" public-fetch --dataset "$ds" --src "$data/$ds" --confirm-download $fetch ||
      die "public-fetch $ds failed"
  fi
  log "converting $ds"
  rm -rf "${out:?}/$ds"
  # shellcheck disable=SC2086
  python3 "$GOLD_PY" public-convert --dataset "$ds" --src "$data/$ds" --out "$out/$ds" $convert ||
    die "public-convert $ds failed"
  sources+=(--source "$ds=$out/$ds")
done

bundle="$work/bundle"
rm -rf "$bundle"
log "building the bundle"
built=$("$SURVEY" bundle-build --version "$VERSION" --seed 457 --per-source 30 --out "$bundle" \
  "${sources[@]}" --original "bln600=$data/bln600/BLN600.zip" --lock "$work/bundle-v$VERSION.lock.json") ||
  die "bundle-build failed"
printf '%s\n' "$built" >&2
tar=$(jq -er .tar <<< "$built") || die "bundle-build named no tar"
verified=$("$SURVEY" bundle-verify --bundle "$tar") || die "bundle-verify failed on $tar"
printf '%s\n' "$verified" >&2

cmp -s "$work/bundle-v$VERSION.lock.json" "$LOCK" ||
  die "the bundle differs from the committed lock $LOCK: $(diff <(jq -S 'del(.crops, .files, .remotes)' "$LOCK") \
    <(jq -S 'del(.crops, .files, .remotes)' "$work/bundle-v$VERSION.lock.json") | tr '\n' ' ')"
got_tar=$(sha256 "$tar")
[ "$got_tar" = "$TAR_SHA" ] || die "the tar's sha256 is $got_tar, not $TAR_SHA"
[ "$(jq -r .tar_sha256 <<< "$built")" = "$TAR_SHA" ] || die "bundle-build reports another tar sha256"
for m in "$(jq -r .manifest_sha256 <<< "$built")" "$(jq -r .manifest_sha256 <<< "$verified")"; do
  [ "$m" = "$MANIFEST_SHA" ] || die "the manifest's sha256 is $m, not $MANIFEST_SHA"
done
[ "$(jq -r .verified <<< "$verified")" = true ] || die "bundle-verify did not report verified"

mkdir -p "$(dirname "$dest")"
cp "$tar" "$dest.part"
[ "$(sha256 "$dest.part")" = "$TAR_SHA" ] || { rm -f "$dest.part"; die "the copy on the share has another sha256"; }
mv "$dest.part" "$dest"
log "wrote $dest (sha256 $TAR_SHA, manifest $MANIFEST_SHA)"
