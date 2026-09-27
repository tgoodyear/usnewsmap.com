#!/usr/bin/env bash
# Read the Static Web App's deployment token (SWA in resource group RG) with
# the signed-in CI identity, masked, as the step output `token`.
set -euo pipefail
: "${SWA:?}" "${RG:?}"
token=$(az staticwebapp secrets list -n "$SWA" -g "$RG" --query properties.apiKey -o tsv)
echo "::add-mask::$token"
echo "token=$token" >> "$GITHUB_OUTPUT"
