#!/usr/bin/env bash
# Fails if the preference values in scripts/azure/boinc-config.sh (a standalone copy,
# because it is embedded into Bicep) differ from scripts/lib/boinc-common.sh.
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
tags() { grep -oE '<[a-z_]+>[^<]*</[a-z_]+>' | sed -E 's/\$\{?[A-Za-z_]+\}?/V/g' | sort; }
# shellcheck source=scripts/lib/boinc-common.sh
. "$HERE/lib/boinc-common.sh"
# The helper takes the same variable-valued fields as the Azure copy; normalise them to V.
a="$(boinc_prefs_xml 1 0 1 | tags | sed -E 's#<(max_ncpus_pct|run_if_user_active|idle_time_to_run|disk_max_used_gb)>[^<]*#<\1>V#' )"
b="$(sed -n '/^<global_preferences>/,/^<\/global_preferences>/p' "$HERE/azure/boinc-config.sh" | tags)"
if [ "$a" != "$b" ]; then
  echo "preference values differ between lib/boinc-common.sh and azure/boinc-config.sh:" >&2
  diff <(echo "$a") <(echo "$b") >&2 || true
  exit 1
fi
echo "prefs consistent"
