#!/usr/bin/env bash
# Start the OCR quality audit (ja-ocr/quality.py) as an execution of the
# environment's ja-ocr job, with the job's own image, settings and replicas
# but the audit's command line, and print the execution's name.
#
#   scripts/ja-ocr/start-quality.sh prod --sample-pct 10
#   scripts/ja-ocr/start-quality.sh prod --sample-pct 2 --min-pages 50
#
# The replicas share the work (quality.py). `az containerapp job start
# --args` sends a container without the image or settings, so this copies the
# job's template and swaps only the arguments. Needs az signed in to the
# environment's subscription and jq. Results: scripts/ja-ocr/quality-rows.sh.
set -euo pipefail
[ $# -ge 1 ] || { sed -n '2,13s/^# \{0,1\}//p' "$0" >&2; exit 2; }
ENV_NAME=$1
shift
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/../.."
. scripts/lib/env.sh
command -v jq > /dev/null || die "jq is needed to build the request"
[ -s "$ENV_FILE" ] || die "no settings for $ENV_NAME"
sub=$(aget AZURE_SUBSCRIPTION_ID)
rg=$(aget AZURE_RESOURCE_GROUP)
job="caj-usnm-jaocr-$ENV_NAME"
for a in "$@"; do
  [[ $a =~ ^(--sample-pct|--min-pages|--metric|[0-9]+(\.[0-9]+)?|v[12])$ ]] || die "unexpected argument \"$a\""
done

template=$(az containerapp job show -n "$job" -g "$rg" --subscription "$sub" --query properties.template -o json) ||
  die "can't read $job (is az signed in to the environment's tenant?)"
body=$(jq -c --args '{containers: [.containers[0] | .args = (["quality"] + $ARGS.positional)],
  initContainers: (.initContainers // [])}' "$@" <<< "$template")
az rest --method post \
  --url "https://management.azure.com/subscriptions/$sub/resourceGroups/$rg/providers/Microsoft.App/jobs/$job/start?api-version=2025-01-01" \
  --body "$body" --query name -o tsv
