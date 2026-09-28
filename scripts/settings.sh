#!/usr/bin/env bash
# Read or change an environment's settings (.azure/<env>/.env).
#
#   scripts/settings.sh prod                 # all of them
#   scripts/settings.sh prod API_URL         # one
#   scripts/settings.sh prod KEY VALUE       # set one ("" clears it)
#
# A change takes effect in Azure on the next scripts/provision.sh (or
# bootstrap) run. The template's outputs (API_URL, ACR_NAME, ...) are
# rewritten by every deployment.
set -euo pipefail
[ $# -ge 1 ] && [ $# -le 3 ] || { sed -n '2,10s/^# \{0,1\}//p' "$0" >&2; exit 2; }
ENV_NAME=$1
cd "$(dirname "$0")/.."
. scripts/lib/env.sh
[ -e "$ENV_FILE" ] || [ $# -eq 3 ] ||
  { echo "error: no settings for $ENV_NAME" >&2; exit 1; }
case $# in
  1) cat "$ENV_FILE" ;;
  2) aget "$2" ;;
  3) aset "$2" "$3" ;;
esac
