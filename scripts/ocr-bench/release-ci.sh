#!/usr/bin/env bash
# Build, check and publish the OCR benchmark releases (#738, docs/ocr-bench.md) in CI.
#   scripts/ocr-bench/release-ci.sh --mode check|publish --survey BIN --work DIR
# For every ocr/bench/v*/spec.json:
# - Published (its GitHub release tag exists): the committed spec and inventory must still hash
#   to the releases.json entry, and scripts/ocr-bench/fetch --verify must pass against the
#   published archive. Published releases never change.
# - Not yet published: download its inputs tar (--inputs-base), check every file against the
#   committed inventory, build it with `usnm-ocr-survey bench-build`, and require the result to
#   equal its releases.json entry. With no entry, print the built entry to add and fail.
#   --mode publish then creates the GitHub release (archive, SHA256SUMS, manifest, notes) and
#   fetches it back with --verify.
# Needs gh (GH_TOKEN), curl, jq, tar and sha256sum.
set -euo pipefail
die() { echo "release-ci: $*" >&2; exit 1; }
MODE="" SURVEY="" WORK=""
INPUTS_BASE=https://boinc.usnewsmap.com/usnmocr_download/bench
REPO=${GITHUB_REPOSITORY:-tgoodyear/usnewsmap.com}
while [ $# -gt 0 ]; do
  case "$1" in
    --mode) MODE=${2:-}; shift 2 ;;
    --survey) SURVEY=${2:-}; shift 2 ;;
    --work) WORK=${2:-}; shift 2 ;;
    --inputs-base) INPUTS_BASE=${2:-}; shift 2 ;;
    -h | --help) sed -n '2,14s/^# \{0,1\}//p' "$0"; exit 0 ;;
    *) die "unknown option $1" ;;
  esac
done
[[ $MODE =~ ^(check|publish)$ ]] || die "--mode must be check or publish"
[ -x "$SURVEY" ] || die "--survey must be the usnm-ocr-survey binary"
[ -n "$WORK" ] || die "--work is required"
root=$(cd "$(dirname "$0")/../.." && pwd)
index="$root/ocr/bench/releases.json"
[ -r "$index" ] || die "no $index"
jq -e '.format == "usnm-ocr-bench-releases" and (.releases | type == "array")' "$index" > /dev/null ||
  die "$index is not a release index"
mkdir -p "$WORK"
sha() { sha256sum "$1" | cut -c1-64; }

status=0
for spec in "$root"/ocr/bench/v*/spec.json; do
  [ -e "$spec" ] || continue
  dir=$(dirname "$spec")
  version=$(basename "$dir")
  [ "$(jq -r .release "$spec")" = "$version" ] || die "$spec names another release than $version"
  inventory="$dir/inputs.sha256"
  tag="ocr-bench-$version"
  entry=$(jq -c --arg v "$version" '.releases[] | select(.release == $v)' "$index")

  if gh release view "$tag" --repo "$REPO" > /dev/null 2>&1; then
    [ -n "$entry" ] || die "$tag is published but not in releases.json"
    [ "$(sha "$spec")" = "$(jq -r .spec_sha256 <<< "$entry")" ] ||
      die "$version is published; its spec.json must not change"
    [ "$(sha "$inventory")" = "$(jq -r .inputs_sha256 <<< "$entry")" ] ||
      die "$version is published; its inputs.sha256 must not change"
    "$root/scripts/ocr-bench/fetch" --version "$version" --verify --dest "$WORK/fetched" > /dev/null
    echo "release-ci: $version is published and verifies"
    continue
  fi

  # Unpublished: build from the staged inputs.
  inputs="$WORK/inputs-$version"
  tarball="$WORK/usnm-ocr-bench-inputs-$version.tar"
  if [ ! -d "$inputs" ]; then
    curl -fL --retry 4 --retry-connrefused -o "$tarball" "$INPUTS_BASE/usnm-ocr-bench-inputs-$version.tar" ||
      die "could not download the inputs of $version"
    bad=$(tar -tvf "$tarball" | awk '
      substr($1, 1, 1) != "-" { print; next }
      { p = $NF; if (p ~ /^\// || p ~ /(^|\/)\.\.(\/|$)/) print }')
    [ -z "$bad" ] || die "unexpected entries in the inputs tar: $bad"
    mkdir -p "$inputs.part"
    tar -xf "$tarball" -C "$inputs.part"
    mv "$inputs.part" "$inputs"
  fi
  (cd "$inputs" && sha256sum -c --quiet "$inventory") || die "the inputs of $version differ from inputs.sha256"
  [ "$(find "$inputs" -type f | wc -l)" -eq "$(wc -l < "$inventory")" ] ||
    die "the inputs of $version hold files not in inputs.sha256"
  out="$WORK/build-$version"
  rm -rf "$out"
  built=$("$SURVEY" bench-build --spec "$spec" --inventory "$inventory" --inputs "$inputs" --out "$out")
  name="usnm-ocr-bench-$version"
  "$SURVEY" bench-verify --release "$out/$name.tar" > /dev/null
  if [ -z "$entry" ]; then
    echo "release-ci: $version is not in releases.json. After review, add this entry:" >&2
    echo "$built" >&2
    status=1
    continue
  fi
  if [ "$(jq -S . <<< "$built")" != "$(jq -S . <<< "$entry")" ]; then
    echo "release-ci: the $version build differs from releases.json:" >&2
    diff <(jq -S . <<< "$entry") <(jq -S . <<< "$built") >&2 || true
    status=1
    continue
  fi
  "$SURVEY" bench-verify --release "$out/$name.tar" --index "$index" > /dev/null
  echo "release-ci: $version builds to its releases.json entry (content digest $(jq -r .content_digest <<< "$built"))"

  if [ "$MODE" = publish ]; then
    assets="$WORK/assets-$version"
    rm -rf "$assets"
    mkdir -p "$assets"
    cp "$out/$name.tar" "$assets/"
    cp "$out/$name/SHA256SUMS" "$out/$name/manifest.json" "$assets/"
    gh release create "$tag" --repo "$REPO" --title "OCR benchmark $version" \
      --notes-file "$out/$name/RELEASE-NOTES.md" "$assets"/*
    "$root/scripts/ocr-bench/fetch" --version "$version" --verify --dest "$WORK/fetched" > /dev/null
    echo "release-ci: published $tag and fetched it back"
  fi
done
exit "$status"
