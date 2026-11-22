#!/usr/bin/env bash
# Runs Quickwit ($USNM_ARC_QUICKWIT) with Blob Storage tokens from arc-token-shim.py, which the
# release's prune step starts. Quickwit's azure_identity takes the App Service managed identity
# protocol, which the shim serves, and can't answer the Arc agent's challenge itself.
set -euo pipefail
: "${USNM_ARC_QUICKWIT:?the Quickwit binary to run}"
: "${USNM_ARC_SHIM_ENDPOINT:?the shim's token URL}"
: "${USNM_ARC_SHIM_HEADER:?the shim's secret}"
export AZURE_CREDENTIAL_KIND=appservice
export IDENTITY_ENDPOINT=$USNM_ARC_SHIM_ENDPOINT IDENTITY_HEADER=$USNM_ARC_SHIM_HEADER
unset IMDS_ENDPOINT AZURE_CLIENT_ID
exec "$USNM_ARC_QUICKWIT" "$@"
