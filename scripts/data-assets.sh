#!/usr/bin/env bash
# Data that the tree does not carry: report data sets and benchmark binaries live as assets of a
# GitHub release, published by .github/workflows/publish-data.yml from a commit, and fetched back
# into place by this script (docs/reports/README.md, "Data").
#
#   scripts/data-assets.sh --members --set report-data            # the files the set holds, from git
#   scripts/data-assets.sh --pack --set report-data --out DIR      # DIR/<set>.tar.gz, <set>.members.txt, SHA256SUMS
#   scripts/data-assets.sh --fetch --set report-data [--tag TAG]  # download, check the digest, extract at the root
#
# A set is a rule over tracked files, so `--members` lists what a commit holds and `--pack` is
# reproducible (sorted entries, no timestamps or owners). `--fetch` takes the release tag and the
# tarball's sha256 from ops/data-assets.json unless --tag names another release; a tarball whose
# digest differs from the recorded one is not extracted.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() {
  echo "usage: scripts/data-assets.sh (--members | --pack --out DIR | --fetch [--tag TAG]) --set <report-data|bench-assets>" >&2
  exit 2
}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
REPO=tgoodyear/usnewsmap.com
INDEX=$ROOT/ops/data-assets.json
MODE="" SET="" OUT="" TAG=""
while [ $# -gt 0 ]; do
  case $1 in
    --members | --pack | --fetch) [ -z "$MODE" ] || usage; MODE=${1#--}; shift ;;
    --set) [ $# -ge 2 ] || usage; SET=$2; shift 2 ;;
    --out) [ $# -ge 2 ] || usage; OUT=$2; shift 2 ;;
    --tag) [ $# -ge 2 ] || usage; TAG=$2; shift 2 ;;
    *) usage ;;
  esac
done
[ -n "$MODE" ] && [ -n "$SET" ] || usage

# The sets. Each is a rule over `git ls-files`, so adding a file of the kind to one of these
# directories adds it to the set, and the published tarball's members list says what it held.
members() {
  case $SET in
    report-data)
      # The reports' data: everything under docs/reports and docs/data that is not prose or code.
      git -C "$ROOT" ls-files docs/reports docs/data | grep -E '\.(csv|json|jsonl|parquet|txt|gz|pdf|tar)$' || true ;;
    bench-assets)
      # The benchmark's page images and raw native result archives (ocr/bench).
      git -C "$ROOT" ls-files ocr/bench | grep -E '\.(jpg|jpeg|png|tar\.gz|tgz)$' || true ;;
    *) die "--set is report-data or bench-assets" ;;
  esac
}

index_field() {
  python3 - "$INDEX" "$SET" "$1" <<'PY'
import json, sys
path, name, field = sys.argv[1:]
entry = json.load(open(path)).get(name) or {}
value = entry.get(field)
print("" if value is None else value)
PY
}

case $MODE in
  members)
    members ;;
  pack)
    [ -n "$OUT" ] || usage
    mkdir -p "$OUT"
    list=$OUT/$SET.members.txt
    members > "$list"
    [ -s "$list" ] || die "the $SET set is empty in this checkout (fetch it first, or there is nothing to publish)"
    # Reproducible: entries sorted, no mtime, owner or group, gzip without a name or timestamp.
    tar -C "$ROOT" --sort=name --mtime=@0 --owner=0 --group=0 --numeric-owner -cf - -T "$list" |
      gzip -n -9 > "$OUT/$SET.tar.gz"
    (cd "$OUT" && sha256sum "$SET.tar.gz" "$SET.members.txt" > SHA256SUMS)
    echo "$(wc -l < "$list") files, $(du -h "$OUT/$SET.tar.gz" | cut -f1) in $OUT/$SET.tar.gz"
    cat "$OUT/SHA256SUMS" ;;
  fetch)
    tag=${TAG:-$(index_field tag)}
    [ -n "$tag" ] || die "the $SET set is not published yet (ops/data-assets.json has no tag); run the publish-data workflow"
    expected=$(index_field sha256)
    [ -n "$TAG" ] || [ -n "$expected" ] || die "ops/data-assets.json has no sha256 for $SET"
    tmp=$(mktemp -d)
    trap 'rm -rf "$tmp"' EXIT
    url="https://github.com/$REPO/releases/download/$tag/$SET.tar.gz"
    curl -fsSL --retry 4 --retry-connrefused -o "$tmp/$SET.tar.gz" "$url" || die "could not download $url"
    got=$(sha256sum "$tmp/$SET.tar.gz" | cut -d' ' -f1)
    if [ -n "$TAG" ] && [ "$tag" != "$(index_field tag)" ]; then
      echo "note: $tag is not the recorded release; not checking its digest against ops/data-assets.json" >&2
    else
      [ "$got" = "$expected" ] || die "$SET.tar.gz from $tag has sha256 $got, ops/data-assets.json records $expected"
    fi
    tar -C "$ROOT" -xzf "$tmp/$SET.tar.gz"
    echo "$SET from $tag extracted under $ROOT ($(tar -tzf "$tmp/$SET.tar.gz" | wc -l) files)" ;;
esac
