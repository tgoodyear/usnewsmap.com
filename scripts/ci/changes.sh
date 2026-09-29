#!/usr/bin/env bash
# Which parts of CI a change needs, as step outputs rust/web/image=true|false.
# Everything runs for a workflow_dispatch (scripts/bootstrap.sh dispatches ci
# to publish images), for a change to the workflows themselves, and whenever
# the changed files can't be worked out.
set -euo pipefail
out=${GITHUB_OUTPUT:-/dev/stdout}
all() {
  printf 'rust=true\nweb=true\nimage=true\n' >> "$out"
  echo "running everything: $1"
  exit 0
}
case "${GITHUB_EVENT_NAME:-}" in
  pull_request) range="origin/$GITHUB_BASE_REF...HEAD" ;;
  push)
    before=${BEFORE_SHA:-}
    [ -n "$before" ] && ! [[ $before =~ ^0+$ ]] || all "no previous commit"
    # Two dots: the trees before and after the push (a force push included).
    range="$before..$GITHUB_SHA"
    ;;
  *) all "event ${GITHUB_EVENT_NAME:-unknown}" ;;
esac
files=$(git diff --name-only "$range") || all "git diff failed for $range"
[ -n "$files" ] || all "no changed files found"
echo "$files" | sed 's/^/changed: /'
matches() { grep -Eq "$1" <<< "$files"; }
matches '^\.github/' && all "workflow changed"
# The Rust build and tests: the crates, the files compiled into binaries
# (index config, place overrides) and the fixtures the tests read.
rust='^(crates/|Cargo\.(toml|lock)$|rust-toolchain\.toml$|fixtures/|infra/quickwit/|catalog/|scripts/(ci/|quickwit-fixtures\.sh))'
web='^(web/|fixtures/|scripts/ci/)'
image="$rust|^(web/|Dockerfile|\.dockerignore$)"
for part in rust web image; do
  if matches "${!part}"; then echo "$part=true" >> "$out"; else echo "$part=false" >> "$out"; fi
done
