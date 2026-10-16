#!/usr/bin/env bash
# Deploy the BOINC server (ADR-0019) from its committed settings, as scripts/provision.sh
# deploys the app:
#
#   # edit contrib-compute/infra/boinc/env/prod.json, open a pull request, merge, pull
#   scripts/provision-boinc.sh --env prod --what-if
#   scripts/provision-boinc.sh --env prod
#
# It deploys contrib-compute/infra/boinc/main.bicep as the deployment stack usnm-boinc-<env>
# into the existing group usnm-boinc-rg-<env> (the main stack creates it, with the guardrails
# and the two waivers of ADR-0019 decision 5), what-if first. It refuses while the settings
# file or its schema has uncommitted changes; --allow-dirty deploys them anyway, in dev only.
#
# First run: with no images in the settings it deploys the base resources (registry, network,
# vault, storage, database, monitoring). Then scripts/build-boinc-images.sh --env <env> builds
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
# --what-if changes no managed resources: Azure's stack-aware preview is retained for three
# hours, including deletion, detachment and deny-setting changes. Needs az with stack-whatif
# support and jq, signed in to the settings' subscription with Owner on the
# BOINC group and on the shared registry and DNS zone's group (infra/names/<env>.json rg): the
# stack writes role assignments in both, and the custom domain's records in the zone.
set -euo pipefail
usage() { echo "usage: scripts/provision-boinc.sh --env <prod|dev> [--publish-image REF] [--allow-dirty] [--what-if]" >&2; exit 2; }
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
cd "$(dirname "$0")/.."
# shellcheck source=scripts/lib/boinc.sh
. scripts/lib/boinc.sh
boinc_clean_check
boinc_load_settings
boinc_az_check
# The publish job (main.bicep) runs the last published app version, images.appver.
PUBLISH_IMAGE=${PUBLISH_OVERRIDE:-$APPVER_IMAGE}

work=$(boinc_workdir)
trap 'rm -rf "$work"' EXIT
# No secret goes in the parameters: the template makes the admin password itself.
names_file="$(dirname "$0")/../infra/names/$ENV_NAME.json"
REGISTRY_NAME=$(jq -er .registry "$names_file") || die "no registry in $names_file"
REGISTRY_RG=$(jq -er .rg "$names_file") || die "no rg in $names_file"
# The custom domain (MASTER_HOST in the app's zone, infra/env/<env>.json USNM_DNS_ZONE) goes on
# in stages, because each needs the last: 0, the DNS records (CNAME and asuid TXT, written by
# this stack into the zone's group); 1, the name on the app unbound and its managed
# certificate; 2, the name bound with the certificate. domain_stage reads where it is.
DNS_ZONE=${BOINC_DNS_ZONE:-$(jq -r '.USNM_DNS_ZONE // empty' "$(dirname "$0")/../infra/env/$ENV_NAME.json")}
DNS_RG=${BOINC_DNS_RG:-$REGISTRY_RG}
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
  # No app in the stack yet (a base-only deploy): no environment, so no certificate to read.
  [ -n "$(boinc_output appFqdn)" ] || { echo 0; return; }
  # A failed read must stop the run: guessing a lower stage would unbind a working domain.
  state=$(baz containerapp env certificate list --name "usnm-boinc-cae-$ENV_NAME" --resource-group "$BOINC_RG" \
    --managed-certificates-only --query "[?name=='$CERT_NAME'].properties.provisioningState | [0]" -o tsv) ||
    die "could not read the managed certificates of usnm-boinc-cae-$ENV_NAME"
  if [ "$state" = Succeeded ]; then echo 2; return; fi
  names=$(baz network dns record-set cname list --zone-name "$DNS_ZONE" --resource-group "$DNS_RG" --query '[].name' -o tsv) ||
    die "could not read the CNAME records of $DNS_ZONE"
  if grep -qx "$record" <<< "$names"; then echo 1; else echo 0; fi
}
# write_params STAGE: the deployment parameters, with the custom domain stage.
write_params() {
jq -n --arg runtime "$runtime_principal" --arg secrets "$secrets_principal" --argjson assess "$ASSESSMENT" --argjson gold "$GOLD_BUNDLE" --arg zone "$DNS_ZONE" --arg zrg "$DNS_RG" --argjson stage "$1" --arg rn "$REGISTRY_NAME" --arg rg "$REGISTRY_RG" --arg env "$ENV_NAME" --arg l "$LOCATION" --arg i "$SERVER_IMAGE" --arg b "$BRIDGE_IMAGE" --arg p "$PUBLISH_IMAGE" \
  --arg h "$MASTER_HOST" --arg o "$OPERATOR_ID" --rawfile s "$BOINC_SETTINGS" '{
  "$schema": "https://schema.management.azure.com/schemas/2019-04-01/deploymentParameters.json#",
  contentVersion: "1.0.0.0",
  parameters: {
    env: {value: $env}, assessment: {value: $assess}, goldBundle: {value: $gold}, registryName: {value: $rn}, registryResourceGroup: {value: $rg}, location: {value: $l}, image: {value: $i}, bridgeImage: {value: $b}, publishImage: {value: $p},
    runtimePrincipalId: {value: $runtime}, keyWriterPrincipalId: {value: $secrets},
    masterHost: {value: $h}, operatorPrincipalIds: {value: [$o]},
    codeSignPublicKey: {value: ($s | fromjson | .codeSignPublicKey)},
    dnsZoneName: {value: $zone}, dnsZoneResourceGroup: {value: $zrg}, customDomainStage: {value: $stage}
  }}' > "$work/params.json"
}
STAGE=$(domain_stage) || exit 1
write_params "$STAGE"

# The images must be in the shared registry: the app and the jobs pull with their identities,
# which this stack gives AcrPull there only.
registry="$REGISTRY_NAME.azurecr.io"
[ -z "$PUBLISH_IMAGE" ] || [ -n "$SERVER_IMAGE" ] || die "the publish image needs the app: set the server and bridge images first"
[ -z "$PUBLISH_IMAGE" ] || [[ $PUBLISH_IMAGE =~ ^[a-z0-9.-]+\.azurecr\.io/usnm-boinc-appver@sha256:[0-9a-f]{64}$ ]] ||
  die "the publish image must be <registry>.azurecr.io/usnm-boinc-appver@sha256:<64 hex>"
if [ -n "$SERVER_IMAGE" ]; then
  for img in "$SERVER_IMAGE" "$BRIDGE_IMAGE" ${PUBLISH_IMAGE:+"$PUBLISH_IMAGE"}; do
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
  --description "usnewsmap BOINC server $ENV_NAME (scripts/provision-boinc.sh)")
stack_args=(--name "$BOINC_STACK" "${stack_settings[@]}")

az stack-whatif group create --help > /dev/null ||
  die "this Azure CLI lacks stack-whatif; update Azure CLI before deploying"
preview="$BOINC_STACK-$(date -u +%Y%m%dT%H%M%SZ)-$$"
stack_id="/subscriptions/$SUBSCRIPTION/resourceGroups/$BOINC_RG/providers/Microsoft.Resources/deploymentStacks/$BOINC_STACK"
echo "previewing stack $BOINC_STACK (includes unmanaged resources and deny settings)"
baz stack-whatif group create --name "$preview" --stack-id "$stack_id" \
  "${stack_settings[@]}" --retention-interval PT3H --no-color ||
  die "the stack-aware what-if of $BOINC_STACK failed; no deployment applied"
echo "preview retained for three hours: /subscriptions/$SUBSCRIPTION/resourceGroups/$BOINC_RG/providers/Microsoft.Resources/deploymentStacksWhatIfResults/$preview"
if $WHAT_IF; then
  [ -n "$SERVER_IMAGE" ] || echo "no images in $BOINC_SETTINGS yet: this deploys the base resources only; then scripts/build-boinc-images.sh --env $ENV_NAME"
  exit 0
fi

echo "deploying stack $BOINC_STACK (custom domain stage $STAGE)"
baz stack group create "${stack_args[@]}" --yes -o none || die "the stack $BOINC_STACK failed"
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
  baz stack group create "${stack_args[@]}" --yes -o none || die "the stack $BOINC_STACK failed at custom domain stage $STAGE"
  [ "$STAGE" -lt 2 ] || break
done
[ -z "$DNS_ZONE" ] || [ -z "$SERVER_IMAGE" ] || [ "$STAGE" -eq 2 ] ||
  die "custom domain $MASTER_HOST did not reach its bound-certificate state"

if [ -z "$SERVER_IMAGE" ]; then
  echo "base resources deployed. Next: scripts/build-boinc-images.sh --env $ENV_NAME, commit $BOINC_SETTINGS, then run this again."
  exit 0
fi

run_job() {
  local job=$1 running execution status=""
  [ -n "$job" ] || die "the stack is missing a required lifecycle job output"
  running=$(baz containerapp job execution list --name "$job" --resource-group "$BOINC_RG" \
    --query "[?properties.status=='Running' || properties.status=='Processing'].name" -o tsv) ||
    die "could not list the executions of $job"
  [ -z "$running" ] || die "$job already has an active execution ($running); refusing concurrent lifecycle changes"
  execution=$(baz containerapp job start --name "$job" --resource-group "$BOINC_RG" --query name -o tsv) ||
    die "could not start $job"
  [ -n "$execution" ] || die "$job returned no execution"
  echo "waiting for $job ($execution)"
  for _ in $(seq 1 90); do
    status=$(baz containerapp job execution show --name "$job" --resource-group "$BOINC_RG" \
      --job-execution-name "$execution" --query properties.status -o tsv) || die "could not read $execution"
    case "$status" in Succeeded | Failed | Stopped | Degraded) break ;; esac
    sleep 10
  done
  [ "$status" = Succeeded ] ||
    die "$job execution $execution ended $status; inspect its ContainerAppConsoleLogs_CL logs"
  echo "$job complete ($execution)"
}

run_job "$(boinc_output secretsJobName)"
run_job "$(boinc_output initJobName)"
scripts/start-boinc-bridge.sh --env "$ENV_NAME" --step register-apps --wait-mins 15
if [ -n "$PUBLISH_IMAGE" ]; then
  run_job "$(boinc_output publishJobName)"
fi

curl --fail --silent --show-error --retry 12 --retry-delay 5 --retry-all-errors \
  --connect-timeout 10 --max-time 15 "${PROJECT_URL}usnmocr/get_project_config.php" > "$work/project.xml" ||
  die "the public BOINC project endpoint is not healthy"
python3 - "$work/project.xml" "$PROJECT_URL" <<'PY'
import sys
import xml.etree.ElementTree as ET

config = ET.parse(sys.argv[1]).getroot()
if (config.tag != "project_config" or config.findtext("master_url") != sys.argv[2]
        or config.findtext("web_stopped") != "0" or config.findtext("sched_stopped") != "0"):
    raise SystemExit("BOINC project configuration is stopped, invalid or names a different master URL")
PY
echo "BOINC deployment ready: ${PROJECT_URL} (secrets, project, app registration and public health verified)"
