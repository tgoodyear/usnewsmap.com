#!/usr/bin/env bash
# The pinned apt bases of the native Tesseract build and the usnm_iris image (#1683), as build
# arguments. The same pattern as scripts/ci/tesseract-base.sh (#1681).
#
#   scripts/ci/apt-base.sh --app native|iris [--check-label docker|podman] [--lock FILE]
#   scripts/ci/apt-base.sh --app native|iris --print-recipe
#
# Prints the lines of boinc/apps/<app>/base.lock (NATIVE_BASE=<image>@sha256:<digest>, or
# IRIS_TOOLS_BASE=... and IRIS_BASE=...), one per line, for `--build-arg`. Prints nothing while the
# app is not pinned: the Containerfile's defaults then install from the Debian snapshot, as before.
#
# A base records what it was built from in two labels: org.usnewsmap.debian-snapshot, the
# Containerfile's SNAPSHOT, and org.usnewsmap.apt-recipe, the sha256 of the text of the
# Containerfile's apt-* stages (and, for iris, of runtime.lock, whose .debs apt-iris installs), which
# --print-recipe prints (.github/workflows/tesseract-base.yml sets it). --check-label pulls each
# pinned base with that tool and fails unless both labels match the checkout, so a base from another
# snapshot or with other packages fails the build. The builds check the snapshot in the base's apt
# sources as well, so a builder without either tool still fails on another snapshot.
#
# Fails on a malformed lock: an unknown or repeated key, a missing key, a value that is not a digest
# of ghcr.io/tgoodyear/usnm-tesseract-base, or iris with one base pinned but not the other.
# bash 3.2 (macOS) runs this too: no associative arrays.
set -euo pipefail

root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
image=ghcr.io/tgoodyear/usnm-tesseract-base
app='' tool='' lock='' print_recipe=false

die() { echo "apt-base.sh: $*" >&2; exit 1; }
usage() { die "usage: scripts/ci/apt-base.sh --app native|iris [--check-label docker|podman] [--lock FILE] | --app native|iris --print-recipe"; }
while [ $# -gt 0 ]; do
  case $1 in
    --app) [ $# -ge 2 ] || usage; app=$2; shift 2 ;;
    --check-label) [ $# -ge 2 ] || usage; tool=$2; shift 2 ;;
    --lock) [ $# -ge 2 ] || usage; lock=$2; shift 2 ;;
    --print-recipe) print_recipe=true; shift ;;
    *) usage ;;
  esac
done

case $app in
  native) keys='NATIVE_BASE' ;;
  iris) keys='IRIS_TOOLS_BASE IRIS_BASE' ;;
  *) usage ;;
esac
case $tool in '' | docker | podman) ;; *) usage ;; esac
dir=$root/boinc/apps/$app
containerfile=$dir/Containerfile
[ -n "$lock" ] || lock=$dir/base.lock

# The text of the apt-* stages, from each `FROM ... AS apt-...` line to the next FROM, without
# comments or blank lines, then runtime.lock's hash for iris.
recipe() {
  local text sum
  text=$(awk '/^FROM /{on = ($0 ~ / AS apt-[a-z-]+$/)} on && !/^#/ && NF' "$containerfile")
  [ -n "$text" ] || die "no apt-* stages in $containerfile"
  if [ "$app" = iris ]; then
    sum=$(sha256 < "$dir/runtime.lock")
    text="$text"$'\n'"runtime.lock $sum"
  fi
  printf '%s\n' "$text" | sha256
}
sha256() {
  if command -v sha256sum > /dev/null; then sha256sum | cut -d' ' -f1; else shasum -a 256 | cut -d' ' -f1; fi
}
if $print_recipe; then
  [ -z "$tool" ] || usage
  recipe
  exit 0
fi

[ -f "$lock" ] || die "no lock file $lock"
seen=
while IFS= read -r line || [ -n "$line" ]; do
  case $line in '' | '#'*) continue ;; esac
  [[ $line =~ ^([A-Z_]+)=(.*)$ ]] || die "unexpected line in $lock: $line"
  key=${BASH_REMATCH[1]} value=${BASH_REMATCH[2]}
  case " $keys " in *" $key "*) ;; *) die "unexpected key in $lock for $app: $key" ;; esac
  case " $seen " in *" $key "*) die "$key is repeated in $lock" ;; esac
  seen="$seen $key"
  [ -z "$value" ] || [[ $value =~ ^ghcr\.io/tgoodyear/usnm-tesseract-base@sha256:[0-9a-f]{64}$ ]] ||
    die "$key must be $image@sha256:<64 hex> or empty, not $value"
  printf -v "pin_$key" '%s' "$value"
done < "$lock"

pinned=0
for key in $keys; do
  case " $seen " in *" $key "*) ;; *) die "$lock has no $key line" ;; esac
  var=pin_$key
  if [ -n "${!var}" ]; then pinned=$((pinned + 1)); fi
done
[ "$pinned" -gt 0 ] || exit 0
[ "$pinned" -eq "$(wc -w <<< "$keys")" ] || die "$app pins some of its bases but not all: pin them all or none"

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
  for key in $keys; do
    var=pin_$key
    ref=${!var}
    "$tool" pull --quiet --platform linux/amd64 "$ref" > /dev/null || die "can't pull $ref"
    got=$(label "$tool" "$ref" org.usnewsmap.debian-snapshot)
    [ "$got" = "$snapshot" ] ||
      die "$ref was built from Debian snapshot '$got', not the Containerfile's SNAPSHOT=$snapshot: rebuild the bases (tesseract-base.yml) and pin them"
    got=$(label "$tool" "$ref" org.usnewsmap.apt-recipe)
    [ "$got" = "$want_recipe" ] ||
      die "$ref was built from other apt stages ('$got', not $want_recipe): rebuild the bases (tesseract-base.yml) and pin them"
  done
fi
for key in $keys; do
  var=pin_$key
  printf '%s=%s\n' "$key" "${!var}"
done
