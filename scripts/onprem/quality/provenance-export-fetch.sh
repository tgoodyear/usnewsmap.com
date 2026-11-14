#!/usr/bin/env bash
# The fetch half of the daily page provenance run (#1956). On pve-laptop (the Proxmox host, as
# root), from usnm-quality-provenance-fetch.timer: runs boinc/scripts/provenance-export.py inside
# the BOINC server container on CT 230 (read-only, the server's own database login) and keeps its
# output on the batch container's outbox mount, where provenance-export-unpack.sh reads it. The
# data goes pve to pve-laptop, host to host; nothing passes through a workstation.
#
#   provenance-export-fetch.sh [--script /opt/usnm-quality-feed/provenance-export.py]
#     [--dest /var/lib/usnm-quality-feed/outbox/provenance] [--boinc-host pve.tjg.fyi] [--boinc-ct 230]
#
# The new file replaces export.tar.gz only after it unpacks cleanly and holds wu.tsv and av.tsv,
# so a failed run leaves yesterday's export (which provenance-export-unpack.sh then ages out).
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
SCRIPT=/opt/usnm-quality-feed/provenance-export.py
DEST=/var/lib/usnm-quality-feed/outbox/provenance
HOST=pve.tjg.fyi CT=230
while [ $# -gt 0 ]; do
  case $1 in
    --script) SCRIPT=${2:-}; shift 2 ;;
    --dest) DEST=${2:-}; shift 2 ;;
    --boinc-host) HOST=${2:-}; shift 2 ;;
    --boinc-ct) CT=${2:-}; shift 2 ;;
    *) die "unknown argument: $1" ;;
  esac
done
[[ $CT =~ ^[0-9]+$ ]] || die "--boinc-ct: a number"
[ -r "$SCRIPT" ] || die "no export script at $SCRIPT"
# 100000 is the batch container's root through the unprivileged mapping, so it can read the file.
install -d -m 0750 -o 100000 -g 100000 "$DEST"
tmp=$(mktemp "$DEST/.export.XXXXXX")
trap 'rm -f "$tmp"' EXIT
ssh -o BatchMode=yes "$HOST" "pct exec $CT -- podman exec -i usnm-boinc-server python3 - export" < "$SCRIPT" > "$tmp"
names=$(tar -tzf "$tmp")
grep -qx wu.tsv <<< "$names" && grep -qx av.tsv <<< "$names" || die "the export holds no wu.tsv or av.tsv"
chown 100000:100000 "$tmp"; chmod 0640 "$tmp"
mv -f "$tmp" "$DEST/export.tar.gz"
trap - EXIT
echo "BOINC export kept: $(stat -c %s "$DEST/export.tar.gz") bytes, $(grep -c '^bundles/.' <<< "$names") bundle files"
