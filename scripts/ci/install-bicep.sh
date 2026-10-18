#!/usr/bin/env bash
# Install the repository-pinned Bicep release ahead of the runner's own,
# root-owned copy on PATH.
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
BICEP_VERSION=$(<"$script_dir/bicep-version")
[[ $BICEP_VERSION =~ ^v[0-9]+\.[0-9]+\.[0-9]+$ ]] || {
  echo "invalid Bicep version pin: $BICEP_VERSION" >&2
  exit 1
}
: "${RUNNER_TEMP:?}"
: "${GITHUB_PATH:?}"
mkdir -p "$RUNNER_TEMP/bin"
curl -fsSL -o "$RUNNER_TEMP/bin/bicep" \
  "https://github.com/Azure/bicep/releases/download/${BICEP_VERSION}/bicep-linux-x64"
chmod +x "$RUNNER_TEMP/bin/bicep"
echo "$RUNNER_TEMP/bin" >> "$GITHUB_PATH"
"$RUNNER_TEMP/bin/bicep" --version | grep -F "${BICEP_VERSION#v}"
