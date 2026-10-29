#!/usr/bin/env bash
# Sets up the BOINC server's LAN listener (#1357) on CT230, as root, from a directory holding
# this directory's files. Safe to run again. The UniFi gateway's local DNS record is separate
# (docs/boinc-onprem.md, "LAN access").
#
#   lan-tls-setup.sh
set -euo pipefail
here=$(cd "$(dirname "$0")" && pwd)
install -d -m 0755 /etc/caddy /etc/usnm-boinc
# Our Caddyfile before the package: its stock one serves :80, which is Apache's.
install -m 0644 "$here/Caddyfile" /etc/caddy/Caddyfile
if ! dpkg -s caddy >/dev/null 2>&1; then
  apt-get update -qq
  DEBIAN_FRONTEND=noninteractive apt-get install -y -qq -o Dpkg::Options::=--force-confold caddy jq
fi
install -m 0644 "$here/lan-guard.nft" /etc/usnm-boinc/lan-guard.nft
install -m 0755 "$here/lan-tls-sync.sh" /usr/local/sbin/usnm-boinc-lan-tls-sync
install -m 0644 "$here/usnm-boinc-lan-guard.service" "$here/usnm-boinc-lan-tls.service" \
  "$here/usnm-boinc-lan-tls.timer" /etc/systemd/system/
systemctl daemon-reload
systemctl enable --now usnm-boinc-lan-guard.service
systemctl restart usnm-boinc-lan-guard.service
# Caddy runs only once a certificate is installed; until then it would fail on every boot.
systemctl disable --now caddy >/dev/null 2>&1 || true
/usr/local/sbin/usnm-boinc-lan-tls-sync
if [ -s /etc/usnm-boinc/lan-tls/fullchain.pem ]; then
  caddy validate --config /etc/caddy/Caddyfile --adapter caddyfile >/dev/null
  systemctl enable caddy >/dev/null 2>&1
  systemctl restart caddy
fi
systemctl enable --now usnm-boinc-lan-tls.timer
