#!/bin/bash
# `git bisect run` predicate for #1835: builds the commit checked out by git bisect and says whether a
# signature of its output on one page equals a reference signature.
#
# usage: bisect-tesseract-test.sh --kind layout|text --page ID --reference FILE [--keep DIR] [step flags...]
#   --kind layout: sha256 of the ALTO's ComposedBlock boxes (the page's block layout).
#   --kind text:   sha256 of the page's text.
#   --reference FILE holds the signature of the good behaviour (print it with --print on the good build).
# Every other flag goes to bisect-tesseract-step.sh (--repo, --base-image, --pages, ...). The commit is
# the repository's HEAD, so use `git -C REPO bisect run bisect-tesseract-test.sh ...`.
# Exit status: 0 the signature equals the reference, 1 it differs, 125 the commit doesn't build.
set -uo pipefail
HERE=$(cd "$(dirname "$0")" && pwd)
KIND='' PAGE='' REF='' PRINT=0 ARGS=() REPO='' OUT=''
while [ $# -gt 0 ]; do
  case $1 in
    --kind) KIND=$2; shift 2;; --page) PAGE=$2; shift 2;; --reference) REF=$2; shift 2;;
    --print) PRINT=1; shift;;
    --repo) REPO=$2; ARGS+=("$1" "$2"); shift 2;;
    --out) OUT=$2; ARGS+=("$1" "$2"); shift 2;;
    *) ARGS+=("$1" "$2"); shift 2;;
  esac
done
[ -n "$KIND" ] && [ -n "$PAGE" ] && [ -n "$REPO" ] && [ -n "$OUT" ] || { echo "--kind, --page, --repo and --out are required" >&2; exit 2; }
HEAD=$(git -C "$REPO" rev-parse HEAD)
"$HERE/bisect-tesseract-step.sh" "${ARGS[@]}" --commit "$HEAD" || exit 125
case $KIND in
  layout) SIG=$(grep -o '<ComposedBlock [^>]*>' "$OUT/$PAGE/page.xml" | sed 's/ID="[^"]*" //' | sha256sum | cut -c1-64);;
  text) SIG=$(sha256sum < "$OUT/$PAGE/page.txt" | cut -c1-64);;
  *) echo "unknown kind" >&2; exit 2;;
esac
echo "$(git -C "$REPO" log -1 --format=%h "$HEAD") $KIND $PAGE $SIG"
[ "$PRINT" = 0 ] || exit 0
[ -n "$REF" ] && [ "$SIG" = "$(cat "$REF")" ]
