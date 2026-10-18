#!/usr/bin/env bash
# Start one run of the BOINC bridge job (contrib-compute/images/server/bridge-job.sh) with its
# step and that step's settings, and print the execution's name:
#
#   scripts/start-boinc-bridge.sh --env prod --step register-apps
#   scripts/start-boinc-bridge.sh --env prod --step gold-prepare --var GOLD_CAMPAIGN=v1-tess550
#   scripts/start-boinc-bridge.sh --env prod --step submit --var JOB_ID=<id> --wait-mins 30
#   scripts/start-boinc-bridge.sh --env prod --target handoff --step collect --var JOB_ID=<id>
#
# --target handoff starts the BOINC hand-off job (infra/modules/boinc-handoff-job.bicep,
# handoff-job.sh) in the main environment instead: the same steps below, with HANDOFF_STEP in
# place of BRIDGE_STEP and HANDOFF_BASE_VARS as the job's own settings. It has no share, but
# the same reason holds: a container override would drop the job's settings. The next
# scripts/provision.sh puts them back.
#
# Why not `az containerapp job start --env-vars`: with any container flag the CLI sends an
# execution template holding one container named after the job (this one is "bridge"), with
# no image, and with only the variables given, so the job's own settings (KEY_VAULT_NAME,
# DB_HOST, PROJECT_DIR, ...) are gone. The API's execution template also has no volumes or
# mounts (scripts/start-job.sh), and every step needs the project share at /project. So this
# script writes the run's settings into the job itself, with
# `az containerapp job update --replace-env-vars`: the job's settings that the template
# defines (BASE_VARS below, kept as they are) plus BRIDGE_STEP and each --var. Settings a
# previous run left are dropped. Then it starts the job with no override, so the image, the
# command, the resources and the share mount are the deployed ones. The next
# scripts/provision-boinc.sh puts the template's settings back.
#
# --var NAME=VALUE is repeatable; NAME is upper case and can't be one of BASE_VARS or
# BRIDGE_STEP. The step and the values are checked by bridge-job.sh, not here. --wait-mins N
# waits up to N minutes for the execution to succeed and exits 1 if it ends Failed, Stopped
# or Degraded or is still running then. --what-if prints the settings and changes nothing.
# One step at a time: a manual job can run several executions at once, and they would share
# the project tree and the database. So it refuses while any execution of the job is still
# running, and after the start it reads the new execution's settings back and fails if they
# are not this run's (another caller updated the job in between). A cross-host blob lease
# serializes the check/update/start transaction for both targets; see docs/boinc-start-guard.md.
# Needs az signed in to the settings' subscription, jq, and python3.
set -euo pipefail
usage() { awk 'NR == 1 { next } !/^#/ { exit } { sub(/^# ?/, ""); print }' "$0" >&2; exit 2; }
ENV_NAME="" STEP="" WAIT_MINS="" WHAT_IF=false TARGET=bridge
VARS=()
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --step) [ $# -ge 2 ] || usage; STEP=$2; shift 2 ;;
    --target)
      [ $# -ge 2 ] || usage
      case $2 in bridge | handoff) TARGET=$2 ;; *) echo "error: --target is bridge or handoff" >&2; exit 2 ;; esac
      shift 2 ;;
    --var) [ $# -ge 2 ] || usage; VARS+=("$2"); shift 2 ;;
    --wait-mins)
      [ $# -ge 2 ] || usage
      [[ $2 =~ ^[0-9]+$ ]] || { echo "error: --wait-mins is a number of minutes" >&2; exit 2; }
      WAIT_MINS=$((10#$2)); shift 2 ;;
    --what-if) WHAT_IF=true; shift ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] && [ -n "$STEP" ] || usage
die() { echo "error: $*" >&2; exit 1; }
# The settings contrib-compute/infra/boinc/modules/bridge-job.bicep gives the container
# (scripts/test_boinc_scripts.py checks the two lists agree).
BASE_VARS=(AZURE_CLIENT_ID KEY_VAULT_NAME DB_HOST DB_NAME MASTER_HOST PROJECT_DIR HANDOFF_DIR)
# The hand-off job's, from infra/modules/boinc-handoff-job.bicep (checked the same way).
HANDOFF_BASE_VARS=(AZURE_CLIENT_ID HANDOFF_URL COSMOS_ENDPOINT CURATED_URL REFERENCE_URL OUT_URL)
# Also the hand-off job's, but kept only when the deployed job has it, so a job deployed before
# the queue steps (#296) still runs export and collect. Steps that need it say so themselves.
HANDOFF_OPTIONAL_VARS=(QUEUE_URL)
OPTIONAL_VARS=()
STEP_VAR=BRIDGE_STEP
if [ "$TARGET" = handoff ]; then
  BASE_VARS=("${HANDOFF_BASE_VARS[@]}")
  OPTIONAL_VARS=("${HANDOFF_OPTIONAL_VARS[@]}")
  STEP_VAR=HANDOFF_STEP
fi
[[ $STEP =~ ^[a-z][a-z-]{0,40}$ ]] || die "--step is a bridge step name, such as register-apps or gold-prepare"
seen=" "
for v in ${VARS[@]+"${VARS[@]}"}; do
  name=${v%%=*}
  [[ $v == *=* ]] || die "--var wants NAME=VALUE, got \"$v\""
  [[ $name =~ ^[A-Z][A-Z0-9_]{0,63}$ ]] || die "--var $name: names are upper case letters, digits and _"
  [ "$name" != "$STEP_VAR" ] || die "--var $STEP_VAR: use --step"
  for b in "${BASE_VARS[@]}" ${OPTIONAL_VARS[@]+"${OPTIONAL_VARS[@]}"}; do [ "$name" != "$b" ] || die "--var $name: the job's own setting can't be changed per run"; done
  [[ $seen != *" $name "* ]] || die "--var $name is given twice"
  seen="$seen$name "
done
cd "$(dirname "$0")/.."
ALLOW_DIRTY=false
if [ "$TARGET" = handoff ]; then
  # shellcheck source=scripts/lib/env.sh
  . scripts/lib/env.sh
  command -v az > /dev/null || die "az is not installed"
  command -v jq > /dev/null || die "jq is not installed"
  SUBSCRIPTION=$(aget AZURE_SUBSCRIPTION_ID)
  [ -n "$SUBSCRIPTION" ] || die "AZURE_SUBSCRIPTION_ID isn't set for $ENV_NAME; run scripts/provision.sh --env $ENV_NAME"
  [ "$(az account show --subscription "$SUBSCRIPTION" --query id -o tsv 2> /dev/null)" = "$SUBSCRIPTION" ] ||
    die "az cannot see subscription $SUBSCRIPTION (az login --tenant <its tenant> first)"
  baz() { az "$@" --subscription "$SUBSCRIPTION" --only-show-errors; }
  job=$(aget BOINC_HANDOFF_JOB)
  [ -n "$job" ] || die "the stack $STACK has no BOINC_HANDOFF_JOB output; run scripts/provision.sh --env $ENV_NAME"
  JOB_RG=$(aget AZURE_RESOURCE_GROUP)
  [ -n "$JOB_RG" ] || die "the stack $STACK has no AZURE_RESOURCE_GROUP output; run scripts/provision.sh --env $ENV_NAME"
  baz group show --name "$JOB_RG" --query name -o none ||
    die "no resource group $JOB_RG: it comes from the main stack (scripts/provision.sh --env $ENV_NAME)"
  PROVISION="scripts/provision.sh --env $ENV_NAME"
  LOGS="the main environment's Log Analytics workspace (ContainerAppConsoleLogs)"
else
  # shellcheck source=scripts/lib/boinc.sh
  . scripts/lib/boinc.sh
  boinc_load_settings
  boinc_az_check
  job=$(boinc_output bridgeJobName)
  [ -n "$job" ] || die "the stack $BOINC_STACK has no bridge job (are the images in $BOINC_SETTINGS deployed? scripts/provision-boinc.sh --env $ENV_NAME)"
  JOB_RG=$BOINC_RG
  PROVISION="scripts/provision-boinc.sh --env $ENV_NAME"
  LOGS="the BOINC stack's Log Analytics workspace (ContainerAppConsoleLogs_CL)"
fi
template=$(baz containerapp job show --name "$job" --resource-group "$JOB_RG" --query properties.template -o json) ||
  die "can't read $job"
container=$(jq -er '.containers[0].name' <<< "$template") || die "$job has no container"
# The job's own settings, as deployed; each must be there.
base=()
for b in "${BASE_VARS[@]}"; do
  val=$(jq -er --arg n "$b" '.containers[0].env // [] | map(select(.name == $n and (.value // "") != "")) | if length == 1 then .[0].value else error end' <<< "$template") ||
    die "$job has no $b setting; run $PROVISION first"
  base+=("$b=$val")
done
for b in ${OPTIONAL_VARS[@]+"${OPTIONAL_VARS[@]}"}; do
  val=$(jq -er --arg n "$b" '.containers[0].env // [] | map(select(.name == $n and (.value // "") != "")) | if length == 1 then .[0].value else empty end' <<< "$template") || val=""
  [ -z "$val" ] || base+=("$b=$val")
done
run=("$STEP_VAR=$STEP" ${VARS[@]+"${VARS[@]}"})
if $WHAT_IF; then
  echo "would set the settings of $job (container $container) to:"
  printf '  %s\n' "${base[@]}" "${run[@]}"
  echo "then start one execution of it with no override"
  exit 0
fi
if [ "$TARGET" = bridge ]; then
  boinc_acquire_lock "start-boinc-bridge.sh --env $ENV_NAME --target $TARGET --step $STEP"
fi
# shellcheck source=scripts/lib/boinc-start-guard.sh
. scripts/lib/boinc-start-guard.sh
trap 'boinc_start_guard_cleanup $?' EXIT
boinc_start_guard_acquire
# running_executions: the job's executions that haven't ended.
running_executions() {
  baz containerapp job execution list --name "$job" --resource-group "$JOB_RG" \
    --query "[?properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv
}
running=$(running_executions) || die "can't list the executions of $job"
[ -z "$running" ] || die "$job is still running $(echo "$running" | tr '\n' ' ')- wait for it to end first"
BOINC_START_SAFE_TO_RELEASE=false
baz containerapp job update --name "$job" --resource-group "$JOB_RG" --container-name "$container" \
  --replace-env-vars "${base[@]}" "${run[@]}" -o none || die "can't set the run's settings on $job"
execution=$(baz containerapp job start --name "$job" --resource-group "$JOB_RG" --query name -o tsv) ||
  die "can't start $job"
[ -n "$execution" ] || die "$job started no execution"
# The execution records the template it started with: it must hold exactly this run's settings.
want=$(printf '%s\n' "${base[@]}" "${run[@]}" | sort)
got=$(baz containerapp job execution show --name "$job" --resource-group "$JOB_RG" --job-execution-name "$execution" \
  --query properties.template.containers[0].env -o json | jq -r '.[] | "\(.name)=\(.value // "")"' | sort) ||
  die "can't read $execution's settings; stop it if it isn't this run's: az containerapp job stop --name $job --resource-group $JOB_RG --job-execution-name $execution"
[ "$got" = "$want" ] ||
  die "$execution did not start with this run's settings (another caller changed $job); stop it: az containerapp job stop --name $job --resource-group $JOB_RG --job-execution-name $execution"
if ! boinc_start_guard_confirm; then
  die "$execution start visibility is uncertain; reconcile it before another launch"
fi
if [ -z "$WAIT_MINS" ]; then
  echo "$execution"
  exit 0
fi
echo "waiting up to $WAIT_MINS minutes for $job execution $execution" >&2
deadline=$(($(date +%s) + WAIT_MINS * 60))
while :; do
  status=$(baz containerapp job execution show --name "$job" --resource-group "$JOB_RG" \
    --job-execution-name "$execution" --query properties.status -o tsv) || die "can't read $execution's status"
  case $status in
    Succeeded) break ;;
    Failed | Stopped | Degraded)
      [ "$TARGET" = handoff ] || boinc_execution_log "$execution"
      die "$execution ended $status; its log is in $LOGS" ;;
  esac
  [ "$(date +%s)" -lt "$deadline" ] || die "$execution is still ${status:-starting} after $WAIT_MINS minutes"
  sleep 15
done
echo "$execution succeeded" >&2
echo "$execution"
