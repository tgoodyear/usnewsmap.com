#!/usr/bin/env bash
# The OCR data store on the Proxmox host pve-big (#368, 12 §12.2.4): ZFS
# datasets under <pool>/ocr for the scans fetched ahead of the workers and
# for ALTO and text later, and their owner. Run on the host as root; safe
# to run again.
#
#   pve-ocr-storage.sh [--pool tank] [--quota 3T] [--dry-run]
#   pve-ocr-storage.sh --lvm pve/data [--size 160G] [--inodes 10485760] [--dry-run]
#
# Creates, or sets the properties of (ZFS mode, pve-big):
#   <pool>/ocr        /<pool>/ocr        lz4, atime=off, quota --quota
#   <pool>/ocr/scans  /<pool>/ocr/scans  lz4, recordsize=1M (JPEGs, written once)
#   <pool>/ocr/text   /<pool>/ocr/text   zstd, recordsize=128K
# or (LVM-thin spool mode, pve-laptop):
#   /dev/<vg>/ocr-spool mounted at /tank/ocr (ext4, high inode density for .done markers)
# and a plain directory /tank/ocr/manifests for the prefetch manifests.
#
# Everything is owned by ocrdata (uid and gid 102000), mode 2775. 102000 is
# what uid 2000 in an unprivileged container maps to, so the scan-fetch
# container (scanfetch-ct.sh) writes as its own uid 2000.
#
# Nothing is exported. The nodes read scans from the container's scan
# server over HTTP, not NFS (ADR-0015 decision 9), so an export file an
# earlier version wrote, /etc/exports.d/usnm-ocr.exports, is removed.
# nfs-kernel-server stays installed, with no usnm exports.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,22s/^# \{0,1\}//p' "$0" >&2; exit 2; }

POOL=tank QUOTA=3T LVM_POOL="" LVM_SIZE=160G LVM_INODES=10485760 DRY=false
OWNER=ocrdata OWNER_ID=102000
EXPORTS=/etc/exports.d/usnm-ocr.exports
while [ $# -gt 0 ]; do
  case "$1" in
    --pool) POOL=${2:-}; shift 2 ;;
    --quota) QUOTA=${2:-}; shift 2 ;;
    --lvm) LVM_POOL=${2:-}; shift 2 ;;
    --size) LVM_SIZE=${2:-}; shift 2 ;;
    --inodes) LVM_INODES=${2:-}; shift 2 ;;
    --dry-run) DRY=true; shift ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[ "$(id -u)" = 0 ] || die "run as root on the Proxmox host"

run() { if $DRY; then echo "+ $*"; else "$@"; fi; }

if [ -n "$LVM_POOL" ]; then
  [[ $LVM_POOL =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "--lvm: expected VG/THINPOOL (e.g. pve/data)"
  [[ $LVM_SIZE =~ ^[0-9]+[KMGTP]$ ]] || die "--size: an LVM size such as 160G"
  [[ $LVM_INODES =~ ^[0-9]+$ ]] || die "--inodes: an inode count"
  VG=${LVM_POOL%/*}
  DEV="/dev/$VG/ocr-spool"
  ROOT=/tank/ocr
  if ! lvs "$DEV" > /dev/null 2>&1; then
    run lvcreate -V "$LVM_SIZE" -T "$LVM_POOL" -n ocr-spool
    run mkfs.ext4 -m 1 -N "$LVM_INODES" -L ocr-spool "$DEV"
  fi
  run mkdir -p "$ROOT"
  if ! grep -qs "^$DEV " /etc/fstab; then
    if $DRY; then
      echo "+ fstab: $DEV $ROOT ext4 defaults,noatime,nofail 0 2"
    else
      printf '%s %s ext4 defaults,noatime,nofail 0 2\n' "$DEV" "$ROOT" >> /etc/fstab
    fi
  fi
  mountpoint -q "$ROOT" || run mount "$ROOT"
  run mkdir -p "$ROOT/scans/keep" "$ROOT/scans/buffer" "$ROOT/text" "$ROOT/manifests"
else
  [[ $POOL =~ ^[A-Za-z0-9_.-]+$ ]] || die "--pool: a pool name"
  [[ $QUOTA =~ ^[0-9]+[KMGTP]?$ ]] || die "--quota: a ZFS size such as 3T"
  command -v zfs > /dev/null || die "no zfs here (use --lvm VG/THINPOOL on LVM hosts)"

  # A dataset with these properties, created or brought up to date.
  dataset() {
    local name=$1; shift
    if zfs list -H -o name "$name" > /dev/null 2>&1; then
      for p in "$@"; do run zfs set "$p" "$name"; done
    else
      local opts=()
      for p in "$@"; do opts+=(-o "$p"); done
      run zfs create "${opts[@]}" "$name"
    fi
  }

  dataset "$POOL/ocr" compression=lz4 atime=off "quota=$QUOTA"
  dataset "$POOL/ocr/scans" compression=lz4 recordsize=1M
  dataset "$POOL/ocr/text" compression=zstd recordsize=128K
  ROOT=$(zfs get -H -o value mountpoint "$POOL/ocr" 2> /dev/null || echo "/$POOL/ocr")
  run mkdir -p "$ROOT/manifests"
fi
if ! getent group "$OWNER" > /dev/null; then
  run groupadd --system --gid "$OWNER_ID" "$OWNER"
fi
if ! id "$OWNER" > /dev/null 2>&1; then
  run useradd --system --uid "$OWNER_ID" --gid "$OWNER_ID" --no-create-home \
    --home-dir /nonexistent --shell /usr/sbin/nologin "$OWNER"
fi
[ "$(id -u "$OWNER" 2> /dev/null || echo "$OWNER_ID")" = "$OWNER_ID" ] ||
  $DRY || die "$OWNER exists with another uid"

for d in "$ROOT" "$ROOT/scans" "$ROOT/text" "$ROOT/manifests"; do
  run chown "$OWNER_ID:$OWNER_ID" "$d"
  run chmod 2775 "$d"
done
for d in "$ROOT/scans/keep" "$ROOT/scans/buffer"; do
  if [ -d "$d" ]; then
    run chown "$OWNER_ID:$OWNER_ID" "$d"
    run chmod 2775 "$d"
  fi
done

# No NFS export: remove the one an earlier version wrote.
[ ! -f "$EXPORTS" ] || run rm -f "$EXPORTS"
# Reloaded every run, so an export left active by a failed earlier run goes.
if command -v exportfs > /dev/null; then run exportfs -ra; fi
if [ -n "$LVM_POOL" ]; then
  run df -h "$ROOT"
  run df -i "$ROOT"
else
  run zfs list -o name,mountpoint,quota,compression,recordsize,atime -r "$POOL/ocr"
fi
