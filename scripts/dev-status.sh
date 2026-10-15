#!/usr/bin/env bash
# Is dev up, and for which experiment (#275; docs/operations.md, "Dev
# environment lifecycle")? Lists its deployment stack and what's in its
# groups (usnm-core-rg-dev, the Container Apps environment's
# usnm-core-infra-rg-dev; the retired rg-usnm-dev-spot and legacy rg-usnm-dev
# and ME_cae-usnm-dev_* too, ADR-0017), the experiment tag on the project group
# and its age, and what those groups have cost so far (Cost Management,
# actual): since the experiment started, or this month when there's no tag.
#
#   scripts/dev-status.sh [--env dev] [--subscription ID]
#
# --env NAME          another environment run the same way (default dev)
# --subscription ID   default: the environment's settings, else az's current
#
# Exit status: 0 when the environment holds nothing, or is up with an
# experiment tag (one older than 7 days is a warning); 1 when anything of it
# exists without the tag, or on an error; 2 for a usage error. Cost query
# throttling is retried (up to 4 attempts, at most 60 seconds of backoff);
# exhausted throttling leaves cost unknown without affecting this status.
#
# Needs az signed in with Reader on the subscription (Cost Management Reader
# too for the cost, which is otherwise left out), jq, and python3.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
TARGET=dev SUBSCRIPTION=""
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; TARGET=$2; shift 2 ;;
    --subscription) [ $# -ge 2 ] || usage; SUBSCRIPTION=$2; shift 2 ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/.."
ENV_NAME=$TARGET
. scripts/lib/env.sh
command -v jq > /dev/null || die "jq is needed"
az account show -o none 2> /dev/null || die "run: az login [--tenant TENANT]"
[ -n "$SUBSCRIPTION" ] || SUBSCRIPTION=$(aget AZURE_SUBSCRIPTION_ID)
[ -n "$SUBSCRIPTION" ] || SUBSCRIPTION=$(az account show --query id -o tsv)
SUBSCRIPTION=$(az account show --subscription "$SUBSCRIPTION" --query id -o tsv) ||
  die "can't select subscription $SUBSCRIPTION"
echo "$ENV_NAME in subscription $SUBSCRIPTION"

# Seconds since the epoch of an ISO 8601 UTC time: GNU date, then BSD date.
epoch() {
  date -u -d "$1" +%s 2> /dev/null || date -u -j -f '%Y-%m-%dT%H:%M:%SZ' "$1" +%s 2> /dev/null
}

stack=false
if out=$(az stack sub show -n "$STACK" --subscription "$SUBSCRIPTION" --query "length(resources)" -o tsv 2>&1); then
  stack=true
  echo "stack $STACK: $out managed resources"
elif ! grep -qiE 'NotFound|could not be found' <<< "$out"; then
  die "can't check the deployment stack $STACK: $out"
fi
# $ENV_GROUPS, $PROJECT_GROUPS and $LEGACY_INFRA_GROUP: scripts/lib/env.sh.
match=""
for g in $ENV_GROUPS; do match="$match || name=='$g'"; done
groups=$(az group list --subscription "$SUBSCRIPTION" \
  --query "[?${match# || } || starts_with(name, '$LEGACY_INFRA_GROUP')].name" -o tsv) ||
  die "can't list resource groups"
for g in $groups; do
  list=$(az resource list -g "$g" --subscription "$SUBSCRIPTION" --query "[].[type, name]" -o tsv) ||
    die "can't list the resources in $g"
  n=0
  [ -z "$list" ] || n=$(wc -l <<< "$list" | tr -d ' ')
  echo "group $g: $n resources"
  [ -z "$list" ] || while IFS=$'\t' read -r type name; do echo "  $type  $name"; done <<< "$list"
done

# The project group, in whichever name form it has.
experiment="" started="" RG=""
for g in $PROJECT_GROUPS; do
  if grep -Fqx "$g" <<< "$groups"; then RG=$g; break; fi
done
if [ -n "$RG" ]; then
  tags=$(az group show -n "$RG" --subscription "$SUBSCRIPTION" --query "[tags.experiment || '', tags.started || '']" -o tsv) ||
    die "can't read the tags of $RG"
  # TSV lists have one value per line; the last empty value may be stripped.
  { IFS= read -r experiment; IFS= read -r started || :; } <<< "$tags"
fi

# The cost of the environment's groups, whatever their case in Cost
# Management's data, since the experiment started (or this month).
now=$(date -u +%Y-%m-%dT%H:%M:%SZ)
from=${started:-$(date -u +%Y-%m-01T00:00:00Z)}
body=$(jq -n --arg from "$from" --arg to "$now" '{
  type: "ActualCost", timeframe: "Custom", timePeriod: {from: $from, to: $to},
  dataset: {granularity: "None", aggregation: {totalCost: {name: "Cost", function: "Sum"}},
    grouping: [{type: "Dimension", name: "ResourceGroupName"}]}}')
# az rest normally prints only the HTTP reason and response body, not headers
# (azure.cli.core.util.send_raw_request). Honour retry headers if included in
# that text; otherwise use exponential backoff, without enabling verbose/debug
# logging of requests. Never retry sooner than the server asks.
cost_query() {
  local attempt=1 waited=0 backoff=2 delay
  while :; do
    if cost=$(az rest --method post \
      --url "https://management.azure.com/subscriptions/$SUBSCRIPTION/providers/Microsoft.CostManagement/query?api-version=2023-11-01" \
      --body "$body" 2>&1); then
      return 0
    fi
    if ! grep -qiE '(^|[^0-9])429([^0-9]|$)|Too[[:space:]]*Many[[:space:]]*Requests' <<< "$cost"; then
      return 1
    fi
    [ "$attempt" -lt 4 ] || return 1
    delay=$(python3 -c '
import math, re, sys
from datetime import datetime, timezone
from email.utils import parsedate_to_datetime
text = sys.stdin.read()
delays = [int(sys.argv[1])]
for value in re.findall(r"(?:\bretry-after|\bx-ms-ratelimit-microsoft\.costmanagement-[\w-]+-retry-after)[\"\x27]?\s*[:=]\s*[\"\x27]?([^\r\n\"\x27}]+)", text, re.I):
    value = value.strip().rstrip(",")
    try:
        delay = int(value) if value.isdigit() else math.ceil(
            (parsedate_to_datetime(value) - datetime.now(timezone.utc)).total_seconds())
        delays.append(max(0, delay))
    except (ValueError, TypeError, OverflowError):
        pass
print(max(delays))
' "$backoff" <<< "$cost") || return 1
    # An over-budget Retry-After means stop, not retry before it has elapsed.
    [ "$delay" -le "$((60 - waited))" ] || return 1
    echo "warning: Cost Management throttled; retrying in $delay seconds (attempt $((attempt + 1))/4)" >&2
    sleep "$delay"
    waited=$((waited + delay))
    backoff=$((backoff * 2))
    attempt=$((attempt + 1))
  done
}
if cost_query; then
  cost=$(jq -r --arg groups "$ENV_GROUPS" --arg infra "$LEGACY_INFRA_GROUP" '
    .properties as $p | ($p.columns | map(.name)) as $c
    | ($groups | ascii_downcase | split(" ")) as $g | ($infra | ascii_downcase) as $i
    | [$p.rows[] | {cost: .[$c | index("Cost")], rg: ((.[$c | index("ResourceGroupName")] // "") | ascii_downcase),
        cur: .[$c | index("Currency")]}
       | select(.rg as $r | ($g | any(. == $r)) or ($r | startswith($i)))]
    | "\((map(.cost) | add // 0) * 100 | round / 100) \(.[0].cur // "")"' <<< "$cost") ||
    die "can't read Cost Management's answer"
  echo "cost since $from: $cost"
elif grep -qiE '(^|[^0-9])429([^0-9]|$)|Too[[:space:]]*Many[[:space:]]*Requests' <<< "$cost"; then
  echo "warning: cost unknown (Cost Management throttled; retry limit reached): $cost" >&2
elif grep -qiE 'AuthorizationFailed|RBACAccessDenied|Forbidden|does not have authorization' <<< "$cost"; then
  # Reading costs needs Cost Management Reader; the rest of the check doesn't.
  echo "warning: no cost (needs Cost Management Reader): $cost" >&2
else
  die "can't read the cost from Cost Management: $cost"
fi

if [ "$stack" = false ] && [ -z "$groups" ]; then
  echo "$ENV_NAME holds nothing"
  exit 0
fi
if [ -z "$experiment" ]; then
  echo "error: $ENV_NAME has resources and no experiment tag on ${RG:-its project group (${PROJECT_GROUPS// / or })}: tear it down (scripts/teardown.sh --env $ENV_NAME), or tag it (scripts/settings.sh --env $ENV_NAME --key USNM_EXPERIMENT --value <issue>; scripts/provision.sh --env $ENV_NAME)" >&2
  exit 1
fi
age=""
if [ -n "$started" ] && s=$(epoch "$started"); then
  age=$(( ($(date -u +%s) - s) / 86400 ))
fi
echo "experiment #$experiment, started ${started:-at an unknown time}${age:+, $age days ago}"
if [ -n "$age" ] && [ "$age" -gt 7 ]; then
  echo "warning: experiment #$experiment has kept $ENV_NAME up for more than 7 days" >&2
fi
