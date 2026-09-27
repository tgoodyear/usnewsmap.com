#!/usr/bin/env bash
# Install the pinned Quickwit release (QUICKWIT_VERSION, checked against
# QUICKWIT_SHA256) and export QUICKWIT_BIN for later steps.
set -euo pipefail
: "${QUICKWIT_VERSION:?}" "${QUICKWIT_SHA256:?}"
curl -fsSL -o "$RUNNER_TEMP/qw.tgz" \
  "https://github.com/quickwit-oss/quickwit/releases/download/${QUICKWIT_VERSION}/quickwit-${QUICKWIT_VERSION}-x86_64-unknown-linux-gnu.tar.gz"
echo "${QUICKWIT_SHA256}  $RUNNER_TEMP/qw.tgz" | sha256sum -c -
tar -xzf "$RUNNER_TEMP/qw.tgz" -C "$RUNNER_TEMP"
echo "QUICKWIT_BIN=$RUNNER_TEMP/quickwit-${QUICKWIT_VERSION}/quickwit" >> "$GITHUB_ENV"
