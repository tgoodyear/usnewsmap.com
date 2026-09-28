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
valid_env_name() {
  printf '%s' "$1" | grep -Eq '^[a-z][a-z0-9]{0,15}$' || {
    echo "error: the environment name must be 1-16 lowercase letters and digits, starting with a letter" >&2
    exit 2
  }
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
  az stack sub create --name "$STACK" --location "$(aget AZURE_LOCATION)" \
    --parameters infra/main.bicepparam \
    --action-on-unmanage deleteResources \
    --deny-settings-mode denyDelete \
    --description "usnewsmap $ENV_NAME (scripts/bootstrap.sh, scripts/provision.sh)" \
    --yes --only-show-errors -o none
)
# The template's outputs become settings (API_URL, ACR_NAME, ...).
save_outputs() {
  local k v
  while IFS=$'\t' read -r k v; do
    [ -n "$k" ] && aset "$k" "$v"
  done < <(paste \
    <(az stack sub show -n "$STACK" --query "keys(outputs)" -o tsv) \
    <(az stack sub show -n "$STACK" --query "outputs.*.value" -o tsv))
}
# New roles and permissions take a minute or two to apply everywhere, so a
# deployment that depends on one can fail once; it's idempotent, so retry.
provision() {
  for attempt in 1 2 3; do
    deploy_stack && { save_outputs; return 0; }
    [ "$attempt" = 3 ] && die "provisioning failed three times"
    echo "retrying in 60s"
    sleep 60
  done
}
