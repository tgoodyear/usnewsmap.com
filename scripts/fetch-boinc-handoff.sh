#!/usr/bin/env bash
# Fetch the USNM-HANDOFF console lines one bridge-job execution printed (BRIDGE_STEP=spool-out
# or trust-out, contrib-compute/images/server/bridge-job.sh) from the BOINC stack's Log
# Analytics workspace, for `usnm-boinc handoff absorb --log FILE` (docs/boinc.md#one-page-from-the-ocr-node):
#
#   scripts/fetch-boinc-handoff.sh --env prod --execution usnm-boinc-bridge-caj-prod-abc12 --out /scratch/spool-out.log
#
# --timespan is an ISO 8601 duration back from now (default P2D). Console lines reach the
# workspace a few minutes after the execution ends; absorb says so when chunks are missing,
# and running this again is safe. Needs az signed in with an identity that can query the
# workspace (Log Analytics Reader on usnm-boinc-log-<env>), and jq.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
ENV_NAME="" EXECUTION="" OUT="" TIMESPAN=P2D
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --execution) [ $# -ge 2 ] || usage; EXECUTION=$2; shift 2 ;;
    --out) [ $# -ge 2 ] || usage; OUT=$2; shift 2 ;;
    --timespan) [ $# -ge 2 ] || usage; TIMESPAN=$2; shift 2 ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] && [ -n "$EXECUTION" ] && [ -n "$OUT" ] || usage
die() { echo "error: $*" >&2; exit 1; }
[[ $EXECUTION =~ ^[a-z0-9][a-z0-9-]{0,62}$ ]] || die "--execution is an execution name, as scripts/start-boinc-bridge.sh prints it"
[[ $TIMESPAN =~ ^P[0-9A-Z]+$|^PT[0-9A-Z]+$ ]] || die "--timespan is an ISO 8601 duration, such as P2D or PT6H"
cd "$(dirname "$0")/.."
ALLOW_DIRTY=false
# shellcheck source=scripts/lib/boinc.sh
. scripts/lib/boinc.sh
boinc_load_settings
command -v az > /dev/null || die "az is not installed"
command -v jq > /dev/null || die "jq is not installed"
workspace="usnm-boinc-log-$ENV_NAME"
customer=$(baz monitor log-analytics workspace show --resource-group "$BOINC_RG" --workspace-name "$workspace" \
  --query customerId -o tsv) || die "can't read the workspace $workspace"
[ -n "$customer" ] || die "$workspace has no customer id"
# A job execution's replica is named after the execution.
query="ContainerAppConsoleLogs_CL | where ContainerGroupName_s startswith '$EXECUTION' | where Log_s has 'USNM-HANDOFF' | project Log_s"
rows=$(baz monitor log-analytics query --workspace "$customer" --analytics-query "$query" --timespan "$TIMESPAN" -o json) ||
  die "can't query $workspace"
tmp=$(mktemp "$OUT.XXXXXX")
trap 'rm -f "$tmp"' EXIT
jq -r '.[].Log_s' <<< "$rows" > "$tmp"
n=$(wc -l < "$tmp" | tr -d ' ')
[ "$n" -gt 0 ] || die "no USNM-HANDOFF lines for $EXECUTION in the last $TIMESPAN (not ingested yet, or the step printed nothing)"
mv "$tmp" "$OUT"
trap - EXIT
echo "$n lines written to $OUT" >&2
