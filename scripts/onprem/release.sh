#!/usr/bin/env bash
# A full release on an on-prem host (docs/operations.md, "Full rebuild
# on-prem"; #283, #917): run inside the release container as its usnm user,
# one step at a time, each in its own run directory.
#
#   scripts/onprem/release.sh --run-dir DIR --step mirror
#   scripts/onprem/release.sh --run-dir DIR --step seed [--sample 1pct]
#   scripts/onprem/release.sh --run-dir DIR --step release [--split-docs N]
#   scripts/onprem/release.sh --run-dir DIR --step publish [--dry-run]
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
# publish  copies the staged version into the index account and the lake
#          (usnm-ingest publish-staged). Promotion is a separate operator step.
#
# Settings come from the environment, with prod's defaults: USNM_LAKE
# (stusnmdffapzy), USNM_INDEX_ACCOUNT (usnmqwstprodffapzy), USNM_INGEST
# (the usnm-ingest binary), USNM_QUICKWIT_BIN. Azure access is the host's Arc
# identity: usnm must be in the himds group.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,26s/^# \{0,1\}//p' "$0" >&2; exit 2; }
RUN="" STEP="" SAMPLE="" SPLIT_DOCS=60000 DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --run-dir) RUN=${2:-}; shift 2 ;;
    --step) STEP=${2:-}; shift 2 ;;
    --sample) SAMPLE=${2:-}; shift 2 ;;
    --split-docs) SPLIT_DOCS=${2:-}; shift 2 ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ -n "$RUN" ] && [ -n "$STEP" ] || usage
[[ $RUN == /* ]] || die "--run-dir: an absolute path"
[ -z "$SAMPLE" ] || [ "$SAMPLE" = 1pct ] || die "--sample: 1pct"
[[ $SPLIT_DOCS =~ ^[0-9]+$ ]] || die "--split-docs: a number"
LAKE=${USNM_LAKE:-stusnmdffapzy}
INDEX_ACCOUNT=${USNM_INDEX_ACCOUNT:-usnmqwstprodffapzy}
INGEST=${USNM_INGEST:-/srv/usnm/target/release/usnm-ingest}
QUICKWIT=${USNM_QUICKWIT_BIN:-/srv/usnm/bin/quickwit}
[ -x "$INGEST" ] || die "no usnm-ingest at $INGEST"
LAKE_URL=https://$LAKE.blob.core.windows.net
export IDENTITY_ENDPOINT=http://localhost:40342/metadata/identity/oauth2/token
export IMDS_ENDPOINT=http://localhost:40342
export RUST_LOG=${RUST_LOG:-info}
mkdir -p "$RUN"
serving() { jq -r .index_version "$RUN/reference/current.json"; }

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
    # The E4 profile's writer tuning (infra/modules/ingestjobs.bicep), so a
    # run here compares with the Azure ones.
    USNM_WORK_DIR=$RUN/work USNM_WRITER_WAL_DIR=$RUN/wal \
      USNM_WRITER_HEAP=6GiB USNM_WRITER_COMMIT_SECS=120 USNM_WRITER_QUEUE=4GiB \
      USNM_WRITER_INGEST=v1 USNM_MERGE_TIMEOUT_SECS=14400 QW_DISABLE_TELEMETRY=1 \
      "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      release --full --stage-index-storage \
      --american-stories --ja-latin --ocr-local \
      --partition-decade --single-text-field --page-language --split-docs "$SPLIT_DOCS" \
      --quickwit-bin "$QUICKWIT" \
      --quickwit-metastore "file://$RUN/qw" --quickwit-index-root "file://$RUN/qw"
    ;;
  publish)
    dry=()
    ! $DRY_RUN || dry=(--dry-run)
    "$INGEST" --state-file "$RUN/state.json" --curated "$LAKE_URL/curated" --reference "$RUN/reference" \
      publish-staged --local-index-root "$RUN/qw" \
      --to-reference "$LAKE_URL/reference" \
      --to-index "https://$INDEX_ACCOUNT.blob.core.windows.net/qw-index" "${dry[@]}"
    ;;
  *) die "--step: mirror, seed, release or publish" ;;
esac
