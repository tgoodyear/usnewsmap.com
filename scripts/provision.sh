#!/usr/bin/env bash
# Deploy an existing environment's stack from its current settings, without
# the rest of bootstrap (no CI run, no GitHub or domain steps). Use it after
# changing a setting:
#
#   scripts/settings.sh prod USNM_SEARCH_BACKEND quickwit
#   scripts/provision.sh prod
#
# Needs az 2.61+, signed in to the environment's subscription with Owner.
set -euo pipefail
[ $# -eq 1 ] || { echo "usage: scripts/provision.sh <env>" >&2; exit 2; }
ENV_NAME=$1
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/.."
. scripts/lib/env.sh
need_stack_az
[ -s "$ENV_FILE" ] || die "no settings for $ENV_NAME; run scripts/bootstrap.sh $ENV_NAME first"
az account set --subscription "$(aget AZURE_SUBSCRIPTION_ID)" 2> /dev/null ||
  die "run: az login (the environment is in subscription $(aget AZURE_SUBSCRIPTION_ID))"
provision
echo "deployed stack $STACK"
