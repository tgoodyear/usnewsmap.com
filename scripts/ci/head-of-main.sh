#!/usr/bin/env bash
# Runs finish out of order: a slower run for an older commit must not replace
# a newer deploy. Sets the step output current=true if GITHUB_SHA is still the
# head of main. Needs GH_TOKEN.
#
#   scripts/ci/head-of-main.sh [--unless-changed FLAG[,FLAG...]]
#
# With --unless-changed, an older commit is current too when every commit main
# has gained since it changes none of the FLAGs (changes.sh's outputs, such as
# image): those commits' own runs publish nothing, so this run's publication is
# the one main's head needs. Without it, a change was never deployed when a
# commit that publishes nothing landed before its run got here (#1063, 24 Oct
# 2026). A newer commit that changes a FLAG publishes in its own run, and this
# one skips. Needs the history from GITHUB_SHA to main (a checkout with
# fetch-depth: 0) and what changes.sh needs.
set -euo pipefail
die() { echo "error: $*" >&2; exit 1; }
usage() { sed -n '2,15s/^# \{0,1\}//p' "$0" >&2; exit 2; }
FLAGS=""
while [ $# -gt 0 ]; do
  case "$1" in
    --unless-changed) FLAGS=${2:-}; shift 2 ;;
    -h|--help) usage ;;
    *) die "unknown option $1" ;;
  esac
done
[[ -z $FLAGS || $FLAGS =~ ^[a-z_]+(,[a-z_]+)*$ ]] || die "--unless-changed takes flag names, comma-separated"
head=$(gh api "repos/$GITHUB_REPOSITORY/commits/main" --jq .sha)
if [ "$head" = "$GITHUB_SHA" ]; then
  echo "current=true" >> "$GITHUB_OUTPUT"
  exit 0
fi
skip() { echo "::notice::main is now at $head; not deploying $GITHUB_SHA${1:+ ($1)}"; exit 0; }
[ -n "$FLAGS" ] || skip
git cat-file -e "$head^{commit}" 2> /dev/null || git fetch -q --no-tags origin "$head" || skip "can't fetch main"
git merge-base --is-ancestor "$GITHUB_SHA" "$head" || skip "main doesn't descend from it"
# Canonical, as cargo metadata reports it: changes.sh matches crate directories
# relative to it (macOS's temporary directory is behind a symlink).
dir=$(cd "$(mktemp -d)" && pwd -P)
out=$(mktemp)
trap 'git worktree remove --force "$dir" 2> /dev/null || true; rm -rf "$dir" "$out"' EXIT
git worktree add -q --detach "$dir" "$head"
# What main's head changed since this commit, as its own run's changes.sh
# would classify a push of all of it.
(cd "$dir" && env -u CI_SCOPE GITHUB_EVENT_NAME=push BEFORE_SHA="$GITHUB_SHA" GITHUB_SHA="$head" \
  GITHUB_OUTPUT="$out" scripts/ci/changes.sh > /dev/null) || skip "can't classify main's newer commits"
for flag in ${FLAGS//,/ }; do
  grep -qx "$flag=false" "$out" || skip "a newer commit changes $flag and publishes it"
done
echo "::notice::main is now at $head, whose commits since $GITHUB_SHA change none of $FLAGS: deploying $GITHUB_SHA"
echo "current=true" >> "$GITHUB_OUTPUT"
