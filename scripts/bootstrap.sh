#!/usr/bin/env bash
# Stand up (or bring up to date) one usnewsmap environment in any Azure
# subscription or tenant, and wire this GitHub repository to deploy it
# (08 §8.9). Safe to re-run: every step is idempotent.
#
#   scripts/bootstrap.sh <env> [options]
#
#   --subscription ID     Azure subscription (default: az's current one)
#   --location REGION     default eastus2 (new environments only)
#   --repo OWNER/NAME     GitHub repository that deploys it (default: this clone's)
#   --domain NAME         create the public DNS zone for NAME (e.g. usnewsmap.com)
#   --alert-email ADDR    alerts and the $80 budget go to ADDR
#   --ingest              deploy the ingest and backfill jobs
#   --cleanup-legacy      remove the repository-level variables and SWA token
#                         secret used before per-environment deployment
#
# Needs: az, azd, gh and curl, signed in (`az login --tenant …`,
# `azd auth login --tenant-id …`, `gh auth login`), Owner on the subscription
# and admin on the GitHub repository.
set -euo pipefail

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
[ $# -ge 1 ] || usage
ENV_NAME=$1; shift
case "$ENV_NAME" in -*|"") usage ;; esac
SUBSCRIPTION="" LOCATION="eastus2" REPO="" DOMAIN="" EMAIL="" INGEST=false CLEANUP=false
while [ $# -gt 0 ]; do
  case "$1" in
    --subscription) SUBSCRIPTION=$2; shift 2 ;;
    --location) LOCATION=$2; shift 2 ;;
    --repo) REPO=$2; shift 2 ;;
    --domain) DOMAIN=$2; shift 2 ;;
    --alert-email) EMAIL=$2; shift 2 ;;
    --ingest) INGEST=true; shift ;;
    --cleanup-legacy) CLEANUP=true; shift ;;
    *) usage ;;
  esac
done

step() { printf '\n==> %s\n' "$*"; }
die() { echo "error: $*" >&2; exit 1; }
for tool in az azd gh curl; do command -v "$tool" > /dev/null || die "$tool is not installed"; done
cd "$(dirname "$0")/.."

step "Checking sign-ins"
az account show -o none 2> /dev/null || die "run: az login [--tenant TENANT]"
[ -n "$SUBSCRIPTION" ] && az account set --subscription "$SUBSCRIPTION"
SUBSCRIPTION=$(az account show --query id -o tsv)
TENANT=$(az account show --query tenantId -o tsv)
azd auth login --check-status > /dev/null 2>&1 || die "run: azd auth login --tenant-id $TENANT"
gh auth status > /dev/null 2>&1 || die "run: gh auth login"
[ -n "$REPO" ] || REPO=$(gh repo view --json nameWithOwner -q .nameWithOwner)
echo "subscription $SUBSCRIPTION (tenant $TENANT), repository $REPO, environment $ENV_NAME"

step "Registering resource providers"
for ns in Microsoft.App Microsoft.ContainerRegistry Microsoft.DocumentDB Microsoft.Web \
  Microsoft.Network Microsoft.Storage Microsoft.OperationalInsights Microsoft.Insights \
  Microsoft.ManagedIdentity Microsoft.Consumption Microsoft.PolicyInsights; do
  [ "$(az provider show -n "$ns" --query registrationState -o tsv 2> /dev/null)" = Registered ] ||
    az provider register -n "$ns" --wait -o none
done

step "Configuring the azd environment"
azd env select "$ENV_NAME" 2> /dev/null ||
  azd env new "$ENV_NAME" --subscription "$SUBSCRIPTION" --location "$LOCATION" --no-prompt
aget() { azd env get-value "$1" 2> /dev/null || true; }
azd env set AZURE_SUBSCRIPTION_ID "$SUBSCRIPTION"
[ -n "$(aget AZURE_LOCATION)" ] || azd env set AZURE_LOCATION "$LOCATION"
azd env set USNM_GITHUB_REPO "$REPO"
azd env set USNM_GITHUB_REPO_IDS \
  "$(gh api "repos/$REPO" --jq '"\(.owner.login)@\(.owner.id)/\(.name)@\(.id)"')"
[ -n "$DOMAIN" ] && azd env set USNM_DNS_ZONE "$DOMAIN"
if [ -n "$EMAIL" ]; then
  azd env set USNM_ALERT_EMAILS "$EMAIL"
  # A budget's start date can never change: set it once.
  [ -n "$(aget USNM_BUDGET_START)" ] || azd env set USNM_BUDGET_START "$(date -u +%Y-%m-01)"
fi
[ "$INGEST" = true ] && azd env set USNM_INGEST_JOBS true
# One Cosmos DB free-tier account per subscription: use it only if it's free.
if [ -z "$(aget USNM_COSMOS_FREE_TIER)" ]; then
  ours="cosmos-usnm-$(tr '[:upper:]' '[:lower:]' <<< "$ENV_NAME")-"
  others=$(az cosmosdb list --query "[?enableFreeTier && !starts_with(name, '$ours')].name" -o tsv)
  if [ -n "$others" ]; then
    echo "the subscription's free tier is taken by: $others"
    azd env set USNM_COSMOS_FREE_TIER false
  fi
fi
USE_ACR=$(aget USNM_USE_ACR)
[ -n "$USE_ACR" ] || { USE_ACR=false; azd env set USNM_USE_ACR false; }

step "Provisioning"
azd provision --no-prompt

step "Wiring GitHub Environment '$ENV_NAME' in $REPO"
# Only main may deploy: the CI identity trusts this environment's jobs.
gh api -X PUT "repos/$REPO/environments/$ENV_NAME" --input - > /dev/null << 'EOF'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
EOF
gh api "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies" --jq '.branch_policies[].name' |
  grep -qx main ||
  gh api -X POST "repos/$REPO/environments/$ENV_NAME/deployment-branch-policies" \
    -f name=main -f type=branch > /dev/null
evar() { gh variable set "$1" --env "$ENV_NAME" -R "$REPO" --body "$2"; }
evar USNM_ACR_LOGIN_SERVER "$(aget ACR_LOGIN_SERVER)"
evar USNM_CI_CLIENT_ID "$(aget CI_CLIENT_ID)"
evar AZURE_TENANT_ID "$(aget AZURE_TENANT_ID)"
evar AZURE_SUBSCRIPTION_ID "$(aget AZURE_SUBSCRIPTION_ID)"
evar USNM_RESOURCE_GROUP "$(aget AZURE_RESOURCE_GROUP)"
evar USNM_SWA_NAME "$(aget SWA_NAME)"
# List the environment for the deploy workflows.
envs=$(gh variable get USNM_DEPLOY_ENVIRONMENTS -R "$REPO" 2> /dev/null || true)
if ! grep -q "\"$ENV_NAME\"" <<< "$envs"; then
  # A JSON array of names: ["dev","prod"].
  names=$( (tr -d '[]" ' <<< "$envs" | tr ',' '\n'; echo "$ENV_NAME") | grep -v '^$' | sort -u)
  envs="[$(sed 's/.*/"&"/' <<< "$names" | paste -sd, -)]"
  gh variable set USNM_DEPLOY_ENVIRONMENTS -R "$REPO" --body "$envs"
fi
echo "deploy environments: $envs"

# Start a workflow on main and wait for it to finish.
run_and_wait() {
  local workflow=$1; shift
  local since
  since=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  gh workflow run "$workflow" -R "$REPO" --ref main "$@"
  local id=""
  for _ in $(seq 30); do
    sleep 5
    id=$(gh run list -R "$REPO" --workflow "$workflow" --branch main --event workflow_dispatch \
      --limit 5 --json databaseId,createdAt \
      --jq "[.[] | select(.createdAt >= \"$since\")][0].databaseId // empty")
    [ -n "$id" ] && break
  done
  [ -n "$id" ] || die "the $workflow run did not start"
  gh run watch "$id" -R "$REPO" --exit-status > /dev/null || die "the $workflow run failed: gh run view $id -R $REPO --log-failed"
}

ACR=$(aget ACR_NAME)
if ! az acr repository show-tags -n "$ACR" --repository usnewsmap-api -o tsv 2> /dev/null | grep -qx main; then
  step "Publishing images to $ACR (ci on main)"
  run_and_wait ci
fi

if [ "$USE_ACR" != true ]; then
  step "Switching to the private registry"
  azd env set USNM_USE_ACR true
  azd env set USNM_API_IMAGE ""
  # A new pull permission can take a few minutes to apply.
  for attempt in 1 2 3; do
    azd provision --no-prompt && break
    [ "$attempt" = 3 ] && die "provisioning failed"
    echo "retrying in 60s"; sleep 60
  done
fi

# Names on the site and the API, once the registrar delegates the domain to
# the zone (the certificates are validated through DNS).
DOMAIN=$(aget USNM_DNS_ZONE)
if [ -n "$DOMAIN" ]; then
  step "Custom domains for $DOMAIN"
  if ! curl -fsS -H 'accept: application/dns-json' "https://cloudflare-dns.com/dns-query?name=$DOMAIN&type=NS" |
    grep -q azure-dns; then
    echo "$DOMAIN is not delegated to Azure DNS yet. Set its name servers to:"
    echo "  $(aget NAME_SERVERS)"
    echo "then re-run this script to bind the names."
  else
    RG=$(aget AZURE_RESOURCE_GROUP) SWA=$(aget SWA_NAME) APP=$(aget API_APP) CAE=$(aget CONTAINER_ENV_NAME)
    names() { tr '\t' '\n' | grep -Fqx "$1"; }
    # The site: www validates by its CNAME; the apex by a TXT token in the zone.
    hosts=$(az staticwebapp hostname list -n "$SWA" -g "$RG" --query "[].[name, domainName]" -o tsv)
    names "www.$DOMAIN" <<< "$hosts" ||
      az staticwebapp hostname set -n "$SWA" -g "$RG" --hostname "www.$DOMAIN" -o none
    if ! names "$DOMAIN" <<< "$hosts"; then
      az staticwebapp hostname set -n "$SWA" -g "$RG" --hostname "$DOMAIN" \
        --validation-method dns-txt-token --no-wait -o none
      token=""
      for _ in $(seq 30); do
        token=$(az staticwebapp hostname show -n "$SWA" -g "$RG" --hostname "$DOMAIN" \
          --query "validationToken || properties.validationToken" -o tsv 2> /dev/null || true)
        [ -n "$token" ] && break
        sleep 10
      done
      [ -n "$token" ] || die "Static Web Apps gave no validation token for $DOMAIN"
      az network dns record-set txt add-record -g "$RG" -z "$DOMAIN" -n _dnsauth -v "$token" -o none
      echo "$DOMAIN is validating (usually minutes, up to a few hours); it serves the site once done"
    fi
    # The API: a managed certificate, recorded so that provisioning keeps the binding.
    if [ -z "$(aget USNM_API_CERT_ID)" ]; then
      az containerapp hostname list -n "$APP" -g "$RG" --query "[].name" -o tsv | names "api.$DOMAIN" ||
        az containerapp hostname add -n "$APP" -g "$RG" --hostname "api.$DOMAIN" -o none
      az containerapp hostname bind -n "$APP" -g "$RG" --hostname "api.$DOMAIN" \
        --environment "$CAE" --validation-method CNAME -o none
      cert=$(az containerapp show -n "$APP" -g "$RG" \
        --query "properties.configuration.ingress.customDomains[?name=='api.$DOMAIN'].certificateId | [0]" -o tsv)
      [ -n "$cert" ] || die "api.$DOMAIN has no certificate after binding"
      azd env set USNM_API_CERT_ID "$cert"
      azd provision --no-prompt
    fi
  fi
fi

API_URL=$(aget API_URL)
evar USNM_API_APP "$(aget API_APP)"
evar USNM_API_URL "$API_URL"

step "Deploying the web app"
run_and_wait "deploy web" -f environment="$ENV_NAME"

step "Checking"
curl -fsS --retry 10 --retry-delay 6 --retry-all-errors "$API_URL/readyz" > /dev/null
echo "API  $API_URL  ready"
SITE=$(aget SITE_URL)
curl -fsS --retry 10 --retry-delay 6 --retry-all-errors -o /dev/null "$SITE"
echo "site $SITE  up"

if [ "$CLEANUP" = true ]; then
  step "Removing the pre-environment repository settings"
  for v in USNM_ACR_LOGIN_SERVER USNM_CI_CLIENT_ID AZURE_TENANT_ID AZURE_SUBSCRIPTION_ID \
    USNM_API_APP USNM_RESOURCE_GROUP USNM_API_URL; do
    gh variable delete "$v" -R "$REPO" 2> /dev/null && echo "deleted variable $v" || true
  done
  gh secret delete AZURE_STATIC_WEB_APPS_API_TOKEN -R "$REPO" 2> /dev/null &&
    echo "deleted secret AZURE_STATIC_WEB_APPS_API_TOKEN" || true
fi

step "Done"
if [ "$(aget USNM_INGEST_JOBS)" = true ]; then
  echo "Load the corpus (08 §8.9): az containerapp job start -n $(aget BACKFILL_JOB) -g $(aget AZURE_RESOURCE_GROUP)"
fi
