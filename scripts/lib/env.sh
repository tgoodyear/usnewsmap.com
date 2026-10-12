# shellcheck shell=bash
# Shared by the environment scripts; source it with ENV_NAME set and the
# repository root as the working directory. Needs az signed in to the
# environment's subscription for the stack functions, and a die() function.
#
# An environment's settings live in three places (#295), and
# infra/env/schema.json says which setting goes where:
#
# - public choices (flags, sizes, caps, schedules) in infra/env/<env>.json,
#   committed, so a change to them goes through a pull request;
# - private ones (addresses, principals, resource ids) in the environment's
#   settings vault, a Key Vault the stack creates (its name is the local
#   SETTINGS_KEYVAULT), which provision reads with the operator's sign-in
#   (load_vault); until it exists, they stay in the local file;
# - everything else in the local file: the stack's outputs, the
#   subscription and tenant, and what the scripts keep (local in the
#   schema). KEY="value" lines (the layout azd used, so older environments
#   carry over) in $USNM_SETTINGS_HOME/<env>/.env (default
#   ~/.config/usnewsmap/<env>/.env) when that directory exists, so every
#   checkout reads the same file, otherwise the checkout's own
#   .azure/<env>/.env (#275).
#
# A setting in the wrong place, or in two, stops provision (settings_check).
#
# The name becomes part of resource names (the registry allows only
# lowercase letters and digits), the stack's name and the settings path.
# "guardrails" is taken: usnm-guardrails is the shared policy stack.
valid_env_name() {
  [[ $1 =~ ^[a-z][a-z0-9]{0,15}$ ]] || {
    echo "error: the environment name must be 1-16 lowercase letters and digits, starting with a letter" >&2
    exit 2
  }
  # usnm-<name> would be another stack's name, and rg-usnm-<name> its group.
  case "$1" in
    guardrails | archive) echo "error: \"$1\" is reserved" >&2; exit 2 ;;
  esac
}
valid_env_name "$ENV_NAME"
# need_stack_az, GUARDRAILS_STACK, stack_param and deploy_guardrails.
# shellcheck source=scripts/lib/guardrails.sh
. scripts/lib/guardrails.sh
# A copy in a checkout can be stale: once the shared directory exists (a
# teardown leaves it, holding the kept file), the checkout's copy is ignored.
SETTINGS_HOME=${USNM_SETTINGS_HOME:-${XDG_CONFIG_HOME:-${HOME:-}/.config}/usnewsmap}
if [ -d "$SETTINGS_HOME/$ENV_NAME" ]; then
  ENV_FILE="$SETTINGS_HOME/$ENV_NAME/.env"
  [ ! -f ".azure/$ENV_NAME/.env" ] ||
    echo "note: $ENV_NAME's settings are $ENV_FILE; ignoring this checkout's .azure/$ENV_NAME/.env (delete it)" >&2
else
  ENV_FILE=".azure/$ENV_NAME/.env"
fi
# A settings file that doesn't parse stops every script up front, before
# anything reads a setting as empty and falls back to a default.
# shellcheck source=/dev/null
if [ -f "$ENV_FILE" ] && ! (set +u; . "$ENV_FILE") > /dev/null; then
  echo "error: $ENV_FILE doesn't parse; fix it before running anything" >&2
  exit 1
fi
# Setting names are shell variable names.
valid_key() {
  [[ $1 =~ ^[A-Za-z_][A-Za-z0-9_]*$ ]] || { echo "error: invalid setting name: $1" >&2; exit 2; }
}
# The committed settings, infra/env/<env>.json (USNM_ENV_DIR replaces
# infra/env in the tests). scripts/lib/env_settings.py checks the file
# against the schema and prints SETTINGS_PUBLIC, SETTINGS_PRIVATE and
# SETTINGS_LOCAL (the schema's settings by where they live) and
# COMMITTED_SETTINGS (KEY="value" lines). A file that doesn't check out
# stops every script up front, like a local file that doesn't parse.
SETTINGS_DIR=${USNM_ENV_DIR:-infra/env}
COMMITTED_FILE="$SETTINGS_DIR/$ENV_NAME.json"
SETTINGS_PUBLIC="" SETTINGS_PRIVATE="" COMMITTED_SETTINGS=""
load_committed() {
  local out
  out=$(python3 scripts/lib/env_settings.py --env "$ENV_NAME" --shell) ||
    { echo "error: fix $COMMITTED_FILE (or $SETTINGS_DIR/schema.json) before running anything" >&2; return 1; }
  eval "$out"
}
load_committed || exit 1
# The settings vault's private settings as KEY="value" lines, once
# load_vault has read them (VAULT_LOADED), and their names.
VAULT_SETTINGS="" VAULT_KEYS="" VAULT_LOADED=false
in_list() { [[ " $2 " == *" $1 "* ]]; }
is_public() { in_list "$1" "$SETTINGS_PUBLIC"; }
is_private() { in_list "$1" "$SETTINGS_PRIVATE"; }
# A value quoted for a KEY="value" line, which is sourced.
quote_value() {
  local v=$1
  v=${v//\\/\\\\}; v=${v//\"/\\\"}; v=${v//\$/\\\$}; v=${v//\`/\\\`}
  printf '%s' "$v"
}
# One setting from the local file (empty when unset).
# shellcheck source=/dev/null
# Only the file counts: the caller's variable of the same name is cleared.
# A settings file that can't be read or parsed fails the read.
local_get() {
  valid_key "$1"
  (set +u; unset "$1"; set -a
   if [ -f "$ENV_FILE" ]; then
     . "$ENV_FILE" || { echo "error: can't read $ENV_FILE" >&2; exit 1; }
   fi
   printf '%s\n' "${!1:-}")
}
# The names the local file sets, one per line.
local_keys() {
  [ -f "$ENV_FILE" ] || return 0
  { grep -oE '^[A-Za-z_][A-Za-z0-9_]*=' "$ENV_FILE" || [ $? -eq 1 ]; } | tr -d =
}
# Whether the local file sets $1, even to "".
local_has() { local_keys | grep -qx "$1"; }
committed_get() {
  (set +u; unset "$1"; eval "$COMMITTED_SETTINGS"; printf '%s\n' "${!1:-}")
}
# Read one setting (empty when unset), wherever it lives: a public one from
# the committed file; a private one from the local file, else the vault (a
# local entry wins, even "": teardown clears USNM_ARCHIVE_ACCOUNT that way);
# anything else from the local file. A public setting still only in the
# local file (an environment from before #295) is read from there with a
# warning, so the scripts keep working until it moves; provision refuses.
aget() {
  valid_key "$1"
  local v
  if is_public "$1"; then
    v=$(committed_get "$1")
    if [ -z "$v" ] && local_has "$1"; then
      v=$(local_get "$1") || return 1
      [ -z "$v" ] ||
        echo "warning: $1 is in $ENV_FILE; it belongs in $COMMITTED_FILE: scripts/settings.sh --env $ENV_NAME --migrate" >&2
    fi
    printf '%s\n' "$v"
    return 0
  fi
  if is_private "$1" && ! local_has "$1"; then
    if $VAULT_LOADED; then
      (set +u; unset "$1"; eval "$VAULT_SETTINGS"; printf '%s\n' "${!1:-}")
    else
      vault_get "$1"
    fi
    return
  fi
  local_get "$1"
}
# One private setting from the settings vault, when load_vault hasn't read
# them all (the scripts but provision): empty when there's no vault or no
# such secret, a failure when the vault can't be read, so a script never
# takes an unreadable setting for an unset one.
vault_get() {
  local vault out
  vault=$(local_get SETTINGS_KEYVAULT) || return 1
  [ -n "$vault" ] || { echo; return 0; }
  if ! out=$(az keyvault secret show --vault-name "$vault" --name "$(vault_secret_name "$1")" --query value -o tsv 2>&1); then
    grep -q SecretNotFound <<< "$out" && { echo; return 0; }
    echo "error: can't read $1 from the settings vault $vault: $out" >&2
    vault_access_hint "$vault"
    return 1
  fi
  printf '%s\n' "$out"
}
# How an operator without access to the settings vault gets it, after a
# refused read or write. Key Vault Secrets Officer goes to the operators in
# USNM_SETTINGS_VAULT_ADMINS (a list in the vault itself) and to the account
# that deployed last, so another operator needs one of them to add their
# object id to the list and provision; or, as an Owner of the subscription,
# a grant of their own on the resource group (one on the vault would
# collide with the assignment the stack then makes), removed once the list
# has them.
vault_access_hint() {
  local rg="rg-usnm-$ENV_NAME"
  cat >&2 << EOF
$1 gives Key Vault Secrets Officer to the operators in USNM_SETTINGS_VAULT_ADMINS and to whoever provisioned last. Either:
  - ask one of them to add your object id (az ad signed-in-user show --query id -o tsv) to that list, comma separated, and provision:
      az keyvault secret set --vault-name $1 --name $(vault_secret_name USNM_SETTINGS_VAULT_ADMINS) --value "<ids>" -o none
  - or, as an Owner of the subscription, grant it to yourself on $rg, add yourself to the list as above, provision, then delete the grant (a new role takes a few minutes to apply):
      az role assignment create --assignee-object-id "\$(az ad signed-in-user show --query id -o tsv)" --assignee-principal-type User --role "Key Vault Secrets Officer" --scope "/subscriptions/\$(az account show --query id -o tsv)/resourceGroups/$rg"
EOF
}
# Write one setting: a public one to the committed file (and out of the
# local one, where an environment from before #295 kept it); a private one,
# once the settings vault exists, to the vault (and out of the local file,
# so a script that sets it again, like bootstrap.sh, never leaves a second
# copy); anything else to the local file, quoted so it can be sourced. An
# empty value removes a public setting, which then takes its default, as an
# empty one always has; an empty private one is a local "", which shadows
# the vault (teardown.sh detaches the archive that way).
aset() {
  valid_key "$1"
  # One setting, one line: the file is edited line by line.
  [[ $2 != *$'\n'* ]] || { echo "error: $1: values can't contain newlines" >&2; return 1; }
  if is_public "$1"; then
    python3 scripts/lib/env_settings.py --env "$ENV_NAME" --set "$1" --value "$2" || return 1
    load_committed || return 1
    local_del "$1"
    return
  fi
  if is_private "$1" && [ -n "$2" ]; then
    local vault
    vault=$(local_get SETTINGS_KEYVAULT) || return 1
    if [ -n "$vault" ]; then
      vault_set "$vault" "$1" "$2" || return 1
      local_del "$1"
      return
    fi
  fi
  local_set "$1" "$2"
}
# One private setting into the settings vault $1, with the operator's
# sign-in (Entra ID, Key Vault Secrets Officer), unless it already holds
# that value.
vault_set() {
  local cur out
  if $VAULT_LOADED; then
    cur=$(set +u; unset "$2"; eval "$VAULT_SETTINGS"; printf '%s\n' "${!2:-}")
  else
    cur=$(vault_get "$2") || return 1
  fi
  [ "$cur" != "$3" ] || return 0
  if ! out=$(az keyvault secret set --vault-name "$1" --name "$(vault_secret_name "$2")" --value "$3" -o none 2>&1); then
    echo "error: can't set $2 in the settings vault $1: $out" >&2
    vault_access_hint "$1"
    return 1
  fi
  echo "set $2 in the settings vault $1"
  # Read from the vault from now on, not from what load_vault read.
  VAULT_LOADED=false
}
# One setting into the local file, quoted so it can be sourced.
local_set() {
  local v tmp
  mkdir -p "$(dirname "$ENV_FILE")"
  touch "$ENV_FILE"
  v=$(quote_value "$2")
  tmp=$(mktemp)
  # grep's 1 means "no other lines"; anything above is a read error, and
  # writing the file back would lose every other setting.
  grep -v "^$1=" "$ENV_FILE" > "$tmp" || [ $? -eq 1 ] ||
    { rm -f "$tmp"; echo "error: can't read $ENV_FILE" >&2; return 1; }
  printf '%s="%s"\n' "$1" "$v" >> "$tmp"
  mv "$tmp" "$ENV_FILE"
}
# Remove one setting: a public one from the committed file (and the local
# one), anything else from the local file. A private setting in the vault
# stays there: az keyvault secret delete removes it.
adel() {
  valid_key "$1"
  if is_public "$1"; then
    python3 scripts/lib/env_settings.py --env "$ENV_NAME" --unset "$1" || return 1
    load_committed || return 1
  fi
  local_del "$1"
}
local_del() {
  [ -f "$ENV_FILE" ] || return 0
  local tmp
  tmp=$(mktemp)
  grep -v "^$1=" "$ENV_FILE" > "$tmp" || [ $? -eq 1 ] ||
    { rm -f "$tmp"; echo "error: can't read $ENV_FILE" >&2; return 1; }
  mv "$tmp" "$ENV_FILE"
}
# A private setting's secret in the settings vault: Key Vault names allow
# letters, digits and hyphens, so USNM_ALERT_EMAILS is USNM-ALERT-EMAILS.
vault_secret_name() { printf '%s' "${1//_/-}"; }
# Read the settings vault's secrets into VAULT_SETTINGS, with the
# operator's sign-in (Key Vault Secrets Officer, which the stack gives the
# operators in USNM_SETTINGS_VAULT_ADMINS and the account that deploys it).
# The vault's name is a stack output; before the first provision that
# creates it, there's nothing to read. A
# secret that isn't a private setting stops here, like an unknown key in a
# committed file.
load_vault() {
  local vault names n key v lines=""
  VAULT_SETTINGS="" VAULT_KEYS=""
  vault=$(local_get SETTINGS_KEYVAULT) || return 1
  # A local file without it (another operator's machine, a new checkout)
  # takes it from the stack, so the private settings aren't deployed empty.
  if [ -z "$vault" ]; then
    vault=$(stack_settings_vault) || return 1
    [ -z "$vault" ] || local_set SETTINGS_KEYVAULT "$vault" || return 1
  fi
  [ -n "$vault" ] || { VAULT_LOADED=true; return 0; }
  names=$(az keyvault secret list --vault-name "$vault" --query "[].name" -o tsv) ||
    { echo "error: can't list the secrets in the settings vault $vault" >&2; vault_access_hint "$vault"; return 1; }
  for n in $names; do
    key=$(tr 'a-z-' 'A-Z_' <<< "$n")
    is_private "$key" ||
      { echo "error: $vault holds $n, which isn't a private setting (infra/env/schema.json)" >&2; return 1; }
    v=$(az keyvault secret show --vault-name "$vault" --name "$n" --query value -o tsv) ||
      { echo "error: can't read $n from $vault" >&2; return 1; }
    [[ $v != *$'\n'* ]] || { echo "error: $vault's $n has more than one line" >&2; return 1; }
    lines+="$key=\"$(quote_value "$v")\""$'\n'
    VAULT_KEYS+=" $key"
  done
  VAULT_SETTINGS=$lines VAULT_LOADED=true
}
# The settings vault's name from the stack's outputs (SETTINGS_KEYVAULT,
# whose case the stack may change): empty without a stack or that output.
stack_settings_vault() {
  local out err errf
  errf=$(mktemp)
  if ! out=$(az stack sub show -n "usnm-$ENV_NAME" --query outputs -o json 2> "$errf"); then
    err=$(cat "$errf")
    rm -f "$errf"
    grep -q DeploymentStackNotFound <<< "$err" && return 0
    echo "error: can't read the stack usnm-$ENV_NAME's outputs: $err" >&2
    return 1
  fi
  rm -f "$errf"
  python3 -c 'import json, sys
for k, v in (json.loads(sys.argv[1] or "{}") or {}).items():
    if k.upper() == "SETTINGS_KEYVAULT":
        print(v.get("value") or "")' "$out"
}
# Moving an environment to another subscription (bootstrap.sh --move)
# rebuilds it there with a new settings vault (its name comes from the
# subscription): the old vault's private settings come back to the local
# file, where they stay until the new one exists (provision then says how
# to move them), and the old vault is forgotten. A setting the local file
# already has is kept.
vault_detach() {
  local k v vault
  vault=$(local_get SETTINGS_KEYVAULT) || return 1
  [ -n "$vault" ] || return 0
  load_vault || return 1
  for k in $VAULT_KEYS; do
    local_has "$k" && continue
    v=$(aget "$k") || return 1
    local_set "$k" "$v" || return 1
  done
  local_del SETTINGS_KEYVAULT || return 1
  VAULT_SETTINGS="" VAULT_KEYS="" VAULT_LOADED=false
  echo "the private settings of $vault are in $ENV_FILE until the new settings vault exists"
}
# vault_detach for a move from subscription $1 to $2: az takes a Key Vault
# token from the selected subscription's tenant, so the old vault is read
# with the old subscription selected (in another tenant, az must be signed
# in there too; az login keeps the other sign-ins), then $2 is selected
# again, whatever happened.
vault_detach_from() {
  local old=$1 target=$2 tenant rc=0
  [ -n "$(local_get SETTINGS_KEYVAULT)" ] || return 0
  tenant=$(local_get AZURE_TENANT_ID) || return 1
  if ! az account set --subscription "$old" 2> /dev/null; then
    echo "error: az can't select the old subscription $old, where the settings vault is: az login --tenant ${tenant:-<its tenant>} (az keeps your other sign-ins), then run this again with --subscription $target" >&2
    return 1
  fi
  vault_detach || rc=1
  az account set --subscription "$target" || return 1
  return $rc
}
# Each setting in its place, before a deployment reads them: a public one
# only in the committed file, a private one in the vault or the local file
# but not both. A private one still in the local file once the vault exists
# is a warning, with the commands that move it. load_vault first.
settings_check() {
  local k v errors="" moves=""
  for k in $(local_keys); do
    if is_public "$k"; then
      errors+="  $k is in $ENV_FILE; public settings are committed, in $COMMITTED_FILE"$'\n'
    elif is_private "$k" && [ -n "$(local_get "$k")" ]; then
      if in_list "$k" "$VAULT_KEYS"; then
        errors+="  $k is in both $ENV_FILE and the settings vault; keep one: scripts/settings.sh --env $ENV_NAME --key $k --unset (the local one)"$'\n'
      elif [ -n "$(local_get SETTINGS_KEYVAULT)" ]; then
        moves+="  az keyvault secret set --vault-name $(local_get SETTINGS_KEYVAULT) --name $(vault_secret_name "$k") --value \"\$(scripts/settings.sh --env $ENV_NAME --key $k)\" -o none && scripts/settings.sh --env $ENV_NAME --key $k --unset"$'\n'
      fi
    fi
  done
  if [ -n "$moves" ]; then
    printf 'warning: private settings still in %s; move them to the settings vault:\n%s' "$ENV_FILE" "$moves" >&2
  fi
  [ -n "$errors" ] || return 0
  printf 'error: settings in the wrong place:\n%s' "$errors" >&2
  echo "scripts/settings.sh --env $ENV_NAME --migrate moves the public ones into $COMMITTED_FILE, to commit" >&2
  return 1
}
# Move an environment from before #295 over: every public setting in the
# local file into the committed file, unless the committed file already
# has another value (then nothing moves). Private settings stay local until
# the vault exists; provision's warning then says how to move them.
migrate_settings() {
  local k v c conflicts="" moved=0
  for k in $(local_keys); do
    is_public "$k" || continue
    v=$(local_get "$k") || return 1
    c=$(committed_get "$k")
    if [ -n "$v" ] && [ -n "$c" ] && [ "$v" != "$c" ]; then
      conflicts+="  $k: $COMMITTED_FILE has \"$c\", $ENV_FILE \"$v\""$'\n'
    fi
  done
  if [ -n "$conflicts" ]; then
    printf 'error: these differ; settle each in one place first (scripts/settings.sh --env %s --key K --value V):\n%s' "$ENV_NAME" "$conflicts" >&2
    return 1
  fi
  for k in $(local_keys); do
    is_public "$k" || continue
    v=$(local_get "$k") || return 1
    c=$(committed_get "$k")
    if [ -n "$v" ] && [ -z "$c" ]; then
      python3 scripts/lib/env_settings.py --env "$ENV_NAME" --set "$k" --value "$v" || return 1
      load_committed || return 1
    fi
    local_del "$k" || return 1
    moved=$((moved + 1))
  done
  echo "moved $moved public settings from $ENV_FILE to $COMMITTED_FILE"
  for k in $(local_keys); do
    is_private "$k" && echo "private, stays in $ENV_FILE until the settings vault exists: $k"
  done
  return 0
}
# Whether the committed file (and the schema) match the checkout's commit:
# scripts/provision.sh deploys only committed settings, unless --allow-dirty.
committed_clean() {
  local dir out
  dir=$(dirname "$COMMITTED_FILE")
  out=$(git -C "$dir" status --porcelain -- "$(basename "$COMMITTED_FILE")" schema.json 2>&1) ||
    { echo "error: can't check $COMMITTED_FILE with git: $out" >&2; return 2; }
  [ -z "$out" ] || { printf '%s\n' "$out" >&2; return 1; }
}

# One deployment stack per environment, at subscription scope (it holds the
# resource groups). Resources dropped from the template are deleted; deny
# settings stop anyone deleting a managed resource outside the stack.
STACK="usnm-$ENV_NAME"
# The region, with the same default as infra/main.bicepparam.
stack_location() { local l; l=$(aget AZURE_LOCATION) || return 1; echo "${l:-eastus2}"; }
deploy_stack() (
  # Export exactly the settings infra/main.bicepparam reads, when they have
  # a value; an unset one takes its default there. Each is read on its own
  # (aget), so the settings file never sets this script's own variables
  # (STACK, ENV_NAME, ...), and a variable left in the caller's shell can't
  # stand in for a setting the file doesn't have.
  local k v
  for k in $(grep -o "readEnvironmentVariable('[A-Za-z0-9_]*'" infra/main.bicepparam | cut -d"'" -f2); do
    unset "$k"
    v=$(aget "$k") || exit 1
    [ -z "$v" ] || export "$k=$v"
  done
  # The stack's name comes from the environment's; the template's must match.
  export AZURE_ENV_NAME=$ENV_NAME
  local location
  location=$(stack_location) || exit 1
  az stack sub create --name "$STACK" --location "$location" \
    --parameters infra/main.bicepparam \
    --action-on-unmanage deleteResources \
    --deny-settings-mode denyDelete \
    --description "usnewsmap $ENV_NAME (scripts/bootstrap.sh, scripts/provision.sh)" \
    --yes --only-show-errors -o none
)
# The template's outputs become settings (API_URL, ACR_NAME, ...). One
# query reads the names and the values from the same object, so they pair
# up in order; tsv prints them as two rows, which awk splits keeping empty
# values. A failed query fails the step, so stale settings aren't kept.
# The stack returns output names with their case changed (BACKFILL_JOB comes
# back as backfilL_JOB); main.bicep declares them all in upper case, so that's
# the name to save. Earlier runs saved the returned names: drop those.
save_outputs() {
  local out k v raw
  out=$(az stack sub show -n "$STACK" \
    --query "[keys(outputs), values(outputs)[].value]" -o tsv) || return 1
  while IFS=$'\t' read -r raw k v; do
    [ -n "$k" ] || continue
    aset "$k" "$v" || return 1
    [ "$raw" = "$k" ] || adel "$raw" || return 1
  done < <(awk -F'\t' 'NR == 1 { n = split($0, k, "\t") }
      NR == 2 { split($0, v, "\t"); for (i = 1; i <= n; i++) print k[i] "\t" toupper(k[i]) "\t" v[i] }' <<< "$out")
}
# With USNM_OCR_NODES off, the stack deletes the nodes' monitoring (their
# workspace, Application Insights component and data collection rule), and
# Azure won't delete a rule that still has associations, so every try below
# would fail. Stops first, naming them; offboard.sh deletes them.
# Returns 0 when there's nothing to check or the rule is already gone.
ocr_nodes_off_check() {
  local nodes dcr out err errf
  nodes=$(aget USNM_OCR_NODES) || return 1
  dcr=$(aget OCR_NODE_DCR_ID) || return 1
  [ "$nodes" != true ] && [ -n "$dcr" ] || return 0
  errf=$(mktemp)
  if ! out=$(az rest --method get --url "https://management.azure.com$dcr/associations?api-version=2022-06-01" \
    --query 'value[].id' -o tsv 2> "$errf"); then
    err=$(cat "$errf")
    rm -f "$errf"
    grep -q ResourceNotFound <<< "$err" && return 0
    echo "error: can't list the associations of ${dcr##*/}: $err" >&2
    return 1
  fi
  rm -f "$errf"
  [ -n "$out" ] || return 0
  echo "error: USNM_OCR_NODES is off, so the provision would delete ${dcr##*/}, which these still use:" >&2
  while IFS= read -r err; do echo "  $err"; done <<< "$out" >&2
  echo "offboard those nodes first (scripts/ocr-node/offboard.sh), or turn USNM_OCR_NODES back on" >&2
  return 1
}
# A deleted Key Vault stays soft-deleted, holding its name, for its
# retention (7 days outside prod), and the stack can't create one under a
# reserved name, so recover a soft-deleted vault of this environment first;
# the stack takes it back over with its secrets. The OCR nodes' vault
# (usnmocrkv<env><suffix>, ADR-0017) goes when USNM_OCR_NODES goes off, so
# it's recovered when they're on; the settings vault
# (usnmcorekv<env><suffix>, #295) always, as the stack always has one.
# Prod's have purge protection and are recovered the same way.
vaults_recover() {
  local nodes rg out v want="starts_with(name, 'usnmcorekv')"
  nodes=$(aget USNM_OCR_NODES) || return 1
  [ "$nodes" != true ] || want="($want || starts_with(name, 'usnmocrkv'))"
  rg=$(aget AZURE_RESOURCE_GROUP) || return 1
  [ -n "$rg" ] || return 0
  # Only a vault deleted from this environment's own group: the name prefix
  # alone could match another environment's vault (the suffix comes from the
  # full environment name), so the original vault id must name this group.
  out=$(az keyvault list-deleted --resource-type vault \
    --query "[?$want && contains(to_lower(properties.vaultId), to_lower('/resourceGroups/$rg/providers/'))].name" \
    -o tsv 2>&1) ||
    { echo "error: can't list deleted Key Vaults: $out" >&2; return 1; }
  [ -n "$out" ] || return 0
  while IFS= read -r v; do
    echo "recovering soft-deleted Key Vault $v before the deployment re-creates it"
    az keyvault recover --name "$v" -o none || return 1
  done <<< "$out"
}
# dev exists only while an experiment runs (#275; docs/operations.md, "Dev
# environment lifecycle"), so its provision needs USNM_EXPERIMENT, the
# experiment's issue number. The template tags rg-usnm-<env> with it and
# with USNM_EXPERIMENT_STARTED, set here: the group's own `started` while
# its `experiment` is the same, otherwise now. teardown.sh's provision sets
# NO_EXPERIMENT_CHECK, since it only takes the environment apart.
experiment_marker() {
  local exp out tagged started=""
  [ "$ENV_NAME" = dev ] || return 0
  # Teardown's provision reads and checks nothing: a bad marker or a tag it
  # can't read mustn't stop the detach.
  [ "${NO_EXPERIMENT_CHECK:-}" = true ] && return 0
  exp=$(aget USNM_EXPERIMENT) || return 1
  if [ -z "$exp" ]; then
    echo "error: dev exists only during an experiment: first scripts/settings.sh --env dev --key USNM_EXPERIMENT --value <issue number> (or scripts/stand-up.sh)" >&2
    return 1
  fi
  [[ $exp =~ ^[0-9]+$ ]] || { echo "error: USNM_EXPERIMENT is an issue number, not \"$exp\"" >&2; return 1; }
  if out=$(az group show -n "rg-usnm-$ENV_NAME" --query "[tags.experiment, tags.started]" -o tsv 2>&1); then
    IFS=$'\t' read -r tagged started <<< "$out"
    [ "$tagged" = "$exp" ] || started=""
  elif ! grep -qiE 'ResourceGroupNotFound|could not be found' <<< "$out"; then
    echo "error: can't read the tags of rg-usnm-$ENV_NAME: $out" >&2
    return 1
  fi
  [ -n "$started" ] || started=$(date -u +%Y-%m-%dT%H:%M:%SZ)
  aset USNM_EXPERIMENT_STARTED "$started"
}
# New roles and permissions take a minute or two to apply everywhere, so a
# deployment that depends on one can fail once; it's idempotent, so retry.
# The guard-rail stack for this environment: created in its region, with
# its alert recipients for the budget on dev's groups, and dev's region when
# dev provisions (scripts/lib/guardrails.sh).
deploy_env_guardrails() {
  local location emails dev_location=""
  location=$(stack_location) || return 1
  emails=$(aget USNM_ALERT_EMAILS) || return 1
  [ "$ENV_NAME" != dev ] || dev_location=$location
  deploy_guardrails "$location" "$emails" "$dev_location"
}
provision() {
  load_vault || die "not provisioning $ENV_NAME"
  settings_check || die "not provisioning $ENV_NAME"
  experiment_marker || die "not provisioning $ENV_NAME"
  ocr_nodes_off_check || die "not provisioning $ENV_NAME"
  vaults_recover || die "not provisioning $ENV_NAME"
  for attempt in 1 2 3; do
    deploy_env_guardrails && deploy_stack && save_outputs && return 0
    [ "$attempt" = 3 ] && die "provisioning failed three times"
    echo "retrying in 60s"
    sleep 60
  done
}
