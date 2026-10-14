#!/usr/bin/env bash
# The BOINC hand-off job's command (infra/modules/boinc-handoff-job.bicep, #469): the bridge
# image run in the main Container Apps environment, which reaches Cosmos, the lake and the
# exchange account privately. It never talks to the BOINC server: the hand-off container
# (HANDOFF_URL) sits between them. Every path is scratch in the replica; the dispatch root
# lives in the container under dispatch/.
#
# The job's own settings: HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL
# AZURE_CLIENT_ID. Each run's (scripts/start-boinc-bridge.sh --target handoff):
#   HANDOFF_STEP=export   HANDOFF_PAGES=<page key>[,<page key>...] HANDOFF_BATCH=<batch>
#                         HANDOFF_PLAN=<plan id> HANDOFF_MANIFEST=<file under manifests/>
#                         ENGINE_IMAGE=sha256:<the worker image's digest>
#                         copies scans/ and the manifest down, runs usnm-boinc export (the
#                         ocr-sync lease in Cosmos, the native ledger reservation) and writes
#                         the dispatch root back; prints the job ids
#   HANDOFF_STEP=collect  JOB_ID [HOST_ID=<the canonical result's host id>]
#                         copies the dispatch root, spool/<id>/ and gold/trust.json down, runs
#                         usnm-boinc collect --trust into ocr-out and writes the root back
set -euo pipefail
log() { echo "usnm-boinc-handoff: $*" >&2; }
fail() { log "$*"; exit 1; }
for v in HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL; do
  [ -n "${!v:-}" ] || fail "$v is not set (the job's own setting)"
done
[[ $HANDOFF_URL =~ ^https://[a-z0-9]{3,24}\.blob\.core\.windows\.net/boinc-handoff$ ]] ||
  fail "HANDOFF_URL must be https://<account>.blob.core.windows.net/boinc-handoff"
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
root="$work/dispatch"
usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix dispatch --to "$root"
case "${HANDOFF_STEP:-}" in
  export)
    [[ ${HANDOFF_MANIFEST:-} =~ ^[A-Za-z0-9][A-Za-z0-9._-]{0,127}$ ]] || fail "HANDOFF_MANIFEST is a file name under manifests/"
    [[ ${HANDOFF_PAGES:-} =~ ^[A-Za-z0-9/_,-]+$ ]] || fail "HANDOFF_PAGES is a comma-separated list of page keys"
    : "${HANDOFF_BATCH:?set HANDOFF_BATCH}" "${HANDOFF_PLAN:?set HANDOFF_PLAN}" "${ENGINE_IMAGE:?set ENGINE_IMAGE}"
    usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix scans --to "$work/scans"
    usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix manifests --to "$work/manifests"
    [ -f "$work/manifests/$HANDOFF_MANIFEST" ] || fail "no manifests/$HANDOFF_MANIFEST in the hand-off container"
    tr ',' '\n' <<< "$HANDOFF_PAGES" > "$work/pages.txt"
    rc=0
    usnm-boinc export --manifest "$work/manifests/$HANDOFF_MANIFEST" --pages "$work/pages.txt" \
      --batch "$HANDOFF_BATCH" --scans "$work/scans" --root "$root" --plan "$HANDOFF_PLAN" \
      --image "$ENGINE_IMAGE" --curated-url "$CURATED_URL" --reference-url "$REFERENCE_URL" \
      --cosmos "$COSMOS_ENDPOINT" || rc=$?
    # Whatever export got to (a reservation is journalled before it returns) goes back.
    [ ! -d "$root" ] || usnm-boinc handoff put --from "$root" --to-url "$HANDOFF_URL" --prefix dispatch
    exit "$rc" ;;
  collect)
    [[ ${JOB_ID:-} =~ ^[0-9a-f]{64}$ ]] || fail "JOB_ID is a job id (64 hex)"
    host=$(usnm-boinc handoff get-spool --from-url "$HANDOFF_URL" --job "$JOB_ID" \
      --spool "$work/spool" --trust-out "$work/trust.json")
    if [ -n "${HOST_ID:-}" ]; then
      [[ $HOST_ID =~ ^[0-9]+$ ]] || fail "HOST_ID is a number"
      [ "$host" = unknown ] || [ "$host" = "$HOST_ID" ] || fail "HOST_ID $HOST_ID is not the server's $host"
      host=$HOST_ID
    fi
    [ "$host" != unknown ] || fail "the server recorded no host id; read it from the ops pages and pass HOST_ID"
    rc=0
    usnm-boinc collect --root "$root" --spool "$work/spool" --job "$JOB_ID" \
      --curated-url "$CURATED_URL" --out-url "$OUT_URL" --trust "$work/trust.json" --host-id "$host" || rc=$?
    usnm-boinc handoff put --from "$root" --to-url "$HANDOFF_URL" --prefix dispatch
    exit "$rc" ;;
  *) fail "HANDOFF_STEP must be export or collect" ;;
esac
