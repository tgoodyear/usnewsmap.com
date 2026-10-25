#!/usr/bin/env bash
# Push scans the fetcher already holds to the BOINC hand-off container (#469,
# docs/boinc/scan-cache-handoff.md#one-page-from-the-scan-cache), for the hand-off job's export. Runs in the
# scan-fetch container (CT 211), signed in as its Arc machine identity
# (az login --identity --allow-no-subscriptions), which has the hand-off writer role on the
# container and nothing else there. It reads /tank/ocr and asks LoC for nothing.
#
#   push-boinc-scan.sh --account <exchange account> --scan /tank/ocr/scans/keep/<batch>/.../<seq>.jp2 \
#     [--scan ...] [--manifest /tank/ocr/manifests/<version>/manifest.parquet]
#     [--scans-root /tank/ocr/scans] [--manifests-root /tank/ocr/manifests] [--what-if]
#   push-boinc-scan.sh --account <exchange account> --keep-page <batch>/<lccn>/<date>/<edition>/<seq> ...
#
# --keep-page names a page by where the fetcher files it (keep/<that path>.jp2 or .jpg), for
# pages that came off the work queues, whose scan format nobody has said. The fetcher's buffer
# set (buffer/<that path>, the same layout) is tried after keep: a page it fetched for a queued
# OCR request is there (#1055). Either way the scan goes up as scans/keep/<that path>.<ext>,
# the name the hand-off export takes, and the upload is a copy, so evicting the buffer scan later
# can't take it from the hand-off. It pushes each one it finds, prints `pushed keep/<that
# path>.<ext>` for it on stdout, prints `missing <page>` for
# one with no cached scan, and then exits 3 if any was missing, so the caller can hand that
# message back to the queue. It may be given with --scan.
#
# --to-onprem (#1116) sends the --keep-page scans to the BOINC host instead of the container: a
# tar of copies, named keep/<that path>.<ext> with their sidecars, piped to the controller's step
# command (each word given with --onprem-step-arg, in order: the ssh command up to its `--`) as
# `--env <--onprem-env> --target scans --step receive`. Nothing goes to Azure then, and
# --manifest is not uploaded (a queue-fed export doesn't read it). It prints the same `pushed` and
# `missing` lines once the host has every file.
#
# Each --scan goes up with its sidecar (<scan>.json) as scans/<path under --scans-root>, and
# --manifest as manifests/<path under --manifests-root> (the hand-off job's HANDOFF_MANIFEST),
# so each manifest snapshot keeps its own name. A blob already there with the same size is
# left as it is (the fetcher never rewrites a scan or a snapshot), so a second run is safe;
# one with another size stops the run. --what-if prints the uploads.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
ACCOUNT="" MANIFEST="" SCANS_ROOT=/tank/ocr/scans MANIFESTS_ROOT=/tank/ocr/manifests WHAT_IF=false
SCANS=() KEEP_PAGES=() TO_ONPREM=false ONPREM_ENV=prod STEP_CMD=()
while [ $# -gt 0 ]; do
  case $1 in
    --account) [ $# -ge 2 ] || usage; ACCOUNT=$2; shift 2 ;;
    --scan) [ $# -ge 2 ] || usage; SCANS+=("$2"); shift 2 ;;
    --keep-page) [ $# -ge 2 ] || usage; KEEP_PAGES+=("$2"); shift 2 ;;
    --manifest) [ $# -ge 2 ] || usage; MANIFEST=$2; shift 2 ;;
    --scans-root) [ $# -ge 2 ] || usage; SCANS_ROOT=${2%/}; shift 2 ;;
    --manifests-root) [ $# -ge 2 ] || usage; MANIFESTS_ROOT=${2%/}; shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    --to-onprem) TO_ONPREM=true; shift ;;
    --onprem-step-arg) [ $# -ge 2 ] || usage; STEP_CMD+=("$2"); shift 2 ;;
    --onprem-env) [ $# -ge 2 ] || usage; ONPREM_ENV=$2; shift 2 ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
die() { echo "error: $*" >&2; exit 1; }
[ -n "$ACCOUNT" ] && { [ ${#SCANS[@]} -gt 0 ] || [ ${#KEEP_PAGES[@]} -gt 0 ] || [ -n "$MANIFEST" ]; } || usage
[[ $ACCOUNT =~ ^[a-z0-9]{3,24}$ ]] || die "--account is a storage account name"
CONTAINER=boinc-handoff
# Blob names are /-separated segments of [A-Za-z0-9._-], starting with a letter or digit
# (usnm_store::validate_path), so the hand-off job can copy them.
segments='^[A-Za-z0-9][A-Za-z0-9._-]*(/[A-Za-z0-9][A-Za-z0-9._-]*)*$'
uploads=() missing=() staged=()
if $TO_ONPREM; then
  [ ${#STEP_CMD[@]} -gt 0 ] || die "--to-onprem needs the step command (--onprem-step-arg ...)"
  [ ${#SCANS[@]} -eq 0 ] || die "--to-onprem takes --keep-page pages only"
  [[ $ONPREM_ENV =~ ^[a-z][a-z0-9-]{0,30}$ ]] || die "--onprem-env is an environment name"
fi
# A page the fetcher filed under keep/ (or, failing that, buffer/) is pushed under its keep/ name.
for p in ${KEEP_PAGES[@]+"${KEEP_PAGES[@]}"}; do
  [[ $p =~ ^[a-z]{2,10}_[A-Za-z0-9_]+/[A-Za-z0-9_-]+/[0-9]{4}-[0-9]{2}-[0-9]{2}/[0-9]+/[0-9]+$ ]] ||
    die "--keep-page $p is <batch>/<lccn>/<date>/<edition>/<seq>"
  found="" name=""
  for set in keep buffer; do
    for ext in jp2 jpg; do
      f="$SCANS_ROOT/$set/$p.$ext"
      if [ -f "$f" ] && [ ! -L "$f" ] && [ -f "$f.json" ]; then found=$f name=keep/$p.$ext; break 2; fi
    done
  done
  if [ -n "$found" ] && $TO_ONPREM; then
    staged+=("$found" "$name")
  elif [ -n "$found" ]; then
    uploads+=("$found" "scans/$name" "$found.json" "scans/$name.json")
    echo "pushed $name"
  else
    missing+=("$p")
  fi
done
if $TO_ONPREM; then
  if [ ${#staged[@]} -gt 0 ]; then
    # Links named as the host files them; tar -h archives the files they point to.
    tree=$(mktemp -d)
    trap 'rm -rf "$tree"' EXIT
    for ((i = 0; i < ${#staged[@]}; i += 2)); do
      file=${staged[i]} name=${staged[i + 1]}
      mkdir -p "$tree/$(dirname "$name")"
      ln -s "$file" "$tree/$name"
      ln -s "$file.json" "$tree/$name.json"
    done
    if $WHAT_IF; then
      echo "would send $((${#staged[@]} / 2)) scans to: ${STEP_CMD[*]} --env $ONPREM_ENV --target scans --step receive"
    else
      COPYFILE_DISABLE=1 tar -C "$tree" -chf - keep |
        "${STEP_CMD[@]}" --env "$ONPREM_ENV" --target scans --step receive --wait-mins 30 >&2 ||
        die "the BOINC host did not take the scans"
    fi
    for ((i = 1; i < ${#staged[@]}; i += 2)); do echo "pushed ${staged[i]}"; done
  fi
  for p in ${missing[@]+"${missing[@]}"}; do echo "missing $p"; done
  [ ${#missing[@]} -eq 0 ] || exit 3
  exit 0
fi
for s in ${SCANS[@]+"${SCANS[@]}"}; do
  case $s in "$SCANS_ROOT"/*) ;; *) die "--scan $s is not under $SCANS_ROOT" ;; esac
  [ -f "$s" ] && [ ! -L "$s" ] || die "--scan $s is not a regular file"
  [ -f "$s.json" ] || die "--scan $s has no sidecar $s.json"
  rel=${s#"$SCANS_ROOT"/}
  [[ $rel =~ $segments ]] && [[ $rel != *..* ]] || die "--scan $s: its path has characters a blob name can't take"
  uploads+=("$s" "scans/$rel" "$s.json" "scans/$rel.json")
  [ ${#KEEP_PAGES[@]} -eq 0 ] || echo "pushed $rel"
done
if [ -n "$MANIFEST" ]; then
  case $MANIFEST in "$MANIFESTS_ROOT"/*) ;; *) die "--manifest $MANIFEST is not under $MANIFESTS_ROOT" ;; esac
  [ -f "$MANIFEST" ] && [ ! -L "$MANIFEST" ] || die "--manifest $MANIFEST is not a regular file"
  rel=${MANIFEST#"$MANIFESTS_ROOT"/}
  [[ $rel =~ $segments ]] && [[ $rel != *..* ]] || die "--manifest $MANIFEST: its path has characters a blob name can't take"
  uploads+=("$MANIFEST" "manifests/$rel")
fi
command -v az > /dev/null || die "az is not installed"
for ((i = 0; i < ${#uploads[@]}; i += 2)); do
  file=${uploads[i]} blob=${uploads[i + 1]}
  if $WHAT_IF; then
    echo "would upload $file to $CONTAINER/$blob"
    continue
  fi
  size=$(az storage blob show --auth-mode login --account-name "$ACCOUNT" --container-name "$CONTAINER" \
    --name "$blob" --query properties.contentLength -o tsv --only-show-errors 2> /dev/null) || size=""
  if [ -n "$size" ]; then
    [ "$size" = "$(wc -c < "$file" | tr -d ' ')" ] || die "$CONTAINER/$blob is already there with another size ($size bytes)"
    echo "already there: $CONTAINER/$blob" >&2
    continue
  fi
  az storage blob upload --auth-mode login --account-name "$ACCOUNT" --container-name "$CONTAINER" \
    --name "$blob" --file "$file" --no-progress --only-show-errors -o none || die "can't upload $file"
  echo "uploaded $CONTAINER/$blob" >&2
done
for p in ${missing[@]+"${missing[@]}"}; do echo "missing $p"; done
[ ${#missing[@]} -eq 0 ] || exit 3
