#!/usr/bin/env bash
# Fetch Reichsanzeiger-GT page images for pages not in an exclusion list.
# Usage: fetch-rz.sh --repo DIR --exclude FILE --dest DIR
set -euo pipefail
repo=""; exclude=""; dest=""
while [ $# -gt 0 ]; do case "$1" in --repo) repo="$2"; shift 2;; --exclude) exclude="$2"; shift 2;; --dest) dest="$2"; shift 2;; *) echo "unknown flag $1" >&2; exit 2;; esac; done
[ -n "$repo" ] && [ -n "$exclude" ] && [ -n "$dest" ] || { echo "--repo --exclude --dest required" >&2; exit 2; }
base=$(grep -A1 'urlbase=' "$repo/data/download_images.sh" | tr -d '\n' | sed 's/.*echo "//; s/".*//' | base64 -d)
mkdir -p "$dest"
n=0
while read -r path name; do
  stem="${name%.jpg}"
  grep -qx "reichsanzeiger-$stem" "$exclude" && continue
  [ -s "$dest/$name" ] || { curl -fsS --retry 3 -m 300 -o "$dest/$name" "$base$path"; sleep 1; }
  n=$((n+1))
done < "$repo/data/imageurls.list"
echo "FETCH-RZ-DONE $n"
