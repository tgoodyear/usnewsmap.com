#!/usr/bin/env bash
# Fail on anything that authenticates with a shared secret instead of an
# Entra identity (ADR-0009): account keys, SAS, workspace or instrumentation
# keys, connection strings with keys, local auth left on, a registry admin
# user. Scans infra, scripts, workflows, Dockerfiles, the Rust crates and
# the web app; docs and tests may name the patterns.
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
  'api_token|apiToken|apiKey|deployment.?token'
  # The same operations through the Azure CLI.
  'keys (list|renew)|list-keys|get-shared-keys|secrets list'
  'generate-sas|--sas-token|--account-key|--auth-mode +key|connection-string'
  'acr credential|--admin-enabled +true'
  # GitHub secrets: any expression that reads them (secrets.x, secrets['x'],
  # toJSON(secrets)), and passing them all to a reusable workflow.
  '\$\{\{[^}]*\bsecrets\b|secrets: *inherit'
)

# Lines that name a pattern without authenticating with it; no exceptions
# remain (ADR-0009). Exact lines (file, then the whole line), so any change
# to one is checked again.
allow=$(cat <<'ALLOW'
infra/modules/deployer.bicep:          'Microsoft.App/containerApps/listSecrets/action'
scripts/bootstrap.sh:  gh secret delete AZURE_STATIC_WEB_APPS_API_TOKEN -R "$REPO" 2> /dev/null &&
scripts/bootstrap.sh:    echo "deleted secret AZURE_STATIC_WEB_APPS_API_TOKEN" || true
crates/usnm-store/src/blob.rs:            "https://acct.blob.core.windows.net/c?sv=2024&sig=x",
ALLOW
)

# Everything tracked that can authenticate: infra, scripts, workflows,
# Dockerfiles, the Rust crates and the web app. Docs, tests and lockfiles
# may name the patterns.
files=$(git ls-files infra scripts .github 'Dockerfile*' crates web \
  | grep -v -e '^scripts/ci/no-shared-keys\.sh$' -e '\.md$' -e '_test\.rs$' -e '/tests/' \
    -e '\.test\.tsx\?$' -e '^web/e2e/' -e 'package-lock\.json$')

matches=$(printf '%s\n' "$files" | xargs grep -inHE "$(IFS='|'; echo "${patterns[*]}")" || true)
# Drop exact allowed lines: "file:line:content" against "file:content".
found=$(ALLOW="$allow" awk '
  BEGIN { n = split(ENVIRON["ALLOW"], a, "\n"); for (i = 1; i <= n; i++) ok[a[i]] = 1 }
  NF {
    f = $0; sub(/:.*/, "", f)
    rest = substr($0, length(f) + 2); c = rest; sub(/^[0-9]+:/, "", c)
    if (!((f ":" c) in ok)) print
  }' <<< "$matches")

if [ -n "$found" ]; then
  echo "Shared-key authentication found (ADR-0009: Entra identities and RBAC only):"
  echo "$found"
  exit 1
fi
echo "no shared-key authentication"
