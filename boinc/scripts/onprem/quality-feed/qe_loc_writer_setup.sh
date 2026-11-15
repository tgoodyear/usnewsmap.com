#!/bin/bash
# Gives CT 230 the quality_text_writer login that qe-text-collect writes quality.qe_loc_scores
# with. Run as root on pve-laptop, where CT 215 holds the role's client certificate and key
# (/etc/usnm/quality-text-db: ca.pem, client.crt, client.key, dsn). They are copied node to node
# over the host trust pve-laptop -> pve into /project/qetext/writer on CT 230 (root only, mode
# 600), and the DSN file's certificate paths are rewritten for the server container. Nothing is
# printed. Usage: qe_loc_writer_setup.sh [--src-ct 215] [--dst-host pve.tjg.fyi] [--dst-ct 230]
set -euo pipefail
src_ct=215; dst_host=pve.tjg.fyi; dst_ct=230
while [ $# -gt 0 ]; do case "$1" in
  --src-ct) src_ct=$2; shift 2;; --dst-host) dst_host=$2; shift 2;; --dst-ct) dst_ct=$2; shift 2;;
  *) echo "unknown $1" >&2; exit 2;; esac; done
dest=/srv/usnm-boinc/project/qetext/writer
pct exec "$src_ct" -- tar -C /etc/usnm/quality-text-db -c . \
  | ssh -o BatchMode=yes "$dst_host" "pct exec $dst_ct -- sh -c 'mkdir -p $dest /srv/usnm-boinc/project/qetext/units && tar -x --no-same-owner -C $dest && chmod 700 $dest && chmod 600 $dest/* && sed -i \"s|/etc/usnm/quality-text-db|/project/qetext/writer|g\" $dest/dsn'"
