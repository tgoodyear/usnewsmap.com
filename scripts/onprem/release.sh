#!/usr/bin/env bash
# A full release on an on-prem host (docs/operations/rebuild-and-index-storage.md, "Full rebuild
# on-prem"; #283, #917): run inside the release container as its usnm user,
# one step at a time, each in its own run directory.
#
#   scripts/onprem/release.sh --run-dir DIR --step mirror
#   scripts/onprem/release.sh --run-dir DIR --step seed [--sample 1pct]
#   scripts/onprem/release.sh --run-dir DIR --step release [--split-docs N]
#   scripts/onprem/release.sh --run-dir DIR --step finish --index-version V [--split-docs N]
#   scripts/onprem/release.sh --run-dir DIR --step publish [--dry-run]
#   scripts/onprem/release.sh --run-dir DIR --step restore [--dry-run]
#   scripts/onprem/release.sh --run-dir DIR --step ready --app-url URL --index-version V
#
# mirror   copies the lake's current.json, catalog/ and the serving
#          version's snapshot into DIR/reference.
# seed     writes DIR/state.json's batch records from the serving version's
#          batch list (--sample 1pct: every 100th batch from the 40th), and
#          reserves the version names the lake has for today and the next
#          two days. Run it the day the release starts.
# release  builds a full staged version: the Quickwit writer on DIR/qw
#          (file://), the WAL in DIR/wal, curated read from the lake, with the
#          #283 settings (one text field, decades, page languages, American
#          Stories, our OCR, the Latin text, v1 ingest). It writes
#          DIR/reference/index-storage-pending.json and nothing in Azure.
#          Its merge wait is USNM_MERGE_TIMEOUT_SECS (default 14400).
# finish   finishes the release of version V in DIR when it stopped after its
#          main index passed the exact count (its merge wait ran out):
#          usnm-ingest release-finish with the release's settings, checked
#          against DIR/release.log. Its merge wait is
#          USNM_FINISH_MERGE_TIMEOUT_SECS (default 86400). Give it the
#          release's Kafka settings (USNM_WRITER_KAFKA and its partitions)
#          and --split-docs, and log it to another file than release.log.
# publish  copies the staged version into the index account and the lake
#          (usnm-ingest publish-staged). Promotion is a separate operator step.
# restore  copies the version the lake already serves back to Azure, to serve
#          from there again after Azure serving was retired (usnm-ingest
#          publish-staged --restore; the disaster recovery runbook, 09 §9.6).
#          DIR is the run directory that built that version, with its
#          state.json, reference/index-storage-pending.json and qw/. It
#          refuses unless the lake's current.json names that version and its
#          indexes, and it writes neither current.json nor the pending pointer.
# ready    waits until the app at --app-url answers /readyz and /v1/meta
#          names --index-version, in USNM_READY_TRIES tries (default 60)
#          USNM_READY_WAIT_SECS apart (default 10), and exits 1 if it never
#          does. Run it before pointing a Worker at the app. It needs only
#          curl and jq.
#
# Settings come from the environment, with prod's defaults: USNM_LAKE
# (stusnmdffapzy), USNM_INDEX_ACCOUNT (usnmqwstprodffapzy), USNM_INGEST
# (the usnm-ingest binary), USNM_QUICKWIT_BIN. Azure access is the host's Arc
# identity: usnm must be in the himds group.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,51s/^# \{0,1\}//p' "$0" >&2; exit 2; }
RUN="" STEP="" SAMPLE="" SPLIT_DOCS=60000 DRY_RUN=false VERSION="" APP_URL=""
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) RUN=${2:-}; shift 2 ;;
    --step) STEP=${2:-}; shift 2 ;;
    --sample) SAMPLE=${2:-}; shift 2 ;;
    --split-docs) SPLIT_DOCS=${2:-}; shift 2 ;;
    --index-version) VERSION=${2:-}; shift 2 ;;
    --app-url) APP_URL=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$RUN" ] && [ -n "$STEP" ] || usage
[[ $RUN == /* ]] || die "--run-dir: an absolute path"
[ -z "$SAMPLE" ] || [ "$SAMPLE" = 1pct ] || die "--sample: 1pct"
[[ $SPLIT_DOCS =~ ^[0-9]+$ ]] || die "--split-docs: a number"
[ -z "$VERSION" ] || [[ $VERSION =~ ^pages-v[0-9]{8}-[0-9]+$ ]] || die "--index-version: pages-vYYYYMMDD-N"
APP_URL_PATTERN='^(https://[A-Za-z0-9.-]+|http://(localhost|127\.0\.0\.1))(:[0-9]+)?/?$'
[ -z "$APP_URL" ] || [[ $APP_URL =~ $APP_URL_PATTERN ]] || die "--app-url: https://HOST (http only for localhost)"
APP_URL=${APP_URL%/}
MERGE_TIMEOUT=${USNM_MERGE_TIMEOUT_SECS:-14400}
FINISH_TIMEOUT=${USNM_FINISH_MERGE_TIMEOUT_SECS:-86400}
[[ $MERGE_TIMEOUT =~ ^[0-9]+$ ]] || die "USNM_MERGE_TIMEOUT_SECS: a number of seconds"
[[ $FINISH_TIMEOUT =~ ^[0-9]+$ ]] || die "USNM_FINISH_MERGE_TIMEOUT_SECS: a number of seconds"
READY_TRIES=${USNM_READY_TRIES:-60}
READY_WAIT=${USNM_READY_WAIT_SECS:-10}
[[ $READY_TRIES =~ ^[1-9][0-9]*$ ]] || die "USNM_READY_TRIES: a number of tries, at least 1"
[[ $READY_WAIT =~ ^[0-9]+$ ]] || die "USNM_READY_WAIT_SECS: a number of seconds"
LAKE=${USNM_LAKE:-stusnmdffapzy}
INDEX_ACCOUNT=${USNM_INDEX_ACCOUNT:-usnmqwstprodffapzy}
INGEST=${USNM_INGEST:-/srv/usnm/target/release/usnm-ingest}
QUICKWIT=${USNM_QUICKWIT_BIN:-/srv/usnm/bin/quickwit}
[ "$STEP" = ready ] || [ -x "$INGEST" ] || die "no usnm-ingest at $INGEST"
LAKE_URL=https://$LAKE.blob.core.windows.net
export IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token
export IMDS_ENDPOINT=http://localhost:40342
export RUST_LOG=${RUST_LOG:-info}
mkdir -p "$RUN"
serving() { jq -r .index_version "$RUN/reference/current.json"; }
# What release and finish share: the writer's environment (the E4
# profile's tuning, infra/modules/ingestjobs.bicep, so a run here compares
# with the Azure ones), the #283 settings and the writer's data. A finish
# must have the release's: the run's build record is checked.
WRITER_ENV=(USNM_WORK_DIR="$RUN/work" USNM_WRITER_WAL_DIR="$RUN/wal"
  USNM_WRITER_HEAP=6GiB USNM_WRITER_COMMIT_SECS=120 USNM_WRITER_QUEUE=4GiB
  USNM_WRITER_INGEST=v1 QW_DISABLE_TELEMETRY=1)
SETTINGS=(--full --stage-index-storage
  --american-stories --ja-latin --ocr-local
  --partition-decade --single-text-field --page-language --split-docs "$SPLIT_DOCS"
  --quickwit-bin "$QUICKWIT"
  --quickwit-metastore "file://$RUN/qw" --quickwit-index-root "file://$RUN/qw")

case "$STEP" in
  mirror)
    [ ! -e "$RUN/reference" ] || die "$RUN/reference exists: mirror into a new run directory"
    "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      mirror-store --from "$LAKE_URL/reference" --to "$RUN/reference" --files current.json
    "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      mirror-store --from "$LAKE_URL/reference" --to "$RUN/reference" --prefixes "catalog,$(serving)"
    ;;
  seed)
    every=()
    [ "$SAMPLE" != 1pct ] || every=(--every 100 --offset 39)
    "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      seed-state --from-version "$(serving)" --reserve-from "$LAKE_URL/reference" "${every[@]}"
    ;;
  release)
    [ -x "$QUICKWIT" ] || die "no quickwit at $QUICKWIT"
    [ ! -e "$RUN/reference/index-storage-pending.json" ] || die "$RUN has a staged version already"
    mkdir -p "$RUN/work" "$RUN/wal" "$RUN/qw"
    env "${WRITER_ENV[@]}" USNM_MERGE_TIMEOUT_SECS="$MERGE_TIMEOUT" \
      "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      release "${SETTINGS[@]}"
    ;;
  finish)
    [ -n "$VERSION" ] || die "--step finish needs --index-version (the stopped run's)"
    [ -x "$QUICKWIT" ] || die "no quickwit at $QUICKWIT"
    [ -s "$RUN/release.log" ] || die "no $RUN/release.log: the finish checks itself against the run's log"
    [ ! -e "$RUN/reference/index-storage-pending.json" ] || die "$RUN has a staged version already"
    env "${WRITER_ENV[@]}" \
      "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      release-finish --run "$VERSION" --release-log "$RUN/release.log" \
      --merge-timeout-secs "$FINISH_TIMEOUT" "${SETTINGS[@]}"
    ;;
  publish|restore)
    mode=()
    if [ "$STEP" = restore ]; then
      [ -s "$RUN/reference/index-storage-pending.json" ] ||
        die "no $RUN/reference/index-storage-pending.json: restore from the run directory that built the version"
      mode=(--restore)
    fi
    dry=()
    ! $DRY_RUN || dry=(--dry-run)
    "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      publish-staged --local-index-root "$RUN/qw" \
      --to-reference "$LAKE_URL/reference" \
      --to-index "https://$INDEX_ACCOUNT.blob.core.windows.net/qw-index" \
      ${dry[@]+"${dry[@]}"} ${mode[@]+"${mode[@]}"}
    ;;
  ready)
    [ -n "$APP_URL" ] || die "--step ready needs --app-url (the app's base URL)"
    [ -n "$VERSION" ] || die "--step ready needs --index-version (the version the app should serve)"
    last="no try yet"
    for ((try = 1; try <= READY_TRIES; try++)); do
      if curl -fsS --max-time 10 -o /dev/null "$APP_URL/readyz"; then
        # In the if, a failure of either command (pipefail) is a failed fetch, not a version.
        if seen=$(curl -fsS --max-time 10 "$APP_URL/v1/meta" | jq -r '.index_version // empty'); then
          if [ "$seen" = "$VERSION" ]; then
            echo "ready: $APP_URL answers /readyz and serves $VERSION"
            exit 0
          fi
          last="/readyz passes, and /v1/meta names ${seen:-no version}"
        else
          last="/readyz passes, and /v1/meta doesn't answer in full with JSON"
        fi
      else
        last="/readyz doesn't pass"
      fi
      echo "try $try of $READY_TRIES: $last" >&2
      [ "$try" -ge "$READY_TRIES" ] || sleep "$READY_WAIT"
    done
    die "$APP_URL isn't ready on $VERSION after $READY_TRIES tries: $last"
    ;;
  *) die "--step: mirror, seed, release, finish, publish, restore or ready" ;;
esac
