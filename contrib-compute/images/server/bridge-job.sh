#!/usr/bin/env bash
# The Container Apps Job's command (infra/boinc/modules/bridge-job.bicep). Renders config.xml
# into tmpfs like the server does, then runs one usnm-boinc step against the hand-off root.
#
#   JOB_ID=<id>              the exported job (required for submit and reconcile)
#   BRIDGE_STEP=submit       or: reconcile (then BRIDGE_FOUND=present|absent is required)
#
# The JP2 against IIIF JPEG assessment (#456, docs/notes/2026-10-16-boinc-assessment.md), under
# $PROJECT_DIR/assess/<ASSESS_CAMPAIGN> on the share (one campaign per engine build;
# ASSESS_CAMPAIGN is required, lower-case letters, digits and hyphens):
#   BRIDGE_STEP=assess-stage    ASSESS_MODE=ocr|crash ASSESS_ENGINE_IMAGE=sha256:... (worker image)
#                               ASSESS_ENGINE_BASE=<its TESSERACT_IMAGE> [ASSESS_ENGINE_VERSION=5.5.0]
#                               [ASSESS_MAX=100]
#                               fetches from LoC, one request at a time, 5 s apart
#   BRIDGE_STEP=assess-submit   ASSESS_MODE=ocr|crash [ASSESS_MAX=500]
#   BRIDGE_STEP=assess-collect  ASSESS_SPOOL=<assimilator results directory>; runs score.py
#                               and aggregate.py into <campaign>/out
#
# The gold check (#462, docs/boinc.md#gold-check), under $SHARE_DIR/gold/<GOLD_CAMPAIGN> (one
# campaign per bundle and engine build; same name rule as ASSESS_CAMPAIGN). Copy the bundle tar
# to <campaign>/bundle.tar first.
#   BRIDGE_STEP=gold-prepare    fetches the original files the bundle names, one request at a
#                               time, 5 s apart, to record their sizes, md5s and the crops'
#                               reference counts (prepared.json); serves none of them
#   BRIDGE_STEP=gold-submit     GOLD_ENGINE_IMAGE=sha256:... GOLD_ENGINE_BASE=<its TESSERACT_IMAGE>
#                               [GOLD_ENGINE_VERSION=5.5.0] [GOLD_UNITS=20]
#   BRIDGE_STEP=gold-onboard    GOLD_ENGINE_IMAGE=sha256:... GOLD_ENGINE_BASE=<its TESSERACT_IMAGE>
#                               [GOLD_ENGINE_VERSION=5.5.0] [GOLD_MAX_HOSTS=10]; one host-trust
#                               onboard pass: a gold unit for each untrusted host without one
#                               outstanding, and a fresh one after a failed unit
#   BRIDGE_STEP=gold-mirror-loc stages only the 18 locked v1 LoC JPEGs; no BLN or work creation
#   BRIDGE_STEP=gold-collect    GOLD_SPOOL=<assimilator results directory>
#                               GOLD_LANGUAGE=<calibration language> GOLD_PROFILE=<model profile>;
#                               records bounded native result metadata and validated reports
#   BRIDGE_STEP=gold-trust      GOLD_MAX_CER=<calibrated threshold> GOLD_MANIFEST_SHA256=<challenge hash>
#                              GOLD_LANGUAGE=<source language> GOLD_PROFILE=<model profile>;
#                               writes $SHARE_DIR/gold/trust.json from this campaign's complete reports
#   BRIDGE_STEP=gold-bundle     builds the bundle v1 from its publishers onto the share, checks it
#                               against the committed lock and writes <campaign>/bundle.tar
#                               (gold-bundle.sh; the gold bundle job, bridge image only)
#
# The Surya 2 test (#516, docs/boinc-surya.md), under $SHARE_DIR/surya/<SURYA_CAMPAIGN> (same
# name rule as ASSESS_CAMPAIGN):
#   BRIDGE_STEP=surya-stage     SURYA_PAGES=group:lang:doc_id:batch[+batch...],... (at most
#                               SURYA_MAX, default 20; batches as <name>_ver<NN>), or
#                               SURYA_SAMPLE=<a quality sample.jsonl on the share> with
#                               SURYA_GROUP=<group> (#968: the rows become pages of the group)
#                               SURYA_ENGINE_IMAGE=sha256:... (the usnm_surya worker image)
#                               SURYA_ENGINE_LLAMA=sha256:... SURYA_ENGINE_WEIGHTS=sha256:...
#                               (its USNM_SURYA_LLAMA_IMAGE and USNM_SURYA_WEIGHTS)
#                               gets each page's IIIF JPEG from the scan archive, else from
#                               tile.loc.gov through its batch, one request at a time, 5 s apart
#   BRIDGE_STEP=surya-collect   SURYA_SPOOL=<assimilator results directory> [SURYA_MODES=whole,capped,tiled]
#                               reads the canonical results and writes <campaign>/texts/<doc_id>.surya.json,
#                               the page's text as the ours-surya reader for quality-stage (#968)
#   BRIDGE_STEP=surya-stage-crops QUALITY_CAMPAIGN=<quality campaign> SURYA_ENGINE_IMAGE=...
#                               SURYA_ENGINE_LLAMA=... SURYA_ENGINE_WEIGHTS=... [SURYA_MAX=50 pages]
#                               [SURYA_TIMEOUT_SECS=600]: one unit per crop of every staged
#                               quality page, cut from its page.jpg (#968)
#   BRIDGE_STEP=surya-stage-gold SURYA_GOLD_MANIFEST=<a gold manifest.jsonl on the share>
#                               SURYA_ENGINE_IMAGE=... SURYA_ENGINE_LLAMA=... SURYA_ENGINE_WEIGHTS=...
#                               [SURYA_MAX=50 crops] [SURYA_TIMEOUT_SECS=600]: one unit per
#                               manifest row (docs/ocr-gold-set.md), the box cut from the row's
#                               tile.loc.gov page, one request at a time, 5 s apart, and kept
#                               only if the page matches the row's sha256 (#968)
#   BRIDGE_STEP=surya-gold-hyp  SURYA_SPOOL=<assimilator results directory>: writes
#                               <campaign>/gold/surya.jsonl, one {crop_id, text} per gold crop
#                               with a successful read, for gold.py score --hyp (#968)
#
# assess-stage, surya-stage and quality-stage read the scan archive first when SCAN_ARCHIVE_URL
# is set (https://<scansAccount>.blob.core.windows.net/scans, with the job's identity); none of
# them asks www.loc.gov's item API.
#   BRIDGE_STEP=surya-submit    [SURYA_MAX=20]
#   BRIDGE_STEP=surya-summary   prints the kept results and compared pairs (<project>/surya) as
#                               a Markdown table, and writes it to <campaign>/summary.md
#
# The OCR quality estimate (#522, docs/boinc-quality.md), under $SHARE_DIR/quality/<QUALITY_CAMPAIGN>
# and $SHARE_DIR/qwen/<QWEN_CAMPAIGN> (same name rule as ASSESS_CAMPAIGN):
#   BRIDGE_STEP=quality-stage   [HANDOFF_URL=...] (pulls texts/ into <campaign>/texts)
#                               [QUALITY_LANGS=eng,deu,...] (writes <campaign>/function-words)
#                               [QUALITY_PAGES=group:lang:doc_id:batch[+batch...],...]
#                               [QUALITY_SAMPLE=<path, default <campaign>/sample.jsonl>]
#                               [QUALITY_GROUP=sample] [QUALITY_READERS=loc,as,ours-tesseract]
#                               [SURYA_CAMPAIGN=<surya campaign>] (its texts/ from surya-collect join
#                               each unit as ours-surya when QUALITY_READERS names it, #968)
#                               [QUALITY_PLACES=<campaign>/places.txt]
#                               QUALITY_ENGINE_IMAGE=sha256:... [QUALITY_MAX=100]
#   BRIDGE_STEP=quality-submit  [QUALITY_MAX=500]
#   BRIDGE_STEP=quality-collect QUALITY_SPOOL=<assimilator results directory>
#                               [QWEN_CAMPAIGN=... QWEN_SPOOL=...]
#                               [SURYA_CAMPAIGN=... SURYA_SPOOL=...] (crop units' texts join
#                               crops.parquet as the reader surya, #968)
#                               [QUALITY_CURATED_URL=...] (else writes <campaign>/out)
#   BRIDGE_STEP=quality-summary writes <campaign>/summary.md from <project>/quality
#   BRIDGE_STEP=qwen-stage      QUALITY_CAMPAIGN=<quality campaign>
#                               QWEN_ENGINE_IMAGE=sha256:... QWEN_ENGINE_LLAMA=sha256:...
#                               QWEN_ENGINE_WEIGHTS=sha256:... [QWEN_MAX=20]
#   BRIDGE_STEP=qwen-submit     [QWEN_MAX=20]
#   BRIDGE_STEP=qwen-summary    writes <campaign>/summary.md from <project>/qwen
#   BRIDGE_STEP=qwen-benchmark-stage QWEN_CROPS=<campaign>/test/crops.jsonl
#                               QWEN_ENGINE=<campaign>/engine.json [QWEN_MAX=20]
#                               stages public gold crops; references stay on the share
#   BRIDGE_STEP=qwen-benchmark-prepare [QWEN_SELECTION=transcription-audited-20261015]
#                               resolves a committed audit against the converted gold cache,
#                               verifies private references and stages without submitting work
#   BRIDGE_STEP=qwen-benchmark-collect QWEN_SPOOL=<canonical assimilator directory>
#                               writes coverage and CER/WER to <campaign>/out
#   BRIDGE_STEP=train-prepare TRAIN_CAMPAIGN=<name> TRAIN_WORKER_IMAGE=<public digest>
#                               prepares two bounded permissive, source-disjoint GPU trials
#   BRIDGE_STEP=train-submit TRAIN_CAMPAIGN=<name>
#                               submits prepared trials with write-ahead markers
#   BRIDGE_STEP=train-collect TRAIN_CAMPAIGN=<name>
#                               reports quarantined candidates; never promotes an adapter
#   BRIDGE_STEP=iris-bundle-stage IRIS_CAMPAIGN=<name> IRIS_WORKER_IMAGE=<public digest>
#                               IRIS_ARCHIVE_URL=<release tar> IRIS_ARCHIVE_SHA256=<hex>
#                               [IRIS_SUBSET=legacy/overproof-en-100] [IRIS_BUNDLE_SIZE=16]
#                               bundles the benchmark subset for host 9 (usnm_iris 1.1); no database
#   BRIDGE_STEP=iris-bundle-submit IRIS_CAMPAIGN=<name>   [IRIS_APPLY=1] (else a dry run)
#   BRIDGE_STEP=iris-bundle-collect IRIS_CAMPAIGN=<name>  per-crop CER and drift from 1.0 results
#   BRIDGE_STEP=iris-bundle-cancel IRIS_CAMPAIGN=<name>   [IRIS_APPLY=1] cancels the workunits of a
#                               batch-105 campaign, which the scheduler can never send (dry run without)
#
# The remote hand-off (#469, docs/boinc.md#one-page-from-the-scan-cache), through the
# hand-off container HANDOFF_URL=https://<account>.blob.core.windows.net/boinc-handoff, read
# and written with this job's identity:
#   BRIDGE_STEP=submit-remote   JOB_ID; copies the exported job from the container's
#                               dispatch/jobs/<id>/ into $HANDOFF_DIR (checking the page's
#                               size and sha256 against the request) and submits it. A job
#                               already past reserved is left. LoC is never asked.
#                               JOB_IDS=<id>[,<id>...] (up to 100) does a tick's jobs in one
#                               execution, stopping at the first failure (#932).
#   BRIDGE_STEP=spool-out       JOB_ID HANDOFF_SPOOL=<assimilator results directory under
#                               $PROJ/results>; writes the job's spool files and the canonical
#                               result's host id to spool/<id>/, and $SHARE_DIR/gold/trust.json
#                               (when there) to gold/trust.json. HANDOFF_SPOOLS=<id>=<spool>[,...]
#                               (up to 100) does a tick's results in one execution (#932).
#
# Registering the apps after init (docs/boinc.md#register-the-apps):
#   BRIDGE_STEP=register-apps   copies this image's project.xml into the project and runs
#                               bin/xadd, which adds the apps and platforms the database lacks
#                               and leaves the others (safe to repeat)
#   BRIDGE_STEP=work-volume     writes one JSON line of aggregate workunit, result, active-user
#                               and active-host counts to stdout; safe to repeat
#   BRIDGE_STEP=batch-inventory BATCH_NONCE=<32 hex> HANDOFF_URL=<container URL>; writes the
#                               read-only queue/host/journal/spool inventory to scheduler/
#
# Start a step with scripts/start-boinc-bridge.sh, which sets these on the job: an
# `az containerapp job start --env-vars` override drops the job's own settings and the share.
#
# The step and its settings (JOB_ID included) are read from the environment. Each run's are
# written into the job's own template by scripts/start-boinc-bridge.sh, which then starts it
# with no execution override (design.md section 9, question 4). UNVERIFIED against a live job.
set -euo pipefail
# shellcheck source=lib.sh disable=SC1091
. /usr/local/lib/usnm-boinc/lib.sh
[ -e "$PROJ/.usnm-initialised" ] || fail "project is not initialised"
step="${BRIDGE_STEP:-submit}"
# A tick's jobs in one execution (#932): JOB_IDS for submit-remote, HANDOFF_SPOOLS for
# spool-out. Each job runs this script's single-job path again, so every check is the same.
# submit-remote stops at the first failure (the next tick reconciles that job before any other
# submission); spool-out goes on and fails at the end, so one bad result holds back no other.
# The secrets are read from Key Vault once for the whole batch (#958): the per-job runs reuse the
# files in tmpfs (BRIDGE_SECRETS_READY), and fetch them again only if one is missing. Submission
# needs every runtime secret and the rendered config; spool-out only the database password.
# runtime_password: the runtime database password into $SECRETS/db-runtime, reusing the batch's.
runtime_password() {
  if [ "${BRIDGE_SECRETS_READY:-}" = 1 ] && [ -s "$SECRETS/db-runtime" ]; then
    return 0
  fi
  mkdir -p "$SECRETS"; chmod 700 "$SECRETS"
  kv_get mysql-runtime-password "$SECRETS/db-runtime"
}
if [ "$step" = submit-remote ] && [ -n "${JOB_IDS:-}" ]; then
  fetch_secrets
  render_config
  export BRIDGE_SECRETS_READY=1
elif [ "$step" = spool-out ] && [ -n "${HANDOFF_SPOOLS:-}" ]; then
  runtime_password
  export BRIDGE_SECRETS_READY=1
fi
if [ "$step" = submit-remote ] && [ -n "${JOB_IDS:-}" ]; then
  [ -z "${JOB_ID:-}" ] || fail "set JOB_ID or JOB_IDS, not both"
  [[ $JOB_IDS =~ ^[0-9a-f]{64}(,[0-9a-f]{64}){0,99}$ ]] || fail "JOB_IDS is 1 to 100 comma-separated job ids"
  IFS=, read -r -a batch_ids <<< "$JOB_IDS"
  for id in "${batch_ids[@]}"; do
    log "submit-remote $id"
    JOB_ID=$id JOB_IDS="" "${BASH_SOURCE[0]}" || fail "submit-remote stopped at $id; the jobs after it were not submitted"
  done
  exit 0
fi
if [ "$step" = spool-out ] && [ -n "${HANDOFF_SPOOLS:-}" ]; then
  [ -z "${JOB_ID:-}" ] && [ -z "${HANDOFF_SPOOL:-}" ] || fail "set JOB_ID and HANDOFF_SPOOL, or HANDOFF_SPOOLS, not both"
  IFS=, read -r -a batch_spools <<< "$HANDOFF_SPOOLS"
  [ "${#batch_spools[@]}" -le 100 ] || fail "HANDOFF_SPOOLS holds at most 100 jobs"
  failed=0
  for pair in "${batch_spools[@]}"; do
    [[ $pair =~ ^([0-9a-f]{64})=(/[A-Za-z0-9_./-]+)$ ]] || fail "HANDOFF_SPOOLS is <job id>=<spool>[,...]"
    log "spool-out ${BASH_REMATCH[1]}"
    JOB_ID=${BASH_REMATCH[1]} HANDOFF_SPOOL=${BASH_REMATCH[2]} HANDOFF_SPOOLS="" "${BASH_SOURCE[0]}" ||
      { log "spool-out failed for ${BASH_REMATCH[1]}"; failed=$((failed + 1)); }
  done
  [ "$failed" -eq 0 ] || fail "spool-out failed for $failed of ${#batch_spools[@]} jobs"
  exit 0
fi
# End of the batch modes (#932).
TRAIN=""
if [[ "$step" == train-* ]]; then
  [[ "${TRAIN_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "TRAIN_CAMPAIGN must be 1-41 of [a-z0-9-]"
  TRAIN="$SHARE_DIR/training/$TRAIN_CAMPAIGN"
fi
ASSESS=""
if [[ "$step" == assess-* ]]; then
  [[ "${ASSESS_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "ASSESS_CAMPAIGN must be 1-41 of [a-z0-9-]"
  ASSESS="$SHARE_DIR/assess/$ASSESS_CAMPAIGN"
fi
SURYA=""
if [[ "$step" == surya-* ]]; then
  [[ "${SURYA_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "SURYA_CAMPAIGN must be 1-41 of [a-z0-9-]"
  SURYA="$SHARE_DIR/surya/$SURYA_CAMPAIGN"
fi
QUALITY=""
if [[ "$step" == quality-* ]]; then
  [[ "${QUALITY_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "QUALITY_CAMPAIGN must be 1-41 of [a-z0-9-]"
  QUALITY="$SHARE_DIR/quality/$QUALITY_CAMPAIGN"
fi
QWEN=""
if [[ "$step" == qwen-* ]]; then
  [[ "${QWEN_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "QWEN_CAMPAIGN must be 1-41 of [a-z0-9-]"
  QWEN="$SHARE_DIR/qwen/$QWEN_CAMPAIGN"
fi
IRIS=""
if [[ "$step" == iris-bundle-* ]]; then
  [[ "${IRIS_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "IRIS_CAMPAIGN must be 1-41 of [a-z0-9-]"
  IRIS="$SHARE_DIR/iris-bundle/$IRIS_CAMPAIGN"
fi
GOLD=""
if [[ "$step" == gold-* ]]; then
  [[ "${GOLD_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "GOLD_CAMPAIGN must be 1-41 of [a-z0-9-]"
  GOLD="$SHARE_DIR/gold/$GOLD_CAMPAIGN"
fi
assess_mode() {
  case "${ASSESS_MODE:-}" in ocr|crash) ;; *) fail "ASSESS_MODE must be ocr or crash" ;; esac
}
case "$step" in
  train-prepare)
    exec python3 "$TEMPLATES/train/campaign.py" prepare \
      --cache "$SHARE_DIR/gold/build-v1/out" --root "$TRAIN" \
      --worker-image "${TRAIN_WORKER_IMAGE:?set TRAIN_WORKER_IMAGE}" ;;
  train-collect)
    exec python3 "$TEMPLATES/train/campaign.py" collect --root "$TRAIN" --seen "$PROJ/qwen-train/seen" ;;
  iris-bundle-stage)
    # No database or secrets: downloads the pinned public benchmark release and writes the share.
    [[ "${IRIS_SUBSET:-legacy/overproof-en-100}" =~ ^[A-Za-z0-9/_.-]+$ ]] || fail "bad IRIS_SUBSET"
    exec python3 "$TEMPLATES/iris/iris-bundle-campaign.py" stage --out "$IRIS" \
      --archive-url "${IRIS_ARCHIVE_URL:?set IRIS_ARCHIVE_URL}" \
      --archive-sha256 "${IRIS_ARCHIVE_SHA256:?set IRIS_ARCHIVE_SHA256}" \
      --subset "${IRIS_SUBSET:-legacy/overproof-en-100}" \
      --worker-image "${IRIS_WORKER_IMAGE:?set IRIS_WORKER_IMAGE}" \
      --bundle-size "${IRIS_BUNDLE_SIZE:-16}" ;;
  iris-bundle-collect)
    exec python3 "$TEMPLATES/iris/iris-bundle-campaign.py" collect --root "$IRIS" --seen "$PROJ/iris/seen" \
      --v1-seen "$PROJ/iris/seen" ;;
  assess-stage)
    # No database or secrets: this step only reads the scan archive and LoC and writes the share.
    assess_mode
    exec usnm-boinc assess stage --sample "$ASSESS/sample-$ASSESS_MODE.jsonl" --root "$ASSESS" \
      --mode "$ASSESS_MODE" --engine-image "${ASSESS_ENGINE_IMAGE:?set ASSESS_ENGINE_IMAGE}" \
      --engine-base "${ASSESS_ENGINE_BASE:?set ASSESS_ENGINE_BASE}" \
      --engine-version "${ASSESS_ENGINE_VERSION:-5.5.0}" \
      --max-pages "${ASSESS_MAX:-100}" --batch-cache "$ASSESS/batch-cache" \
      ${SCAN_ARCHIVE_URL:+--scan-archive "$SCAN_ARCHIVE_URL"} ;;
  assess-collect)
    usnm-boinc assess collect --root "$ASSESS" --spool "${ASSESS_SPOOL:?set ASSESS_SPOOL}" --out "$ASSESS/out"
    python3 /usr/local/lib/usnm-boinc/assess/score.py --sample "$ASSESS/out/sample.jsonl" \
      --dir "$ASSESS/out" --results "$ASSESS/out/results.jsonl" --out "$ASSESS/out/scores.jsonl"
    python3 /usr/local/lib/usnm-boinc/assess/aggregate.py --scores "$ASSESS/out/scores.jsonl" \
      --results "$ASSESS/out/results.jsonl" --crash "$ASSESS/out/crash.jsonl" > "$ASSESS/out/tables.md"
    exit 0 ;;
  surya-benchmark-stage)
    [[ "${SURYA_BENCHMARK_SHA256:-}" =~ ^[a-f0-9]{64}$ ]] || fail "SURYA_BENCHMARK_SHA256 must pin inputs.json"
    exec usnm-boinc surya benchmark-stage --manifest "$SHARE_DIR/surya-inputs/$SURYA_CAMPAIGN/inputs.json" \
      --manifest-sha256 "$SURYA_BENCHMARK_SHA256" --root "$SURYA" \
      --engine-image "${SURYA_ENGINE_IMAGE:?set SURYA_ENGINE_IMAGE}" \
      --engine-llama "${SURYA_ENGINE_LLAMA:?set SURYA_ENGINE_LLAMA}" \
      --engine-weights "${SURYA_ENGINE_WEIGHTS:?set SURYA_ENGINE_WEIGHTS}" ;;
  surya-stage)
    # No database or secrets: this step only reads the scan archive and LoC and writes the share.
    page_args=()
    if [ -n "${SURYA_PAGES:-}" ]; then
      [[ "$SURYA_PAGES" =~ ^[a-z0-9:_,+-]+$ ]] || fail "SURYA_PAGES must be group:lang:doc_id:batch[+batch...],..."
      IFS=, read -r -a pages <<< "$SURYA_PAGES"
      for p in "${pages[@]}"; do page_args+=(--page "$p"); done
    fi
    if [ -n "${SURYA_SAMPLE:-}" ]; then
      [[ "$SURYA_SAMPLE" == "$SHARE_DIR"/* && -f "$SURYA_SAMPLE" ]] || fail "SURYA_SAMPLE must be a file under $SHARE_DIR"
      [[ "${SURYA_GROUP:-}" =~ ^[a-z0-9][a-z0-9-]{0,31}$ ]] || fail "SURYA_GROUP must be 1-32 of [a-z0-9-]"
      page_args+=(--sample "$SURYA_SAMPLE" --group "$SURYA_GROUP")
    fi
    [ "${#page_args[@]}" -gt 0 ] || fail "set SURYA_PAGES or SURYA_SAMPLE"
    exec usnm-boinc surya stage "${page_args[@]}" --root "$SURYA" \
      --engine-image "${SURYA_ENGINE_IMAGE:?set SURYA_ENGINE_IMAGE}" \
      --engine-llama "${SURYA_ENGINE_LLAMA:?set SURYA_ENGINE_LLAMA}" \
      --engine-weights "${SURYA_ENGINE_WEIGHTS:?set SURYA_ENGINE_WEIGHTS}" \
      --max-pages "${SURYA_MAX:-20}" --batch-cache "$SURYA/batch-cache" \
      ${SCAN_ARCHIVE_URL:+--scan-archive "$SCAN_ARCHIVE_URL"} ;;
  surya-stage-crops)
    # No database, secrets or network: cuts the staged quality pages' crops on the share.
    [[ "${QUALITY_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "QUALITY_CAMPAIGN must be 1-41 of [a-z0-9-]"
    [[ "${SURYA_TIMEOUT_SECS:-600}" =~ ^[0-9]{2,5}$ ]] || fail "SURYA_TIMEOUT_SECS must be seconds"
    exec usnm-boinc surya stage-crops --quality-root "$SHARE_DIR/quality/$QUALITY_CAMPAIGN" --root "$SURYA" \
      --engine-image "${SURYA_ENGINE_IMAGE:?set SURYA_ENGINE_IMAGE}" \
      --engine-llama "${SURYA_ENGINE_LLAMA:?set SURYA_ENGINE_LLAMA}" \
      --engine-weights "${SURYA_ENGINE_WEIGHTS:?set SURYA_ENGINE_WEIGHTS}" \
      --timeout-secs "${SURYA_TIMEOUT_SECS:-600}" --max-pages "${SURYA_MAX:-50}" ;;
  surya-stage-gold)
    # No database or secrets: this step only reads LoC and writes the share.
    [[ "${SURYA_GOLD_MANIFEST:-}" == "$SHARE_DIR"/* && -f "$SURYA_GOLD_MANIFEST" ]] || fail "SURYA_GOLD_MANIFEST must be a file under $SHARE_DIR"
    [[ "${SURYA_TIMEOUT_SECS:-600}" =~ ^[0-9]{2,5}$ ]] || fail "SURYA_TIMEOUT_SECS must be seconds"
    [[ "${SURYA_MAX:-50}" =~ ^[0-9]{1,4}$ ]] || fail "SURYA_MAX must be a number"
    exec usnm-boinc surya stage-gold --manifest "$SURYA_GOLD_MANIFEST" --root "$SURYA" \
      --engine-image "${SURYA_ENGINE_IMAGE:?set SURYA_ENGINE_IMAGE}" \
      --engine-llama "${SURYA_ENGINE_LLAMA:?set SURYA_ENGINE_LLAMA}" \
      --engine-weights "${SURYA_ENGINE_WEIGHTS:?set SURYA_ENGINE_WEIGHTS}" \
      --timeout-secs "${SURYA_TIMEOUT_SECS:-600}" --max-crops "${SURYA_MAX:-50}" ;;
  surya-gold-hyp)
    # No database or secrets: reads the assimilated results and writes the share.
    exec usnm-boinc surya gold-hyp --root "$SURYA" --spool "${SURYA_SPOOL:?set SURYA_SPOOL}" \
      --out "$SURYA/gold/surya.jsonl" ;;
  surya-collect)
    # No database or secrets: reads the assimilated results and writes the share.
    [[ "${SURYA_MODES:-whole,capped,tiled}" =~ ^[a-z,]+$ ]] || fail "SURYA_MODES must be comma-separated modes"
    exec usnm-boinc surya collect --root "$SURYA" --spool "${SURYA_SPOOL:?set SURYA_SPOOL}" \
      --texts-dir "$SURYA/texts" --modes "${SURYA_MODES:-whole,capped,tiled}" ;;
  surya-summary)
    # No database or secrets: reads what the validator kept and writes the campaign's summary.
    usnm-boinc surya summary --root "$SURYA" --seen "$PROJ/surya/seen" --pairs "$PROJ/surya/pairs.jsonl" \
      | tee "$SURYA/summary.md"
    exit 0 ;;
  quality-stage)
    # No database or secrets: reads the hand-off texts, scan archive and LoC, writes the share.
    mkdir -p "$QUALITY/texts" "$QUALITY/function-words"
    if [ -n "${HANDOFF_URL:-}" ]; then
      [[ "$HANDOFF_URL" =~ ^https://[a-z0-9]{3,24}\.blob\.core\.windows\.net/boinc-handoff$ ]] ||
        fail "HANDOFF_URL must be https://<account>.blob.core.windows.net/boinc-handoff"
      usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix texts --to "$QUALITY/texts"
    fi
    if [ -n "${QUALITY_LANGS:-}" ]; then
      [[ "$QUALITY_LANGS" =~ ^[a-z,]+$ ]] || fail "QUALITY_LANGS must be comma-separated ISO 639-3 codes"
      lang_args=()
      IFS=, read -r -a langs <<< "$QUALITY_LANGS"
      for l in "${langs[@]}"; do lang_args+=(--lang "$l"); done
      usnm-boinc quality function-words "${lang_args[@]}" --out-dir "$QUALITY/function-words"
    fi
    page_args=()
    if [ -n "${QUALITY_PAGES:-}" ]; then
      [[ "$QUALITY_PAGES" =~ ^[a-z0-9:_,+-]+$ ]] || fail "QUALITY_PAGES must be group:lang:doc_id:batch[+batch...],..."
      IFS=, read -r -a pages <<< "$QUALITY_PAGES"
      for p in "${pages[@]}"; do page_args+=(--page "$p"); done
    fi
    sample="${QUALITY_SAMPLE:-}"
    if [ -n "$sample" ] && [[ "$sample" != /* ]] && [ ! -f "$sample" ] && [ -n "${HANDOFF_URL:-}" ]; then
      usnm-boinc handoff get --from-url "$HANDOFF_URL" --prefix quality-samples --to "$QUALITY/samples"
      if [ -f "$QUALITY/samples/$sample" ]; then
        sample="$QUALITY/samples/$sample"
      fi
      if [ ! -f "${QUALITY_PLACES:-$QUALITY/places.txt}" ] && [ -f "$QUALITY/samples/places.txt" ]; then
        cp "$QUALITY/samples/places.txt" "$QUALITY/places.txt"
      fi
    fi
    if [ -z "$sample" ] && [ -f "$QUALITY/sample.jsonl" ]; then
      sample="$QUALITY/sample.jsonl"
    fi
    if [ -n "$sample" ]; then
      [[ "${QUALITY_GROUP:-sample}" =~ ^[a-z0-9_-]+$ ]] || fail "QUALITY_GROUP must be [a-z0-9_-]+"
      page_args+=(--sample "$sample" --group "${QUALITY_GROUP:-sample}")
    fi
    [ "${#page_args[@]}" -gt 0 ] || fail "set QUALITY_PAGES or QUALITY_SAMPLE (or place $QUALITY/sample.jsonl)"
    [[ "${QUALITY_READERS:-loc,as,ours-tesseract}" =~ ^[a-z0-9_,-]+$ ]] ||
      fail "QUALITY_READERS must be comma-separated reader names"
    [[ "${QUALITY_BUNDLE_SIZE:-300}" =~ ^[1-9][0-9]*$ ]] || fail "QUALITY_BUNDLE_SIZE must be a positive integer"
    surya_args=()
    if [ -n "${SURYA_CAMPAIGN:-}" ]; then
      [[ "$SURYA_CAMPAIGN" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "SURYA_CAMPAIGN must be 1-41 of [a-z0-9-]"
      surya_args=(--surya-texts "$SHARE_DIR/surya/$SURYA_CAMPAIGN/texts")
    fi
    exec usnm-boinc quality stage "${page_args[@]}" --root "$QUALITY" \
      --texts-dir "$QUALITY/texts" --scans-dir "${QUALITY_SCANS_DIR:-$QUALITY/texts}" "${surya_args[@]}" \
      --places "${QUALITY_PLACES:-$QUALITY/places.txt}" \
      --function-words-dir "$QUALITY/function-words" \
      --readers "${QUALITY_READERS:-loc,as,ours-tesseract}" \
      --engine-image "${QUALITY_ENGINE_IMAGE:?set QUALITY_ENGINE_IMAGE}" \
      --max-pages "${QUALITY_MAX:-100}" --bundle-size "${QUALITY_BUNDLE_SIZE:-300}" \
      --batch-cache "$QUALITY/batch-cache" \
      ${SCAN_ARCHIVE_URL:+--scan-archive "$SCAN_ARCHIVE_URL"} ;;
  quality-collect)
    qwen_root="$QUALITY/no-qwen"
    qwen_spool="${QWEN_SPOOL:-$QUALITY/no-qwen}"
    if [ -n "${QWEN_CAMPAIGN:-}" ]; then
      [[ "$QWEN_CAMPAIGN" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "QWEN_CAMPAIGN must be 1-41 of [a-z0-9-]"
      qwen_root="$SHARE_DIR/qwen/$QWEN_CAMPAIGN"
      qwen_spool="${QWEN_SPOOL:?set QWEN_SPOOL}"
    fi
    surya_args=()
    if [ -n "${SURYA_CAMPAIGN:-}" ]; then
      [[ "$SURYA_CAMPAIGN" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "SURYA_CAMPAIGN must be 1-41 of [a-z0-9-]"
      surya_args=(--surya-root "$SHARE_DIR/surya/$SURYA_CAMPAIGN" --surya-spool "${SURYA_SPOOL:?set SURYA_SPOOL}")
    fi
    out_args=(--out-dir "$QUALITY/out")
    [ -n "${QUALITY_CURATED_URL:-}" ] && out_args=(--curated-url "$QUALITY_CURATED_URL")
    exec usnm-boinc quality collect --campaign "$QUALITY_CAMPAIGN" --root "$QUALITY" \
      --spool "${QUALITY_SPOOL:?set QUALITY_SPOOL}" --qwen-root "$qwen_root" --qwen-spool "$qwen_spool" \
      "${surya_args[@]}" "${out_args[@]}" ;;
  quality-summary)
    usnm-boinc quality summary --root "$QUALITY" --seen "$PROJ/quality/seen" --pairs "$PROJ/quality/pairs.jsonl" \
      | tee "$QUALITY/summary.md"
    exit 0 ;;
  qwen-stage)
    [[ "${QUALITY_CAMPAIGN:-}" =~ ^[a-z0-9][a-z0-9-]{0,40}$ ]] || fail "QUALITY_CAMPAIGN must be 1-41 of [a-z0-9-]"
    exec usnm-boinc qwen stage --quality-root "$SHARE_DIR/quality/$QUALITY_CAMPAIGN" --root "$QWEN" \
      --engine-image "${QWEN_ENGINE_IMAGE:?set QWEN_ENGINE_IMAGE}" \
      --engine-llama "${QWEN_ENGINE_LLAMA:?set QWEN_ENGINE_LLAMA}" \
      --engine-weights "${QWEN_ENGINE_WEIGHTS:?set QWEN_ENGINE_WEIGHTS}" \
      --max-pages "${QWEN_MAX:-20}" ;;
  qwen-benchmark-prepare)
    selection="${QWEN_SELECTION:-transcription-audited-20261015}"
    [[ "$selection" =~ ^[a-z0-9][a-z0-9-]{0,60}$ ]] || fail "invalid reviewed selection name"
    python3 /usr/local/lib/usnm-boinc/gold/scripts/ocr-survey/transcription.py \
      --selection "/usr/local/lib/usnm-boinc/gold/selections/$selection.json" \
      --cache "$SHARE_DIR/gold/build-v1/out" --out "$QWEN"
    exec usnm-boinc qwen benchmark-stage --root "$QWEN" \
      --crops "$QWEN/test/crops.jsonl" --engine "$QWEN/engine.json" --max-crops "${QWEN_MAX:-20}" ;;
  qwen-benchmark-stage)
    exec usnm-boinc qwen benchmark-stage --root "$QWEN" \
      --crops "${QWEN_CROPS:-$QWEN/test/crops.jsonl}" \
      --engine "${QWEN_ENGINE:-$QWEN/engine.json}" --max-crops "${QWEN_MAX:-20}" ;;
  qwen-benchmark-collect)
    exec usnm-boinc qwen benchmark-collect --root "$QWEN" \
      --spool "${QWEN_SPOOL:?set QWEN_SPOOL}" --out "$QWEN/out" ;;
  qwen-summary)
    usnm-boinc qwen summary --root "$QWEN" --seen "$PROJ/qwen/seen" --pairs "$PROJ/qwen/pairs.jsonl" \
      | tee "$QWEN/summary.md"
    exit 0 ;;
  gold-prepare)
    # No database or secrets: this step only talks to the publishers and writes the share.
    exec usnm-boinc gold prepare --bundle "$GOLD/bundle.tar" --root "$GOLD" ;;
  gold-mirror-loc)
    # No database, secrets, publisher fetches, bundle or BLN600 reads.
    python3 "$TEMPLATES/gold-mirror-config.py" --project "$PROJ" \
      --config "$SECRETS/config.xml" --template "$TEMPLATES/config.xml.template"
    exec usnm-boinc gold mirror-loc --root "$GOLD" --project "$PROJ" ;;
  gold-bundle)
    # No database or secrets: this step only talks to the datasets' publishers and writes the share.
    [ -f /usr/local/lib/usnm-boinc/gold-bundle.sh ] || fail "gold-bundle needs the bridge image"
    exec bash /usr/local/lib/usnm-boinc/gold-bundle.sh --share "$SHARE_DIR" --campaign "$GOLD_CAMPAIGN" ;;
  gold-trust)
    # No database or secrets: reads this campaign's reports and writes the share's trust list.
    exec usnm-boinc gold trust --reports "$GOLD/out/reports.jsonl" \
      --outcomes "$GOLD/out/native-outcomes.jsonl" \
      --max-cer "${GOLD_MAX_CER:?set GOLD_MAX_CER from campaign calibration}" \
      --manifest-sha256 "${GOLD_MANIFEST_SHA256:?set GOLD_MANIFEST_SHA256 to the calibrated challenge}" \
      --language "${GOLD_LANGUAGE:?set GOLD_LANGUAGE to the calibrated source language}" \
      --profile "${GOLD_PROFILE:?set GOLD_PROFILE to the calibrated model profile}" \
      --out "$SHARE_DIR/gold/trust.json" ;;
esac
if [ "$step" != assess-submit ] && [ "$step" != gold-submit ] && [ "$step" != surya-submit ] &&
  [ "$step" != quality-submit ] && [ "$step" != qwen-submit ] && [ "$step" != train-submit ] &&
  [ "$step" != register-apps ] && [ "$step" != work-volume ] && [ "$step" != batch-inventory ] &&
  [ "$step" != gold-onboard ] && [ "$step" != gold-collect ] && [ "$step" != iris-bundle-submit ] && [ "$step" != iris-bundle-cancel ]; then
  : "${JOB_ID:?set JOB_ID}" "${HANDOFF_DIR:?}"
  [[ "$JOB_ID" =~ ^[A-Za-z0-9_-]+$ ]] || fail "JOB_ID has characters outside [A-Za-z0-9_-]"
fi
if [ "$step" = submit-remote ] || [ "$step" = spool-out ]; then
  [[ "${HANDOFF_URL:-}" =~ ^https://[a-z0-9]{3,24}\.blob\.core\.windows\.net/boinc-handoff$ ]] ||
    fail "HANDOFF_URL must be https://<account>.blob.core.windows.net/boinc-handoff"
fi
if [ "$step" = spool-out ]; then
  # Reads the spool and, for the host id, the database; writes only the hand-off container.
  spool="${HANDOFF_SPOOL:?set HANDOFF_SPOOL}"
  case "$spool" in "$PROJ"/results/*) ;; *) fail "HANDOFF_SPOOL must be under $PROJ/results" ;; esac
  [[ "$spool" != *..* ]] || fail "HANDOFF_SPOOL must not contain .."
  host_args=()
  mkdir -p "$SECRETS"; chmod 700 "$SECRETS"
  mycnf="$SECRETS/mysql-runtime.cnf"
  trap 'rm -f "$mycnf"' EXIT
  runtime_password
  (umask 077; { echo '[client]'; echo "host=$DB_HOST"; echo "user=$DB_RUNTIME_USER"; echo "password=$(cat "$SECRETS/db-runtime")"; } > "$mycnf")
  # The canonical result's host (BOINC's result.hostid); JOB_ID is [A-Za-z0-9_-] (above).
  # UNVERIFIED against a live database.
  if host=$(mysql --defaults-extra-file="$mycnf" --ssl --ssl-verify-server-cert \
      --ssl-ca=/usr/local/share/usnm/mysql-ca.pem --batch --skip-column-names "$DB_NAME" \
      -e "SELECT r.hostid FROM workunit w JOIN result r ON r.id = w.canonical_resultid WHERE w.name = 'usnm_$JOB_ID'"); then
    if [[ "$host" =~ ^[0-9]+$ ]]; then host_args=(--host-id "$host"); else log "no canonical result host for usnm_$JOB_ID yet"; fi
  else
    log "could not read the canonical result's host id; the node must find it another way"
  fi
  trust_args=()
  [ -f "$SHARE_DIR/gold/trust.json" ] && trust_args=(--trust "$SHARE_DIR/gold/trust.json")
  usnm-boinc handoff put-spool --to-url "$HANDOFF_URL" --spool "$spool" --job "$JOB_ID" \
    ${host_args[@]+"${host_args[@]}"} ${trust_args[@]+"${trust_args[@]}"}
  exit 0
fi
# share_readable <stamp> <dir>...: makes files a step created since <stamp> readable by the
# server's boincadm group. Skipped on SMB/CIFS: the share's mount options fix the owner, group
# and modes, so chgrp/chmod change nothing there, and walking its download tree takes minutes.
share_readable() {
  local stamp=$1 d
  shift
  for d in "$@"; do
    [ -d "$d" ] || continue
    case "$(stat -f -c %T "$d" 2>/dev/null)" in smb2 | cifs) continue ;; esac
    find "$d" -newer "$stamp" -exec chgrp boincadm {} + -exec chmod g+rX,o+rX {} + 2>/dev/null || true
  done
}
# A private MySQL defaults file for the runtime database user, removed on exit.
write_mycnf() {
  mkdir -p "$SECRETS"; chmod 700 "$SECRETS"
  mycnf="$SECRETS/mysql-runtime.cnf"
  # In a batch the password file is the batch's, kept for the jobs after this one.
  if [ "${BRIDGE_SECRETS_READY:-}" = 1 ]; then trap 'rm -f "$mycnf"' EXIT; else trap 'rm -f "$mycnf" "$SECRETS/db-runtime"' EXIT; fi
  runtime_password
  (umask 077; python3 - "$mycnf" "$SECRETS/db-runtime" <<'PY'
import os
import sys
from pathlib import Path

def quote(value):
    for before, after in (("\\", "\\\\"), ('"', '\\"'), ("\n", "\\n"), ("\r", "\\r"), ("\t", "\\t")):
        value = value.replace(before, after)
    return '"' + value + '"'

values = {"host": os.environ["DB_HOST"], "user": os.environ["DB_RUNTIME_USER"],
          "password": Path(sys.argv[2]).read_bytes().decode("utf-8").removesuffix("\n")}
Path(sys.argv[1]).write_text("[client]\n" + "".join(f"{key}={quote(value)}\n" for key, value in values.items()))
PY
  )
}
if [ "$step" = work-volume ] || [ "$step" = batch-inventory ]; then
  write_mycnf
  if [ "$step" = batch-inventory ]; then
    [[ "${HANDOFF_URL:-}" =~ ^https://[a-z0-9]{3,24}\.blob\.core\.windows\.net/boinc-handoff$ ]] ||
      fail "HANDOFF_URL must name the hand-off container"
    [[ "${BATCH_NONCE:-}" =~ ^[0-9a-f]{32}$ ]] || fail "BATCH_NONCE must be 32 hex"
    mkdir -p "$HANDOFF_DIR/scheduler"
    python3 "$TEMPLATES/batch-inventory.py" --mysql-defaults-file "$mycnf" --database "$DB_NAME" \
      --root "$HANDOFF_DIR" --results "$PROJ/results" --nonce "$BATCH_NONCE" \
      --out "$HANDOFF_DIR/scheduler/inventory.json"
    usnm-boinc handoff put --from "$HANDOFF_DIR/scheduler" --to-url "$HANDOFF_URL" --prefix scheduler
  else
    python3 "$TEMPLATES/work-volume.py" --mysql-defaults-file "$mycnf" --database "$DB_NAME"
  fi
  exit 0
fi
if [ "${BRIDGE_SECRETS_READY:-}" = 1 ] && secrets_present; then
  log "reusing this batch's secrets"
else
  fetch_secrets
fi
render_config
umask 0002
case "$step" in
  iris-bundle-cancel)
    apply=()
    [ "${IRIS_APPLY:-}" = 1 ] && apply=(--apply)
    exec python3 "$TEMPLATES/iris/iris-bundle-campaign.py" cancel-stranded --root "$IRIS" --project "$PROJ" \
      ${apply[@]+"${apply[@]}"} ;;
  iris-bundle-submit)
    for template in usnm_iris_bundle_in usnm_iris_bundle_out; do
      cp "$TEMPLATES/$template" "$PROJ/templates/$template"
      chmod 0664 "$PROJ/templates/$template"
    done
    apply=()
    [ "${IRIS_APPLY:-}" = 1 ] && apply=(--apply)
    stamp="$(mktemp)"
    rc=0
    python3 "$TEMPLATES/iris/iris-bundle-campaign.py" submit --root "$IRIS" --project "$PROJ" \
      ${apply[@]+"${apply[@]}"} || rc=$?
    share_readable "$stamp" "$PROJ/download" "$PROJ/templates" "$PROJ/iris"
    rm -f "$stamp"
    exit "$rc" ;;
  train-submit)
    for template in usnm_qwen_train_in usnm_qwen_train_out; do
      cp "$TEMPLATES/$template" "$PROJ/templates/$template"
      chmod 0664 "$PROJ/templates/$template"
    done
    exec python3 "$TEMPLATES/train/campaign.py" submit --root "$TRAIN" --project "$PROJ" ;;
  assess-submit)
    assess_mode
    stamp="$(mktemp)"
    rc=0
    usnm-boinc assess submit --root "$ASSESS" --project "$PROJ" --mode "$ASSESS_MODE" \
      --max-units "${ASSESS_MAX:-500}" || rc=$?
    share_readable "$stamp" "$PROJ/download"
    rm -f "$stamp"
    exit "$rc" ;;
  surya-submit)
    stamp="$(mktemp)"
    rc=0
    target_args=()
    if [ -n "${SURYA_TARGET_HOST:-}" ]; then
      [[ "$SURYA_TARGET_HOST" =~ ^[1-9][0-9]*$ ]] || fail "SURYA_TARGET_HOST must be positive"
      target_args=(--target-host "$SURYA_TARGET_HOST")
      case "${SURYA_GPU_PROBE:-0}" in
        0) ;;
        1) target_args+=(--gpu-probe) ;;
        *) fail "SURYA_GPU_PROBE must be 0 or 1" ;;
      esac
    elif [ "${SURYA_GPU_PROBE:-0}" != 0 ]; then
      fail "SURYA_GPU_PROBE requires SURYA_TARGET_HOST"
    fi
    usnm-boinc surya submit --root "$SURYA" --project "$PROJ" --max-units "${SURYA_MAX:-20}" "${target_args[@]}" || rc=$?
    share_readable "$stamp" "$PROJ/download" "$PROJ/templates"
    # The status page caches its counts for a minute; the controller's next tick reads the
    # queue these units joined (as quality-submit and qwen-submit do).
    rm -f "$stamp" "$PROJ/html/cache/f2/server_status.php_job_status" 2>/dev/null || true
    exit "$rc" ;;
  quality-submit)
    stamp="$(mktemp)"
    rc=0
    usnm-boinc quality submit --root "$QUALITY" --project "$PROJ" --max-units "${QUALITY_MAX:-500}" || rc=$?
    share_readable "$stamp" "$PROJ/download" "$PROJ/templates"
    rm -f "$stamp" "$PROJ/html/cache/f2/server_status.php_job_status" 2>/dev/null || true
    exit "$rc" ;;
  qwen-submit)
    stamp="$(mktemp)"
    rc=0
    usnm-boinc qwen submit --root "$QWEN" --project "$PROJ" --target "${QWEN_TARGET:-container}" --max-units "${QWEN_MAX:-20}" || rc=$?
    share_readable "$stamp" "$PROJ/download" "$PROJ/templates"
    rm -f "$stamp" "$PROJ/html/cache/f2/server_status.php_job_status" 2>/dev/null || true
    exit "$rc" ;;
  gold-collect)
    write_mycnf
    usnm-boinc gold collect --root "$GOLD" --project "$PROJ" \
      --spool "${GOLD_SPOOL:?set GOLD_SPOOL}" --out "$GOLD/out" \
      --mysql-defaults-file "$mycnf" --mysql-ca-file /usr/local/share/usnm/mysql-ca.pem \
      --database "$DB_NAME" --language "${GOLD_LANGUAGE:?set GOLD_LANGUAGE to the calibrated source language}" \
      --profile "${GOLD_PROFILE:?set GOLD_PROFILE to the calibrated model profile}"
    exit 0 ;;
  gold-submit)
    stamp="$(mktemp)"
    rc=0
    target_args=()
    if [ -n "${GOLD_TARGET_HOST:-}" ]; then
      [[ "$GOLD_TARGET_HOST" =~ ^[1-9][0-9]*$ ]] || fail "GOLD_TARGET_HOST must be positive"
      target_args=(--target-host "$GOLD_TARGET_HOST")
    fi
    usnm-boinc gold submit --root "$GOLD" --project "$PROJ" \
      --engine-image "${GOLD_ENGINE_IMAGE:?set GOLD_ENGINE_IMAGE}" \
      --engine-base "${GOLD_ENGINE_BASE:?set GOLD_ENGINE_BASE}" \
      --engine-version "${GOLD_ENGINE_VERSION:-5.5.0}" --units "${GOLD_UNITS:-20}" \
      ${target_args[@]+"${target_args[@]}"} || rc=$?
    share_readable "$stamp" "$PROJ/download" "$PROJ/templates" "$PROJ/gold"
    rm -f "$stamp"
    exit "$rc" ;;
  gold-onboard)
    # One bounded host-trust onboard pass: a targeted gold unit for each untrusted host
    # without one outstanding, including hosts whose last unit failed (docs/notes/2026-10-16-boinc-host-trust-followups.md).
    [[ "${GOLD_MAX_HOSTS:-10}" =~ ^[1-9][0-9]?$|^100$ ]] || fail "GOLD_MAX_HOSTS must be 1 to 100"
    write_mycnf
    controller="$SECRETS/host-trust.json"
    trap 'rm -f "$mycnf" "$SECRETS/db-runtime" "$controller"' EXIT
    (umask 077; jq -n --arg root "$SHARE_DIR/host-trust" --arg project "$PROJ" \
      --arg jobs "$HANDOFF_DIR" --arg mycnf "$mycnf" --arg ca /usr/local/share/usnm/mysql-ca.pem \
      --arg db "$DB_NAME" --arg trust "$SHARE_DIR/gold/trust.json" --arg prepared "$GOLD" \
      --arg language "${GOLD_LANGUAGE:?set GOLD_LANGUAGE to the calibrated source language}" \
      --arg profile "${GOLD_PROFILE:?set GOLD_PROFILE to the calibrated model profile}" \
      --arg image "${GOLD_ENGINE_IMAGE:?set GOLD_ENGINE_IMAGE}" \
      --arg base "${GOLD_ENGINE_BASE:?set GOLD_ENGINE_BASE}" \
      --arg version "${GOLD_ENGINE_VERSION:-5.5.0}" --argjson max "${GOLD_MAX_HOSTS:-10}" \
      '{root:$root, project:$project, jobs_root:$jobs, mysql_defaults:$mycnf, mysql_ca:$ca,
        database:$db, trust:$trust, prepared:$prepared,
        scope:{language:$language, profile:$profile, psm:3, dpi:300},
        engine:{base:$base, dpi:300, image:$image, psm:3, timeout_secs:300, version:$version},
        max_hosts:$max, max_spots:1, sample_every:1}' > "$controller")
    stamp="$(mktemp)"
    rc=0
    usnm-boinc host-trust onboard --config "$controller" --execute || rc=$?
    share_readable "$stamp" "$PROJ/download" "$PROJ/templates" "$PROJ/gold"
    rm -f "$stamp"
    exit "$rc" ;;
  register-apps)
    # BOINC's tools/xadd looks up each <platform> and <app> of project.xml by name: it inserts
    # the ones the database lacks, updates a changed user_friendly_name or min_version, and
    # leaves the rest; it never deletes. So a rerun changes nothing (BOINC 22bbebe, tools/xadd).
    cp "$TEMPLATES/project.xml" "$PROJ/project.xml"
    cd "$PROJ"
    exec bin/xadd ;;
  submit-remote)
    # The page comes from the hand-off container, which the scan fetcher filled; never from
    # LoC. Submission then runs as for submit, which checks the page again.
    state=$(usnm-boinc handoff pull --from-url "$HANDOFF_URL" --root "$HANDOFF_DIR" --job "$JOB_ID")
    if [ "$state" != reserved ]; then
      log "job $JOB_ID is already $state on the server; nothing submitted"
      exit 0
    fi
    exec usnm-boinc submit --root "$HANDOFF_DIR" --project "$PROJ" --job "$JOB_ID" ;;
  submit)
    # Submission publishes readable download inputs before create_work. The
    # hand-off journal stays private; no permission repair happens afterwards.
    exec usnm-boinc submit --root "$HANDOFF_DIR" --project "$PROJ" --job "$JOB_ID" ;;
  reconcile)
    case "${BRIDGE_FOUND:-}" in present|absent) ;; *) fail "BRIDGE_FOUND must be present or absent" ;; esac
    exec usnm-boinc reconcile --root "$HANDOFF_DIR" --job "$JOB_ID" --found "$BRIDGE_FOUND" ;;
  *) fail "BRIDGE_STEP must be submit, submit-remote, spool-out, reconcile, register-apps, work-volume, assess-stage, assess-submit, assess-collect, surya-stage, surya-stage-crops, surya-stage-gold, surya-submit, surya-collect, surya-gold-hyp, surya-summary, quality-stage, quality-submit, quality-collect, quality-summary, qwen-stage, qwen-submit, qwen-summary, qwen-benchmark-prepare, qwen-benchmark-stage, qwen-benchmark-collect, train-prepare, train-submit, train-collect, gold-prepare, gold-submit, gold-collect, gold-trust, gold-bundle, iris-bundle-stage, iris-bundle-submit, iris-bundle-cancel or iris-bundle-collect" ;;
esac
