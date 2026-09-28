# shellcheck shell=bash
# Shared by the environment scripts; source it with ENV_NAME set and the
# repository root as the working directory. Needs az signed in to the
# environment's subscription for the stack functions, and a die() function.
#
# An environment's settings live in .azure/$ENV_NAME/.env as KEY="value"
# lines (the layout azd used, so older environments carry over).
#
# The name becomes part of resource names (the registry allows only
# lowercase letters and digits), the stack's name and the settings path.
# "guardrails" is taken: usnm-guardrails is the shared policy stack.
valid_env_name() {
  printf '%s' "$1" | grep -Eq '^[a-z][a-z0-9]{0,15}$' || {
    echo "error: the environment name must be 1-16 lowercase letters and digits, starting with a letter" >&2
    exit 2
  }
  [ "$1" != guardrails ] || { echo "error: \"guardrails\" is reserved" >&2; exit 2; }
}
valid_env_name "$ENV_NAME"
ENV_FILE=".azure/$ENV_NAME/.env"
# Setting names are shell variable names.
valid_key() {
  [[ $1 =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || { echo "error: invalid setting name: $1" >&2; exit 2; }
}
# Read one setting (empty when unset).
# shellcheck source=/dev/null
aget() { valid_key "$1"; (set +u; set -a; [ ! -f "$ENV_FILE" ] || . "$ENV_FILE"; printf '%s\n' "${!1:-}"); }
# Write one setting, quoted so the file can be sourced.
aset() {
  valid_key "$1"
  local v=$2 tmp
  mkdir -p "$(dirname "$ENV_FILE")"
  touch "$ENV_FILE"
  v=${v//\\/\\\\}; v=${v//\"/\\\"}; v=${v//\$/\\\$}; v=${v//\`/\\\`}
  tmp=$(mktemp)
  grep -v "^$1=" "$ENV_FILE" > "$tmp" || true
  printf '%s="%s"\n' "$1" "$v" >> "$tmp"
  mv "$tmp" "$ENV_FILE"
}

# One deployment stack per environment, at subscription scope (it holds the
# resource groups). Resources dropped from the template are deleted; deny
# settings stop anyone deleting a managed resource outside the stack.
STACK="usnm-$ENV_NAME"
# The guard-rail policy definitions every environment in the subscription
# assigns: one stack of their own, so no two stacks manage the same resource.
GUARDRAILS_STACK=usnm-guardrails
deploy_guardrails() {
  # A stack's location is fixed when it's created; environments in other
  # regions reuse it.
  local location
  location=$(az stack sub show -n "$GUARDRAILS_STACK" --query location -o tsv 2> /dev/null ||
    aget AZURE_LOCATION)
  az stack sub create --name "$GUARDRAILS_STACK" --location "$location" \
    --template-file infra/guardrails.bicep \
    --action-on-unmanage deleteResources \
    --deny-settings-mode denyDelete \
    --description "usnewsmap guard-rail policy definitions, shared by every environment in the subscription" \
    --yes --only-show-errors -o none
}
deploy_stack() (
  # Export the settings that have a value; infra/main.bicepparam reads them,
  # and an unset one takes its default there. Clear every name it reads
  # first, so a variable left in the caller's shell can't stand in for a
  # setting the file doesn't have.
  for k in $(grep -o "readEnvironmentVariable('[A-Za-z0-9_]*'" infra/main.bicepparam | cut -d"'" -f2); do
    unset "$k"
  done
  set -a
  # shellcheck source=/dev/null
  . "$ENV_FILE"
  set +a
  for k in $(sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' "$ENV_FILE"); do
    [ -n "${!k}" ] || unset "$k"
  done
  # The stack's name comes from the environment's; the template's must match.
  export AZURE_ENV_NAME=$ENV_NAME
  az stack sub create --name "$STACK" --location "$(aget AZURE_LOCATION)" \
    --parameters infra/main.bicepparam \
    --action-on-unmanage deleteResources \
    --deny-settings-mode denyDelete \
    --description "usnewsmap $ENV_NAME (scripts/bootstrap.sh, scripts/provision.sh)" \
    --yes --only-show-errors -o none
)
# The template's outputs become settings (API_URL, ACR_NAME, ...). One
# query reads the names and the values from the same object, so they pair
# up in order; tsv prints them as two rows, which awk splits keeping empty
# values.
save_outputs() {
  local k v
  while IFS=$'\t' read -r k v; do
    [ -n "$k" ] && aset "$k" "$v"
  done < <(az stack sub show -n "$STACK" \
      --query "[keys(outputs), values(outputs)[].value]" -o tsv |
    awk -F'\t' 'NR == 1 { n = split($0, k, "\t") }
      NR == 2 { split($0, v, "\t"); for (i = 1; i <= n; i++) print k[i] "\t" v[i] }')
}
# New roles and permissions take a minute or two to apply everywhere, so a
# deployment that depends on one can fail once; it's idempotent, so retry.
provision() {
  for attempt in 1 2 3; do
    deploy_guardrails && deploy_stack && { save_outputs; return 0; }
    [ "$attempt" = 3 ] && die "provisioning failed three times"
    echo "retrying in 60s"
    sleep 60
  done
}
