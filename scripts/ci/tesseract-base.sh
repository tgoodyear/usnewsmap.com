#!/usr/bin/env bash
# The pinned apt bases of the Tesseract engine and BOINC worker images (#1681), as build arguments.
#
#   scripts/ci/tesseract-base.sh --arch amd64|arm64 [--check-label docker|podman] [--lock FILE]
#   scripts/ci/tesseract-base.sh --print-recipe
#
# Prints the architecture's lines of ocr/engines/tesseract/base.lock without their AMD64_/ARM64_
# prefix (ENGINE_BASE=<image>@sha256:<digest>, TOOLS_BASE=...), one per line, for
# `--build-arg`. Prints nothing while the architecture is not pinned: the Containerfiles' defaults
# then install from the Debian snapshot, as before.
#
# A base records what it was built from in two labels: org.usnewsmap.debian-snapshot, the
# Containerfile's SNAPSHOT, and org.usnewsmap.apt-recipe, the sha256 of the text of the
# Containerfile's apt-* stages, which --print-recipe prints (.github/workflows/tesseract-base.yml
# sets it). --check-label pulls each pinned base with that tool and fails unless both labels match
# the Containerfile as it is now, so a base from another snapshot or other packages fails the
# build. The builds check the snapshot in the base's apt sources as well, so a builder without
# either tool still fails on another snapshot.
#
# Fails on a malformed lock: an unknown or repeated key, a value that is not a digest of
# ghcr.io/tgoodyear/usnm-tesseract-base, or an architecture with one base pinned but not the other.
# bash 3.2 (macOS) runs this too: no associative arrays.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
lock=$root/ocr/engines/tesseract/base.lock
containerfile=$root/ocr/engines/tesseract/Containerfile
image=ghcr.io/tgoodyear/usnm-tesseract-base
arch='' tool='' print_recipe=false

die() { echo "tesseract-base.sh: $*" >&2; exit 1; }
usage() { die "usage: scripts/ci/tesseract-base.sh --arch amd64|arm64 [--check-label docker|podman] [--lock FILE] | --print-recipe"; }
while [ $# -gt 0 ]; do
  case $1 in
    --arch) [ $# -ge 2 ] || usage; arch=$2; shift 2 ;;
    --check-label) [ $# -ge 2 ] || usage; tool=$2; shift 2 ;;
    --lock) [ $# -ge 2 ] || usage; lock=$2; shift 2 ;;
    --print-recipe) print_recipe=true; shift ;;
    *) usage ;;
  esac
done

# The text of the apt-* stages, from each `FROM ... AS apt-...` line to the next FROM, without
# comments or blank lines.
recipe() {
  local text
  text=$(awk '/^FROM /{on = ($0 ~ / AS apt-[a-z-]+$/)} on && !/^#/ && NF' "$containerfile")
  [ -n "$text" ] || die "no apt-* stages in $containerfile"
  if command -v sha256sum > /dev/null; then
    printf '%s\n' "$text" | sha256sum | cut -d' ' -f1
  else
    printf '%s\n' "$text" | shasum -a 256 | cut -d' ' -f1
  fi
}
if $print_recipe; then
  [ -z "$arch$tool" ] || usage
  recipe
  exit 0
fi

case $arch in amd64 | arm64) ;; *) usage ;; esac
case $tool in '' | docker | podman) ;; *) usage ;; esac
[ -f "$lock" ] || die "no lock file $lock"

seen=
while IFS= read -r line || [ -n "$line" ]; do
  case $line in '' | '#'*) continue ;; esac
  [[ $line =~ ^(AMD64|ARM64)_(ENGINE|TOOLS)_BASE=(.*)$ ]] || die "unexpected line in $lock: $line"
  key=${line%%=*} value=${line#*=}
  case " $seen " in *" $key "*) die "$key is repeated in $lock" ;; esac
  seen="$seen $key"
  [ -z "$value" ] || [[ $value =~ ^ghcr\.io/tgoodyear/usnm-tesseract-base@sha256:[0-9a-f]{64}$ ]] ||
    die "$key must be $image@sha256:<64 hex> or empty, not $value"
  printf -v "pin_$key" '%s' "$value"
done < "$lock"
for key in AMD64_ENGINE_BASE AMD64_TOOLS_BASE ARM64_ENGINE_BASE ARM64_TOOLS_BASE; do
  case " $seen " in *" $key "*) ;; *) die "$lock has no $key line" ;; esac
done

prefix=$(tr '[:lower:]' '[:upper:]' <<< "$arch")
engine_var=pin_${prefix}_ENGINE_BASE tools_var=pin_${prefix}_TOOLS_BASE
engine=${!engine_var} tools=${!tools_var}
[ -z "$engine" ] && [ -z "$tools" ] && exit 0
[ -n "$engine" ] && [ -n "$tools" ] || die "$arch pins one base but not the other: pin both or neither"

# label TOOL REF NAME: the image's label NAME, empty if it has none.
label() {
  if [ "$1" = docker ]; then
    docker image inspect --format "{{index .Config.Labels \"$3\"}}" "$2"
  else
    podman image inspect --format "{{index .Labels \"$3\"}}" "$2"
  fi
}
if [ -n "$tool" ]; then
  snapshot=$(sed -n 's/^ARG SNAPSHOT=//p' "$containerfile")
  [[ $snapshot =~ ^[0-9]{8}T[0-9]{6}Z$ ]] || die "no ARG SNAPSHOT=<date> in $containerfile"
  want_recipe=$(recipe)
  for ref in "$engine" "$tools"; do
    "$tool" pull --quiet --platform "linux/$arch" "$ref" > /dev/null || die "can't pull $ref"
    got=$(label "$tool" "$ref" org.usnewsmap.debian-snapshot)
    [ "$got" = "$snapshot" ] ||
      die "$ref was built from Debian snapshot '$got', not the Containerfile's SNAPSHOT=$snapshot: rebuild the bases (tesseract-base.yml) and pin them"
    got=$(label "$tool" "$ref" org.usnewsmap.apt-recipe)
    [ "$got" = "$want_recipe" ] ||
      die "$ref was built from other apt stages ('$got', not $want_recipe): rebuild the bases (tesseract-base.yml) and pin them"
  done
fi
printf 'ENGINE_BASE=%s\nTOOLS_BASE=%s\n' "$engine" "$tools"
