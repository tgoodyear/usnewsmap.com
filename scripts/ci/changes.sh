#!/usr/bin/env bash
# Which parts of CI a change needs, as step outputs rust/web/image/fixtures/
# code=true|false (code: anything but documentation changed, so the
# infrastructure checks and the shared-key scan run), and provision=true|false:
# whether it changes the Azure stack beyond what a deploy applies (the API
# image and infra/quickwit/searcher.yaml, which scripts/ci/roll-api.sh
# applies), so scripts/provision.sh must run. Documentation (Markdown
# anywhere, anything under docs/) counts for no output: a change to it alone
# runs no other job of this workflow.
# Everything runs for a workflow_dispatch (scripts/bootstrap.sh dispatches ci
# to publish images), for a change to the workflows themselves, and whenever
# the changed files can't be worked out.
set -euo pipefail
out=${GITHUB_OUTPUT:-/dev/stdout}
# Known only once the changed files are; "run everything" keeps it.
provision=false
all() {
  printf 'rust=true\nweb=true\nimage=true\nfixtures=true\ncode=true\nprovision=%s\n' "$provision" >> "$out"
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
# Documentation is only read: nothing below counts it. No README is compiled
# into a binary or an image.
files=$(grep -Ev '\.md$|^docs/' <<< "$files" || true)
matches() { [ -n "$files" ] && grep -Eq "$1" <<< "$files"; }
# infra/quickwit/ holds the index configs (read by the ingest image) and the
# searcher config (applied on deploy).
if matches '^infra/' && grep -Evq '^infra/quickwit/' <<< "$files"; then
  provision=true
fi
matches '^\.github/' && all "workflow changed"
# The Rust build and tests: the crates, the files compiled into binaries
# (index config, place overrides, the example searches the API warms, the
# reconstructed index history) and the fixtures the tests read.
rust='^(crates/|Cargo\.(toml|lock)$|rust-toolchain\.toml$|fixtures/|infra/quickwit/|catalog/|web/src/examples\.json$|ops/index-history\.json$|scripts/(ci/|quickwit-fixtures\.sh))'
web='^(web/|fixtures/|scripts/ci/)'
image="$rust|^(web/|ja-ocr/|Dockerfile|\.dockerignore$)"
# The synthetic corpus must match its generator.
fixtures='^fixtures/'
for part in rust web image fixtures; do
  if matches "${!part}"; then echo "$part=true" >> "$out"; else echo "$part=false" >> "$out"; fi
done
if [ -n "$files" ]; then echo "code=true" >> "$out"; else echo "code=false" >> "$out"; fi
echo "provision=$provision" >> "$out"
