# shellcheck shell=bash disable=SC2034
# Shared by scripts/provision-boinc.sh and scripts/build-boinc-images.sh. Sourced from the
# repository root after ENV_NAME and ALLOW_DIRTY are set; needs a die() function.
#
# The BOINC server's settings are committed in contrib-compute/infra/boinc/env/<env>.json
# (schema alongside). Its resources are the deployment stack usnm-boinc-<env> in the group
# usnm-boinc-rg-<env>, which the main stack creates (infra/main.bicep, deployBoincGroup).

BOINC_DIR=contrib-compute/infra/boinc
BOINC_SETTINGS="$BOINC_DIR/env/$ENV_NAME.json"
# The tests (scripts/test_boinc_provision.py) run against fixture settings with stubbed
# Azure calls: USNM_BOINC_SETTINGS_FILE, honoured only with USNM_BOINC_TEST=1.
if [ -n "${USNM_BOINC_SETTINGS_FILE:-}" ]; then
  [ "${USNM_BOINC_TEST:-}" = 1 ] || { echo "error: USNM_BOINC_SETTINGS_FILE is for the tests" >&2; exit 1; }
  BOINC_SETTINGS=$USNM_BOINC_SETTINGS_FILE
fi
BOINC_RG="usnm-boinc-rg-$ENV_NAME"
BOINC_STACK="usnm-boinc-$ENV_NAME"
# Never touched by these scripts (other production resources).
FORBIDDEN_SUBSCRIPTION_PREFIX=8d356797

# Keep operator build/staging files private and on the checkout's filesystem.
boinc_workdir() {
  python3 - <<'PY'
from pathlib import Path
import uuid
work = Path.cwd() / (".boinc-work-" + uuid.uuid4().hex)
work.mkdir(mode=0o700)
print(work)
PY
}

case "$ENV_NAME" in prod | dev) ;; *) die "--env must be prod or dev (the BOINC server has no experiment environments)" ;; esac
[ -f "$BOINC_SETTINGS" ] || die "no settings file $BOINC_SETTINGS"

# The settings file and its schema must be committed, as scripts/provision.sh requires of
# infra/env/<env>.json, unless --allow-dirty (dev only).
boinc_clean_check() {
  local out
  # Fixture settings in the tests live outside the repository.
  [ -z "${USNM_BOINC_SETTINGS_FILE:-}" ] || return 0
  out=$(git status --porcelain -- "$BOINC_SETTINGS" "$BOINC_DIR/env/schema.json" 2>&1) ||
    die "can't check $BOINC_SETTINGS with git: $out"
  [ -z "$out" ] && return 0
  printf '%s\n' "$out" >&2
  $ALLOW_DIRTY || die "$BOINC_SETTINGS has uncommitted changes (above): commit them through a pull request, or pass --allow-dirty for an experiment in dev"
  [ "$ENV_NAME" != prod ] || die "--allow-dirty is refused for prod: commit $BOINC_SETTINGS first"
  echo "warning: deploying uncommitted settings (--allow-dirty)" >&2
}

# Checks the settings against the rules in schema.json, with jq, and reads them into
# ASSESSMENT, GOLD_BUNDLE, QUALITY_WAVE_SCRIPT, QUALITY_WAVE_SHA256, SUBSCRIPTION, LOCATION, PROJECT_URL, MASTER_HOST, OPERATOR_ID, OPERATOR_LOGIN,
# SERVER_IMAGE, BRIDGE_IMAGE, WORKER_IMAGE and APPVER_IMAGE.
boinc_load_settings() {
  local err
  err=$(jq -r '
    def guid: test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$");
    def acr: test("^$|^[a-z0-9.-]+\\.azurecr\\.io/[a-z0-9/_.-]+@sha256:[0-9a-f]{64}$");
    [ (keys - ["$schema", "assessment", "codeSignPublicKey", "goldBundle", "images", "keyVaultAllowedIps", "location", "onPremGroupId", "operator", "projectShare", "projectUrl", "qualityWave", "requireSecureTransport", "server", "subscriptionId"]
        | if length > 0 then "unknown keys: \(join(", "))" else empty end),
      (if keys_unsorted != keys then "keys are not sorted" else empty end),
      (if (.assessment | type) != "boolean" then "assessment must be true or false" else empty end),
      (if (.codeSignPublicKey | type) != "string" then "codeSignPublicKey must be a string" else empty end),
      (if (.goldBundle | type) != "boolean" then "goldBundle must be true or false" else empty end),
      (if has("qualityWave") and ((.qualityWave | type) != "object" or (.qualityWave | keys) != ["script", "sha256"]
          or (.qualityWave.script | test("^https://[a-z0-9]+\\.blob\\.core\\.windows\\.net/[a-z0-9-]+/[A-Za-z0-9/._-]+\\.py$") | not)
          or (.qualityWave.sha256 | test("^[0-9a-f]{64}$") | not))
        then "qualityWave must be {script: an https blob URL of a .py file, sha256: 64 hex}" else empty end),
      (if has("requireSecureTransport") and (.requireSecureTransport | type) != "boolean" then "requireSecureTransport must be true or false" else empty end),
      (if has("server") and ([.server] | inside(["azure", "onprem"]) | not) then "server must be azure or onprem" else empty end),
      (if has("projectShare") and ([.projectShare] | inside(["standard", "copy", "provisioned", "provisioned-final"]) | not) then "projectShare must be standard, copy, provisioned or provisioned-final" else empty end),
      (if has("keyVaultAllowedIps") and ((.keyVaultAllowedIps | type) != "array" or (.keyVaultAllowedIps | length) > 4 or any(.keyVaultAllowedIps[]; type != "string" or (test("^(25[0-5]|2[0-4][0-9]|1?[0-9]?[0-9])([.](25[0-5]|2[0-4][0-9]|1?[0-9]?[0-9])){3}$") | not))) then "keyVaultAllowedIps must be up to 4 public IPv4 addresses" else empty end),
      (if has("onPremGroupId") and ((.onPremGroupId | type) != "string" or (.onPremGroupId | test("^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$") | not)) then "onPremGroupId must be an object id" else empty end),
      (if (.images | type) != "object" or ((.images | keys) - ["web"]) != ["appver", "bridge", "server", "worker"] then "images must have appver, bridge, server and worker, with optional web" else empty end),
      (if has("images") and (.images | has("web")) and (.images.web | type) != "string" then "images.web must be a string" else empty end),
      (if (.images.web // "" | acr) then empty else "images.web must be empty or an immutable ACR image" end),
      (if (.images.server // "" | acr) and (.images.bridge // "" | acr) then empty else "images.server and images.bridge must be empty or <registry>.azurecr.io/<name>@sha256:<64 hex>" end),
      (if (.images.appver // "") | test("^$|^[a-z0-9.-]+\\.azurecr\\.io/usnm-boinc-appver@sha256:[0-9a-f]{64}$") then empty else "images.appver must be empty or <registry>.azurecr.io/usnm-boinc-appver@sha256:<64 hex>" end),
      (if (.images.web // "" | length > 0) and (.images.server // "" | length == 0) then "images.web needs images.server" else empty end),
      (if (.images.appver // "" | length > 0) and (.images.server // "" | length == 0) then "images.appver needs images.server (the publish job runs in the app environment)" else empty end),
      (if (.images.worker // "") | test("^$|^ghcr\\.io/tgoodyear/usnm-boinc-worker@sha256:[0-9a-f]{64}$") then empty else "images.worker must be empty or ghcr.io/tgoodyear/usnm-boinc-worker@sha256:<64 hex>" end),
      (if (.images.server // "" | length > 0) != (.images.bridge // "" | length > 0) then "images.server and images.bridge are set together (the bridge image is built on the server image)" else empty end),
      (if (.location // "") | test("^[a-z0-9]+$") then empty else "location must be an Azure region name" end),
      (if (.operator.objectId // "") | guid then empty else "operator.objectId must be a lower-case GUID" end),
      (if (.operator.login // "") | test("^[[:print:]]+$") then empty else "operator.login must be one printable line" end),
      (if .projectUrl == "https://boinc.usnewsmap.com/" or .projectUrl == "https://boinc-dev.usnewsmap.com/" then empty else "projectUrl must be https://boinc.usnewsmap.com/ or https://boinc-dev.usnewsmap.com/" end),
      (if (.subscriptionId // "") | guid then empty else "subscriptionId must be a lower-case GUID" end)
    ] | .[]' "$BOINC_SETTINGS") || die "$BOINC_SETTINGS is not valid JSON"
  [ -z "$err" ] || die "$BOINC_SETTINGS: $(printf '%s; ' "$err" | sed 's/; $//')"
  ASSESSMENT=$(jq -r .assessment "$BOINC_SETTINGS")
  GOLD_BUNDLE=$(jq -r .goldBundle "$BOINC_SETTINGS")
  QUALITY_WAVE_SCRIPT=$(jq -r '.qualityWave.script // ""' "$BOINC_SETTINGS")
  QUALITY_WAVE_SHA256=$(jq -r '.qualityWave.sha256 // ""' "$BOINC_SETTINGS")
  REQUIRE_SECURE_TRANSPORT=$(jq -r '.requireSecureTransport // false' "$BOINC_SETTINGS")
  SUBSCRIPTION=$(jq -r .subscriptionId "$BOINC_SETTINGS")
  LOCATION=$(jq -r .location "$BOINC_SETTINGS")
  PROJECT_URL=$(jq -r .projectUrl "$BOINC_SETTINGS")
  OPERATOR_ID=$(jq -r .operator.objectId "$BOINC_SETTINGS")
  OPERATOR_LOGIN=$(jq -r .operator.login "$BOINC_SETTINGS")
  WEB_IMAGE=$(jq -r '.images.web // ""' "$BOINC_SETTINGS")
  SERVER_IMAGE=$(jq -r .images.server "$BOINC_SETTINGS")
  BRIDGE_IMAGE=$(jq -r .images.bridge "$BOINC_SETTINGS")
  WORKER_IMAGE=$(jq -r .images.worker "$BOINC_SETTINGS")
  APPVER_IMAGE=$(jq -r .images.appver "$BOINC_SETTINGS")
  MASTER_HOST=${PROJECT_URL#https://}
  MASTER_HOST=${MASTER_HOST%/}
  case "$ENV_NAME:$MASTER_HOST" in
    prod:boinc.usnewsmap.com | dev:boinc-dev.usnewsmap.com) ;;
    *) die "projectUrl $PROJECT_URL is not $ENV_NAME's master URL" ;;
  esac
  case "$SUBSCRIPTION" in "$FORBIDDEN_SUBSCRIPTION_PREFIX"-*) die "refusing subscription $SUBSCRIPTION: it holds other production resources" ;; esac
}

# Every az call names the subscription; the default one is never used.
baz() { az "$@" --subscription "$SUBSCRIPTION" --only-show-errors; }
# For acr_build_verified: Azure CLI 2.90 acknowledges a --no-wait build only in a warning.
baz_build() { az "$@" --subscription "$SUBSCRIPTION"; }

boinc_az_check() {
  command -v az > /dev/null || die "az is not installed"
  command -v jq > /dev/null || die "jq is not installed"
  [ "$(az account show --subscription "$SUBSCRIPTION" --query id -o tsv 2> /dev/null)" = "$SUBSCRIPTION" ] ||
    die "az cannot see subscription $SUBSCRIPTION (az login --tenant <its tenant> first)"
  baz group show --name "$BOINC_RG" --query name -o none 2> /dev/null ||
    die "no resource group $BOINC_RG: it comes from the main stack (USNM_BOINC_GROUP true in infra/env/$ENV_NAME.json, then scripts/provision.sh --env $ENV_NAME)"
}

# boinc_output NAME: one output of the BOINC stack, empty if the stack or the output is
# missing. Matched without case: a stack can return output names with their case changed
# (scripts/lib/env.sh, save_outputs).
boinc_output() {
  local out
  if ! out=$(baz stack group show --name "$BOINC_STACK" --resource-group "$BOINC_RG" --query outputs -o json 2>&1); then
    grep -qiE 'NotFound|could not be found' <<< "$out" && return 0
    die "can't read the stack $BOINC_STACK: $out"
  fi
  jq -r --arg n "$1" '(. // {}) | to_entries[] | select((.key | ascii_downcase) == ($n | ascii_downcase)) | .value.value // empty' <<< "$out"
}

BOINC_LOCK_DIR=""
BOINC_LOCK_HELD_BY_SELF=false

boinc_pid_alive() {
  kill -0 "$1" 2>/dev/null || ps -p "$1" >/dev/null 2>&1
}

# boinc_acquire_lock LABEL: takes a host-local mutex directory per environment so concurrent
# local invocations of provision-boinc.sh, start-boinc-bridge.sh, and boinc-app-release.sh
# cannot collide. Re-entrant for child processes when USNM_BOINC_LOCK_OWNER matches the live
# lock holder PID (e.g. provision-boinc.sh calling start-boinc-bridge.sh).
boinc_acquire_lock() {
  local label=${1:-$0} holder_pid holder_cmd
  BOINC_LOCK_DIR="${USNM_BOINC_LOCK_DIR:-/var/tmp/usnm-boinc-$ENV_NAME.lock}"
  if [ -n "${USNM_BOINC_LOCK_OWNER:-}" ] && [ -f "$BOINC_LOCK_DIR/pid" ]; then
    holder_pid=$(cat "$BOINC_LOCK_DIR/pid" 2>/dev/null || true)
    if [ "$holder_pid" = "$USNM_BOINC_LOCK_OWNER" ] && boinc_pid_alive "$holder_pid"; then
      BOINC_LOCK_HELD_BY_SELF=false
      return 0
    fi
  fi
  if ! mkdir "$BOINC_LOCK_DIR" 2>/dev/null; then
    holder_pid=$(cat "$BOINC_LOCK_DIR/pid" 2>/dev/null || true)
    if [[ "$holder_pid" =~ ^[0-9]+$ ]] && ! boinc_pid_alive "$holder_pid"; then
      rm -rf "$BOINC_LOCK_DIR"
      mkdir "$BOINC_LOCK_DIR" 2>/dev/null ||
        die "another BOINC operation is holding $BOINC_LOCK_DIR; wait for it to finish"
    else
      holder_cmd=$(cat "$BOINC_LOCK_DIR/cmd" 2>/dev/null || echo "unknown")
      die "another BOINC operation is already holding $BOINC_LOCK_DIR (pid ${holder_pid:-unknown}: $holder_cmd); wait for it to finish"
    fi
  fi
  printf '%s\n' "$$" > "$BOINC_LOCK_DIR/pid"
  printf '%s\n' "$label" > "$BOINC_LOCK_DIR/cmd"
  export USNM_BOINC_LOCK_OWNER="$$"
  BOINC_LOCK_HELD_BY_SELF=true
}

boinc_release_lock() {
  if [ "${BOINC_LOCK_HELD_BY_SELF:-false}" = true ] && [ -n "${BOINC_LOCK_DIR:-}" ]; then
    rm -rf "$BOINC_LOCK_DIR"
    BOINC_LOCK_HELD_BY_SELF=false
  fi
}

# boinc_execution_log EXECUTION [WAIT_SECS]: prints the last console lines of one job execution
# from the stack's Log Analytics workspace (operators have Log Analytics Reader on it), to stderr.
# Ingestion lags a few minutes, so it polls until lines arrive or WAIT_SECS (default 300) pass.
# Best effort: it never fails the caller.
boinc_execution_log() {
  local execution=$1 wait=${2:-300} workspace rows deadline
  [[ $execution =~ ^[a-z0-9-]+$ ]] || return 0
  workspace=$(baz monitor log-analytics workspace list --resource-group "$BOINC_RG" --query "[0].customerId" -o tsv 2> /dev/null) || workspace=""
  if [ -z "$workspace" ]; then
    echo "(no Log Analytics workspace in $BOINC_RG; its log is in ContainerAppConsoleLogs_CL)" >&2
    return 0
  fi
  deadline=$(($(date +%s) + wait))
  echo "reading $execution's log from Log Analytics (up to ${wait}s for ingestion)" >&2
  while :; do
    rows=$(python3 scripts/log_analytics.py --subscription "$SUBSCRIPTION" --workspace "$workspace" --timespan P1D --query \
      "ContainerAppConsoleLogs_CL | where ContainerGroupName_s startswith '$execution-' | order by TimeGenerated desc | take 50 | order by TimeGenerated asc | project TimeGenerated, Log_s" \
      | jq -r '.tables[0].rows[] | "\(.[0])  \(.[1])"' 2> /dev/null) || rows=""
    [ -z "$rows" ] || break
    if [ "$(date +%s)" -ge "$deadline" ]; then
      echo "(no log lines for $execution yet; query ContainerAppConsoleLogs_CL in workspace $workspace later)" >&2
      return 0
    fi
    sleep 20
  done
  echo "--- $execution (last 50 lines) ---" >&2
  echo "$rows" >&2
  echo "---" >&2
}
