#!/usr/bin/env bash
set -euo pipefail
script_dir=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
"$script_dir/check-bicep-version.sh"

while IFS= read -r -d '' file; do
  bicep build "$file" --stdout > /dev/null
done < <(git ls-files -z -- '*.bicep')
