#!/usr/bin/env bash
# Run the hand-off job's texts-export step on an on-prem host that holds a local copy of curated
# (#1116), so the American Stories page shards are read from disk, not from Azure:
#
#   boinc/scripts/texts-export-onprem.sh --settings /etc/usnm/texts-export.env \
#     --usnm-boinc /usr/local/bin/usnm-boinc --american-stories-root /srv/curated \
#     --sample <file under quality-samples/> [--doc-id <doc id> ...]
#
# --settings is an env file with the hand-off job's settings (HANDOFF_URL, CURATED_URL,
# COSMOS_ENDPOINT, REFERENCE_URL, OUT_URL) and the host's Arc identity endpoints
# (IDENTITY_ENDPOINT, IMDS_ENDPOINT); nothing else is read from it. The ocr-local overlays and
# their ALTO still come from CURATED_URL, and the step logs how many bytes it read there
# (curated_bytes). The texts go to the hand-off container's texts/ as the Azure job's did. The
# host's identity needs read on curated's ocr-local/ and the hand-off container's writer role.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
SETTINGS="" BIN="" AS_ROOT="" SAMPLE="" DOCS=()
while [ $# -gt 0 ]; do
  case $1 in
    --settings) [ $# -ge 2 ] || usage; SETTINGS=$2; shift 2 ;;
    --usnm-boinc) [ $# -ge 2 ] || usage; BIN=$2; shift 2 ;;
    --american-stories-root) [ $# -ge 2 ] || usage; AS_ROOT=${2%/}; shift 2 ;;
    --sample) [ $# -ge 2 ] || usage; SAMPLE=$2; shift 2 ;;
    --doc-id) [ $# -ge 2 ] || usage; DOCS+=("$2"); shift 2 ;;
    -h | --help) usage ;;
    *) die "unknown argument: $1" ;;
  esac
done
[ -n "$SETTINGS" ] && [ -n "$BIN" ] && [ -n "$AS_ROOT" ] || usage
[ -n "$SAMPLE" ] || [ ${#DOCS[@]} -gt 0 ] || die "give --sample or --doc-id"
[ -r "$SETTINGS" ] || die "can't read $SETTINGS"
[ -x "$BIN" ] || die "$BIN is not an executable usnm-boinc"
[ -d "$AS_ROOT/american-stories/pages" ] || die "$AS_ROOT has no american-stories/pages"
allowed=" HANDOFF_URL CURATED_URL COSMOS_ENDPOINT REFERENCE_URL OUT_URL IDENTITY_ENDPOINT IMDS_ENDPOINT "
env_args=()
while IFS= read -r line || [ -n "$line" ]; do
  case $line in '' | '#'*) continue ;; esac
  [[ $line =~ ^([A-Z][A-Z0-9_]{0,63})=(.*)$ ]] || die "bad line in $SETTINGS"
  [[ $allowed == *" ${BASH_REMATCH[1]} "* ]] || die "$SETTINGS sets ${BASH_REMATCH[1]}; it takes only$allowed"
  env_args+=("$line")
done < "$SETTINGS"
pages=""
if [ ${#DOCS[@]} -gt 0 ]; then pages=$(IFS=,; echo "${DOCS[*]}"); fi
bindir=$(mktemp -d)
trap 'rm -rf "$bindir"' EXIT
ln -s "$(cd "$(dirname "$BIN")" && pwd)/$(basename "$BIN")" "$bindir/usnm-boinc"
env PATH="$bindir:$PATH" "${env_args[@]}" \
  HANDOFF_STEP=texts-export HANDOFF_PAGES="$pages" HANDOFF_SAMPLE="$SAMPLE" AMERICAN_STORIES_ROOT="$AS_ROOT" \
  bash "$(dirname "$0")/../../boinc/images/bridge/handoff-job.sh"
