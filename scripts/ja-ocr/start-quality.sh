#!/usr/bin/env bash
# Start the OCR quality audit (ja-ocr/quality.py) as an execution of the
# environment's ja-ocr job (or its one-replica audit job, below), with the job's own image, settings and replicas
# but the audit's command line, and print the execution's name.
#
#   scripts/ja-ocr/start-quality.sh prod --sample-pct 10
#   scripts/ja-ocr/start-quality.sh prod --sample-pct 2 --min-pages 50
#   scripts/ja-ocr/start-quality.sh prod mixed     # jaocr.py mixed instead (mixed.py)
#   scripts/ja-ocr/start-quality.sh prod american-stories --year 1865 --year 1925   (american_stories.py)
#   scripts/ja-ocr/start-quality.sh prod american-stories-write [--year 1865 ...]  (american_stories_write.py)
#
# The quality audit runs on the ja-ocr job, whose replicas share its work
# (quality.py); mixed and the american-stories commands, which one replica does, run on the
# one-replica audit job (caj-usnm-jaone-<env>), so a second replica can't
# fail the execution. `az containerapp job start
# --args` sends a container without the image or settings, so this copies the
# job's template and swaps only the arguments. Needs az signed in to the
# environment's subscription and jq. Results: scripts/ja-ocr/quality-rows.sh.
set -euo pipefail
[ $# -ge 1 ] || { sed -n '2,19s/^# \{0,1\}//p' "$0" >&2; exit 2; }
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
command=quality
if [ "${1:-}" = mixed ]; then
  command=mixed
  job="caj-usnm-jaone-$ENV_NAME"
  shift
  [ $# -eq 0 ] || die "mixed takes no options"
elif [ "${1:-}" = american-stories ] || [ "${1:-}" = american-stories-write ]; then
  command=$1
  job="caj-usnm-jaone-$ENV_NAME"
  shift
  for a in "$@"; do
    [[ $a =~ ^(--year|--sample-pct|[0-9]+(\.[0-9]+)?)$ ]] || die "unexpected argument \"$a\""
  done
else
  for a in "$@"; do
    [[ $a =~ ^(--sample-pct|--min-pages|--metric|[0-9]+(\.[0-9]+)?|v[12])$ ]] || die "unexpected argument \"$a\""
  done
fi

template=$(az containerapp job show -n "$job" -g "$rg" --subscription "$sub" --query properties.template -o json) ||
  die "can't read $job (is az signed in to the environment's tenant?)"
# `--args --`: the audit's options are positional strings, not jq options.
body=$(jq -c --arg command "$command" '{containers: [.containers[0] | .args = ([$command] + $ARGS.positional)],
  initContainers: (.initContainers // [])}' --args -- "$@" <<< "$template")
az rest --method post \
  --url "https://management.azure.com/subscriptions/$sub/resourceGroups/$rg/providers/Microsoft.App/jobs/$job/start?api-version=2025-01-01" \
  --body "$body" --query name -o tsv
