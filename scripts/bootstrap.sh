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
#   --cleanup-legacy      remove the repository-level variables and the SWA token
#                         secret used before per-environment deployment
#   --move                allow an existing environment to move to another
#                         subscription (it starts there with no data)
#
# Needs: az, azd, gh and curl, signed in (`az login --tenant …`,
# `azd auth login --tenant-id …`, `gh auth login`), Owner on the subscription
# and admin on the GitHub repository.
set -euo pipefail

usage() { sed -n '2,20p' "$0" | sed 's/^# \{0,1\}//'; exit 2; }
[ $# -ge 1 ] || usage
ENV_NAME=$1; shift
case "$ENV_NAME" in -*|"") usage ;; esac
# It becomes part of resource names (the registry allows only lowercase
# letters and digits) and of the CI identity's OIDC subject.
if ! printf '%s' "$ENV_NAME" | grep -Eq '^[a-z][a-z0-9]{0,15}$'; then
  echo "error: the environment name must be 1-16 lowercase letters and digits, starting with a letter" >&2
  exit 2
fi
SUBSCRIPTION="" LOCATION="eastus2" REPO="" DOMAIN="" EMAIL="" INGEST=false CLEANUP=false MOVE=false
while [ $# -gt 0 ]; do
  case "$1" in
    --subscription) SUBSCRIPTION=$2; shift 2 ;;
    --location) LOCATION=$2; shift 2 ;;
    --repo) REPO=$2; shift 2 ;;
    --domain) DOMAIN=$2; shift 2 ;;
    --alert-email) EMAIL=$2; shift 2 ;;
    --ingest) INGEST=true; shift ;;
    --cleanup-legacy) CLEANUP=true; shift ;;
    --move) MOVE=true; shift ;;
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
# Settings that describe what exists in one subscription can't carry over to
# another: moving starts again from an empty registry, with no certificate
# and a fresh look at the Cosmos DB free tier.
MOVING=false
previous=$(aget AZURE_SUBSCRIPTION_ID)
if [ -n "$previous" ] && [ "$previous" != "$SUBSCRIPTION" ]; then
  [ "$MOVE" = true ] ||
    die "environment $ENV_NAME is in subscription $previous; pass --move to rebuild it in $SUBSCRIPTION (it starts with no data)"
  MOVING=true
  azd env set USNM_USE_ACR false
  azd env set USNM_API_IMAGE ""
  azd env set USNM_API_CERT_ID ""
  azd env set USNM_SITE_CERT_ID ""
  azd env set USNM_WWW_CERT_ID ""
  [ -z "$(aget USNM_BUDGET_START)" ] || azd env set USNM_BUDGET_START "$(date -u +%Y-%m-01)"
fi
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
if [ -z "$(aget USNM_COSMOS_FREE_TIER)" ] || [ "$MOVING" = true ]; then
  others=$(az cosmosdb list --query "[?enableFreeTier && !starts_with(name, 'cosmos-usnm-$ENV_NAME-')].name" -o tsv)
  if [ -n "$others" ]; then
    echo "the subscription's free tier is taken by: $others"
    azd env set USNM_COSMOS_FREE_TIER false
  else
    azd env set USNM_COSMOS_FREE_TIER true
  fi
fi
USE_ACR=$(aget USNM_USE_ACR)
[ -n "$USE_ACR" ] || { USE_ACR=false; azd env set USNM_USE_ACR false; }

# New roles and permissions take a minute or two to apply everywhere, so a
# provision that depends on one can fail once; it's idempotent, so retry.
provision() {
  for attempt in 1 2 3; do
    azd provision --no-prompt && return 0
    [ "$attempt" = 3 ] && die "provisioning failed three times"
    echo "retrying in 60s"
    sleep 60
  done
}

step "Provisioning"
provision

step "Wiring GitHub Environment '$ENV_NAME' in $REPO"
# Only main may deploy: the CI identity trusts this environment's jobs.
gh api -X PUT "repos/$REPO/environments/$ENV_NAME" --input - > /dev/null << 'EOF'
{"deployment_branch_policy": {"protected_branches": false, "custom_branch_policies": true}}
EOF
# Exactly one policy may exist: the branch main. Anything else (another
# branch pattern, or a tag named main) would let other refs use the identity.
policies="repos/$REPO/environments/$ENV_NAME/deployment-branch-policies"
has_main=false
while read -r id name type; do
  [ -n "$id" ] || continue
  if [ "$name" = main ] && [ "$type" = branch ] && [ "$has_main" = false ]; then
    has_main=true
  else
    echo "removing deployment policy $type $name"
    gh api -X DELETE "$policies/$id" > /dev/null
  fi
done <<< "$(gh api "$policies" --paginate --jq '.branch_policies[] | "\(.id) \(.name) \(.type // "branch")"')"
[ "$has_main" = true ] || gh api -X POST "$policies" -f name=main -f type=branch > /dev/null
evar() { gh variable set "$1" --env "$ENV_NAME" -R "$REPO" --body "$2"; }
evar USNM_ACR_LOGIN_SERVER "$(aget ACR_LOGIN_SERVER)"
evar USNM_CI_CLIENT_ID "$(aget CI_CLIENT_ID)"
evar AZURE_TENANT_ID "$(aget AZURE_TENANT_ID)"
evar AZURE_SUBSCRIPTION_ID "$(aget AZURE_SUBSCRIPTION_ID)"
evar USNM_RESOURCE_GROUP "$(aget AZURE_RESOURCE_GROUP)"
# List the environment for the deploy workflows.
envs=$(gh variable get USNM_DEPLOY_ENVIRONMENTS -R "$REPO" 2> /dev/null || true)
if ! grep -q "\"$ENV_NAME\"" <<< "$envs"; then
  # A JSON array of names: ["dev","prod"].
  names=$( (tr -d '[]" ' <<< "$envs" | tr ',' '\n'; echo "$ENV_NAME") | grep -v '^$' | sort -u)
  envs="[$(sed 's/.*/"&"/' <<< "$names" | paste -sd, -)]"
  gh variable set USNM_DEPLOY_ENVIRONMENTS -R "$REPO" --body "$envs"
fi
echo "deploy environments: $envs"

# Start a workflow on main and wait for that run to finish. The run is found
# by a unique correlation id, which the workflows put in their run name.
run_and_wait() {
  local workflow=$1; shift
  local correlation
  correlation="bootstrap-$ENV_NAME-$(date -u +%Y%m%dT%H%M%S)-$$"
  gh workflow run "$workflow" -R "$REPO" --ref main -f correlation="$correlation" "$@"
  local id=""
  for _ in $(seq 60); do
    sleep 5
    id=$(gh run list -R "$REPO" --workflow "$workflow" --event workflow_dispatch \
      --limit 20 --json databaseId,displayTitle \
      --jq "[.[] | select(.displayTitle | contains(\"$correlation\"))][0].databaseId // empty")
    [ -n "$id" ] && break
  done
  [ -n "$id" ] || die "the $workflow run did not start"
  gh run watch "$id" -R "$REPO" --exit-status > /dev/null || die "the $workflow run failed: gh run view $id -R $REPO --log-failed"
}

# Always: the registry must hold the current main, not just some earlier build.
step "Publishing main's images to $(aget ACR_NAME) (ci on main)"
run_and_wait ci

if [ "$USE_ACR" != true ]; then
  step "Switching to the private registry"
  azd env set USNM_USE_ACR true
  azd env set USNM_API_IMAGE ""
  provision
fi

# The Static Web App that used to serve the site deployed with a shared token
# (ADR-0009). Provisioning never deletes, so remove it here, in every
# environment, whether or not it has a domain.
SWA="swa-usnm-$ENV_NAME"
if az staticwebapp show -n "$SWA" -g "$(aget AZURE_RESOURCE_GROUP)" -o none 2> /dev/null; then
  echo "removing the Static Web App $SWA: the API app serves the site now"
  az staticwebapp delete -n "$SWA" -g "$(aget AZURE_RESOURCE_GROUP)" --yes -o none
fi

# Names on the app (the site and the API), once the registrar delegates the
# domain to the zone.
DOMAIN=$(aget USNM_DNS_ZONE)
if [ -n "$DOMAIN" ]; then
  step "Custom domains for $DOMAIN"
  if ! curl -fsS -H 'accept: application/dns-json' "https://cloudflare-dns.com/dns-query?name=$DOMAIN&type=NS" |
    grep -q azure-dns; then
    echo "$DOMAIN is not delegated to Azure DNS yet. Set its name servers to:"
    echo "  $(aget NAME_SERVERS)"
    echo "then re-run this script to bind the names."
  else
    RG=$(aget AZURE_RESOURCE_GROUP) APP=$(aget API_APP) CAE=$(aget CONTAINER_ENV_NAME)
    names() { tr '\t' '\n' | grep -Fqx "$1"; }
    # The TXT record that validated the Static Web App's apex.
    az network dns record-set txt delete -g "$RG" -z "$DOMAIN" -n _dnsauth --yes -o none 2> /dev/null || true
    # Each name: add it to the app, bind a managed certificate, and record the
    # certificate so provisioning keeps the binding. www and api validate by
    # their CNAMEs; the apex over HTTP, through its A record.
    bound=false
    bind() {
      local host=$1 how=$2 var=$3 cert
      [ -z "$(aget "$var")" ] || return 0
      az containerapp hostname list -n "$APP" -g "$RG" --query "[].name" -o tsv | names "$host" ||
        az containerapp hostname add -n "$APP" -g "$RG" --hostname "$host" -o none
      az containerapp hostname bind -n "$APP" -g "$RG" --hostname "$host" \
        --environment "$CAE" --validation-method "$how" -o none ||
        die "binding $host failed. If its DNS record changed in the last hour, resolvers may still cache the old one; re-run later."
      cert=$(az containerapp show -n "$APP" -g "$RG" \
        --query "properties.configuration.ingress.customDomains[?name=='$host'].certificateId | [0]" -o tsv)
      [ -n "$cert" ] || die "$host has no certificate after binding"
      azd env set "$var" "$cert"
      bound=true
    }
    bind "api.$DOMAIN" CNAME USNM_API_CERT_ID
    bind "www.$DOMAIN" CNAME USNM_WWW_CERT_ID
    bind "$DOMAIN" HTTP USNM_SITE_CERT_ID
    [ "$bound" = false ] || provision
  fi
fi

evar USNM_API_APP "$(aget API_APP)"
# Variables of the retired web deploy workflow.
for v in USNM_SWA_NAME USNM_API_URL; do
  gh variable delete "$v" --env "$ENV_NAME" -R "$REPO" 2> /dev/null && echo "deleted variable $v" || true
done

step "Checking"
API_URL=$(aget API_URL) SITE=$(aget SITE_URL)
curl -fsS --retry 10 --retry-delay 6 --retry-all-errors "$API_URL/readyz" > /dev/null
echo "API  $API_URL  ready"
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
