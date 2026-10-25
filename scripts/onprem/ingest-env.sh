#!/usr/bin/env bash
# Print the settings file for usnm-ingest on an on-prem host (#917): what the Azure jobs
# usnm-ocrs-caj, caj-usnm-ingest and caj-usnm-backfill had in their environment, from the
# environment's settings (scripts/lib/env.sh): the stack's outputs (the Cosmos, lake,
# exchange account and Application Insights endpoints) and the committed choices
# (USNM_OCR_SEED_*, USNM_OCR_BULK_*, USNM_OCR_USER_DAILY_MAX). Run from the repository
# root with az signed in to the environment's subscription; the host installs it as
# /etc/usnm/ingest.env (home-infra scripts/lab/batch-ct-ingest.sh --env-file -).
#
#   scripts/onprem/ingest-env.sh --env prod > ingest.env
#   scripts/onprem/ingest-env.sh --env prod | ssh root@<proxmox host> /root/batch-ct-ingest.sh --env-file -
#
# Nothing in it is a secret: every store takes Entra tokens only (the host's Arc
# identity), and the Application Insights component refuses ingestion without one, so
# its connection string names the component and nothing more. No AZURE_CLIENT_ID: the
# Arc identity is the machine's own.
set -euo pipefail
usage() { sed -n '2,16s/^# \{0,1\}//p' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }
ENV_NAME=""
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    -h|--help) usage ;;
    *) echo "error: unexpected \"$1\"" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] || usage
cd "$(dirname "$0")/../.."
. scripts/lib/env.sh
[ -s "$ENV_FILE" ] || die "no settings for $ENV_NAME"
need() {
  local v
  v=$(aget "$1")
  [ -n "$v" ] || die "$1 isn't set for $ENV_NAME (is the stack provisioned?)"
  printf '%s' "$v"
}
lake=$(need STORAGE_ACCOUNT)
[[ $lake =~ ^[a-z0-9]{3,24}$ ]] || die "STORAGE_ACCOUNT \"$lake\" isn't a storage account name"
blob="https://$lake.blob.core.windows.net/"
cosmos=$(need COSMOS_ENDPOINT)
[[ $cosmos =~ ^https://[a-z0-9-]+\.documents\.azure\.com(:443)?/$ ]] || die "COSMOS_ENDPOINT \"$cosmos\" isn't a Cosmos endpoint"
line() {
  [[ $2 =~ ^[^[:space:]]*$ ]] || die "$1's value has white space"
  [ -z "$2" ] || printf '%s=%s\n' "$1" "$2"
}
echo "# usnm-ingest's settings for $ENV_NAME, written by scripts/onprem/ingest-env.sh on $(date -u +%Y-%m-%dT%H:%MZ)."
line USNM_COSMOS_ENDPOINT "$cosmos"
line USNM_CURATED_URL "${blob}curated"
line USNM_REFERENCE_URL "${blob}reference"
line RUST_LOG info
line APPLICATIONINSIGHTS_CONNECTION_STRING "$(aget APPLICATIONINSIGHTS_CONNECTION_STRING)"
# The OCR pickup (ocr-sync, ocr-seed, ocr-enqueue, ocr-requeue), with the exchange account.
queue=$(aget OCR_QUEUE_ENDPOINT)
if [ -n "$queue" ]; then
  out=$(need OCR_BLOB_ENDPOINT)
  line USNM_OCR_QUEUE_URL "$queue"
  line USNM_OCR_OUT_URL "${out}ocr-out"
  line USNM_OCR_QUARANTINE_URL "${out}ocr-quarantine"
  line USNM_OCR_REQUESTS_URL "${blob}ocr-requests"
  line USNM_OCR_USER_DAILY_MAX "$(aget USNM_OCR_USER_DAILY_MAX)"
  # The seeder's settings (infra/main.bicepparam's ocrSeed); empty ones are left out, so
  # the command's defaults apply, as in the job.
  for k in USNM_OCR_SEED_PLAN USNM_OCR_SEED_BATCHES USNM_OCR_SEED_LANGUAGES USNM_OCR_SEED_YEARS \
    USNM_OCR_SEED_MIN_DAMAGE USNM_OCR_SEED_MAX_PAGES USNM_OCR_SEED_PILOT \
    USNM_OCR_SEED_SKIP_AMERICAN_STORIES USNM_OCR_SEED_SOURCE USNM_OCR_SEED_MANIFEST \
    USNM_OCR_BULK_WINDOW USNM_OCR_BULK_REFILL_BELOW; do
    line "$k" "$(aget "$k")"
  done
fi
