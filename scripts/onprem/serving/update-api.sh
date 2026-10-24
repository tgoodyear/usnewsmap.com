#!/usr/bin/env bash
# Keeps the serving container's API on the image CI rolled out to prod
# (#955), modelled on scripts/ocr-node/update-worker.sh. install.sh installs
# it as /usr/local/sbin/usnm-serving-update, and usnm-serving-update.timer
# runs it 2 minutes after the last run ended.
#
#   usnm-serving-update --registry NAME [--tag main] [--commit SHA]
#     [--redeploy] [--ready-secs 800] [--drain-secs 130] [--dry-run]
#
# Each run reads the registry's usnewsmap-api:main manifest (NAME.azurecr.io,
# the tag CI's publish job moves on every prod run) with the container's Arc
# identity, which has AcrPull: its Resource Manager token is exchanged for
# a registry refresh token, as scripts/onprem/install-quickwit.sh does. When
# the manifest is the one serving, the run ends there, without asking GitHub
# anything.
#
# A new manifest is deployed once it has a build provenance attestation
# signed by this repository's ci.yml on main. CI attests the API image as
# the publish job's last step, after the Azure app has rolled onto it, so an
# attested image is one prod runs. The attestation is read from the
# repository's public API without a token, and `gh attestation verify
# --bundle` checks it (gh is never signed in); its source commit names the
# commit, whose own tag must point at the same manifest. Until the
# attestation appears, the run asks every time for 30 minutes after it
# first saw the manifest, then every 10 minutes; after 3 hours the
# manifest is rejected. GitHub allows 60 requests an hour from an address
# without a token, and only a new image costs any.
#
# A deploy:
#  1. pulls the image by digest and writes the idle color's
#     /etc/usnm-serving/api-<color>.env (blue on :8081, green on :8082);
#  2. starts that color and waits up to --ready-secs for its /readyz: by
#     default the API's own start budget as containerapp.bicep allows it
#     (420 s to load the published version, then a readiness cap of 360 s),
#     plus 20 s;
#  3. points Caddy at it (upstream.caddy, then `systemctl reload caddy`) and
#     checks that Caddy's admin API names the new port and that /readyz
#     answers through Caddy;
#  4. enables the new color at boot, disables the old one, waits
#     --drain-secs for requests in flight (a search computes for up to
#     120 s), stops the old color and removes images neither color names.
# If the new color doesn't get ready, or the switch doesn't take, the old
# color keeps serving, the new one stops, and the manifest's digest goes
# into /var/lib/usnm-serving/rejected, never to be tried again. A new color
# that doesn't get ready while the serving one isn't ready either points at
# the host (an index the searcher lacks, the lake out of reach), so then
# nothing is rejected; nor on the first deploy, with no color to compare. If
# Caddy can't be shown to route back to the old color after a failed
# switch, both colors stay up and /etc/usnm-serving/hold stops the timer
# until someone sets things right. A manifest whose attestation doesn't
# verify is rejected too.
#
# --commit deploys the image of that commit (its usnewsmap-api:<commit> tag,
# attested for that commit) whatever main says, for a manual rollback, and
# then creates /etc/usnm-serving/hold, also when that commit serves
# already. While that file exists, the timer's runs do nothing; delete it
# to follow main again. --redeploy deploys the target to the idle color even
# when the active one serves it already: the way to apply a changed api.env
# without downtime.
#
# A run does nothing while the searcher isn't ready, since no new color
# could get ready, and takes /run/lock/usnm-serving-update.lock, so runs
# never overlap. Its lines go to the journal under usnm-serving-update; a
# deploy, a rejection and any problem are logged at Warning. --dry-run reads
# the registry and the attestation and says what it would deploy, and
# changes nothing.
set -euo pipefail
usage() { sed -n '7,8s/^# \{0,1\}//p' "$0" >&2; exit 2; }
# Under systemd, a <N> prefix sets a line's syslog priority.
pri() { if [ -n "${JOURNAL_STREAM:-}" ]; then printf '<%s>%s\n' "$1" "$2"; else printf '%s\n' "$2"; fi; }
log() { pri 5 "$*"; }
warn() { pri 4 "$*"; }

REGISTRY="" TAG=main COMMIT="" REDEPLOY=false READY_SECS=800 DRAIN_SECS=130 DRY_RUN=false
while [ $# -gt 0 ]; do
  case "$1" in
    --registry) REGISTRY=${2:-}; shift 2 || usage ;;
    --tag) TAG=${2:-}; shift 2 || usage ;;
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
[[ $TAG =~ ^[a-z0-9][a-z0-9._-]{0,63}$ ]] || { echo "error: --tag: a registry tag, such as main" >&2; usage; }
[ -z "$COMMIT" ] || [[ $COMMIT =~ ^[0-9a-f]{40}$ ]] || { echo "error: --commit: a full commit hash" >&2; usage; }
[[ $READY_SECS =~ ^[1-9][0-9]{0,4}$ ]] || { echo "error: --ready-secs: 1 to 99999" >&2; usage; }
[[ $DRAIN_SECS =~ ^[0-9]{1,4}$ ]] || { echo "error: --drain-secs: 0 to 9999" >&2; usage; }

REPO=tgoodyear/usnewsmap.com
WORKFLOW="$REPO/.github/workflows/ci.yml"
API="https://api.github.com/repos/$REPO"
IMAGE_REPO=usnewsmap-api
HOST=$REGISTRY.azurecr.io
# How long a new manifest may wait for its attestation, and how often it's
# asked for after the first half hour.
WAIT_SECS=$((3 * 3600)) EAGER_SECS=1800 BACKOFF_SECS=600
# USNM_SERVING_TEST_ROOT: the tests' stand-in for /. Never set in the container.
R=${USNM_SERVING_TEST_ROOT:-}
[ "$DRY_RUN" = true ] || [ -n "$R" ] || [ "$(id -u)" -eq 0 ] || { echo "error: run as root" >&2; exit 1; }
ETC=$R/etc/usnm-serving
STATE=$R/var/lib/usnm-serving
REJECTED=$STATE/rejected
PENDING=$STATE/pending
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
current="" current_image=""
if [ -n "$active" ]; then
  current=$(env_get "$ETC/api-$active.env" API_COMMIT)
  current_image=$(env_get "$ETC/api-$active.env" API_IMAGE)
fi

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
# The manifest a tag points to, saved as $2; its digest on stdout. The
# tag's own manifest is what the attestation names: an index when the build
# added its own provenance, otherwise the image manifest. Fails with
# "missing" on stderr when there's no such tag.
manifest() {
  if ! skopeo inspect --raw --authfile "$TMP/auth.json" "docker://$HOST/$IMAGE_REPO:$1" > "$2" 2> "$TMP/skopeo.err"; then
    if grep -qi 'manifest unknown\|not found' "$TMP/skopeo.err"; then echo missing >&2; else tail -1 "$TMP/skopeo.err" >&2; fi
    return 1
  fi
  echo "sha256:$(sha256sum "$2" | awk '{ print $1 }')"
}
reject() {
  if $DRY_RUN; then
    warn "dry run: $*; would reject $IMAGE_REPO@$digest"
    exit 1
  fi
  mkdir -p "$STATE"
  echo "$digest" >> "$REJECTED"
  rm -f "$PENDING"
  warn "$*; $IMAGE_REPO@$digest is rejected"
  exit 1
}

registry_login "$TMP/auth.json" ||
  { warn "warning: can't sign in to $HOST with the Arc identity (is AcrPull granted?)"; exit 1; }
ref=${COMMIT:-$TAG}
if ! digest=$(manifest "$ref" "$TMP/manifest.json" 2> "$TMP/why"); then
  warn "warning: can't read $IMAGE_REPO:$ref from $HOST: $(cat "$TMP/why")"
  exit 1
fi
if [ "$HOST/$IMAGE_REPO@$digest" = "$current_image" ] && ! $REDEPLOY; then
  if [ -n "$COMMIT" ]; then
    $DRY_RUN || touch "$ETC/hold"
    log "$active serves $COMMIT already; held at it until $ETC/hold is deleted"
  fi
  exit 0
fi
if [ -z "$COMMIT" ] && rejected "$digest"; then exit 0; fi

# The attestation. For main, not asked for more often than the schedule
# above allows.
now=$(date +%s)
if [ -z "$COMMIT" ]; then
  p_digest="" p_first=0 p_last=0
  [ ! -s "$PENDING" ] || read -r p_digest p_first p_last < "$PENDING" || true
  [[ $p_first =~ ^[0-9]+$ && $p_last =~ ^[0-9]+$ ]] || p_digest=""
  if [ "$p_digest" != "$digest" ]; then
    p_first=$now p_last=0
  elif [ $((now - p_first)) -gt "$WAIT_SECS" ]; then
    reject "no attestation $((WAIT_SECS / 3600)) hours after the registry first showed it"
  elif [ $((now - p_first)) -gt "$EAGER_SECS" ] && [ $((now - p_last)) -lt "$BACKOFF_SECS" ]; then
    exit 0
  fi
  $DRY_RUN || { mkdir -p "$STATE"; echo "$digest $p_first $now" > "$PENDING"; }
fi
if ! curl -fsSL --retry 3 --max-time 60 -H 'Accept: application/vnd.github+json' \
  "$API/attestations/$digest" > "$TMP/attestations.json" 2> /dev/null; then
  # The API answers 404 for a digest it has no attestation of.
  echo '{"attestations": []}' > "$TMP/attestations.json"
fi
jq -c '.attestations[]?.bundle' "$TMP/attestations.json" > "$TMP/bundles.jsonl" || true
if [ ! -s "$TMP/bundles.jsonl" ]; then
  if [ -n "$COMMIT" ]; then
    warn "warning: $IMAGE_REPO:$COMMIT ($digest) has no attestation; not deploying it"
    exit 1
  fi
  log "waiting for the attestation of $IMAGE_REPO@$digest (CI attests it once prod runs it)"
  exit 0
fi
mkdir -p "$TMP/gh"
source_digest=()
[ -z "$COMMIT" ] || source_digest=(--source-digest "$COMMIT")
if ! env -u GH_TOKEN -u GITHUB_TOKEN GH_CONFIG_DIR="$TMP/gh" GH_NO_UPDATE_NOTIFIER=1 GH_PROMPT_DISABLED=1 \
  gh attestation verify "$TMP/manifest.json" --bundle "$TMP/bundles.jsonl" --repo "$REPO" \
  --signer-workflow "$WORKFLOW" --source-ref refs/heads/main ${source_digest[@]+"${source_digest[@]}"} \
  --format json > "$TMP/verified.json" 2> /dev/null; then
  reject "its attestation doesn't verify as built by $WORKFLOW on main${COMMIT:+ from $COMMIT}"
fi
commits=$(jq -r '[.[].verificationResult.signature.certificate.sourceRepositoryDigest] | unique | .[]' "$TMP/verified.json")
[[ $commits =~ ^[0-9a-f]{40}$ ]] || reject "its attestations name no single source commit"
target=$commits
# The commit's own tag must point at the same manifest. Only an answer from
# the registry rejects; a failed request is tried again next run.
if [ -z "$COMMIT" ]; then
  if ! tagged=$(manifest "$target" "$TMP/commit-manifest.json" 2> "$TMP/why"); then
    [ "$(cat "$TMP/why")" = missing ] || { warn "warning: can't read $IMAGE_REPO:$target from $HOST: $(cat "$TMP/why")"; exit 1; }
    reject "the registry has no $IMAGE_REPO:$target"
  fi
  [ "$tagged" = "$digest" ] || reject "$IMAGE_REPO:$target isn't the same manifest as $IMAGE_REPO:$TAG"
fi
$DRY_RUN || rm -f "$PENDING"
log "$IMAGE_REPO@$digest: attestation verified for $target"

idle=blue
[ "$active" != blue ] || idle=green
port=$(port_of "$idle")
if [ "$DRY_RUN" = true ]; then
  log "dry run: would deploy $target to $idle (now ${current:-nothing}${active:+ on $active})"
  exit 0
fi
log "deploying $target to $idle (now ${current:-nothing}${active:+ on $active})"
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
  # index or an unreachable lake keeps every color from getting ready. With
  # no serving color (the first deploy) nothing shows the host is well, so
  # nothing is rejected either.
  if [ -z "$active" ]; then
    warn "warning: $idle didn't get ready on :$port within $READY_SECS s, and no color serves to compare; $target isn't rejected (journalctl -u usnm-api@$idle)"
    exit 1
  fi
  if ! curl -fsS --max-time 5 -o /dev/null "http://127.0.0.1:$(port_of "$active")/readyz"; then
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
# Points Caddy at port $1 and waits up to 10 s for caddy_on.
switch_to() {
  point_caddy "$1" || return 1
  for _ in 1 2 3 4 5; do
    caddy_on "$1" && return 0
    sleep 2
  done
  return 1
}
if ! switch_to "$port"; then
  # Stop the new color only once Caddy is shown to route to the old one
  # again; it may still carry the traffic otherwise.
  if [ -n "$active" ] && ! switch_to "$(port_of "$active")"; then
    mkdir -p "$STATE"
    echo "$digest" >> "$REJECTED"
    touch "$ETC/hold"
    warn "warning: Caddy didn't switch to $idle on :$port, and its return to $active can't be confirmed. Both colors stay up, and $ETC/hold stops the timer. See which port Caddy routes to (curl -s 127.0.0.1:2019/config/), write that color to $STATE/active, stop the other one, then delete the hold. $IMAGE_REPO@$digest is rejected"
    exit 1
  fi
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
