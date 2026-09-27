#!/usr/bin/env bash
# Runs finish out of order: a slower run for an older commit must not replace
# a newer deploy. Sets the step output current=true only if GITHUB_SHA is
# still the head of main. Needs GH_TOKEN.
set -euo pipefail
head=$(gh api "repos/$GITHUB_REPOSITORY/commits/main" --jq .sha)
if [ "$head" = "$GITHUB_SHA" ]; then
  echo "current=true" >> "$GITHUB_OUTPUT"
else
  echo "::notice::main is now at $head; not deploying $GITHUB_SHA"
fi
