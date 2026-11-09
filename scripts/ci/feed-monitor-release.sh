#!/usr/bin/env bash
# Publishes the feed monitor bundle (#1441) from feed-monitor-release.yml,
# after the job has attested both files: the release
# feed-monitor-<yyyymmdd>-<sha7>, which never changes, and the same two
# files as the assets of feed-monitor-latest, which hosts poll
# (boinc/scripts/onprem/feed-monitor/update.sh). feed-monitor-latest only
# moves forward along main: it's replaced only when this commit is the one
# it holds or a descendant of it, and when its bundle can't be read the run
# fails and leaves it. A versioned release that exists already (a re-run) is left as
# it is.
#   scripts/ci/feed-monitor-release.sh --dist DIR
#
# DIR holds bundle.sh's usnm-feed-monitor.tar.gz and SHA256SUMS. Needs
# GH_TOKEN with contents: write, GITHUB_REPOSITORY and GITHUB_SHA.
set -euo pipefail
usage() { sed -n '10s/^# \{0,1\}//p' "$0" >&2; exit 2; }
DIST=""
while [ $# -gt 0 ]; do
  case "$1" in
    --dist) DIST=${2:-}; shift 2 ;;
    -h|--help) usage ;;
    *) echo "error: unknown option $1" >&2; usage ;;
  esac
done
[ -n "$DIST" ] || { echo "error: missing --dist" >&2; usage; }
files="SHA256SUMS usnm-feed-monitor.tar.gz"
for f in $files; do
  [ -s "$DIST/$f" ] || { echo "error: $DIST/$f is missing" >&2; exit 1; }
done
(cd "$DIST" && sha256sum --check --quiet SHA256SUMS)
meta=$(tar -xzOf "$DIST/usnm-feed-monitor.tar.gz" VERSION.json)
tag=$(jq -r .version <<< "$meta")
[ "$(jq -r .commit <<< "$meta")" = "$GITHUB_SHA" ] || { echo "error: the bundle isn't of $GITHUB_SHA" >&2; exit 1; }
[[ $tag =~ ^feed-monitor-[0-9]{8}-[0-9a-f]{7}$ ]] || { echo "error: the bundle names no release ($tag)" >&2; exit 1; }
digest=$(awk '{ print $1 }' "$DIST/SHA256SUMS")
want=$(echo "$files" | tr ' ' '\n' | LC_ALL=C sort | paste -sd, -)
# "<isDraft> <asset,names>", or nothing if there's no such release.
state() {
  gh release view "$1" --repo "$GITHUB_REPOSITORY" --json isDraft,assets \
    --jq '"\(.isDraft) \([.assets[].name] | sort | join(","))"' 2> /dev/null || true
}
upload() {
  for f in $files; do
    gh release upload "$1" "$DIST/$f" --repo "$GITHUB_REPOSITORY" --clobber
  done
}

# The versioned release: created as a draft, given its files, and published
# only then, so a published one always has both. A rerun finishes a draft.
s=$(state "$tag")
if [ "$s" = "false $want" ]; then
  echo "::notice::release $tag exists with every file; leaving it"
elif [ -n "$s" ]; then
  [ "${s%% *}" = true ] || { echo "error: release $tag is published without every file ($s)" >&2; exit 1; }
  upload "$tag"
  gh release edit "$tag" --repo "$GITHUB_REPOSITORY" --draft=false --latest=false
  echo "finished and published $tag"
else
  notes=$(mktemp)
  cat > "$notes" <<NOTES
The BOINC feed monitor scripts (#1441), built from $GITHUB_SHA.

- \`usnm-feed-monitor.tar.gz\`: \`feed_collect.py\`, \`feed_status.py\`, \`client_account.py\`, \`acceptance_client.py\` and \`VERSION.json\`.
- \`SHA256SUMS\`: its sha256 (\`$digest\`).

Both have a build provenance attestation from feed-monitor-release.yml on main. Hosts install it with \`usnm-feed-monitor-update\`; pin this release with \`--version $tag\`.
NOTES
  gh release create "$tag" "$DIST/SHA256SUMS" "$DIST/usnm-feed-monitor.tar.gz" --repo "$GITHUB_REPOSITORY" \
    --target "$GITHUB_SHA" --title "$tag" --notes-file "$notes" --draft
  gh release edit "$tag" --repo "$GITHUB_REPOSITORY" --draft=false --latest=false
  echo "published $tag"
fi

# feed-monitor-latest: the archive first, then SHA256SUMS. A host that reads
# them in between sees a mismatch and tries again on its next run.
latest=feed-monitor-latest
s=$(state "$latest")
if [ -z "$s" ]; then
  gh release create "$latest" "$DIST/usnm-feed-monitor.tar.gz" "$DIST/SHA256SUMS" --repo "$GITHUB_REPOSITORY" \
    --target "$GITHUB_SHA" --title "$latest" --prerelease --latest=false \
    --notes "The newest feed monitor bundle (#1441). Its files are replaced on every release; each one is also its own release, feed-monitor-<yyyymmdd>-<sha7>. Hosts follow it with usnm-feed-monitor-update."
  echo "created $latest with $tag"
  exit 0
fi
if [[ ",${s#* }," == *,usnm-feed-monitor.tar.gz,* ]]; then
  old=$(mktemp -d)
  gh release download "$latest" --repo "$GITHUB_REPOSITORY" --pattern usnm-feed-monitor.tar.gz --dir "$old" ||
    { echo "error: can't download $latest's bundle; leaving it" >&2; exit 1; }
  old_commit=$(tar -xzOf "$old/usnm-feed-monitor.tar.gz" VERSION.json | jq -r .commit) || old_commit=""
  [[ $old_commit =~ ^[0-9a-f]{40}$ ]] || { echo "error: $latest's bundle names no commit; leaving it" >&2; exit 1; }
  if [ "$old_commit" != "$GITHUB_SHA" ]; then
    status=$(gh api "repos/$GITHUB_REPOSITORY/compare/$old_commit...$GITHUB_SHA?per_page=1" --jq .status) ||
      { echo "error: can't compare $old_commit with $GITHUB_SHA; leaving $latest" >&2; exit 1; }
    case $status in
      ahead) ;;
      behind)
        echo "::notice::$latest holds a bundle of a later commit ($old_commit); leaving it"
        exit 0 ;;
      *) echo "error: $GITHUB_SHA isn't a descendant of $latest's $old_commit ($status); leaving it" >&2; exit 1 ;;
    esac
  fi
fi
gh release upload "$latest" "$DIST/usnm-feed-monitor.tar.gz" --repo "$GITHUB_REPOSITORY" --clobber
gh release upload "$latest" "$DIST/SHA256SUMS" --repo "$GITHUB_REPOSITORY" --clobber
echo "$latest now holds $tag"
