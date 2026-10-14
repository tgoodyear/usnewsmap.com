#!/usr/bin/env bash
# The publish job's command (infra/boinc/main.bicep, publishJob): copies the signed app version
# baked into this image (Containerfile) onto the project share, installs its templates, and runs
# the stock bin/update_versions, which reads the .sig files and adds the app_version rows.
#
# Versions are immutable: if apps/<app>/<version>/ is already on the share it must be identical,
# and then only update_versions runs again (it skips versions it already has).
set -euo pipefail
# shellcheck source=SCRIPTDIR/../server/lib.sh
. /usr/local/lib/usnm-boinc/lib.sh
: "${APPVER_APP:?}" "${APPVER_VERSION:?}"
[ -e "$PROJ/.usnm-initialised" ] || fail "project is not initialised"
src=/usr/local/lib/usnm-boinc/appver
dst="$PROJ/apps/$APPVER_APP/$APPVER_VERSION"
if [ -e "$dst" ]; then
  diff -r "$src/apps/$APPVER_APP/$APPVER_VERSION" "$dst" > /dev/null ||
    fail "$dst is already on the share and differs from this image: versions are immutable, release a new version"
  log "$dst is already on the share and identical"
else
  mkdir -p "$PROJ/apps/$APPVER_APP"
  rm -rf "$dst.partial"
  cp -r "$src/apps/$APPVER_APP/$APPVER_VERSION" "$dst.partial"
  mv "$dst.partial" "$dst"
  log "copied $APPVER_APP $APPVER_VERSION to $dst"
fi
mkdir -p "$PROJ/templates"
cp "$src"/templates/* "$PROJ/templates/"
fetch_secrets
render_config
(cd "$PROJ" && bin/update_versions --noconfirm)
chown -R boincadm:boincadm "$dst" "$PROJ/templates" "$PROJ/download" 2> /dev/null ||
  log "chown on the share failed (UNVERIFIED semantics, design.md 2.3)"
log "published $APPVER_APP $APPVER_VERSION"
