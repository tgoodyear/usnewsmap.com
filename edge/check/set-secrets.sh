#!/usr/bin/env bash
# Sets the Worker's three secrets (README.md, "Secrets"). Run by hand, signed in to az, from the
# address the Workers token is limited to. Values go from Key Vault (or the prompt) to Wrangler's
# stdin; none is printed or written to disk.
#
#   edge/check/set-secrets.sh --vault NAME --token-secret NAME --account-id ID [--rotate]
#
# --rotate: make new SESSION_KEY and ADMIN_TOKEN values (signs everyone out). Without it, values
#   already in the vault are reused and only missing ones are made.
# TURNSTILE_SECRET is read from the vault secret usnm-prod-check-turnstile-secret; if that is
# missing, paste it from the Cloudflare dashboard (Turnstile, widget "usnm-check") at the prompt.
set -euo pipefail
cd "$(dirname "$0")"
VAULT="" TOKEN_SECRET="" ACCOUNT="" ROTATE=false
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=$2; shift 2 ;;
    --token-secret) TOKEN_SECRET=$2; shift 2 ;;
    --account-id) ACCOUNT=$2; shift 2 ;;
    --rotate) ROTATE=true; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$VAULT" ] && [ -n "$TOKEN_SECRET" ] && [[ "$ACCOUNT" =~ ^[0-9a-f]{32}$ ]] || { echo "usage: $0 --vault NAME --token-secret NAME --account-id ID [--rotate]" >&2; exit 2; }

export CLOUDFLARE_ACCOUNT_ID=$ACCOUNT
CLOUDFLARE_API_TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n "$TOKEN_SECRET" --query value -o tsv)
export CLOUDFLARE_API_TOKEN

# A secret that doesn't exist reads as empty. Any other failure (network, sign-in, throttling)
# stops the script, so a failed read can never lead to a new value replacing a good one.
vault_get() {
  local out err
  err=$(mktemp)
  if out=$(az keyvault secret show --vault-name "$VAULT" -n "$1" --query value -o tsv 2>"$err"); then
    rm -f "$err"; printf %s "$out"; return 0
  fi
  if grep -q SecretNotFound "$err"; then rm -f "$err"; return 0; fi
  echo "error: reading $1 from $VAULT failed:" >&2; cat "$err" >&2; rm -f "$err"
  exit 1
}
put() { printf %s "$2" | npx --yes wrangler@4.154.0 secret put "$1" >/dev/null && echo "set $1"; }

for pair in SESSION_KEY:usnm-prod-check-session-key ADMIN_TOKEN:usnm-prod-check-admin-token; do
  name=${pair%%:*} secret=${pair#*:}
  value=""
  if ! $ROTATE; then value=$(vault_get "$secret") || exit 1; fi
  if [ -z "$value" ]; then
    value=$(openssl rand -base64 36 | tr '+/' '-_' | tr -d '=')
    az keyvault secret set --vault-name "$VAULT" -n "$secret" --value "$value" -o none
  fi
  put "$name" "$value"
done

value=$(vault_get usnm-prod-check-turnstile-secret) || exit 1
if [ -z "$value" ]; then
  read -r -s -p "Turnstile secret key for widget usnm-check: " value; echo
  [ -n "$value" ] || { echo "no Turnstile secret given" >&2; exit 1; }
  az keyvault secret set --vault-name "$VAULT" -n usnm-prod-check-turnstile-secret --value "$value" -o none
fi
put TURNSTILE_SECRET "$value"
