#!/usr/bin/env bash
# Prints a link that opens a maintainer-only set (a language whose configuration has `readers_prefix`) with a
# signed maintainer session (30 days). The link carries the session in the URL fragment, which browsers never send
# to the server; the page stores it and removes it from the address bar. Treat the link like a password.
#
#   edge/check/mint-maintainer-link.sh --vault NAME --lang de-968 --name tg
set -euo pipefail
VAULT="" LANG_CODE="" NAME=""
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=$2; shift 2 ;;
    --lang) LANG_CODE=$2; shift 2 ;;
    --name) NAME=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[[ "$VAULT" =~ ^[a-z0-9-]+$ && "$LANG_CODE" =~ ^[a-z]{2,3}(-[A-Za-z0-9]{2,8})*$ && "$NAME" =~ ^[a-z0-9-]{1,40}$ ]] || { echo "usage: $0 --vault NAME --lang CODE --name NAME" >&2; exit 2; }
TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n usnm-prod-check-admin-token --query value -o tsv)
[ -n "$TOKEN" ] || { echo "no admin token in $VAULT" >&2; exit 1; }
SESSION=$(printf 'authorization: Bearer %s\n' "$TOKEN" | curl -fsS -X POST -H @- -d "{\"contributor\":\"maintainer:$NAME\"}" https://check.usnewsmap.com/admin/session | python3 -c 'import json,sys; print(json.load(sys.stdin)["session"])')
echo "https://check.usnewsmap.com/$LANG_CODE#session=$SESSION"
