#!/usr/bin/env bash
# Builds the feed monitor's bundle (#1441): the scripts every feed source and
# the evaluator run, as one reproducible archive. feed-monitor-release.yml
# builds it on main, attests it and publishes it; update.sh installs it on
# each host.
#
#   boinc/scripts/onprem/feed-monitor/bundle.sh --out DIR [--commit SHA]
#
# --out: an empty or missing directory for usnm-feed-monitor.tar.gz and
#   SHA256SUMS.
# --commit: the commit to bundle (default HEAD). Its files are read from
#   git, never from the working tree, so the bundle is exactly that commit's.
#
# The archive holds the files below, flat, mode 0644, owned by 0:0, with the
# commit's time as every mtime, plus VERSION.json: the version (its release
# tag, feed-monitor-<yyyymmdd>-<sha7>, the date the commit's in UTC), the
# commit, its time in Unix seconds, and each file's sha256. The same commit
# always gives the same bytes. Needs GNU tar and gzip.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '7s/^# \{0,1\}//p' "$0" >&2; exit 2; }

# What the source units and the evaluator run, and what those import:
# feed_collect.py imports feed_status, client_account (client source) and
# acceptance_client (server source); feed_status.py imports feed_collect.
# test_feed_monitor.py checks no other local module is imported.
FILES="acceptance_client.py client_account.py feed_collect.py feed_status.py"

OUT="" COMMIT=HEAD
while [ $# -gt 0 ]; do
  case "$1" in
    --out) OUT=${2:-}; shift 2 || usage ;;
    --commit) COMMIT=${2:-}; shift 2 || usage ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[ -n "$OUT" ] || { echo "error: missing --out" >&2; usage; }
if [ -e "$OUT" ] && [ -n "$(ls -A "$OUT")" ]; then die "$OUT isn't empty"; fi
ROOT=$(git -C "$(dirname "$0")" rev-parse --show-toplevel)
sha=$(git -C "$ROOT" rev-parse --verify "$COMMIT^{commit}") || die "no commit $COMMIT"
at=$(git -C "$ROOT" show -s --format=%ct "$sha")
version="feed-monitor-$(date -u -d "@$at" +%Y%m%d)-${sha:0:7}"

stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT
sums=()
for f in $FILES; do
  git -C "$ROOT" show "$sha:boinc/scripts/$f" > "$stage/$f" || die "$sha has no boinc/scripts/$f"
  sums+=("$f" "$(sha256sum "$stage/$f" | cut -d' ' -f1)")
done
jq -nS --arg version "$version" --arg commit "$sha" --argjson committed_at "$at" \
  '{version: $version, commit: $commit, committed_at: $committed_at,
    files: ($ARGS.positional | [range(0; length; 2) as $i | {(.[$i]): .[$i + 1]}] | add)}' \
  --args "${sums[@]}" > "$stage/VERSION.json"
chmod 0644 "$stage"/*
mkdir -p "$OUT"
# shellcheck disable=SC2086 # FILES is a fixed list of plain names.
tar --sort=name --mtime="@$at" --owner=0 --group=0 --numeric-owner --format=gnu \
  -C "$stage" -cf - VERSION.json $FILES | gzip -n -9 > "$OUT/usnm-feed-monitor.tar.gz"
(cd "$OUT" && sha256sum usnm-feed-monitor.tar.gz > SHA256SUMS)
echo "$version"
