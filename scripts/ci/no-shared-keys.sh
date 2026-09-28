#!/usr/bin/env bash
# Fail on anything that authenticates with a shared secret instead of an
# Entra identity (ADR-0009): account keys, SAS, workspace or instrumentation
# keys, connection strings with keys, local auth left on, a registry admin
# user. Scans infra, scripts, workflows, Dockerfiles and the Rust crates;
# docs and tests may name the patterns.
set -euo pipefail

patterns=(
  'listKeys\('                                # storage / workspace / Cosmos keys
  'listConnectionStrings'
  'AccountKey='                               # key connection strings
  'SharedAccessSignature|sig=[A-Za-z0-9%]'    # SAS
  'generateUserDelegationKey|user_delegation' # user-delegation SAS
  'sharedKey:'                                # e.g. Log Analytics workspace key
  'InstrumentationKey='
  'allowSharedKeyAccess: *true'
  'disableLocalAuth: *false'
  'DisableLocalAuth: *false'
  'adminUserEnabled: *true'
  'anonymousPullEnabled: *true'
  'listSecrets'                               # any listSecrets call or grant
  'secrets\.[A-Z_]+'                          # GitHub secrets in workflows
  'api_token|apiToken|deployment.?token'
)

# The one open exception (ADR-0009): the Static Web App deploy token, read
# at deploy time with the CI identity and never stored. The API update's
# listSecrets is an az CLI round trip; the app has no secrets.
allow=(
  '^\.github/workflows/deploy-web\.yml:'
  '^scripts/ci/swa-token\.sh:'
  '^infra/modules/deployer\.bicep:.*(listSecrets|deployment token)'
  '^infra/modules/staticwebapp\.bicep:.*deployment token'
  '^infra/main\.bicep:.*deployment token'
  '^scripts/bootstrap\.sh:.*AZURE_STATIC_WEB_APPS_API_TOKEN'  # deletes the legacy secret
  '^crates/usnm-store/src/blob\.rs:.*sig=x'  # the test that SAS URLs are refused
)

files=$(git ls-files infra scripts .github/workflows 'Dockerfile*' 'crates/*.rs' 'crates/**/*.rs' \
  | grep -v -e '^scripts/ci/no-shared-keys\.sh$' -e '\.md$' -e '_test\.rs$' -e '/tests/')

found=$(printf '%s\n' "$files" | xargs grep -nHE "$(IFS='|'; echo "${patterns[*]}")" || true)
for a in "${allow[@]}"; do
  found=$(printf '%s\n' "$found" | grep -vE "$a" || true)
done

if [ -n "$found" ]; then
  echo "Shared-key authentication found (ADR-0009: Entra identities and RBAC only):"
  echo "$found"
  exit 1
fi
echo "no shared-key authentication"
