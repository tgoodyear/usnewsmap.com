#!/usr/bin/env bash
# Keeps the serving container's API on CI's prod deployment (#955),
# modelled on scripts/ocr-node/update-worker.sh. install.sh installs it as
# /usr/local/sbin/usnm-serving-update, and usnm-serving-update.timer runs it
# 2 minutes after the last run ended.
#
#   usnm-serving-update --registry NAME [--environment prod] [--commit SHA]
#     [--redeploy] [--ready-secs 600] [--drain-secs 130] [--dry-run]
#
# The target is the commit of the newest deployment of the GitHub
# environment (CI's publish job records one per run) whose latest status is
# success, read from the repository's public API without a token. The
# requests are conditional (If-None-Match, with the ETag kept from the last
# answer), so a run that finds nothing new uses none of the 60 requests an
# hour GitHub allows an address without a token. A commit this host
# rejected is skipped for the next older one; the commit it serves stops
# the search, so it never goes back.
#
# A deploy:
#  1. reads the registry's usnewsmap-api:<commit> manifest (NAME.azurecr.io)
#     with the container's Arc identity, which has AcrPull: its Resource
#     Manager token is exchanged for a registry refresh token, as
#     scripts/onprem/install-quickwit.sh does;
#  2. checks the manifest's build provenance attestation: signed by this
#     repository's ci.yml on main, for that commit (`gh attestation verify
#     --bundle`; gh is never signed in, and the public attestations API
#     needs no token);
#  3. pulls the image by digest and writes the idle color's
#     /etc/usnm-serving/api-<color>.env (blue on :8081, green on :8082);
#  4. starts that color and waits up to --ready-secs for its /readyz;
#  5. points Caddy at it (upstream.caddy, then `systemctl reload caddy`) and
#     checks Caddy's admin API names the new port and its /readyz answers
#     through Caddy;
#  6. enables the new color at boot, disables the old one, waits
#     --drain-secs for requests in flight (a search computes for up to
#     120 s), stops the old color and removes images neither color names.
# If the new color doesn't get ready, or the switch doesn't take, the old
# color keeps serving, the new one stops, and the commit goes into
# /var/lib/usnm-serving/rejected, never to be tried again; but a new color
# that doesn't get ready while the serving one isn't ready either points at
# the host (the index, the lake), so its commit isn't rejected. A commit
# whose deployment succeeded but whose image has no valid attestation is
# rejected too.
#
# --commit deploys that commit whatever the deployments say (a manual
# rollback; the image still needs its attestation) and then creates
# /etc/usnm-serving/hold. While that file exists, the timer's runs do
# nothing; delete it to follow CI again. --redeploy deploys the target to
# the idle color even when the active one serves it already: the way to
# apply a changed api.env without downtime.
#
# A run does nothing while the searcher isn't ready, since no new color
# could get ready, and takes /run/lock/usnm-serving-update.lock, so runs
# never overlap. Its lines go to the journal under usnm-serving-update; a
# deploy, a rejection and any problem are logged at Warning. --dry-run says
# what it would deploy and changes nothing.
set -euo pipefail
usage() { sed -n '7,8s/^# \{0,1\}//p' "$0" >&2; exit 2; }
# Under systemd, a <N> prefix sets a line's syslog priority.
pri() { if [ -n "${JOURNAL_STREAM:-}" ]; then printf '<%s>%s\n' "$1" "$2"; else printf '%s\n' "$2"; fi; }
log() { pri 5 "$*"; }
warn() { pri 4 "$*"; }

REGISTRY="" ENVIRONMENT=prod COMMIT="" REDEPLOY=false READY_SECS=600 DRAIN_SECS=130 DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --registry) REGISTRY=${2:-}; shift 2 || usage ;;
    --environment) ENVIRONMENT=${2:-}; shift 2 || usage ;;
    --commit) COMMIT=${2:-}; shift 2 || usage ;;
    --redeploy) REDEPLOY=true; shift ;;
    --ready-secs) READY_SECS=${2:-}; shift 2 || usage ;;
    --drain-secs) DRAIN_SECS=${2:-}; shift 2 || usage ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $REGISTRY =~ ^[a-z0-9]{5,50}$ ]] || { echo "error: --registry: the registry's name, as in NAME.azurecr.io" >&2; usage; }
[[ $ENVIRONMENT =~ ^[a-z0-9-]{1,32}$ ]] || { echo "error: --environment: a GitHub environment, such as prod" >&2; usage; }
[ -z "$COMMIT" ] || [[ $COMMIT =~ ^[0-9a-f]{40}$ ]] || { echo "error: --commit: a full commit hash" >&2; usage; }
[[ $READY_SECS =~ ^[1-9][0-9]{0,4}$ ]] || { echo "error: --ready-secs: 1 to 99999" >&2; usage; }
[[ $DRAIN_SECS =~ ^[0-9]{1,4}$ ]] || { echo "error: --drain-secs: 0 to 9999" >&2; usage; }

REPO=tgoodyear/usnewsmap.com
WORKFLOW="$REPO/.github/workflows/ci.yml"
API="https://api.github.com/repos/$REPO"
IMAGE_REPO=usnewsmap-api
HOST=$REGISTRY.azurecr.io
# USNM_SERVING_TEST_ROOT: the tests' stand-in for /. Never set in the container.
R=${USNM_SERVING_TEST_ROOT:-}
[ "$DRY_RUN" = true ] || [ -n "$R" ] || [ "$(id -u)" -eq 0 ] || { echo "error: run as root" >&2; exit 1; }
ETC=$R/etc/usnm-serving
STATE=$R/var/lib/usnm-serving
REJECTED=$STATE/rejected
LOCK=$R/run/lock/usnm-serving-update.lock
TOKENS=$R/var/opt/azcmagent/tokens

port_of() { case $1 in blue) echo 8081 ;; green) echo 8082 ;; esac; }
# One KEY=value from an environment file, empty if it isn't there.
env_get() { [ ! -f "$1" ] || sed -n "s/^$2=//p" "$1" | tail -1; }
rejected() { [ -s "$REJECTED" ] && grep -qxF "$1" "$REJECTED"; }

if [ -z "$COMMIT" ] && [ -e "$ETC/hold" ]; then
  log "held: $ETC/hold exists, so the API stays on $(env_get "$ETC/api-$(cat "$STATE/active" 2> /dev/null).env" API_COMMIT)"
  exit 0
fi
TMP=$(mktemp -d "${TMPDIR:-/var/tmp}/usnm-serving-update.XXXXXX")
trap 'rm -rf "$TMP"' EXIT
if [ "$DRY_RUN" = false ]; then
  mkdir -p "$(dirname "$LOCK")" "$STATE"
  exec 9> "$LOCK"
  if ! flock -n 9; then
    log "another run holds $LOCK"
    exit 0
  fi
fi
# The kept ETags and answers. A dry run outside the tests keeps them in its
# temporary directory.
CACHE=$STATE/github
[ "$DRY_RUN" = false ] || [ -n "$R" ] || CACHE=$TMP/github

if ! systemctl is-active --quiet usnm-searcher.service ||
  ! curl -fsS --max-time 5 -o /dev/null http://127.0.0.1:7280/health/readyz; then
  log "the searcher isn't ready; no update"
  exit 0
fi

active=$(cat "$STATE/active" 2> /dev/null || true)
case $active in
  blue | green | "") ;;
  *) warn "warning: $STATE/active holds \"$active\", not blue or green; fix it by hand"; exit 1 ;;
esac
current=""
[ -z "$active" ] || current=$(env_get "$ETC/api-$active.env" API_COMMIT)

# A GET of the public API, conditional on the ETag of the last answer,
# which is kept with its body in $CACHE/NAME.*: a 304 reuses the body. The
# body on stdout.
gh_get() {
  local url=$1 name=$2 etag code
  local hdr=()
  mkdir -p "$CACHE"
  etag=$(cat "$CACHE/$name.etag" 2> /dev/null || true)
  [ -z "$etag" ] || [ ! -s "$CACHE/$name.json" ] || hdr=(-H "If-None-Match: $etag")
  code=$(curl -sS --retry 3 --max-time 60 -H 'Accept: application/vnd.github+json' ${hdr[@]+"${hdr[@]}"} \
    -D "$CACHE/$name.headers" -o "$CACHE/$name.body" -w '%{http_code}' "$url") || return 1
  case $code in
    200)
      mv "$CACHE/$name.body" "$CACHE/$name.json"
      tr -d '\r' < "$CACHE/$name.headers" | sed -n 's/^[Ee][Tt][Aa][Gg]: *//p' | tail -1 > "$CACHE/$name.etag"
      ;;
    304) rm -f "$CACHE/$name.body" ;;
    *) rm -f "$CACHE/$name.body" "$CACHE/$name.headers"; return 1 ;;
  esac
  rm -f "$CACHE/$name.headers"
  cat "$CACHE/$name.json"
}

target=$COMMIT
if [ -z "$target" ]; then
  deployments=$(gh_get "$API/deployments?environment=$ENVIRONMENT&per_page=10" deployments) ||
    { warn "warning: can't list the $ENVIRONMENT deployments of $REPO"; exit 1; }
  # Newest first: "id sha" lines.
  list=$(jq -r '.[] | select(.task == "deploy") | "\(.id) \(.sha)"' <<< "$deployments") ||
    { warn "warning: the deployments answer isn't a list"; exit 1; }
  checked=0 kept=""
  while read -r id sha; do
    [[ $id =~ ^[0-9]+$ && $sha =~ ^[0-9a-f]{40}$ ]] || continue
    checked=$((checked + 1))
    [ "$checked" -le 5 ] || break
    kept="$kept status-$id"
    statuses=$(gh_get "$API/deployments/$id/statuses?per_page=1" "status-$id") ||
      { warn "warning: can't read the status of deployment $id"; exit 1; }
    [ "$(jq -r '.[0].state // ""' <<< "$statuses")" = success ] || continue
    if [ "$sha" = "$current" ]; then
      target=$sha
      break
    fi
    if rejected "$sha"; then continue; fi
    target=$sha
    break
  done <<< "$list"
  # The answers kept for deployments no longer looked at.
  for f in "$CACHE"/status-*.json; do
    [ -e "$f" ] || continue
    name=$(basename "$f" .json)
    case " $kept " in *" $name "*) ;; *) rm -f "$CACHE/$name.json" "$CACHE/$name.etag" ;; esac
  done
  if [ -z "$target" ]; then
    log "no successful $ENVIRONMENT deployment among the newest five that isn't rejected"
    exit 0
  fi
fi
if [ "$target" = "$current" ] && ! $REDEPLOY; then
  [ -z "$COMMIT" ] || log "$active serves $current already"
  exit 0
fi

idle=blue
[ "$active" != blue ] || idle=green
port=$(port_of "$idle")
if [ "$DRY_RUN" = true ]; then
  log "dry run: would deploy $target to $idle (now ${current:-nothing}${active:+ on $active})"
  exit 0
fi
log "deploying $target to $idle (now ${current:-nothing}${active:+ on $active})"

# A token from the Arc agent's identity endpoint for the resource (URL-
# encoded), on stdout. Only one of the agent's own challenge files is read:
# a .key file directly in its token directory, regular, at most 4 KiB.
arc_token() {
  local u key file tokens
  u="http://localhost:40342/metadata/identity/oauth2/token?api-version=2020-06-01&resource=$1"
  key=$(curl -s --noproxy '*' --max-time 20 -D - -o /dev/null -H Metadata:true "$u" | tr -d '\r' |
    sed -n 's/^[Ww][Ww][Ww]-[Aa]uthenticate: Basic realm=//p')
  [ -n "$key" ] || { echo "no challenge from the Arc agent's identity endpoint" >&2; return 1; }
  tokens=$(realpath "$TOKENS") || return 1
  file=$(realpath -e -- "$R$key" 2> /dev/null) || { echo "the challenge names a file that can't be read" >&2; return 1; }
  [[ $file == *.key && $(dirname "$file") == "$tokens" && -f $file && ! -L $R$key ]] ||
    { echo "the challenge file isn't one of the agent's" >&2; return 1; }
  [ "$(stat -c %s "$file")" -le 4096 ] || { echo "the challenge file is too large" >&2; return 1; }
  curl -s --noproxy '*' --max-time 20 -H Metadata:true \
    -H @<(printf 'Authorization: Basic %s\n' "$(cat "$file")") "$u" | jq -er .access_token
}
# A registry auth file for $HOST at $1 (mode 0600): the Resource Manager
# token exchanged for a registry refresh token. Neither token is ever on a
# command line.
registry_login() {
  local arm refresh
  arm=$(arc_token https%3A%2F%2Fmanagement.azure.com%2F) || return 1
  refresh=$(printf '%s' "$arm" | curl -fsS --max-time 30 -X POST "https://$HOST/oauth2/exchange" \
    --data-urlencode grant_type=access_token --data-urlencode "service=$HOST" \
    --data-urlencode "access_token@-" | jq -er .refresh_token) || return 1
  (
    umask 077
    printf '{"auths":{"%s":{"auth":"%s"}}}\n' "$HOST" \
      "$(printf '00000000-0000-0000-0000-000000000000:%s' "$refresh" | base64 -w0)" > "$1"
  )
}
reject() {
  mkdir -p "$STATE"
  echo "$target" >> "$REJECTED"
  warn "$*; $target is rejected"
  exit 1
}

registry_login "$TMP/auth.json" ||
  { warn "warning: can't sign in to $HOST with the Arc identity (is AcrPull granted?)"; exit 1; }
if ! skopeo inspect --raw --authfile "$TMP/auth.json" "docker://$HOST/$IMAGE_REPO:$target" \
  > "$TMP/manifest.json" 2> "$TMP/skopeo.err"; then
  if grep -qi 'manifest unknown\|not found' "$TMP/skopeo.err"; then
    log "no $IMAGE_REPO:$target in $HOST yet"
    exit 0
  fi
  warn "warning: can't read $IMAGE_REPO:$target from $HOST: $(tail -1 "$TMP/skopeo.err")"
  exit 1
fi
# The tag's own manifest, whose digest the attestation names: an index when
# the build added its own provenance, otherwise the image manifest.
digest=sha256:$(sha256sum "$TMP/manifest.json" | awk '{ print $1 }')

if ! curl -fsSL --retry 3 --max-time 60 -H 'Accept: application/vnd.github+json' \
  "$API/attestations/$digest" > "$TMP/attestations.json"; then
  warn "warning: can't read the attestations of $IMAGE_REPO@$digest"
  exit 1
fi
jq -c '.attestations[]?.bundle' "$TMP/attestations.json" > "$TMP/bundles.jsonl" || true
[ -s "$TMP/bundles.jsonl" ] || reject "$IMAGE_REPO@$digest has no attestation"
mkdir -p "$TMP/gh"
env -u GH_TOKEN -u GITHUB_TOKEN GH_CONFIG_DIR="$TMP/gh" GH_NO_UPDATE_NOTIFIER=1 GH_PROMPT_DISABLED=1 \
  gh attestation verify "$TMP/manifest.json" --bundle "$TMP/bundles.jsonl" --repo "$REPO" \
  --signer-workflow "$WORKFLOW" --source-ref refs/heads/main --source-digest "$target" > /dev/null ||
  reject "$IMAGE_REPO@$digest has no valid attestation from $WORKFLOW on main for $target"
log "$IMAGE_REPO@$digest: attestation verified for $target"

podman pull --quiet --authfile "$TMP/auth.json" "$HOST/$IMAGE_REPO@$digest" > /dev/null ||
  { warn "warning: can't pull $HOST/$IMAGE_REPO@$digest"; exit 1; }
rm -f "$TMP/auth.json"

mkdir -p "$ETC"
printf 'API_IMAGE=%s\nAPI_COMMIT=%s\nUSNM_BIND=127.0.0.1:%s\n' "$HOST/$IMAGE_REPO@$digest" "$target" "$port" \
  > "$ETC/api-$idle.env.new"
mv "$ETC/api-$idle.env.new" "$ETC/api-$idle.env"
systemctl restart "usnm-api@$idle.service"
ready=false
deadline=$((SECONDS + READY_SECS))
while [ "$SECONDS" -lt "$deadline" ]; do
  if curl -fsS --max-time 5 -o /dev/null "http://127.0.0.1:$port/readyz"; then
    ready=true
    break
  fi
  systemctl is-active --quiet "usnm-api@$idle.service" || break
  sleep 2
done
if ! $ready; then
  systemctl stop "usnm-api@$idle.service" || true
  # Only the new image's fault if the serving color is ready: a missing
  # index or an unreachable lake keeps every color from getting ready.
  if [ -n "$active" ] && ! curl -fsS --max-time 5 -o /dev/null "http://127.0.0.1:$(port_of "$active")/readyz"; then
    warn "warning: $idle didn't get ready on :$port within $READY_SECS s, and neither is $active; $target isn't rejected (journalctl -u usnm-api@$idle)"
    exit 1
  fi
  reject "$idle didn't get ready on :$port within $READY_SECS s (journalctl -u usnm-api@$idle)"
fi

# Caddy's upstream, written whole and renamed into place.
point_caddy() {
  printf 'reverse_proxy 127.0.0.1:%s\n' "$1" > "$ETC/upstream.caddy.new"
  mv "$ETC/upstream.caddy.new" "$ETC/upstream.caddy"
  systemctl reload caddy.service
}
# True when Caddy forwards to port $1 alone and the API answers through it.
caddy_on() {
  local dials
  dials=$(curl -fsS --max-time 5 http://127.0.0.1:2019/config/apps/http/servers |
    jq -r '[.. | objects | .dial? // empty] | unique | join(" ")') || return 1
  [ "$dials" = "127.0.0.1:$1" ] && curl -fsS --max-time 5 -o /dev/null http://127.0.0.1:8080/readyz
}
switched=false
if point_caddy "$port"; then
  for _ in 1 2 3 4 5; do
    if caddy_on "$port"; then
      switched=true
      break
    fi
    sleep 2
  done
fi
if ! $switched; then
  [ -z "$active" ] || point_caddy "$(port_of "$active")" || warn "warning: can't point Caddy back at $active"
  systemctl stop "usnm-api@$idle.service" || true
  reject "Caddy didn't switch to $idle on :$port"
fi
echo "$idle" > "$STATE/active.new"
mv "$STATE/active.new" "$STATE/active"
systemctl enable --quiet "usnm-api@$idle.service"
if [ -n "$active" ]; then
  systemctl disable --quiet "usnm-api@$active.service" || true
  sleep "$DRAIN_SECS"
  systemctl stop "usnm-api@$active.service" || warn "warning: can't stop usnm-api@$active"
fi
[ -z "$COMMIT" ] || touch "$ETC/hold"
warn "usnm-api deployed: ${current:-nothing} -> $target on $idle${COMMIT:+; held at it until $ETC/hold is deleted}"

# Images neither color names. One still in use stays.
keep=" $(env_get "$ETC/api-blue.env" API_IMAGE) $(env_get "$ETC/api-green.env" API_IMAGE) "
podman images --digests --format '{{.Repository}}@{{.Digest}}' | while read -r ref; do
  [ "${ref%@*}" = "$HOST/$IMAGE_REPO" ] || continue
  case $keep in *" $ref "*) continue ;; esac
  podman rmi "$ref" > /dev/null 2>&1 || true
done
