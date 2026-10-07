#!/usr/bin/env bash
# Print one OCR quality audit run's (or jaocr.py mixed or american-stories run's) table rows and summary as JSON lines, from
# the environment's Log Analytics workspace: the "ocr quality" rows the
# reducing replica logs (every table) and the "ocr quality finished" line,
# without timestamps. The audit's own outputs (audit/*.json, CSVs) are in the
# curated store, which the network rules keep private; the logs are not.
#
#   scripts/ja-ocr/quality-rows.sh prod caj-usnm-jaocr-prod-a6wvy42 > rows.jsonl
#   scripts/ja-ocr/quality-rows.sh prod caj-usnm-jaocr-prod-a6wvy42 3d
#   scripts/ja-ocr/quality-rows.sh prod caj-usnm-jaaudit-prod-x1y2z3a   # a mixed or american-stories run
#
# Needs az signed in with an account that can read the workspace and jq.
set -euo pipefail
[ $# -ge 2 ] && [ $# -le 3 ] || { sed -n '2,13s/^# \{0,1\}//p' "$0" >&2; exit 2; }
ENV_NAME=$1
EXECUTION=$2
SPAN=${3:-2d}
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/../.."
. scripts/lib/env.sh
command -v jq > /dev/null || die "jq is needed"
[[ $EXECUTION =~ ^caj-usnm-ja(ocr|audit)-[a-z0-9]+-[a-z0-9]+$ ]] ||
  die "\"$EXECUTION\" isn't an execution of the ja-ocr or ja-audit job (caj-usnm-jaocr-… or caj-usnm-jaaudit-…)"
[[ $SPAN =~ ^[0-9]+[hd]$ ]] || die "timespan \"$SPAN\": use e.g. 12h or 3d"
case $SPAN in *h) iso="PT${SPAN%h}H" ;; *d) iso="P${SPAN%d}D" ;; esac
[ -s "$ENV_FILE" ] || die "no settings for $ENV_NAME"
workspace=$(aget LOG_ANALYTICS_WORKSPACE_ID)
[ -n "$workspace" ] || die "LOG_ANALYTICS_WORKSPACE_ID isn't set for $ENV_NAME"

query="ContainerAppConsoleLogs
| where ContainerName == \"jaocr\" and ContainerGroupName startswith \"$EXECUTION-\"
| where (Log has \"ocr quality\" or Log has \"mixed pages\" or Log has \"american stories\") and Log !has \"IDENTITY_HEADER\" and Log !has \"MSI_SECRET\"
| extend j = parse_json(Log)
| where tostring(j.message) in (\"ocr quality\", \"ocr quality finished\", \"mixed pages\", \"mixed pages finished\", \"american stories\", \"american stories finished\")
| project TimeGenerated, Log
| order by TimeGenerated asc"
body=$(mktemp)
trap 'rm -f "$body"' EXIT
jq -n --arg q "$query" --arg t "$iso" '{query: $q, timespan: $t}' > "$body"
az rest --method post --resource https://api.loganalytics.io \
  --url "https://api.loganalytics.io/v1/workspaces/$workspace/query" \
  --headers Content-Type=application/json --body "@$body" -o json |
  jq -c '.tables[0].rows[] | .[1] | fromjson | del(.ts)'
