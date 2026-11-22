#!/usr/bin/env bash
# Deploy an existing environment's stack from its current settings, without
# the rest of bootstrap (no CI run, no GitHub or domain steps). Use it once
# a settings change is merged (docs/operations/environments-and-deploys.md, "Settings"):
#
#   scripts/azure/settings.sh --env prod --key USNM_SEARCH_BACKEND --value quickwit
#   # commit infra/env/prod.json, open a pull request, merge, pull
#   scripts/azure/provision.sh --env prod
#
# It deploys the committed settings (infra/env/<env>.json), the private
# ones in the settings store (under the environment's label) and the local
# file's (scripts/lib/env.sh), and refuses while infra/env/<env>.json or the
# schema has uncommitted changes; --allow-dirty deploys them anyway (an
# experiment in dev). The first provision in a subscription creates the
# settings store (scripts/azure/settings-store.sh).
#
# --what-if changes nothing: it reads the settings the same way, runs
# Azure's preflight validation of the stack (a policy denial fails it) and
# shows the template's changes.
#
# --allow-rename=NAME explicitly permits removal of that protected resource.
# Repeat for each intentional deletion; this never bypasses a failed or
# incomplete what-if (ADR-0017).
#
# An environment that pins USNM_API_IMAGE_TAG (prod does) deploys the API at
# the pin, and CI's rollouts leave the pin behind. So a provision, or its
# what-if, refuses while the pin differs from the tag the API app runs, and
# says how to go on (#1071). --api-tag=live keeps the running tag;
# --api-tag=TAG deploys TAG over the pin, for this provision only (it does
# not edit infra/env/<env>.json: move the pin through a pull request).
#
# Needs az 2.61+, signed in to the environment's subscription with Owner.
set -euo pipefail
usage() { echo "usage: scripts/azure/provision.sh --env <env> [--allow-dirty] [--what-if] [--api-tag <live|TAG>] [--allow-rename=<exact-old-name>]..." >&2; exit 2; }
# The positional form (`provision.sh prod`) works for one release, with a
# warning (#277).
if [ $# -eq 1 ] && [[ $1 != -* ]]; then
  echo "warning: \`scripts/azure/provision.sh ENV\` is deprecated; use --env (#277)" >&2
  set -- --env "$1"
fi
ENV_NAME="" ALLOW_DIRTY=false WHAT_IF=false API_TAG="" API_TAG_GIVEN=false
ALLOW_RENAMES=()
while [ $# -gt 0 ]; do
  case $1 in
    --env) [ $# -ge 2 ] || usage; ENV_NAME=$2; shift 2 ;;
    --allow-dirty) ALLOW_DIRTY=true; shift ;;
    --what-if) WHAT_IF=true; shift ;;
    --api-tag) [ $# -ge 2 ] || usage; API_TAG=$2; API_TAG_GIVEN=true; shift 2 ;;
    --api-tag=*) API_TAG=${1#*=}; API_TAG_GIVEN=true; shift ;;
    --allow-rename=*)
      name=${1#*=}
      [[ $name =~ ^[A-Za-z0-9][A-Za-z0-9_.\ -]*$ ]] || usage
      ALLOW_RENAMES+=("--allow-rename=$name")
      shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown argument: $1" >&2; usage ;;
  esac
done
[ -n "$ENV_NAME" ] || usage
die() { echo "error: $*" >&2; exit 1; }
cd "$(dirname "$0")/../.."
. scripts/lib/env.sh
# live, or a tag the stack accepts (the schema's pattern), before any Azure call.
if $API_TAG_GIVEN; then
  [ "$API_TAG" = live ] || [[ $API_TAG =~ $IMAGE_TAG_RE ]] || {
    echo "error: --api-tag takes live or an image tag (letters, digits, _ . -; at most 128 characters), not \"$API_TAG\"" >&2
    usage
  }
  API_TAG_REQUEST=$API_TAG
fi
if ! $ALLOW_DIRTY; then
  rc=0
  committed_clean || rc=$?
  [ "$rc" -ne 1 ] ||
    die "$COMMITTED_FILE has uncommitted changes (above): commit them (through a pull request for a shared environment), or pass --allow-dirty for an experiment"
  [ "$rc" -eq 0 ] || exit 1
fi
# A searcher switch without its account must fail before any Azure change.
if [ "$(aget USNM_INDEX_STORAGE_SERVE)" = true ] && [ "$(aget USNM_INDEX_STORAGE)" != true ]; then
  die "USNM_INDEX_STORAGE_SERVE requires USNM_INDEX_STORAGE; see docs/operations/rebuild-and-index-storage.md, Index storage cutover"
fi
need_stack_az
[ -s "$ENV_FILE" ] || die "no settings for $ENV_NAME; run scripts/azure/bootstrap.sh --env $ENV_NAME first"
az account set --subscription "$(aget AZURE_SUBSCRIPTION_ID)" 2> /dev/null ||
  die "run: az login (the environment is in subscription $(aget AZURE_SUBSCRIPTION_ID))"
if $WHAT_IF; then
  provision_what_if
  exit 0
fi
provision
echo "deployed stack $STACK"
