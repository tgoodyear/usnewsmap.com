#!/usr/bin/env bash
# Lint every template; any warning fails.
set -euo pipefail
out=$(for f in infra/main.bicep infra/modules/*.bicep; do bicep lint "$f" 2>&1; done)
echo "$out"
test -z "$out"
