#!/usr/bin/env bash
# Sets up serving in the serving container (#955): the searcher, the two API
# colors, Caddy, the cloudflared unit and the update timer. Run as root in
# the container, from a checkout of this repository, after home-infra's
# scripts/lab/serving-ct.sh has made the container (with podman, skopeo,
# Caddy, cloudflared, gh, jq and curl) and it has joined Azure Arc. Safe to
# run again.
#
#   scripts/onprem/serving/install.sh --registry NAME --config-dir DIR
#     [--primary acr|ghcr] [--env prod] [--skip-quickwit] [--production] [--dry-run]
#
# --registry: the environment's registry name (NAME.azurecr.io), which the
#   Arc identity can pull from (AcrPull).
# --primary: where usnm-serving-update reads and pulls the API image: acr, or
#   ghcr with ACR as its fallback (update-api.sh). Without this flag, the
#   value already in host.env stays; a new install starts on acr.
# --config-dir: gen-env.sh's output: api.env, searcher.env, searcher.yaml.
# --env: the environment whose pinned Quickwit to install
#   (infra/quickwit-image.json, through scripts/onprem/install-quickwit.sh).
# --skip-quickwit: keep the Quickwit binary that's installed.
# --dry-run: say what would change, and change nothing.
#
# It installs:
#   /etc/usnm-serving/      api.env (0600), searcher.env, searcher.yaml,
#                           host.env (HIMDS_GID, the registry's name and
#                           the primary source of the API image),
#                           upstream.caddy (only if missing: blue's port)
#   /etc/caddy/Caddyfile, /etc/tmpfiles.d/usnm-serving.conf,
#   /etc/systemd/journald.conf.d/usnm-serving.conf (30 days of journal)
#   /etc/systemd/system/    usnm-searcher.service, usnm-api@.service,
#                           cloudflared-usnm-serving.service,
#                           usnm-serving-update.service and .timer,
#                           usnm-edge-warm.service and .timer
#   /usr/local/sbin/usnm-serving-update (update-api.sh)
#   /usr/local/libexec/usnm-serving/run-cloudflared (run-cloudflared.sh)
#   /usr/local/libexec/usnm-serving/warm-edge (warm-edge.py)
#   /srv/usnm-serving/bin/quickwit
# and the system user usnm-qw, which runs the searcher. It enables Caddy,
# the searcher, the timer and cloudflared, and starts or reloads them. The
# searcher restarts only when its config, environment, binary or unit
# changed, since a restart empties its caches; cloudflared only when its unit
# changed, and it starts only once its token file exists.
# No API color starts here: the timer's first run deploys the image prod
# runs to blue. The response cache, /srv/usnm-serving/cache, should be the
# host's bind mount from the NVMe pool; it is created if missing and given
# to the API image's node user (1000), and the tmpfiles rule makes the shared
# directory in it. Entries from the older per-color directories are hard-linked
# in, so the next color starts warm. A running color keeps the mount it started
# with until the next deploy.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '9,10s/^# \{0,1\}//p' "$0" >&2; exit 2; }

REGISTRY="" PRIMARY="" CONFIG="" ENV_NAME=prod SKIP_QW=false DRY_RUN=false PRODUCTION=false
while [ $# -gt 0 ]; do
  case "$1" in
    --registry) REGISTRY=${2:-}; shift 2 || usage ;;
    --primary) PRIMARY=${2:-}; shift 2 || usage ;;
    --config-dir) CONFIG=${2:-}; shift 2 || usage ;;
    --env) ENV_NAME=${2:-}; shift 2 || usage ;;
    --skip-quickwit) SKIP_QW=true; shift ;;
    --production) PRODUCTION=true; shift ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[[ $REGISTRY =~ ^[a-z0-9]{5,50}$ ]] || { echo "error: --registry: the registry's name, as in NAME.azurecr.io" >&2; usage; }
[[ $PRIMARY =~ ^(acr|ghcr|)$ ]] || { echo "error: --primary: acr or ghcr" >&2; usage; }
{ [ -n "$CONFIG" ] && [ -d "$CONFIG" ]; } || { echo "error: --config-dir: gen-env.sh's output directory" >&2; usage; }
[[ $ENV_NAME =~ ^[a-z0-9]{1,8}$ ]] || { echo "error: --env: an environment name, such as prod" >&2; usage; }
HERE=$(cd "$(dirname "$0")" && pwd)
# USNM_SERVING_TEST_ROOT: the tests' stand-in for /. Never set in the container.
R=${USNM_SERVING_TEST_ROOT:-}
[ "$DRY_RUN" = true ] || [ -n "$R" ] || [ "$(id -u)" -eq 0 ] || die "run as root in the serving container"
ETC=$R/etc/usnm-serving
SRV=$R/srv/usnm-serving
UNITS=$R/etc/systemd/system
CACHE_UID=1000

for f in api.env searcher.env searcher.yaml; do
  [ -s "$CONFIG/$f" ] || die "no $CONFIG/$f; run gen-env.sh --out $CONFIG first"
done
if ! $PRODUCTION; then
  grep -qE '^(USNM_SEARCH_LOG_URL|USNM_OCR_REQUESTS_URL)=' "$CONFIG/api.env" &&
    die "$CONFIG/api.env sets the search log or OCR requests, which require explicit --production at cutover (#955 Phase 3)"
fi
for tool in podman skopeo caddy cloudflared gh jq curl flock sha256sum; do
  command -v "$tool" > /dev/null || die "no $tool here: make the container with home-infra's scripts/lab/serving-ct.sh"
done
HIMDS_GID=$(getent group himds | cut -d: -f3 || true)
[[ $HIMDS_GID =~ ^[0-9]+$ ]] || die "no himds group: connect the container to Azure Arc first"

changed=false
# Installs $1 as $2 with mode $3 when it differs; sets changed=true if so.
put() {
  if [ -f "$2" ] && cmp -s "$1" "$2" && [ "$(stat -c %a "$2")" = "${3#0}" ]; then
    return 0
  fi
  changed=true
  if $DRY_RUN; then
    echo "would install $2 (mode $3)${4:+ from $4}"
  else
    mkdir -p "$(dirname "$2")"
    install -m "$3" "$1" "$2.new"
    mv "$2.new" "$2"
    echo "installed $2"
  fi
}
run() { if $DRY_RUN; then echo "would run: $*"; else "$@"; fi; }

# The searcher's user, and the directories.
if ! getent passwd usnm-qw > /dev/null; then
  run useradd --system --user-group --no-create-home --home-dir /nonexistent --shell /usr/sbin/nologin usnm-qw
fi
run mkdir -p "$ETC" "$SRV/bin" "$R/var/lib/usnm-serving"
run chmod 700 "$R/var/lib/usnm-serving"
if [ ! -d "$SRV/cache" ]; then
  run mkdir -p "$SRV/cache"
fi
mountpoint -q "$SRV/cache" 2> /dev/null ||
  echo "note: $SRV/cache isn't a mount point; bind-mount the NVMe pool's cache dataset there before serving"
if [ "$(stat -c %u "$SRV/cache" 2> /dev/null || true)" != "$CACHE_UID" ]; then
  run chown "$CACHE_UID:$CACHE_UID" "$SRV/cache"
fi
run chmod 750 "$SRV/cache"
run mkdir -p "$SRV/sitemap-state"
run chown "$CACHE_UID:$CACHE_UID" "$SRV/sitemap-state"
run chmod 750 "$SRV/sitemap-state"

# The index the searcher reads must be there, as the config names it.
index=$(sed -n 's|^default_index_root_uri: file://||p' "$CONFIG/searcher.yaml")
[[ $index == /* ]] || die "$CONFIG/searcher.yaml has no file:// default_index_root_uri"
[ -d "$R$index" ] || echo "note: no $index here yet; the searcher won't start until the host mounts the index there"

# Settings.
T=$(mktemp -d)
trap 'rm -rf "$T"' EXIT
if [ -z "$PRIMARY" ]; then
  PRIMARY=$(sed -n 's/^USNM_SERVING_PRIMARY=//p' "$ETC/host.env" 2> /dev/null | tail -1 || true)
  [[ $PRIMARY =~ ^(acr|ghcr)$ ]] || PRIMARY=acr
fi
printf 'HIMDS_GID=%s\nUSNM_SERVING_REGISTRY=%s\nUSNM_SERVING_PRIMARY=%s\n' "$HIMDS_GID" "$REGISTRY" "$PRIMARY" > "$T/host.env"
put "$T/host.env" "$ETC/host.env" 0644
changed=false
put "$CONFIG/api.env" "$ETC/api.env" 0600
! $changed || [ ! -s "$R/var/lib/usnm-serving/active" ] ||
  echo "note: api.env changed; the serving color keeps the old settings until the next deploy, or run: usnm-serving-update --registry $REGISTRY --redeploy"
changed=false
put "$CONFIG/searcher.env" "$ETC/searcher.env" 0644
put "$CONFIG/searcher.yaml" "$ETC/searcher.yaml" 0644
searcher_changed=$changed cloudflared_changed=false
if [ ! -f "$ETC/upstream.caddy" ]; then
  printf 'reverse_proxy 127.0.0.1:8081\n' > "$T/upstream.caddy"
  put "$T/upstream.caddy" "$ETC/upstream.caddy" 0644
fi

# Units and configs from this directory. A changed unit restarts only its
# own service.
units_changed=false
for unit in usnm-searcher.service usnm-api@.service cloudflared-usnm-serving.service \
  usnm-serving-update.service usnm-serving-update.timer \
  usnm-edge-warm.service usnm-edge-warm.timer; do
  changed=false
  put "$HERE/$unit" "$UNITS/$unit" 0644
  if $changed; then
    units_changed=true
    case $unit in
      usnm-searcher.service) searcher_changed=true ;;
      cloudflared-usnm-serving.service) cloudflared_changed=true ;;
    esac
  fi
done
changed=false
put "$HERE/Caddyfile" "$R/etc/caddy/Caddyfile" 0644
caddy_changed=$changed
put "$HERE/usnm-serving.tmpfiles" "$R/etc/tmpfiles.d/usnm-serving.conf" 0644
changed=false
put "$HERE/journald-usnm-serving.conf" "$R/etc/systemd/journald.conf.d/usnm-serving.conf" 0644
journald_changed=$changed
put "$HERE/update-api.sh" "$R/usr/local/sbin/usnm-serving-update" 0755
put "$HERE/warm-edge.py" "$R/usr/local/libexec/usnm-serving/warm-edge" 0755
changed=false
put "$HERE/run-cloudflared.sh" "$R/usr/local/libexec/usnm-serving/run-cloudflared" 0755
! $changed || cloudflared_changed=true

if ! $SKIP_QW; then
  changed=false
  before=$(readlink "$SRV/bin/quickwit" 2> /dev/null || true)
  run "$HERE/../install-quickwit.sh" --registry "$REGISTRY" --env "$ENV_NAME" --dest "$SRV/bin"
  [ "$(readlink "$SRV/bin/quickwit" 2> /dev/null || true)" = "$before" ] || searcher_changed=true
fi

! $units_changed || run systemctl daemon-reload
run systemd-tmpfiles --create usnm-serving.conf
# Entries are immutable, so hard links carry the per-color caches over at no cost.
for color in blue green; do
  [ -d "$SRV/cache/$color" ] || continue
  run cp -aln "$SRV/cache/$color/." "$SRV/cache/shared/" ||
    echo "note: some $color cache entries weren't linked; they're rebuilt on demand" >&2
done
! $journald_changed || run systemctl restart systemd-journald.service
run systemctl enable --quiet caddy.service usnm-searcher.service usnm-serving-update.timer usnm-edge-warm.timer cloudflared-usnm-serving.service
if systemctl is-active --quiet caddy.service; then
  ! $caddy_changed || run systemctl reload caddy.service
else
  run systemctl start caddy.service
fi
if systemctl is-active --quiet usnm-searcher.service; then
  if $searcher_changed; then
    echo "the searcher's config, binary or unit changed: restarting it (its caches start cold)"
    run systemctl restart usnm-searcher.service
  fi
else
  run systemctl start usnm-searcher.service
fi
run systemctl start usnm-serving-update.timer usnm-edge-warm.timer
if [ -s "$R/etc/cloudflared/usnm-serving.env" ]; then
  if systemctl is-active --quiet cloudflared-usnm-serving.service; then
    ! $cloudflared_changed || run systemctl restart cloudflared-usnm-serving.service
  else
    run systemctl start cloudflared-usnm-serving.service
  fi
else
  echo "note: no /etc/cloudflared/usnm-serving.env yet; home-infra's serving-ct.sh --tunnel installs the tunnel token"
fi
