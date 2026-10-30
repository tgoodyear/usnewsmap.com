#!/usr/bin/env bash
# Pulls the BOINC LAN certificate (#1357) from the TLS vault as this host's Arc identity,
# checks it, and hands it to Caddy, which serves boinc.usnewsmap.com to the home LAN.
#
#   lan-tls-sync.sh [--vault NAME] [--secret NAME] [--host NAME] [--dir DIR] [--group GROUP]
#
# The secret (published by .github/workflows/origin-tls.yml, --target boinc-lan) is the
# private key followed by the full chain, in PEM. The host can read only that secret. A
# certificate that fails a check (wrong name, under 7 days left, key mismatch, untrusted
# chain, or older than the installed one) leaves the installed pair untouched and exits 1.
# "uninitialized", the bootstrap placeholder, is not an error.
set -euo pipefail
VAULT=usnmtlskvprodffapzy SECRET=usnm-prod-boinc-lan-tls HOST=boinc.usnewsmap.com
DIR=/etc/usnm-boinc/lan-tls GROUP=caddy
LOCK_FILE=/run/usnm-boinc-lan-tls.lock
IDENTITY_ENDPOINT=${IDENTITY_ENDPOINT:-http://localhost:40342/metadata/identity/oauth2/token}
log() { echo "lan-tls-sync: $*" >&2; }
fail() { log "$*"; exit 1; }
while [ $# -gt 0 ]; do
  case "$1" in
    --vault) VAULT=${2:?}; shift 2 ;;
    --secret) SECRET=${2:?}; shift 2 ;;
    --host) HOST=${2:?}; shift 2 ;;
    --dir) DIR=${2:?}; shift 2 ;;
    --group) GROUP=${2:?}; shift 2 ;;
    *) fail "unknown option $1" ;;
  esac
done
[[ $VAULT =~ ^[a-z0-9-]{3,24}$ && $SECRET =~ ^[A-Za-z0-9-]{1,127}$ ]] || fail "bad --vault or --secret"
[[ $DIR =~ ^/[A-Za-z0-9._/-]+$ ]] || fail "--dir: an absolute path"
# The Arc key file's contents go to this endpoint: loopback only, as lib.sh's kv_token checks.
[[ $IDENTITY_ENDPOINT =~ ^http://(localhost|127\.[0-9]+\.[0-9]+\.[0-9]+|\[::1\])(:[0-9]{1,5})?/[^[:space:]?#]*$ ]] ||
  fail "IDENTITY_ENDPOINT must be plain HTTP on a loopback address"

kv_token() {
  local url realm
  url="$IDENTITY_ENDPOINT?api-version=2020-06-01&resource=https://vault.azure.net"
  # himds answers first with a 401 naming a root-only key file; its contents are the password.
  realm="$(curl --noproxy '*' --connect-timeout 5 --max-time 30 -sS -o /dev/null -D - -H "Metadata: true" "$url" | tr -d '\r' |
    sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: *Basic realm=\([^[:space:]]*\).*/\1/p')"
  realm=${realm#\"}; realm=${realm%\"}
  case "$realm" in
    /var/opt/azcmagent/tokens/*.key) [[ $realm != *..* ]] && [ -f "$realm" ] || fail "unexpected Arc challenge file" ;;
    *) fail "no Arc challenge from $IDENTITY_ENDPOINT" ;;
  esac
  curl --noproxy '*' --connect-timeout 5 --max-time 30 -fsS -H "Metadata: true" \
    -H @<(printf 'Authorization: Basic %s\n' "$(cat "$realm")") "$url" | jq -er .access_token
}

# Caddy runs whenever a validated pair is installed. Called on every run, so a restart that
# failed once (or a first certificate after setup) is retried without the files changing.
activate() {
  [ -s "$DIR/fullchain.pem" ] || return 0
  systemctl is-enabled -q caddy 2>/dev/null || systemctl enable -q caddy
  if [ "${1:-}" = restart ] || ! systemctl is-active -q caddy; then
    # Caddy keeps loaded certificates across a reload of an unchanged config; restart instead.
    systemctl restart caddy || fail "caddy failed to start"
  fi
}

sync_certificate() {
umask 077
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
token=$(kv_token) || fail "no Key Vault token"
curl -fsS --max-time 30 -H @<(printf 'Authorization: Bearer %s\n' "$token") \
  "https://$VAULT.vault.azure.net/secrets/$SECRET?api-version=7.4" | jq -er .value > "$work/bundle" ||
  fail "can't read secret $SECRET"
if [ "$(cat "$work/bundle")" = uninitialized ]; then
  log "no certificate published yet"
  activate
  exit 0
fi
awk -v key="$work/privkey.pem" -v crt="$work/fullchain.pem" -v leaf="$work/leaf.pem" '
  /^-----BEGIN .*PRIVATE KEY-----$/ { out = key }
  /^-----BEGIN CERTIFICATE-----$/ { out = crt; n++ }
  out { print > out; if (n == 1 && out == crt) print > leaf }
  /^-----END / { out = "" }' "$work/bundle"
[ -s "$work/privkey.pem" ] && [ -s "$work/leaf.pem" ] || fail "secret is not a key and certificate chain"

openssl x509 -in "$work/leaf.pem" -noout -checkhost "$HOST" | grep -q "does match" || fail "certificate is not for $HOST"
openssl x509 -in "$work/leaf.pem" -noout -checkend 604800 >/dev/null || fail "certificate expires within 7 days"
[ "$(openssl x509 -in "$work/leaf.pem" -noout -pubkey | openssl sha256)" = \
  "$(openssl pkey -in "$work/privkey.pem" -pubout | openssl sha256)" ] || fail "key does not match certificate"
openssl verify -purpose sslserver -untrusted "$work/fullchain.pem" "$work/leaf.pem" >/dev/null ||
  fail "chain does not verify against the system trust store"

if cmp -s "$work/fullchain.pem" "$DIR/fullchain.pem" && cmp -s "$work/privkey.pem" "$DIR/privkey.pem"; then
  activate
  exit 0
fi
if [ -s "$DIR/fullchain.pem" ]; then
  old=$(date -d "$(openssl x509 -in "$DIR/fullchain.pem" -noout -enddate | cut -d= -f2)" +%s)
  new=$(date -d "$(openssl x509 -in "$work/leaf.pem" -noout -enddate | cut -d= -f2)" +%s)
  [ "$new" -ge "$old" ] || fail "published certificate is older than the installed one"
fi
install -d -m 0750 -o root -g "$GROUP" "$DIR"
# New files beside the old, then renames, so Caddy never reads half a pair.
install -m 0640 -o root -g "$GROUP" "$work/privkey.pem" "$DIR/.privkey.pem.new"
install -m 0644 -o root -g "$GROUP" "$work/fullchain.pem" "$DIR/.fullchain.pem.new"
mv -f "$DIR/.privkey.pem.new" "$DIR/privkey.pem"
mv -f "$DIR/.fullchain.pem.new" "$DIR/fullchain.pem"
log "installed $(openssl x509 -in "$DIR/fullchain.pem" -noout -enddate)"
activate restart
}

run_locked() {
  umask 077
  # One run at a time from fetch to activation, so two versions' key and chain never mix.
  exec 9>"$LOCK_FILE"
  flock 9
  sync_certificate
}

if [[ ${BASH_SOURCE[0]} = "$0" ]]; then
  run_locked
fi
