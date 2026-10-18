#!/usr/bin/env bash
# The BOINC hand-off job's command (infra/modules/boinc-handoff-job.bicep, #469): the bridge
# image run in the main Container Apps environment, which reaches Cosmos, the lake and the
# exchange account privately. It never talks to the BOINC server: the hand-off container
# (HANDOFF_URL) sits between them. Every path is scratch in the replica; the dispatch root
# lives in the container under dispatch/.
#
# The job's own settings: HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL
# QUEUE_URL AZURE_CLIENT_ID. Each run's (scripts/start-boinc-bridge.sh --target handoff):
#   HANDOFF_STEP=export   HANDOFF_PAGES=<page key>[,<page key>...] HANDOFF_BATCH=<batch>
#                         HANDOFF_PLAN=<plan id> HANDOFF_MANIFEST=<path under manifests/, as
#                         push-boinc-scan.sh names it: <version>/manifest.parquet>
#                         ENGINE_IMAGE=sha256:<the worker image's digest>
#                         [HANDOFF_SCANS=<relative scan>[,...] HANDOFF_JOBS=<id>[,...]]
#                         bounds transfers to these scans/sidecars, the exact manifest and
#                         previously journaled ids (the batch controller always supplies this)
#                         [HANDOFF_CLAIMS=1] the pages came off the work queues (step claim):
#                         their claims/<doc_id>.json come down too and each job keeps its queue
#                         message and ledger row (usnm-boinc export --claims)
#                         copies scans/ and the manifest down, runs usnm-boinc export (the
#                         ocr-sync lease in Cosmos, the native ledger reservation) and writes
#                         back the journal entries it touched; prints the job ids
#   HANDOFF_STEP=claim    HANDOFF_MANIFEST=<as for export> CLAIM_NONCE=<8 to 64 letters or digits>
#                         [CLAIM_MAX=<1 to 100, default 10>] [CLAIM_QUEUES=ocr-bulk[,ocr-priority]]
#                         [CLAIM_LEASE_HOURS=<1 to 168, default 96>]
#                         takes usable pages off the work queues, as a second consumer beside
#                         the on-prem nodes (docs/boinc-batch-scheduler.md#queue-fed-work):
#                         writes claims/<doc_id>.json, then scheduler/claims.json, which names
#                         the pages and their scans and echoes the nonce. Messages it can't use
#                         here go back to the queue for the nodes. Only pages on the newest
#                         fetched-scans list for the manifest's version are taken
#   HANDOFF_STEP=release  HANDOFF_DOCS=<doc_id>[,<doc_id>...]
#                         gives claimed pages back to their queue, for the nodes (a scan that
#                         wasn't on the scan server after all)
#   HANDOFF_STEP=collect  JOB_ID [HOST_ID=<the canonical result's host id>]
#                         copies the dispatch root, spool/<id>/ and gold/trust.json down, runs
#                         usnm-boinc collect --trust into ocr-out (a terminal BOINC failure:
#                         collect without a host or trust list, which marks the job failed) and
#                         writes back that job's entry; a job that came off the work queues
#                         then has its queue message deleted (a failure there only warns: the
#                         message comes back and the ledger makes the page's second read a no-op)
#   HANDOFF_STEP=texts-export
#                         [HANDOFF_PAGES=<doc_id>[,<doc_id>...]]
#                         [HANDOFF_SAMPLE=<file name under quality-samples/>]
#                         exports texts/<doc_id>.json (and <doc_id>.ours.xml when present) from
#                         CURATED_URL with usnm-boinc quality texts-export, and uploads the
#                         directory to HANDOFF_URL under texts/ (#522)
#
# Only the entries a run touched go back, so runs for different jobs don't undo each other.
# Run one execution at a time all the same (scripts/start-boinc-bridge.sh refuses to start
# while one runs). If writing an entry back keeps failing, its record is printed to the log
# (USNM-HANDOFF-RECORD lines) so it can be restored by hand: a lost reservation's record
# can't be made again (its queued_at is in the ledger).
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
# put_back DIR...: writes each journal directory (relative to the root) back, retrying.
put_back() {
  local d try
  for d in "$@"; do
    [ -d "$root/$d" ] || continue
    for try in 1 2 3 4 5; do
      usnm-boinc handoff put --from "$root/$d" --to-url "$HANDOFF_URL" --prefix "dispatch/$d" && continue 2
      log "writing dispatch/$d back failed (attempt $try)"
      sleep $((try * 10))
    done
    find "$root/$d" -name record.json -exec sh -c 'printf "USNM-HANDOFF-RECORD %s %s\n" "$1" "$(tr -d "\n" < "$1")"' _ {} \;
    fail "could not write dispatch/$d back; its record is in this log"
  done
}
case "${HANDOFF_STEP:-}" in
  export)
    [[ ${HANDOFF_MANIFEST:-} =~ ^[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*$ ]] &&
      [[ $HANDOFF_MANIFEST != *..* ]] || fail "HANDOFF_MANIFEST is a path under manifests/"
    [[ ${HANDOFF_PAGES:-} =~ ^[A-Za-z0-9/_,-]+$ ]] || fail "HANDOFF_PAGES is a comma-separated list of page keys"
    : "${HANDOFF_BATCH:?set HANDOFF_BATCH}" "${HANDOFF_PLAN:?set HANDOFF_PLAN}" "${ENGINE_IMAGE:?set ENGINE_IMAGE}"
    if [ -n "${HANDOFF_SCANS:-}" ]; then
      IFS=, read -r -a scans <<< "$HANDOFF_SCANS"
      [ "${#scans[@]}" -le 100 ] || fail "HANDOFF_SCANS is at most 100 cached scans"
      for scan in "${scans[@]}"; do
        [[ "$scan" =~ ^keep/[A-Za-z0-9][A-Za-z0-9._/-]*$ ]] && [[ "$scan" != *..* ]] ||
          fail "HANDOFF_SCANS must name cached scans under keep/"
        for suffix in "" .json; do
          usnm-boinc handoff get-file --from-url "$HANDOFF_URL" --path "scans/$scan$suffix" \
            --to "$work/scans/$scan$suffix"
        done
      done
      if [ -n "${HANDOFF_JOBS:-}" ]; then
        IFS=, read -r -a jobs <<< "$HANDOFF_JOBS"
        [ "${#jobs[@]}" -le 100 ] || fail "HANDOFF_JOBS is at most 100 existing jobs"
        for id in "${jobs[@]}"; do
          [[ "$id" =~ ^[0-9a-f]{64}$ ]] || fail "HANDOFF_JOBS must be 64 hex job ids"
          usnm-boinc handoff get-file --from-url "$HANDOFF_URL" --path "dispatch/jobs/$id/record.json" \
            --to "$root/jobs/$id/record.json"
        done
      fi
      usnm-boinc handoff get-file --from-url "$HANDOFF_URL" --path "manifests/$HANDOFF_MANIFEST" \
        --to "$work/manifests/$HANDOFF_MANIFEST"
    else
      usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix dispatch --to "$root"
      usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix scans --to "$work/scans"
      usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix manifests --to "$work/manifests"
    fi
    [ -f "$work/manifests/$HANDOFF_MANIFEST" ] || fail "no manifests/$HANDOFF_MANIFEST in the hand-off container"
    tr ',' '\n' <<< "$HANDOFF_PAGES" > "$work/pages.txt"
    claim_args=()
    if [ "${HANDOFF_CLAIMS:-}" = 1 ]; then
      IFS=, read -r -a pages <<< "$HANDOFF_PAGES"
      [ "${#pages[@]}" -le 100 ] || fail "HANDOFF_PAGES is at most 100 queue-fed pages"
      for page in "${pages[@]}"; do
        doc=${page//\//_}
        usnm-boinc handoff get-file --from-url "$HANDOFF_URL" --path "claims/$doc.json" \
          --to "$work/claims/$doc.json"
      done
      claim_args=(--claims "$work/claims")
    fi
    stamp="$work/stamp"
    touch "$stamp"
    sleep 1
    rc=0
    usnm-boinc export --manifest "$work/manifests/$HANDOFF_MANIFEST" --pages "$work/pages.txt" \
      --batch "$HANDOFF_BATCH" --scans "$work/scans" --root "$root" --plan "$HANDOFF_PLAN" \
      --image "$ENGINE_IMAGE" --curated-url "$CURATED_URL" --reference-url "$REFERENCE_URL" \
      --cosmos "$COSMOS_ENDPOINT" ${claim_args[@]+"${claim_args[@]}"} || rc=$?
    # Whatever export got to (it journals each entry before it reserves) goes back.
    touched=()
    if [ -d "$root/jobs" ]; then
      while IFS= read -r f; do touched+=("jobs/$(basename "$(dirname "$f")")"); done \
        < <(find "$root/jobs" -mindepth 2 -maxdepth 2 -name record.json -newer "$stamp")
    fi
    put_back ${touched[@]+"${touched[@]}"}
    exit "$rc" ;;
  claim)
    [ -n "${QUEUE_URL:-}" ] || fail "QUEUE_URL is not set (the job's own setting)"
    [[ $QUEUE_URL =~ ^https://[a-z0-9]{3,24}\.queue\.core\.windows\.net$ ]] ||
      fail "QUEUE_URL must be https://<account>.queue.core.windows.net"
    [[ ${HANDOFF_MANIFEST:-} =~ ^[A-Za-z0-9][A-Za-z0-9._-]*/manifest\.parquet$ ]] && [[ $HANDOFF_MANIFEST != *..* ]] ||
      fail "HANDOFF_MANIFEST is <version>/manifest.parquet"
    version=${HANDOFF_MANIFEST%/manifest.parquet}
    [[ ${CLAIM_NONCE:-} =~ ^[A-Za-z0-9]{8,64}$ ]] || fail "CLAIM_NONCE is 8 to 64 letters or digits"
    max=${CLAIM_MAX:-10}
    [[ $max =~ ^[0-9]+$ ]] && [ "$max" -ge 1 ] && [ "$max" -le 100 ] || fail "CLAIM_MAX is 1 to 100"
    hours=${CLAIM_LEASE_HOURS:-96}
    [[ $hours =~ ^[0-9]+$ ]] && [ "$hours" -ge 1 ] && [ "$hours" -le 168 ] || fail "CLAIM_LEASE_HOURS is 1 to 168"
    queue_args=()
    IFS=, read -r -a queues <<< "${CLAIM_QUEUES:-ocr-bulk}"
    for q in "${queues[@]}"; do
      [ "$q" = ocr-bulk ] || [ "$q" = ocr-priority ] || fail "CLAIM_QUEUES is ocr-bulk and/or ocr-priority"
      queue_args+=(--queue "$q")
    done
    mkdir -p "$work/claims" "$work/summary"
    rc=0
    usnm-boinc claim pull --queue-url "$QUEUE_URL" --out-url "$OUT_URL" --curated-url "$CURATED_URL" \
      --reference-url "$REFERENCE_URL" --cosmos "$COSMOS_ENDPOINT" --manifest-version "$version" \
      --max "$max" --lease-hours "$hours" --out "$work/claims" --summary "$work/summary/claims.json" \
      --nonce "$CLAIM_NONCE" "${queue_args[@]}" || rc=$?
    # The claim files first, even after a failure: the messages are hidden now and their pop
    # receipts exist nowhere else. A message whose claim can't be written comes back to the
    # queue when its lease runs out.
    for try in 1 2 3 4 5; do
      usnm-boinc handoff put --from "$work/claims" --to-url "$HANDOFF_URL" --prefix claims && break
      [ "$try" -lt 5 ] || fail "could not write the claims; their messages come back in $hours hours"
      log "writing the claims failed (attempt $try)"
      sleep $((try * 10))
    done
    if [ -f "$work/summary/claims.json" ]; then
      usnm-boinc handoff put --from "$work/summary" --to-url "$HANDOFF_URL" --prefix scheduler
    fi
    exit "$rc" ;;
  release)
    [ -n "${QUEUE_URL:-}" ] || fail "QUEUE_URL is not set (the job's own setting)"
    [[ ${HANDOFF_DOCS:-} =~ ^[A-Za-z0-9_,-]+$ ]] || fail "HANDOFF_DOCS is a comma-separated list of doc ids"
    IFS=, read -r -a docs <<< "$HANDOFF_DOCS"
    [ "${#docs[@]}" -le 100 ] || fail "HANDOFF_DOCS is at most 100 doc ids"
    doc_args=()
    for d in "${docs[@]}"; do doc_args+=(--doc-id "$d"); done
    usnm-boinc claim release --handoff-url "$HANDOFF_URL" --queue-url "$QUEUE_URL" "${doc_args[@]}" ;;
  collect)
    [[ ${JOB_ID:-} =~ ^[0-9a-f]{64}$ ]] || fail "JOB_ID is a job id (64 hex)"
    usnm-boinc handoff get-file --from-url "$HANDOFF_URL" --path "dispatch/jobs/$JOB_ID/record.json" \
      --to "$root/jobs/$JOB_ID/record.json"
    usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix "dispatch/jobs/$JOB_ID/accepted" \
      --to "$root/jobs/$JOB_ID/accepted"
    host=$(usnm-boinc handoff get-spool --from-url "$HANDOFF_URL" --job "$JOB_ID" \
      --spool "$work/spool" --trust-out "$work/trust.json")
    rc=0
    if [ -e "$work/spool/usnm_${JOB_ID}_error" ] || [ -e "$work/spool/usnm_${JOB_ID}_no_output_files" ]; then
      # A terminal BOINC failure has no canonical host; collect marks the job failed.
      usnm-boinc collect --root "$root" --spool "$work/spool" --job "$JOB_ID" \
        --curated-url "$CURATED_URL" --out-url "$OUT_URL" || rc=$?
    else
      if [ -n "${HOST_ID:-}" ]; then
        [[ $HOST_ID =~ ^[0-9]+$ ]] || fail "HOST_ID is a number"
        [ "$host" = unknown ] || [ "$host" = "$HOST_ID" ] || fail "HOST_ID $HOST_ID is not the server's $host"
        host=$HOST_ID
      fi
      [ "$host" != unknown ] || fail "the server recorded no host id; read it from the ops pages and pass HOST_ID"
      [ -f "$work/trust.json" ] || fail "no gold/trust.json in the hand-off container (run spool-out after gold-trust)"
      usnm-boinc collect --root "$root" --spool "$work/spool" --job "$JOB_ID" \
        --curated-url "$CURATED_URL" --out-url "$OUT_URL" --trust "$work/trust.json" --host-id "$host" || rc=$?
    fi
    put_back "jobs/$JOB_ID" "quarantine/$JOB_ID"
    if [ "$rc" -eq 0 ] && [ -n "${QUEUE_URL:-}" ]; then
      # A job from the work queues is done: take its message off. NotQueueFed, NoClaim and
      # Already are all fine; an error only leaves the message to come back.
      usnm-boinc claim finish --root "$root" --job "$JOB_ID" --handoff-url "$HANDOFF_URL" \
        --queue-url "$QUEUE_URL" || log "could not delete the queue message for $JOB_ID; it comes back and is deleted as answered"
    fi
    exit "$rc" ;;
  texts-export)
    [ -n "${HANDOFF_PAGES:-}" ] || [ -n "${HANDOFF_SAMPLE:-}" ] ||
      fail "set HANDOFF_PAGES and/or HANDOFF_SAMPLE"
    doc_args=()
    if [ -n "${HANDOFF_PAGES:-}" ]; then
      [[ $HANDOFF_PAGES =~ ^[A-Za-z0-9_,-]+$ ]] || fail "HANDOFF_PAGES is a comma-separated list of doc ids"
      IFS=, read -r -a docs <<< "$HANDOFF_PAGES"
      for d in "${docs[@]}"; do doc_args+=(--doc-id "$d"); done
    fi
    if [ -n "${HANDOFF_SAMPLE:-}" ]; then
      [[ $HANDOFF_SAMPLE =~ ^[A-Za-z0-9][A-Za-z0-9._-]*$ ]] && [[ $HANDOFF_SAMPLE != *..* ]] ||
        fail "HANDOFF_SAMPLE is a file name under quality-samples/"
      usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix quality-samples --to "$work/samples"
      [ -f "$work/samples/$HANDOFF_SAMPLE" ] || fail "no quality-samples/$HANDOFF_SAMPLE in the hand-off container"
      doc_args+=(--sample "$work/samples/$HANDOFF_SAMPLE")
    fi
    mkdir -p "$work/texts"
    usnm-boinc quality texts-export --curated-url "$CURATED_URL" --out-dir "$work/texts" "${doc_args[@]}"
    usnm-boinc handoff put --from "$work/texts" --to-url "$HANDOFF_URL" --prefix texts ;;
  *) fail "HANDOFF_STEP must be export, claim, release, collect or texts-export" ;;
esac
