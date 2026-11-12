#!/usr/bin/env bash
# A full release on an on-prem host (docs/operations/rebuild-and-index-storage.md, "Full rebuild
# on-prem"; #283, #917): run inside the release container as its usnm user,
# one step at a time, each in its own run directory.
#
#   scripts/onprem/release.sh --run-dir DIR --step mirror
#   scripts/onprem/release.sh --run-dir DIR --step seed [--sample 1pct]
#   scripts/onprem/release.sh --run-dir DIR --step release [--split-docs N]
#   scripts/onprem/release.sh --run-dir DIR --step finish --index-version V [--split-docs N]
#   scripts/onprem/release.sh --run-dir DIR --step publish (--dry-run | --cosmos-endpoint URL)
#   scripts/onprem/release.sh --run-dir DIR --step restore [--dry-run]
#   scripts/onprem/release.sh --run-dir DIR --step ready --app-url URL --index-version V
#   scripts/onprem/release.sh --run-dir DIR --step prune --app-url URL --index-version V [--delete]
#
# mirror   copies the lake's current.json, catalog/ and the serving
#          version's snapshot into DIR/reference.
# seed     writes DIR/state.json's batch records from the serving version's
#          batch list (--sample 1pct: every 100th batch from the 40th), and
#          reserves the version names the lake has for today and the next
#          two days. Run it the day the release starts.
# release  builds a full staged version: the Quickwit writer on DIR/qw
#          (file://), the WAL in DIR/wal, curated read from the lake (or
#          USNM_CURATED), with the
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
#          (usnm-ingest publish-staged), then records its run in Cosmos
#          index_runs (usnm-ingest import-staged-run, #1096), so /v1/versions
#          and /datasets list it. --cosmos-endpoint, or USNM_COSMOS_ENDPOINT,
#          is https://<account>.documents.azure.com/; it is checked before
#          anything is copied. --dry-run copies and records nothing and needs
#          no endpoint. A rerun continues a copy and keeps a run already
#          recorded. Promotion is a separate operator step.
# restore  copies the version the lake already serves back to Azure, to serve
#          from there again after Azure serving was retired (usnm-ingest
#          publish-staged --restore; the disaster recovery runbook, 09 §9.6).
#          DIR is the run directory that built that version, with its
#          state.json, reference/index-storage-pending.json and qw/. It
#          refuses unless the lake's current.json names that version and its
#          indexes, and it writes neither current.json nor the pending pointer.
#          It writes nothing to Cosmos and needs no endpoint.
# ready    waits until the app at --app-url answers /readyz and /v1/meta
#          names --index-version, in USNM_READY_TRIES tries (default 60)
#          USNM_READY_WAIT_SECS apart (default 10), and exits 1 if it never
#          does. Run it before pointing a Worker at the app. It needs only
#          curl and jq.
# prune    removes the index versions older than the rollback from the index
#          account (usnm-ingest prune, #1703), after a release is verified. Off
#          unless USNM_RELEASE_PRUNE=true, and it needs the host's prune role
#          (USNM_ONPREM_RELEASE_PRUNE). It runs the ready step first, so the
#          app at --app-url must serve V; then a dry run that lists what it
#          would delete and what it keeps. It refuses unless the lake's
#          current.json names V as serving and a rollback version, and no
#          published version is newer than V. Without --delete it stops after
#          the dry run (the report is in DIR/prune-dry-run.log). With --delete
#          it then deletes, keeping V and its rollback. The pipeline state is a
#          fresh file in DIR (the Cosmos writer lock isn't taken: keep ingest
#          jobs off, and never run it beside a release).
#
# Settings come from the environment, with prod's defaults: USNM_LAKE
# (stusnmdffapzy), USNM_INDEX_ACCOUNT (usnmqwstprodffapzy), USNM_INGEST
# (the usnm-ingest binary), USNM_QUICKWIT_BIN. USNM_CURATED reads curated
# from a local copy instead of the lake (#308). The copy must keep each
# blob's last-modified time as its file's (`azcopy copy
# --preserve-last-modified-time`): a finish lists curated as of the run's
# start by those times. Azure access is the host's Arc
# identity: usnm must be in the himds group.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,77s/^# \{0,1\}//p' "$0" >&2; exit 2; }
RUN="" STEP="" SAMPLE="" SPLIT_DOCS=60000 DRY_RUN=false VERSION="" APP_URL="" DELETE=false COSMOS_ENDPOINT=${USNM_COSMOS_ENDPOINT:-}
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) RUN=${2:-}; shift 2 ;;
    --step) STEP=${2:-}; shift 2 ;;
    --sample) SAMPLE=${2:-}; shift 2 ;;
    --split-docs) SPLIT_DOCS=${2:-}; shift 2 ;;
    --index-version) VERSION=${2:-}; shift 2 ;;
    --app-url) APP_URL=${2:-}; shift 2 ;;
    --cosmos-endpoint) COSMOS_ENDPOINT=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    --delete) DELETE=true; shift ;;
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
CURATED=${USNM_CURATED:-$LAKE_URL/curated}
export IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token
export IMDS_ENDPOINT=http://localhost:40342
export RUST_LOG=${RUST_LOG:-info}
# usnm-ingest takes its state from Cosmos (--cosmos) or from a file (--state-file), never both,
# and reads both from these variables too. The steps pass --state-file and the record passes
# --cosmos, so neither call gets the other's setting from the environment.
unset USNM_COSMOS_ENDPOINT USNM_STATE_FILE
mkdir -p "$RUN"
serving() { jq -r .index_version "$RUN/reference/current.json"; }
# The version the release staged, from the pointer publish-staged copies.
staged_version() {
  local pointer=$RUN/reference/index-storage-pending.json v
  [ -s "$pointer" ] || die "no $pointer: this run directory has no finished release to publish"
  v=$(jq -r '.index_version // empty' "$pointer") || die "$pointer isn't JSON"
  [[ $v =~ ^pages-v[0-9]{8}-[0-9]+$ ]] || die "$pointer: index_version isn't pages-vYYYYMMDD-N"
  echo "$v"
}
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
    "$INGEST" --state-file "$RUN/state.json" --curated "$CURATED" --reference "$RUN/reference" \
      mirror-store --from "$LAKE_URL/reference" --to "$RUN/reference" --files current.json
    "$INGEST" --state-file "$RUN/state.json" --curated "$CURATED" --reference "$RUN/reference" \
      mirror-store --from "$LAKE_URL/reference" --to "$RUN/reference" --prefixes "catalog,$(serving)"
    ;;
  seed)
    every=()
    [ "$SAMPLE" != 1pct ] || every=(--every 100 --offset 39)
    "$INGEST" --state-file "$RUN/state.json" --curated "$CURATED" --reference "$RUN/reference" \
      seed-state --from-version "$(serving)" --reserve-from "$LAKE_URL/reference" "${every[@]}"
    ;;
  release)
    [ -x "$QUICKWIT" ] || die "no quickwit at $QUICKWIT"
    [ ! -e "$RUN/reference/index-storage-pending.json" ] || die "$RUN has a staged version already"
    mkdir -p "$RUN/work" "$RUN/wal" "$RUN/qw"
    env "${WRITER_ENV[@]}" USNM_MERGE_TIMEOUT_SECS="$MERGE_TIMEOUT" \
      "$INGEST" --state-file "$RUN/state.json" --curated "$CURATED" --reference "$RUN/reference" \
      release "${SETTINGS[@]}"
    ;;
  finish)
    [ -n "$VERSION" ] || die "--step finish needs --index-version (the stopped run's)"
    [ -x "$QUICKWIT" ] || die "no quickwit at $QUICKWIT"
    [ -s "$RUN/release.log" ] || die "no $RUN/release.log: the finish checks itself against the run's log"
    [ ! -e "$RUN/reference/index-storage-pending.json" ] || die "$RUN has a staged version already"
    env "${WRITER_ENV[@]}" \
      "$INGEST" --state-file "$RUN/state.json" --curated "$CURATED" --reference "$RUN/reference" \
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
    # Only a publish records the run: a restore writes nothing to Cosmos. What the record needs is
    # checked first, so a missing endpoint doesn't leave a long copy done and its run unrecorded.
    record=false
    if [ "$STEP" = publish ] && ! $DRY_RUN; then
      record=true
      [ -n "$COSMOS_ENDPOINT" ] ||
        die "--step publish records the run in Cosmos: give --cosmos-endpoint (https://<account>.documents.azure.com/) or set USNM_COSMOS_ENDPOINT"
      # The Arc identity's token goes to this host, so only a Cosmos account's will do.
      cosmos_re='^https://[a-z0-9-]+\.documents\.azure\.com(:443)?/?$'
      [[ $COSMOS_ENDPOINT =~ $cosmos_re ]] ||
        die "the Cosmos endpoint (--cosmos-endpoint or USNM_COSMOS_ENDPOINT) isn't https://<account>.documents.azure.com/"
      V=$(staged_version)
    fi
    "$INGEST" --state-file "$RUN/state.json" --curated "$CURATED" --reference "$RUN/reference" \
      publish-staged --local-index-root "$RUN/qw" \
      --to-reference "$LAKE_URL/reference" \
      --to-index "https://$INDEX_ACCOUNT.blob.core.windows.net/qw-index" \
      ${dry[@]+"${dry[@]}"} ${mode[@]+"${mode[@]}"}
    if $record; then
      # The run's record is the snapshot's index_run.json, which publish-staged wrote into the
      # local reference from the state file. import-staged-run creates the run, keeps an identical
      # published one (a rerun), replaces a failed placeholder and refuses a different published
      # run of the name. No state file, and no --current: the host's role covers index_runs
      # only, so ops/current stays the ingest job's step after the switch.
      "$INGEST" --cosmos "$COSMOS_ENDPOINT" --curated "$CURATED" --reference "$RUN/reference" \
        import-staged-run --version "$V" ||
        die "$V is copied to Azure, but its run isn't recorded in Cosmos: fix the cause and run --step publish again (it skips what is copied and keeps a run recorded already)"
    fi
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
  prune)
    [ "${USNM_RELEASE_PRUNE:-false}" = true ] ||
      die "--step prune is off: set USNM_RELEASE_PRUNE=true once the host has the prune role (USNM_ONPREM_RELEASE_PRUNE; docs/operations/rebuild-and-index-storage.md, \"Prune from the on-prem release\")"
    [ -n "$APP_URL" ] || die "--step prune needs --app-url (the app that serves the version)"
    [ -n "$VERSION" ] || die "--step prune needs --index-version (the version the app serves)"
    [ -x "$QUICKWIT" ] || die "no quickwit at $QUICKWIT"
    command -v jq > /dev/null 2>&1 || die "--step prune needs jq"
    # After a release is verified: the app answers on the version. Its own errors say why not.
    "$0" --run-dir "$RUN" --step ready --app-url "$APP_URL" --index-version "$VERSION"
    # Pipeline state is a fresh file: prune reads the serving version's run from its snapshot, and
    # the Arc identity has no grant on the Cosmos `ops` container, where the writer lock lives.
    rm -f "$RUN/prune-state.json"
    prune_ingest() {
      QW_AZURE_STORAGE_ACCOUNT="$INDEX_ACCOUNT" "$INGEST" --state-file "$RUN/prune-state.json" \
        --curated "$CURATED" --reference "$LAKE_URL/reference" \
        prune --quickwit-bin "$QUICKWIT" --quickwit-metastore azure://qw-index \
        --quickwit-index-root azure://qw-index "$@"
    }
    prune_report() { # the pretty JSON report that ends the log, from its last line that is just "{"
      awk '/^\{$/ { buf = "" } { buf = buf $0 "\n" } END { printf "%s", buf }' "$1"
    }
    prune_ingest | tee "$RUN/prune-dry-run.log"
    report=$(prune_report "$RUN/prune-dry-run.log")
    # Both versions to keep must be known, and nothing newer: a hand rollback leaves a published
    # version newer than the serving one, whose run this fresh state doesn't show. Prune also leaves
    # newer indexes alone (newer_than_serving), but a release's own prune waits for the roll-forward.
    jq -e 'type == "object" and (.kept | type) == "object" and (.dry_run | type) == "boolean"' <<< "$report" > /dev/null 2>&1 ||
      die "no prune report (a JSON object with kept and dry_run) ends $RUN/prune-dry-run.log: nothing is deleted"
    serving=$(jq -r '.kept.serving // empty' <<< "$report")
    rollback=$(jq -r '.kept.rollback // empty' <<< "$report")
    newer=$(jq -r '(.kept.newer // []) | length' <<< "$report")
    was_dry=$(jq -r '.dry_run' <<< "$report")
    [ "$was_dry" = true ] || die "the prune dry run reports dry_run=$was_dry: nothing is deleted"
    [ "$serving" = "$VERSION" ] || die "the lake serves ${serving:-no version}, not $VERSION: nothing is deleted"
    [[ $rollback =~ ^pages-v[0-9]{8}-[0-9]+$ ]] ||
      die "current.json names no rollback version (${rollback:-none}): can't identify both versions to keep, nothing is deleted"
    [ "$rollback" != "$VERSION" ] || die "the rollback is the serving version: nothing is deleted"
    [ "$newer" = 0 ] || die "$newer published version(s) are newer than $VERSION (a hand rollback?): nothing is deleted"
    echo "prune keeps $VERSION (serving) and $rollback (rollback); candidates and sizes are above"
    if ! $DELETE; then
      echo "dry run only: rerun with --delete to delete the candidates"
      exit 0
    fi
    # The delete run plans again and rechecks what it keeps before each delete.
    prune_ingest --delete | tee "$RUN/prune.log"
    ;;
  *) die "--step: mirror, seed, release, finish, publish, restore, ready or prune" ;;
esac
