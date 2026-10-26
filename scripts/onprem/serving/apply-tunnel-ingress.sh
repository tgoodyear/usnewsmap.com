#!/usr/bin/env bash
# Applies the usnm-serving tunnel's ingress (#955) from tunnel-ingress.json,
# which mirrors it in the repository (ADR-0005 decision 3): each hostname to
# Caddy on the serving container's loopback, and 404 for anything else.
# Run by hand from the operator's machine, signed in to az, from the address
# the Cloudflare API token is limited to (ADR-0024).
#
#   scripts/onprem/serving/apply-tunnel-ingress.sh --account-id ID
#     --tunnel-id UUID --vault NAME --token-secret NAME
#     [--hostname NAME ...] [--dry-run]
#
# --vault and --token-secret: the Key Vault secret holding the Cloudflare
#   API token (Cloudflare Tunnel: Edit on the account). It's read with your
#   az sign-in into this process only: never printed, never on a command
#   line, never written to disk.
# --hostname: route only these hostnames, each usnewsmap.com or a name under
#   it, in place of the file's list. Repeat it for more than one. Phase 2
#   routes one test hostname; the public cutover (Phase 3) applies the file
#   as it is.
# --dry-run: print the configuration it would apply, and stop before reading
#   the token or calling Cloudflare.
#
# It prints the tunnel's current configuration and the new one, sends the
# new one (PUT .../cfd_tunnel/<id>/configurations; cloudflared picks it up
# without a restart), and reads it back to check it took. DNS records and
# zone settings aren't touched here.
set -euo pipefail
usage() { sed -n '8,10s/^# \{0,1\}//p' "$0" >&2; exit 2; }
die() { echo "error: $*" >&2; exit 1; }

ACCOUNT="" TUNNEL="" VAULT="" SECRET="" HOSTNAMES="" DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --account-id) ACCOUNT=${2:-}; shift 2 || usage ;;
    --tunnel-id) TUNNEL=${2:-}; shift 2 || usage ;;
    --vault) VAULT=${2:-}; shift 2 || usage ;;
    --token-secret) SECRET=${2:-}; shift 2 || usage ;;
    --hostname)
      [[ ${2:-} =~ ^([a-z0-9]([a-z0-9-]*[a-z0-9])?\.)*usnewsmap\.com$ ]] ||
        { echo "error: --hostname: usnewsmap.com or a name under it" >&2; usage; }
      HOSTNAMES="$HOSTNAMES $2"
      shift 2
      ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $ACCOUNT =~ ^[0-9a-f]{32}$ ]] || { echo "error: --account-id: the Cloudflare account id (32 hex digits)" >&2; usage; }
[[ $TUNNEL =~ ^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$ ]] ||
  { echo "error: --tunnel-id: the usnm-serving tunnel's id (a UUID)" >&2; usage; }
$DRY_RUN || [[ $VAULT =~ ^[A-Za-z0-9-]{3,24}$ && $SECRET =~ ^[A-Za-z0-9-]{1,127}$ ]] ||
  { echo "error: --vault and --token-secret: the Key Vault secret with the Cloudflare API token" >&2; usage; }
command -v jq > /dev/null || die "jq is needed"
HERE=$(cd "$(dirname "$0")" && pwd)

# The file's rules, with their hostnames replaced by --hostname's if given:
# each named rule takes the first named rule's service, and the catch-all
# stays last.
config=$(jq -c --arg names "$HOSTNAMES" '
  ($names | split(" ") | map(select(length > 0))) as $names
  | .config.ingress as $rules
  | ($rules | map(select(has("hostname")))) as $named
  | ($rules | map(select(has("hostname") | not))) as $rest
  | if ($rest | length) != 1 or ($rest[0].service | startswith("http_status:") | not) or ($rules[-1] | has("hostname"))
    then error("tunnel-ingress.json needs one catch-all rule, last") else . end
  | if ($names | length) == 0 then .
    else .config.ingress = ([$names[] as $n | $named[0] + {hostname: $n}] + $rest) end
  | if [.config.ingress[] | select(has("hostname")) | .service] | all(. == "http://127.0.0.1:8080") then .
    else error("every hostname must go to http://127.0.0.1:8080") end
' "$HERE/tunnel-ingress.json") || die "tunnel-ingress.json doesn't check out"

echo "The ingress to apply:"
jq . <<< "$config"
if $DRY_RUN; then
  echo "dry run: nothing read or sent"
  exit 0
fi

URL="https://api.cloudflare.com/client/v4/accounts/$ACCOUNT/cfd_tunnel/$TUNNEL/configurations"
TOKEN=$(az keyvault secret show --vault-name "$VAULT" -n "$SECRET" --query value -o tsv) ||
  die "can't read $SECRET from $VAULT"
[ -n "$TOKEN" ] || die "$SECRET in $VAULT is empty"
# The token goes to curl in a header file it reads from a pipe, never as an
# argument.
cf() { curl -fsS --max-time 60 -H @<(printf 'Authorization: Bearer %s\n' "$TOKEN") "$@"; }
before=$(cf "$URL") || die "can't read the tunnel's configuration (is the token limited to another address?)"
echo "The tunnel's ingress now:"
jq '.result.config.ingress' <<< "$before"
after=$(cf -X PUT -H 'Content-Type: application/json' --data-binary @- "$URL" <<< "$config") ||
  die "Cloudflare refused the configuration"
[ "$(jq -r .success <<< "$after")" = true ] || die "Cloudflare refused the configuration: $(jq -c .errors <<< "$after")"
check=$(cf "$URL")
unset TOKEN
# The hostnames and services, which is what the file sets.
rules='map(with_entries(select(.key == "hostname" or .key == "service")))'
want=$(jq -S ".config.ingress | $rules" <<< "$config")
got=$(jq -S ".result.config.ingress | $rules" <<< "$check")
[ "$want" = "$got" ] || die "the tunnel's ingress reads back differently: $got"
echo "applied: the tunnel's ingress is as above"
