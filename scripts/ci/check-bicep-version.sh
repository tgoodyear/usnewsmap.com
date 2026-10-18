#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
expected=$(<"$script_dir/bicep-version")
actual=$(bicep --version)
case "$actual" in
  "Bicep CLI version ${expected#v} "*) ;;
  *)
    echo "error: expected Bicep CLI $expected, got: $actual" >&2
    exit 1
    ;;
esac
