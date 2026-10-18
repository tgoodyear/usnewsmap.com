#!/usr/bin/env bash
# Lint every template; any warning fails.
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
"$script_dir/check-bicep-version.sh"
# The parameter file reads the environment's settings; lint it as a sample
# one. dev, not ci: environment names are a closed list and ci is a
# function word (ADR-0017).
export AZURE_ENV_NAME=${AZURE_ENV_NAME:-dev}
# A sample VNet block from RFC 5737's documentation range: an environment's
# real one is a private setting, never committed.
export USNM_VNET_ADDRESS_PREFIX=${USNM_VNET_ADDRESS_PREFIX:-192.0.2.0/24}
out=$(
  while IFS= read -r -d '' file; do
    bicep lint "$file" 2>&1
  done < <(git ls-files -z -- '*.bicep')
  for f in infra/main.bicepparam infra/policy-exemptions.bicepparam infra/names/*.bicepparam; do
    bicep lint "$f" 2>&1
  done
)
echo "$out"
test -z "$out"
