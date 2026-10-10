#!/usr/bin/env bash
# Lint every template; any warning fails.
set -euo pipefail
# The parameter file reads the environment's settings; lint it as a sample one.
export AZURE_ENV_NAME=${AZURE_ENV_NAME:-ci}
out=$(for f in infra/*.bicep infra/main.bicepparam infra/modules/*.bicep infra/archive/*.bicep; do bicep lint "$f" 2>&1; done)
echo "$out"
test -z "$out"
