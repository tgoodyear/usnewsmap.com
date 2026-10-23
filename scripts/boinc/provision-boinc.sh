#!/usr/bin/env bash
# Deploy the BOINC server (ADR-0019) from its committed settings, as scripts/azure/provision.sh
# deploys the app:
#
#   # edit contrib-compute/infra/boinc/env/prod.json, open a pull request, merge, pull
#   scripts/boinc/provision-boinc.sh --env prod --what-if
#   scripts/boinc/provision-boinc.sh --env prod
#
# It deploys contrib-compute/infra/boinc/main.bicep as the deployment stack usnm-boinc-<env>
# into the existing group usnm-boinc-rg-<env> (the main stack creates it, with the guardrails
# and the two waivers of ADR-0019 decision 5), what-if first. It refuses while the settings
# file or its schema has uncommitted changes; --allow-dirty deploys them anyway, in dev only.
#
# First run: with no images in the settings it deploys the base resources (registry, network,
# vault, storage, database, monitoring). Then scripts/boinc/build-boinc-images.sh --env <env> builds
# the server and bridge images in Azure and writes their digests to the settings; commit them
# and run this again to deploy the app and the jobs.
#
# Secrets, none of which pass through this machine:
# - mysql-admin-password: a new random value made inside each deployment (main.bicep's
#   parameter default) and written to Key Vault by the same deployment;
# - mysql-runtime-password and the upload key pair: created inside the VNet by the secrets job
#   (infra/boinc/modules/secrets-job.bicep), which this script starts after each deployment.
#   Each is created only if absent, and a failed lookup stops the job;
# - code-sign-public-key: the PUBLIC half of the offline code-signing key, from the settings.
#   The private half never comes here or to Azure.
# After deployment, ensure the secrets and project, register the applications, replay the
# configured signed app version and check the public project configuration.
#
# --what-if changes no managed resources: Azure's stack-aware preview is retained for up to
# three hours (the documented range is PT1H–PT3H), including deletion, detachment and
# deny-setting changes. Needs GA `az stack-whatif` and jq, signed in to the settings'
# subscription with Owner on the BOINC group and the shared registry and DNS zone's
# group (infra/names/<env>.json rg): the
# stack writes role assignments in both, and the custom domain's records in the zone.
set -euo pipefail
usage() { echo "usage: scripts/boinc/provision-boinc.sh --env <prod|dev> [--publish-image REF] [--allow-dirty] [--what-if]" >&2; exit 2; }
ENV_NAME="" ALLOW_DIRTY=false WHAT_IF=false PUBLISH_IMAGE=""
PUBLISH_OVERRIDE=""
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --allow-dirty) ALLOW_DIRTY=true; shift ;;
    --what-if) WHAT_IF=true; shift ;;
    # Override images.appver (the publish job's image) for this deployment only.
    --publish-image) [ $# -ge 2 ] || usage; PUBLISH_OVERRIDE=$2; shift 2 ;;
    -h | --help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] || usage
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/../.."
# shellcheck source=scripts/lib/boinc.sh
. scripts/lib/boinc.sh
# shellcheck source=scripts/lib/boinc-start-guard.sh
. scripts/lib/boinc-start-guard.sh
boinc_clean_check
boinc_load_settings
boinc_az_check
# The publish job (main.bicep) runs the last published app version, images.appver.
PUBLISH_IMAGE=${PUBLISH_OVERRIDE:-$APPVER_IMAGE}

work=$(boinc_workdir)
if ! $WHAT_IF; then
  boinc_acquire_lock "provision-boinc.sh --env $ENV_NAME"
fi
cleanup() {
  local result=$?
  boinc_start_guard_release || result=1
  rm -rf "$work"
  boinc_release_lock
  exit "$result"
}
trap cleanup EXIT
# No secret goes in the parameters: the template makes the admin password itself.
names_file="$(dirname "$0")/../../infra/names/$ENV_NAME.json"
REGISTRY_NAME=$(jq -er .registry "$names_file") || die "no registry in $names_file"
REGISTRY_RG=$(jq -er .rg "$names_file") || die "no rg in $names_file"
# The custom domain (MASTER_HOST in the app's zone, infra/env/<env>.json USNM_DNS_ZONE) goes on
# in stages, because each needs the last: 0, the DNS records (CNAME and asuid TXT, written by
# this stack into the zone's group); 1, the name on the app unbound and its managed
# certificate; 2, the name bound with the certificate. domain_stage reads where it is.
# server: onprem (#942, docs/boinc-onprem.md): the server, its jobs, MySQL, the project storage,
# the workspace and the private endpoints leave this stack (the stack deletes them); only the
# identities, the vault, the VNet and the static site stay. The master URL is the on-prem host's
# tunnel, outside Azure DNS, so there is no custom domain to stage.
SERVER_MODE=$(jq -r '.server // "azure"' "$BOINC_SETTINGS")
DNS_ZONE=$(jq -r '.USNM_DNS_ZONE // empty' "$(dirname "$0")/../../infra/env/$ENV_NAME.json")
[ "$SERVER_MODE" != onprem ] || DNS_ZONE=""
[ -z "$DNS_ZONE" ] || [[ $MASTER_HOST == *".$DNS_ZONE" ]] || die "$MASTER_HOST is not in the zone $DNS_ZONE"
CERT_NAME="cert-${MASTER_HOST//./-}"
identities=$(baz identity list --resource-group "$BOINC_RG" --query '[].{name:name,principalId:principalId}' -o json) ||
  die "cannot resolve the existing managed identities for stack What-If"
principal_id() {
  jq -er --arg name "$1" '
    map(select(.name == $name)) |
    if length == 0 then ""
    elif length == 1 and (.[0].principalId | test("^[0-9a-f-]{36}$")) then .[0].principalId
    else error("invalid or duplicate managed identity principal") end' <<< "$identities"
}
runtime_principal=$(principal_id "usnm-boinc-id-$ENV_NAME") || die "invalid runtime identity"
secrets_principal=$(principal_id "usnm-boinc-secrets-id-$ENV_NAME") || die "invalid secrets identity"
domain_stage() {
  local record=${MASTER_HOST%".$DNS_ZONE"} state names
  [ -n "$DNS_ZONE" ] && [ -n "$SERVER_IMAGE" ] || { echo 0; return; }
  # No app yet (a base-only deploy): no environment, so no certificate to read. A failed
  # deployment leaves the stack with no outputs at all, so ask the app itself before deciding
  # there is none: stage 0 would unbind the live domain (#529, 2026-10-19).
  # Only a confirmed ResourceNotFound means no app; any other failed lookup stops the run.
  if [ -z "$(boinc_output appFqdn)" ]; then
    local out
    if ! out=$(baz containerapp show --name "usnm-boinc-ca-$ENV_NAME" --resource-group "$BOINC_RG" \
      --query properties.configuration.ingress.fqdn -o tsv 2>&1); then
      grep -q ResourceNotFound <<< "$out" || die "could not look up usnm-boinc-ca-$ENV_NAME: $out"
      echo 0
      return
    fi
    [ -n "$out" ] || die "usnm-boinc-ca-$ENV_NAME exists but has no ingress fqdn"
  fi
  # A failed read must stop the run: guessing a lower stage would unbind a working domain.
  state=$(baz containerapp env certificate list --name "usnm-boinc-cae-$ENV_NAME" --resource-group "$BOINC_RG" \
    --managed-certificates-only --query "[?name=='$CERT_NAME'].properties.provisioningState | [0]" -o tsv) ||
    die "could not read the managed certificates of usnm-boinc-cae-$ENV_NAME"
  if [ "$state" = Succeeded ]; then echo 2; return; fi
  names=$(baz network dns record-set cname list --zone-name "$DNS_ZONE" --resource-group "$REGISTRY_RG" --query '[].name' -o tsv) ||
    die "could not read the CNAME records of $DNS_ZONE"
  if grep -qx "$record" <<< "$names"; then echo 1; else echo 0; fi
}
# write_params STAGE: the deployment parameters, with the custom domain stage.
write_params() {
jq -n --arg runtime "$runtime_principal" --arg secrets "$secrets_principal" --argjson assess "$ASSESSMENT" --argjson gold "$GOLD_BUNDLE" --arg qws "${QUALITY_WAVE_SCRIPT:-}" --arg qwh "${QUALITY_WAVE_SHA256:-}" --argjson tls "$REQUIRE_SECURE_TRANSPORT" --arg zone "$DNS_ZONE" --arg zrg "$REGISTRY_RG" --argjson stage "$1" --arg rn "$REGISTRY_NAME" --arg rg "$REGISTRY_RG" --arg env "$ENV_NAME" --arg l "$LOCATION" --arg i "$SERVER_IMAGE" --arg b "$BRIDGE_IMAGE" --arg p "$PUBLISH_IMAGE" \
  --arg web "${WEB_IMAGE:-}" --arg h "$MASTER_HOST" --arg o "$OPERATOR_ID" --rawfile s "$BOINC_SETTINGS" '{
  "$schema": "https://schema.management.azure.com/schemas/2019-04-01/deploymentParameters.json#",
  contentVersion: "1.0.0.0",
  parameters: {
    env: {value: $env}, webImage: {value: $web}, requireSecureTransport: {value: $tls}, assessment: {value: $assess}, goldBundle: {value: $gold}, qualityWaveScript: {value: $qws}, qualityWaveScriptSha256: {value: $qwh}, registryName: {value: $rn}, registryResourceGroup: {value: $rg}, location: {value: $l}, image: {value: $i}, bridgeImage: {value: $b}, publishImage: {value: $p},
    runtimePrincipalId: {value: $runtime}, keyWriterPrincipalId: {value: $secrets},
    masterHost: {value: $h}, operatorPrincipalIds: {value: [$o]},
    codeSignPublicKey: {value: ($s | fromjson | .codeSignPublicKey)},
    projectShare: {value: ($s | fromjson | .projectShare // "standard")},
    keyVaultAllowedIps: {value: ($s | fromjson | .keyVaultAllowedIps // [])},
    onPremGroupId: {value: ($s | fromjson | .onPremGroupId // "")},
    serverOnPrem: {value: (($s | fromjson | .server // "azure") == "onprem")},
    dnsZoneName: {value: $zone}, dnsZoneResourceGroup: {value: $zrg}, customDomainStage: {value: $stage}
  }}' > "$work/params.json"
}
STAGE=$(domain_stage) || exit 1
write_params "$STAGE"
# The on-prem server mode deletes every BOINC job: refuse while any of them is running, as
# a deleteResources deployment would stop it mid-step.
if [ "$SERVER_MODE" = onprem ]; then
  # A failed listing must stop here: in a for word list its failure would pass unnoticed.
  jobs=$(baz containerapp job list --resource-group "$BOINC_RG" --query '[].name' -o tsv) ||
    die "could not list the jobs in $BOINC_RG"
  for job in $jobs; do
    running=$(baz containerapp job execution list --name "$job" --resource-group "$BOINC_RG" \
      --query "[?properties.status=='Running' || properties.status=='Processing'].name" -o tsv) ||
      die "could not list the executions of $job"
    [ -z "$running" ] || die "$job is still running (${running//$'\n'/ }); the on-prem server mode deletes it: wait for it to finish"
  done
fi

# Leaving the copy stage deletes the share copy job (deleteResources) and points /project at
# the provisioned share: only after a final pass, run with the server stopped, has succeeded.
# Its latest execution must have succeeded within two hours (a failed one has no end time:
# its start counts), and none may be running.
PROJECT_SHARE=$(jq -r '.projectShare // "standard"' "$BOINC_SETTINGS")
# The environment storage /project comes from (app.bicep): its name follows the account.
if [[ $PROJECT_SHARE == provisioned* ]]; then PROJECT_STORAGE=project-v2; else PROJECT_STORAGE=project; fi
if [[ $PROJECT_SHARE == provisioned* ]]; then
  copy_job="usnm-boinc-sharecopy-caj-$ENV_NAME"
  if out=$(baz containerapp job show --name "$copy_job" --resource-group "$BOINC_RG" --query name -o tsv 2>&1); then
    execs=$(baz containerapp job execution list --name "$copy_job" --resource-group "$BOINC_RG" \
      --query "sort_by([].{n:name,s:properties.status,t:properties.startTime,e:properties.endTime}, &t)" -o json) ||
      die "could not list the executions of $copy_job"
    jq -e 'all(.s != "Running" and .s != "Processing")' <<< "$execs" > /dev/null ||
      die "$copy_job is still running: wait for it before leaving the copy stage"
    # BOINC_COPY_ACCEPTED=<execution name>: the operator has read that pass's failed transfers
    # and accepts them (files regenerated on start, such as run_state and the web cache).
    jq -e --arg ok "${BOINC_COPY_ACCEPTED:-}" 'length > 0 and (last | (.s == "Succeeded" or ($ok != "" and .n == $ok))
      and (now - ((.e // .t) | sub("\\.[0-9]+"; "") | sub("\\+00:00$"; "Z") | fromdateiso8601)) < 7200)' <<< "$execs" > /dev/null ||
      die "the latest $copy_job execution did not succeed within the last two hours: run the final pass (docs/boinc.md, \"Moving the project share\") first"
  elif ! grep -q ResourceNotFound <<< "$out"; then
    die "could not look up $copy_job: $out"
  fi
fi

# The images must be in the shared registry: the app and the jobs pull with their identities,
# which this stack gives AcrPull there only.
registry="$REGISTRY_NAME.azurecr.io"
[ -z "$PUBLISH_IMAGE" ] || [ -n "$SERVER_IMAGE" ] || die "the publish image needs the app: set the server and bridge images first"
[ -z "$PUBLISH_IMAGE" ] || [[ $PUBLISH_IMAGE =~ ^[a-z0-9.-]+\.azurecr\.io/usnm-boinc-appver@sha256:[0-9a-f]{64}$ ]] ||
  die "the publish image must be <registry>.azurecr.io/usnm-boinc-appver@sha256:<64 hex>"
if [ -n "$SERVER_IMAGE" ]; then
  for img in "$SERVER_IMAGE" "$BRIDGE_IMAGE" ${WEB_IMAGE:+"$WEB_IMAGE"} ${PUBLISH_IMAGE:+"$PUBLISH_IMAGE"}; do
    [ "${img%%/*}" = "$registry" ] || die "$img is not in the shared registry $registry"
  done
fi

stack_settings=(--resource-group "$BOINC_RG"
  --template-file "$BOINC_DIR/main.bicep" --parameters "@$work/params.json"
  --action-on-unmanage deleteResources --deny-settings-mode denyDelete
  # Private endpoints manage their own A records in the stack's DNS zones; denyDelete
  # would otherwise block that and leave a zone group with no record.
  --deny-settings-excluded-actions
  "Microsoft.Network/privateDnsZones/A/delete Microsoft.Network/privateEndpoints/privateDnsZoneGroups/delete"
  --description "usnewsmap BOINC server $ENV_NAME (scripts/boinc/provision-boinc.sh)")
stack_args=(--name "$BOINC_STACK" "${stack_settings[@]}")

az stack-whatif group create --help > /dev/null ||
  die "this Azure CLI lacks GA stack-whatif; update Azure CLI before deploying"
preview="$BOINC_STACK-$(date -u +%Y%m%dT%H%M%SZ)-$$"
stack_id="/subscriptions/$SUBSCRIPTION/resourceGroups/$BOINC_RG/providers/Microsoft.Resources/deploymentStacks/$BOINC_STACK"
echo "previewing stack $BOINC_STACK (includes unmanaged resources and deny settings)"
az stack-whatif group create --subscription "$SUBSCRIPTION" \
  --name "$preview" --stack-id "$stack_id" "${stack_settings[@]}" \
  --retention-interval PT3H --no-color ||
  die "the stack-aware what-if of $BOINC_STACK failed; no deployment applied"
echo "preview retained for three hours: /subscriptions/$SUBSCRIPTION/resourceGroups/$BOINC_RG/providers/Microsoft.Resources/deploymentStacksWhatIfResults/$preview"
if $WHAT_IF; then
  [ -n "$SERVER_IMAGE" ] || echo "no images in $BOINC_SETTINGS yet: this deploys the base resources only; then scripts/boinc/build-boinc-images.sh --env $ENV_NAME"
  exit 0
fi

bridge_job_template_matches() {
  local template_json=$1 cpu=$2 memory=$3 extra_env_json=$4
  local expected_kv expected_db_host
  expected_kv=$(boinc_output keyVaultName) || return 1
  expected_db_host=$(boinc_output mysqlHost) || return 1
  jq -e --arg img "$BRIDGE_IMAGE" --arg master_host "$MASTER_HOST" \
    --arg kv "$expected_kv" --arg db_host "$expected_db_host" \
    --argjson cpu "$cpu" --arg memory "$memory" \
    --argjson extra_env "$extra_env_json" --arg storage "$PROJECT_STORAGE" '
    (.containers | length) == 1 and
    .containers[0].name == "bridge" and
    .containers[0].image == $img and
    ((.containers[0].command // ["/usr/local/bin/bridge-job"]) == ["/usr/local/bin/bridge-job"]) and
    .containers[0].resources.cpu == $cpu and
    .containers[0].resources.memory == $memory and
    .containers[0].volumeMounts == [{"volumeName": "project", "mountPath": "/project"}] and
    (.volumes | length) == 1 and
    .volumes[0].name == "project" and
    .volumes[0].storageType == "AzureFile" and
    .volumes[0].storageName == $storage and
    .volumes[0].mountOptions == "mfsymlinks,uid=1000,gid=1000,dir_mode=0775,file_mode=0775,cache=strict" and
    ((.containers[0].env // []) | all(((keys - ["name", "value"]) | length) == 0 and (.value | type == "string" and length > 0))) and
    (
      ([(.containers[0].env // [])[] | select(.name == "AZURE_CLIENT_ID" or .name == "KEY_VAULT_NAME" or .name == "DB_HOST") | .name] | sort)
      == ["AZURE_CLIENT_ID", "DB_HOST", "KEY_VAULT_NAME"]
    ) and
    (if $kv != "" then ([(.containers[0].env // [])[] | select(.name == "KEY_VAULT_NAME") | .value] == [$kv]) else true end) and
    (if $db_host != "" then ([(.containers[0].env // [])[] | select(.name == "DB_HOST") | .value] == [$db_host]) else true end) and
    (
      [(.containers[0].env // [])[] | select(.name != "AZURE_CLIENT_ID" and .name != "KEY_VAULT_NAME" and .name != "DB_HOST") | {name, value}]
      | sort_by(.name)
    ) == (
      (
        [
          {"name": "DB_NAME", "value": "usnmocr"},
          {"name": "HANDOFF_DIR", "value": "/project/handoff"},
          {"name": "MASTER_HOST", "value": $master_host},
          {"name": "PROJECT_DIR", "value": "/project"}
        ] + $extra_env
      ) | sort_by(.name)
    )
  ' <<< "$template_json" > /dev/null
}

for lifecycle_output in secretsJobName initJobName publishJobName; do
  existing_job=$(boinc_output "$lifecycle_output")
  if [ -n "$existing_job" ]; then
    active_job=$(baz containerapp job execution list --name "$existing_job" --resource-group "$BOINC_RG" \
      --query "[?properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv) ||
      die "could not list the executions of $existing_job"
    [ -z "$active_job" ] ||
      die "$existing_job already has an active execution (${active_job//$'\n'/ }); wait for it to finish before mutating $existing_job"
  fi
done
existing_gold=$(boinc_output goldBundleJobName)
if [ -n "$existing_gold" ]; then
  active_gold=$(baz containerapp job execution list --name "$existing_gold" --resource-group "$BOINC_RG" \
    --query "[?properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv) ||
    die "could not list the executions of $existing_gold"
  if [ -n "$active_gold" ]; then
    $GOLD_BUNDLE ||
      die "$existing_gold already has an active execution (${active_gold//$'\n'/ }) and goldBundle=false would remove it; wait for it to finish before disabling goldBundle"
    gold_template=$(baz containerapp job show --name "$existing_gold" --resource-group "$BOINC_RG" --query properties.template -o json) ||
      die "could not read $existing_gold template while active execution (${active_gold//$'\n'/ }) is running"
    if ! bridge_job_template_matches "$gold_template" 1 2Gi \
      '[{"name":"BRIDGE_STEP","value":"gold-bundle"},{"name":"GOLD_CAMPAIGN","value":"v1-tess550"}]'; then
      die "$existing_gold already has an active execution (${active_gold//$'\n'/ }) and its template would change; wait for it to finish before updating the gold-bundle template"
    fi
  fi
fi
# Removing qualityWave deletes the quality job (the stack deletes unmanaged resources), so it
# must not happen mid-wave. Updating the job never blocks: a running wave keeps its template.
existing_quality=""
[ -n "${QUALITY_WAVE_SCRIPT:-}" ] || existing_quality=$(boinc_output qualityWaveJobName)
if [ -n "$existing_quality" ]; then
  active_quality=$(baz containerapp job execution list --name "$existing_quality" --resource-group "$BOINC_RG" \
    --query "[?properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv) ||
    die "could not list the executions of $existing_quality"
  [ -z "$active_quality" ] ||
    die "$existing_quality has an active execution (${active_quality//$'\n'/ }) and removing qualityWave would delete it; wait for it to finish before removing qualityWave"
fi
active_bridge=""
existing_bridge=$(boinc_output bridgeJobName)
if [ -n "$existing_bridge" ]; then
  JOB_RG="$BOINC_RG"
  job="$existing_bridge"
  boinc_start_guard_acquire
  active_bridge=$(baz containerapp job execution list --name "$existing_bridge" --resource-group "$BOINC_RG" \
    --query "[?properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv) ||
    die "could not list the executions of $existing_bridge"
  if [ -n "$active_bridge" ]; then
    bridge_template=$(baz containerapp job show --name "$existing_bridge" --resource-group "$BOINC_RG" --query properties.template -o json) ||
      die "could not read $existing_bridge template while active execution (${active_bridge//$'\n'/ }) is running"
    bridge_extra_env='[]'
    if [ "$ENV_NAME" = prod ]; then
      bridge_extra_env='[{"name":"BRIDGE_STEP","value":"iris-bundle-collect"},{"name":"IRIS_CAMPAIGN","value":"overproof-100-v12"}]'
    fi
    if ! bridge_job_template_matches "$bridge_template" 0.5 1Gi "$bridge_extra_env"; then
      die "$existing_bridge already has an active execution (${active_bridge//$'\n'/ }) and its template would change; wait for it to finish before updating the bridge template"
    fi
  fi
fi

echo "deploying stack $BOINC_STACK (custom domain stage $STAGE)"
[ -z "$BOINC_START_LEASE" ] || { BOINC_START_SAFE_TO_RELEASE=false; BOINC_START_UNCERTAIN=deploy; }
baz stack group create "${stack_args[@]}" --yes -o none || die "the stack $BOINC_STACK failed"
[ -z "$BOINC_START_LEASE" ] || { BOINC_START_SAFE_TO_RELEASE=true; BOINC_START_UNCERTAIN=""; }
echo "deployed stack $BOINC_STACK"
# Wait for DNS/certificate readiness instead of requiring another manual deployment.
for _ in $(seq 1 60); do
  next=$(domain_stage) || exit 1
  if [ "$next" -le "$STAGE" ]; then
    [ -z "$DNS_ZONE" ] || [ -z "$SERVER_IMAGE" ] || [ "$STAGE" -lt 2 ] || break
    [ -n "$DNS_ZONE" ] && [ -n "$SERVER_IMAGE" ] || break
    sleep 10
    continue
  fi
  STAGE=$next
  write_params "$STAGE"
  echo "deploying stack $BOINC_STACK again for custom domain stage $STAGE"
  [ -z "$BOINC_START_LEASE" ] || { BOINC_START_SAFE_TO_RELEASE=false; BOINC_START_UNCERTAIN=deploy; }
  baz stack group create "${stack_args[@]}" --yes -o none || die "the stack $BOINC_STACK failed at custom domain stage $STAGE"
  [ -z "$BOINC_START_LEASE" ] || { BOINC_START_SAFE_TO_RELEASE=true; BOINC_START_UNCERTAIN=""; }
  [ "$STAGE" -lt 2 ] || break
done
[ -z "$DNS_ZONE" ] || [ -z "$SERVER_IMAGE" ] || [ "$STAGE" -eq 2 ] ||
  die "custom domain $MASTER_HOST did not reach its bound-certificate state"

if [ "$SERVER_MODE" = onprem ]; then
  # Nothing in Azure to start: the on-prem server must answer at the master URL.
  boinc_check_public_health "$work/project.xml"
  echo "BOINC Azure resources reduced to the on-prem set; the server answers at ${PROJECT_URL}"
  exit 0
fi
if [ -z "$SERVER_IMAGE" ]; then
  echo "base resources deployed. Next: scripts/boinc/build-boinc-images.sh --env $ENV_NAME, commit $BOINC_SETTINGS, then run this again."
  exit 0
fi

run_job() {
  local job=$1 template=${2:-} running execution status=""
  local start_args=()
  [ -z "$template" ] || start_args+=(--yaml "$template")
  [ -n "$job" ] || die "the stack is missing a required lifecycle job output"
  running=$(baz containerapp job execution list --name "$job" --resource-group "$BOINC_RG" \
    --query "[?properties.status!='Succeeded' && properties.status!='Failed' && properties.status!='Stopped' && properties.status!='Degraded'].name" -o tsv) ||
    die "could not list the executions of $job"
  [ -z "$running" ] || die "$job already has an active execution ($running); refusing concurrent lifecycle changes"
  [ -z "$template" ] || { BOINC_START_SAFE_TO_RELEASE=false; BOINC_START_UNCERTAIN=start; }
  execution=$(baz containerapp job start --name "$job" --resource-group "$BOINC_RG" ${start_args[@]+"${start_args[@]}"} --query name -o tsv) ||
    die "could not start $job"
  [ -n "$execution" ] || die "$job returned no execution"
  if [ -n "$template" ]; then
    boinc_start_guard_confirm || die "could not confirm registration execution visibility"
  fi
  echo "waiting for $job ($execution)"
  for _ in $(seq 1 90); do
    status=$(baz containerapp job execution show --name "$job" --resource-group "$BOINC_RG" \
      --job-execution-name "$execution" --query properties.status -o tsv) || die "could not read $execution"
    case "$status" in Succeeded | Failed | Stopped | Degraded) break ;; esac
    sleep 10
  done
  if [ "$status" != Succeeded ]; then
    boinc_execution_log "$execution"
    die "$job execution $execution ended $status"
  fi
  echo "$job complete ($execution)"
}

run_job "$(boinc_output secretsJobName)"
run_job "$(boinc_output initJobName)"
register_apps() {
  local job JOB_RG="$BOINC_RG" template="$work/register-apps.json"
  job=$(boinc_output bridgeJobName)
  [ -n "$job" ] || die "missing bridge job for registration"
  if [ -z "$BOINC_START_LEASE" ]; then
    boinc_start_guard_acquire
  fi
  baz containerapp job show --name "$job" --resource-group "$JOB_RG" --query properties.template -o json |
    jq -e '
      if (.containers | length) != 1 then error("expected one registration container") else . end |
      .containers[0].env = ((.containers[0].env // [] | map(select(.name != "BRIDGE_STEP"))) +
        [{name: "BRIDGE_STEP", value: "register-apps"}])' > "$template" ||
    die "cannot prepare the complete registration execution template"
  run_job "$job" "$template"
  boinc_start_guard_release || die "cannot release the registration start lease"
}
if [ -n "$active_bridge" ]; then
  echo "deferring $existing_bridge registration while ${active_bridge//$'\n'/ } is running; bridge template is unchanged"
  JOB_RG="$BOINC_RG"
  job="$existing_bridge"
  boinc_start_guard_release || die "cannot release the registration start lease"
else
  register_apps
fi
if [ -n "$PUBLISH_IMAGE" ]; then
  run_job "$(boinc_output publishJobName)"
fi

boinc_check_public_health "$work/project.xml"
if [ "${BOINC_MAINTENANCE:-0}" = 1 ]; then
  echo "BOINC deployment up in maintenance: ${PROJECT_URL} (scheduler may be closed); after resuming, check again without BOINC_MAINTENANCE"
else
  echo "BOINC deployment ready: ${PROJECT_URL} (secrets, project, app registration and public health verified)"
fi
workspace=$(baz monitor log-analytics workspace list --resource-group "$BOINC_RG" \
  --query '[0].customerId' -o tsv) || workspace=""
if [ -z "$workspace" ]; then
  echo "warning: workbook live validation incomplete; run scripts/boinc/validate-boinc-workbook.py separately" >&2
else
  workbook_receipt_dir=$(boinc_workdir)
  workbook_query_host=$(boinc_output logAnalyticsQueryHost) || workbook_query_host=""
  workbook_query_hosts=$(boinc_output logAnalyticsQueryHosts | jq -r 'if type == "array" then join(",") else empty end') || workbook_query_hosts=""
  if ! (umask 077
    export LOG_ANALYTICS_QUERY_HOST="${workbook_query_host:-${LOG_ANALYTICS_QUERY_HOST:-}}"
    if [ "${LOG_ANALYTICS_QUERY_HOSTS+x}" != x ] && [ -n "$workbook_query_hosts" ]; then
      export LOG_ANALYTICS_QUERY_HOSTS="$workbook_query_hosts"
    fi
    python3 scripts/boinc/validate-boinc-workbook.py \
    --workspace "$workspace" --subscription "$SUBSCRIPTION" > "$workbook_receipt_dir/workbook-validation.json"); then
    echo "warning: workbook live validation incomplete; inspect the retained receipt" >&2
  fi
  echo "Workbook validation receipt retained: $workbook_receipt_dir/workbook-validation.json" >&2
fi
