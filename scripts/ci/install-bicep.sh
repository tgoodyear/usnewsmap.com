#!/usr/bin/env bash
# Install the pinned Bicep (BICEP_VERSION) ahead of the runner's own,
# root-owned copy on PATH.
set -euo pipefail
: "${BICEP_VERSION:?}"
mkdir -p "$RUNNER_TEMP/bin"
curl -fsSL -o "$RUNNER_TEMP/bin/bicep" \
  "https://github.com/Azure/bicep/releases/download/${BICEP_VERSION}/bicep-linux-x64"
chmod +x "$RUNNER_TEMP/bin/bicep"
echo "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"
"$RUNNER_TEMP/bin/bicep" --version | grep -F "${BICEP_VERSION#v}"
