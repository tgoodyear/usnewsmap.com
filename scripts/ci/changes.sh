#!/usr/bin/env bash
# Which parts of CI a change needs, as step outputs rust/web/image/e2e/fixtures/
# ops/code/markdown/ocr_worker/page_language=true|false (ocr_worker: the OCR
# worker's EL9 build, its Tesseract image and, on main, its release;
# page_language: the page-language detector's test on text of known
# language; code: anything but
# documentation changed, so the infrastructure checks and the shared-key scan run; markdown: a Markdown
# file changed, so Prettier checks its format), and provision=true|false:
# whether it changes the Azure stack beyond what a deploy applies (the API
# image and infra/quickwit/searcher.yaml, which scripts/ci/roll-api.sh
# applies), so scripts/provision.sh must run. Documentation (Markdown
# anywhere, anything under docs/) counts for no output but markdown: a change
# to it alone runs only the Markdown format check.
# ocr_gold: the OCR gold set (scripts/ocr-survey/, fixtures/ocr-gold/, and the
# Rust scorer's golden files that gold.py writes), whose unit tests and golden
# check run in the infra job. Most fixtures/ocr-gold/ files count for none
# of rust/quickwit/web/image/fixtures. The v1 lock is embedded in the Rust
# mirror CLI, and it and reviewed selections also build the bridge.
# boinc_pages: the BOINC project's own web pages (contrib-compute/boinc/html/), which
# only need a PHP syntax check; a change to them alone runs nothing else.
# contrib: the hosting for the stock BOINC server (contrib-compute/: shell,
# XML and Bicep; ADR-0019), which no other job builds.
# Everything runs for a default workflow_dispatch (scripts/bootstrap.sh dispatches ci
# to publish images), for a change to the workflows themselves, and whenever
# the changed files can't be worked out; everything but ops, which runs only
# when its own files change (the snapshots, the index history, their schemas,
# checkers and writers, its requirements, or ci.yml, which defines the job).
# quickwit: what the Quickwit job's two tests build or load: the workspace
# crates usnm-search and usnm-ingest depend on (from cargo metadata), the
# workspace manifest and lock file only when the packages Cargo resolves for
# those crates change, and the fixtures, index configs and Quickwit scripts.
# image and e2e are narrowed the same way: image for the workspace crates
# usnm-api (Dockerfile) and usnm-ingest (Dockerfile.ingest) build, e2e (the web
# end-to-end job) for those usnm-api builds, the API that
# scripts/ci/start-api-fixtures.sh starts. A crate outside a closure (the BOINC
# or OCR survey crates, say) runs neither.
# The infra job (code) checks the workbook definitions, so any change to
# infra/workbooks/ runs it; infra/workbooks/boinc.json, which the BOINC stack
# loads, also runs contrib, which builds that stack.
# infra: what the infra job's Bicep, naming, workbook and script-test steps read.
# `code` still runs that job's shared-key and named-argument scans over every
# crate; `infra` alone runs the rest. It is false for a change confined to
# src/ and tests/ of the dashboard crate, which none of those steps name
# (test_changes.py checks that).
# ocr_worker, ocr_dash: the OCR worker's and the LAN dashboard's EL9 build.
# ocr_worker also runs the RHEL 10 and Tesseract image check. On main, either
# one runs the release job, whose release holds both binaries.
# boinc_worker, boinc_qwen, boinc_surya, boinc_ndlocr, ocr_dashboard: the BOINC
# worker image workflows and the dashboard's static build, which read these
# outputs from their own `inputs` job. They run for what each builds, not for
# anything under crates/: a crate counts when it is in the closure of what the
# image or binary builds (normal and build dependencies from cargo metadata,
# plus the roots' own dev-dependencies for the two images that build
# examples), the files those crates embed with include_str!/include_bytes!
# count, and the workspace manifest and lock file count only when the packages
# Cargo resolves for the roots change or the manifest's other settings do.
# Files the image's own recipe reads are listed below. test_changes.py checks
# the lists and the embedded files against each workflow's `paths:`.
set -euo pipefail
out=${GITHUB_OUTPUT:-/dev/stdout}
# Known only once the changed files are; "run everything" keeps them.
provision=false
ops=false
forced=
all() {
  printf 'rust=true\nquickwit=true\nweb=true\nimage=true\ne2e=true\nfixtures=true\nops=%s\ncode=true\nmarkdown=true\nprovision=%s\nocr_worker=true\nocr_dash=true\nocr_dashboard=true\nboinc_worker=true\nboinc_qwen=true\nboinc_surya=true\nboinc_ndlocr=true\ninfra=true\npage_language=true\ncontrib=true\nocr_gold=true\nboinc_pages=true\n' "$ops" "$provision" >> "$out"
  echo "running everything: $1"
  exit 0
}
if [ "${GITHUB_EVENT_NAME:-}" = workflow_dispatch ]; then
  case "${CI_SCOPE:-full}" in
    full) all "full workflow dispatch" ;;
    ocr-worker)
      [ "${GITHUB_REF:-}" = refs/heads/main ] || {
        echo "ocr-worker publication scope requires refs/heads/main" >&2
        exit 1
      }
      for part in rust quickwit web image e2e fixtures ops code markdown provision ocr_worker ocr_dash ocr_dashboard boinc_worker boinc_qwen boinc_surya boinc_ndlocr infra page_language contrib ocr_gold boinc_pages; do
        case "$part" in rust|ocr_worker) value=true ;; *) value=false ;; esac
        printf '%s=%s\n' "$part" "$value" >> "$out"
      done
      echo "OCR worker checks and attested release only; no application rollout"
      exit 0
      ;;
    *) echo "unknown workflow dispatch scope: $CI_SCOPE" >&2; exit 1 ;;
  esac
fi
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
# Both sides of a rename: a source file moved under docs/ is still a removal
# the checks must see.
files=$(git diff --name-only --no-renames "$range") || all "git diff failed for $range"
[ -n "$files" ] || all "no changed files found"
echo "$files" | sed 's/^/changed: /'
# Prettier formats Markdown anywhere (.prettierrc.json, .prettierignore).
if grep -Eq '\.md$' <<< "$files"; then markdown=true; else markdown=false; fi
# Documentation is only read: nothing below counts it. No README is compiled
# into a binary or an image.
files=$(grep -Ev '\.md$|^docs/' <<< "$files" || true)
if grep -Eq '^contrib-compute/boinc/html/' <<< "$files"; then boinc_pages=true; else boinc_pages=false; fi
files=$(grep -Ev '^contrib-compute/boinc/html/' <<< "$files" || true)
matches() { [ -n "$files" ] && grep -Eq "$1" <<< "$files"; }
# Gold fixtures are excluded from application/image rules. The embedded v1 lock
# is considered separately for Rust below, and for BOINC's contrib checks.
built=$(grep -Ev '^fixtures/ocr-gold/' <<< "$files" || true)
matches_built() { [ -n "$built" ] && grep -Eq "$1" <<< "$built"; }
# infra/quickwit/ holds the index configs (read by the ingest image) and the
# searcher config (applied on deploy). Everything else under infra/ needs a
# provision, an environment's committed settings (infra/env/) included.
if [ -n "$files" ] && grep -E '^infra/' <<< "$files" | grep -Evq '^infra/quickwit/'; then
  provision=true
fi
# The index version snapshots and the index history must match their schemas
# (and what writes them); searches.sh's date-span summary has its tests there too.
matches '^(ops/version-snapshots/|ops/index-history(\.schema)?\.json$|scripts/(check-version-snapshots|test_check_version_snapshots|compare-versions|check-index-history|test_check_index_history|reconstruct-index-history|test_date_spans)\.py$|scripts/lib/date-spans\.jq$|scripts/ci/requirements-ops\.txt$)' && ops=true
# Another workflow or dependabot.yml defines no job here. An edit to ci.yml
# outside a job (triggers, env) or to the changes job runs everything; an edit
# inside other jobs forces just the flags those jobs' `if:` lines read.
matches '^\.github/workflows/ci\.yml$' && {
  lines=$(mktemp)
  forced=$(git diff -U0 "$range" -- .github/workflows/ci.yml | awk '
    /^@@/ { split($3, a, ","); start = substr(a[1], 2) + 0; n = (a[2] == "" ? 1 : a[2] + 0)
            for (i = 0; i < n; i++) changed[start + i] = 1; if (n == 0) changed[start] = 1; next }
    END { for (l in changed) print l }' | sort -n > "$lines"
    awk -v list="$lines" '
      BEGIN { while ((getline l < list) > 0) hit[l] = 1 }
      /^jobs:/ { injobs = 1; next }
      !injobs { if (hit[NR]) print "ALL"; next }
      /^  [a-z0-9_-]+:$/ { job = $1; if (job == "changes:") job = "ALL"; hitjob[job] = hitjob[job] || hit[NR]; cur = job; next }
      { if (hit[NR]) hitjob[cur] = 1
        while (match($0, /outputs\.[a-z_]+/)) { flags[cur] = flags[cur] " " substr($0, RSTART + 8, RLENGTH - 8); $0 = substr($0, RSTART + RLENGTH) } }
      END { for (j in hitjob) if (hitjob[j]) { if (j == "ALL") print "ALL"; else print flags[j] } }' .github/workflows/ci.yml | tr -s ' \n' ' ')
  rm -f "$lines"
  case " $forced " in *" ALL "*) ops=true; all "workflow changed" ;; esac
  for flag in markdown ops provision; do
    if [[ " $forced " == *" $flag "* ]]; then printf -v "$flag" true; fi
  done
  echo "workflow jobs changed, forcing:$forced"
}
# A workflow, or the composite action it builds with, is an input of its own
# job: editing it runs that job and no other.
workflow_input() {
  local re="^\.github/(workflows/$2\.yml$"
  [ -z "${3:-}" ] || re="$re|actions/$3/"
  if matches "$re)"; then forced="$forced $1"; fi
}
workflow_input boinc_worker boinc-worker boinc-worker-build
workflow_input boinc_qwen boinc-qwen boinc-qwen-build
workflow_input boinc_surya boinc-surya boinc-surya-build
workflow_input boinc_ndlocr boinc-ndlocr-lite
workflow_input ocr_dashboard ocr-dashboard
# Workflow files are settled above; they are not code for the file rules below.
files=$(grep -Ev '^\.github/' <<< "$files" || true)
# The Rust build and tests: the crates, the files compiled into binaries
# (index config, place overrides, the example searches the API warms, the
# reconstructed index history), the fixtures the tests read, and the Python
# metric the damage port is checked against (its tables, its word lists and
# their sha256).
rust_files='^(ocr/engines/engines\.toml$|ocr/engines/tesseract/models\.lock$|rust-toolchain\.toml$|fixtures/|ocr/contract/|infra/quickwit/|catalog/|web/src/examples\.json$|ops/index-history\.json$|ja-ocr/damage_v2\.py$|ja-ocr/wordlists/|scripts/quickwit-fixtures\.sh)'
rust="^(crates/|Cargo\.(toml|lock)$|ocr/engines/engines\.toml$|ocr/routing/)|$rust_files"
# The Quickwit job (parity against a real searcher, a release into a real
# writer) runs `cargo test -p usnm-search --test quickwit_parity` and
# `cargo test -p usnm-ingest --test pipeline`: the files those tests load
# here, plus (below) the workspace crates they build and the dependencies
# Cargo resolves for them. Not catalog/, web/src/examples.json,
# ops/index-history.json or other CI scripts, which the rust job's own tests
# cover; editing them doesn't change what Quickwit is asked to do.
quickwit='^(rust-toolchain\.toml$|fixtures/|infra/quickwit/|scripts/quickwit-fixtures\.sh$|scripts/ci/install-quickwit\.sh$)'
# The web job also checks the format, so Prettier's config counts.
web='^(web/|ocr/engines/engines\.toml$|ocr/engines/tesseract/models\.lock$|fixtures/|scripts/ci/(web-e2e|start-api-fixtures)\.sh$|\.prettier(rc\.json|ignore)$)'
# The Quickwit pin is the ingest image's base, so a new one republishes it.
# Crates count for these two below, by closure.
e2e="$rust_files|$web"
image="$rust_files|^(web/|ja-ocr/|Dockerfile|\.dockerignore$|infra/quickwit-image\.json$|scripts/build-page-language-words\.py$|scripts/ci/(check-(api|ingest|ja-ocr)-image|quickwit-image|roll-api|head-of-main|indexnow)\.sh$)"
# The BOINC integration tests execute this wrapper at run time. Its changes
# need the Rust tests, but it is not an input to the serving/ingest images above.
rust="$rust|^contrib-compute/images/server/bridge-job\.sh$"
# The synthetic corpus must match its generator.
fixtures='^fixtures/'
# The OCR worker's EL9 build, RHEL 10 check and release, and the LAN dashboard's
# EL9 build. The crates each builds are a closure (narrow_release, below); what
# is listed here is what is not a crate: the Tesseract image, the catalog and
# routes the worker reads, the toolchain and the release script. The dashboard
# shares the release but not the engine: it reads the engine catalog, the
# Tesseract models lock and the routes only through the worker crate it links.
ocr_worker='^(ocr/engines/tesseract/|ocr/engines/engines\.toml$|ocr/routing/|rust-toolchain\.toml$|scripts/ci/ocr-worker-release\.sh$)'
ocr_dash='^(ocr/engines/engines\.toml$|ocr/engines/tesseract/models\.lock$|rust-toolchain\.toml$)'
# ocr-dashboard.yml builds the same binary for musl, from the same inputs.
ocr_dashboard=$ocr_dash
# The BOINC worker images (.github/workflows/boinc-*.yml) build usnm-boinc from
# the checkout (COPY . .), so what each one adds to the crates is its own
# Containerfile, its smoke tests and what they download or pin. The worker and
# NDLOCR images also build usnm-boinc's examples (narrow_examples).
boinc_worker='^(ocr/engines/tesseract/|ocr/engines/engines\.toml$|ocr/boinc/Containerfile$|scripts/boinc/smoke-worker\.py$|rust-toolchain\.toml$|\.dockerignore$)'
boinc_qwen='^(ocr/boinc/qwen/|scripts/boinc/qwen_(disk|cold_cache)\.py$|rust-toolchain\.toml$|\.dockerignore$)'
boinc_surya='^(ocr/boinc/surya/|rust-toolchain\.toml$|\.dockerignore$)'
boinc_ndlocr='^(ocr/engines/ndlocr-lite/|ocr/boinc/ndlocr-lite/|scripts/boinc/smoke-ndlocr(-worker)?\.py$|ja-ocr/(testdata/|alto\.py$|jaocr\.py$)|rust-toolchain\.toml$|\.dockerignore$)'
# The page-language detector's test on text of known language (05 §5.5.8):
# the detector, its word lists and the language table it reads them by, its
# crate's manifest and the lock file, the samples and their test, and the
# scripts that write the lists and the samples.
page_language='^(crates/usnm-core/src/page_language/|crates/usnm-core/src/lang\.rs$|crates/usnm-core/data/page-language-words\.txt$|crates/usnm-core/Cargo\.toml$|Cargo\.lock$|crates/usnm-core/tests/page_language_samples\.rs$|fixtures/page-language/|scripts/(page-language-samples|build-page-language-words)\.py$)'
# The BOINC server hosting: its scripts, templates and Bicep (its Markdown counts as
# documentation, above), and the workflow, which matches earlier.
contrib='^(contrib-compute/|fixtures/ocr-gold/(selections/|bundle-v1\.lock\.json$)|scripts/ocr-survey/transcription\.py$|infra/workbooks/boinc\.json$|crates/usnm-boinc/|ocr/boinc/|scripts/boinc/|scripts/boinc-app-release\.sh$|scripts/ci/check-boinc-server-image\.sh$)'
ocr_gold='^(scripts/ocr-survey/|fixtures/ocr-gold/|crates/usnm-ocr-survey/tests/golden/)'
# closure_crates ROOT...: the directories of the workspace crates the roots
# depend on, directly or not (normal, build and dev dependencies: path
# dependencies in their manifests), from cargo metadata, so a new dependency
# can't leave the list stale.
closure_crates() {
  cargo metadata --format-version 1 --no-deps --offline | jq -r --arg root "$PWD/" --args '
    (.packages | map({key: .name, value: .}) | from_entries) as $p
    | def close: . as $s | ([$s[], ($s[] | $p[.].dependencies[] | select(.path) | .name)] | unique) as $n
        | if $n == $s then $s else $n | close end;
    $ARGS.positional | close | .[] | $p[.].manifest_path | ltrimstr($root) | rtrimstr("Cargo.toml")' "$@"
}
# closure_build_crates DEV ROOT...: the directories of the workspace crates a
# build of the roots compiles: the roots and their normal and build
# dependencies, transitively, as `cargo build` follows them. A dev-dependency
# of a dependency is never built, so the worker's dev-dependency on the ingest
# crate doesn't make the dashboard's binary depend on it. DEV=true adds the
# roots' own dev-dependencies (a build of their examples compiles them).
closure_build_crates() {
  local dev=$1; shift
  cargo metadata --format-version 1 --no-deps --offline | jq -r --arg root "$PWD/" --argjson dev "$dev" --args '
    (.packages | map({key: .name, value: .}) | from_entries) as $p
    | def build_deps: map($p[.].dependencies[] | select(.path and .kind != "dev") | .name);
      def close: . as $s | (($s + ($s | build_deps)) | unique) as $n
        | if $n == $s then $s else $n | close end;
    $ARGS.positional as $r
    | (if $dev then $r + ($r | map($p[.].dependencies[] | select(.path) | .name)) else $r end | unique) | close
    | .[] | $p[.].manifest_path | ltrimstr($root) | rtrimstr("Cargo.toml")' "$@"
}
# closure_embeds DIR...: the repository files the crates in DIR... embed in
# their binaries with include_str! or include_bytes! (a literal path, relative
# to the file naming it, in src/, examples/ or build.rs), as regular
# expressions, one per line. Tests and their fixtures are not built.
closure_embeds() {
  local dir sub paths=()
  for dir in "$@"; do
    [ -d "$dir" ] || continue
    for sub in src examples build.rs; do
      [ ! -e "$dir$sub" ] || paths+=("$dir$sub")
    done
  done
  [ "${#paths[@]}" -gt 0 ] || return 0
  find "${paths[@]}" -name '*.rs' -type f -exec perl -0777 -ne '
    use File::Basename;
    while (/\binclude_(?:str|bytes)!\s*\(\s*"([^"]+)"/g) {
      my @p;
      for (split m{/}, dirname($ARGV) . "/" . $1) {
        next if $_ eq "" or $_ eq ".";
        if ($_ eq "..") { pop @p } else { push @p, $_ }
      }
      print join("/", @p), "\n";
    }' {} + | sort -u | sed -e 's/[][\.*^$()+?{|]/\\&/g' -e 's/^/^/' -e 's/$/$/'
}
# closure_tree DIR ROOT...: every package (with its features) Cargo builds for
# the roots' tests, in the checkout at DIR, without the workspace crates' local
# paths (a git source keeps its revision).
closure_tree() {
  local dir=$1 p args=(); shift
  for p in "$@"; do args+=(-p "$p"); done
  (cd "$dir" && cargo tree --locked "${args[@]}" -e normal,build,dev --prefix none -f '{p} {f}' | sed 's| (/[^)]*)||' | sort -u)
}
# root_settings: the workspace manifest at revision $1 without its member list
# and [workspace.dependencies], whose effect on these crates the package tree
# shows. Anything else there (profiles, workspace.package, lints, patches)
# can change how the tests build without changing the tree.
root_settings() {
  git show "$1:Cargo.toml" | awk '
    /^members = \[/ { skip = 1 } skip { if (/^\]/) skip = 0; next }
    /^\[/ { deps = ($0 == "[workspace.dependencies]") } !deps'
}
# lock_changed ROOT...: whether the packages resolved for the roots differ
# between the base and this revision (a crate added outside them changes
# neither). True if that can't be worked out.
lock_cache=
lock_changed() {
  local key="$*" result=true
  case $lock_cache in
    *"|$key=true|"*) return 0 ;;
    *"|$key=false|"*) return 1 ;;
  esac
  if lock_changed_uncached "$@"; then result=true; else result=false; fi
  lock_cache="$lock_cache|$key=$result|"
  [ "$result" = true ]
}
lock_changed_uncached() {
  local base dir before after
  case "$range" in *...*) base=$(git merge-base "${range%%...*}" "${range##*...}") ;; *) base=${range%%..*} ;; esac || return 0
  [ "$(root_settings "$base")" = "$(root_settings HEAD)" ] || return 0
  dir=$(mktemp -d)
  git worktree add -q --detach "$dir" "$base" || { rm -rf "$dir"; return 0; }
  before=$(closure_tree "$dir" "$@") || before=
  git worktree remove --force "$dir"
  after=$(closure_tree . "$@") || after=
  [ -z "$before" ] || [ -z "$after" ] || [ "$before" != "$after" ]
}
# fail_open KIND PART: run PART because what it builds can't be worked out. The
# build kinds are forced, which the loop below honours for any change; the
# pattern '.' matches only built files, and a change to the gold set alone has
# none, though the BOINC images embed its lock. KIND test keeps '.', so the
# application jobs still leave the gold set out.
fail_open() {
  if [ "$1" = test ]; then printf -v "$2" '.'; else forced="$forced $2"; fi
}
# _narrow KIND PART ROOT...: run PART when a crate in the roots' closure
# changed, or the workspace manifest or lock file changed what Cargo resolves
# for them. KIND test takes the closure the tests build (closure_crates).
# KIND release takes what a release build of the roots compiles and KIND
# examples also the roots' dev-dependencies (closure_build_crates); both add the
# files those crates embed (the gold set's embedded lock among them). Fails
# open: when cargo or perl can't say, PART runs.
_narrow() {
  local kind=$1 part=$2 dirs= embeds listed=true; shift 2
  matches_built "${!part}" && return 0
  case $kind in
    test) dirs=$(closure_crates "$@") || listed=false ;;
    release) dirs=$(closure_build_crates false "$@") || listed=false ;;
    examples) dirs=$(closure_build_crates true "$@") || listed=false ;;
  esac
  if [ "$listed" = false ] || [ -z "$dirs" ]; then
    echo "cargo metadata failed: running $part"
    fail_open "$kind" "$part"
    return 0
  elif matches_built "^($(paste -sd'|' - <<< "$dirs"))"; then
    printf -v "$part" '.'
    return 0
  fi
  if [ "$kind" != test ]; then
    # shellcheck disable=SC2086 # one argument per crate directory
    if ! embeds=$(closure_embeds $dirs); then
      echo "can't list what $* embed: running $part"
      fail_open "$kind" "$part"
      return 0
    elif [ -n "$embeds" ] && matches "$(paste -sd'|' - <<< "$embeds")"; then
      forced="$forced $part"
      return 0
    fi
  fi
  if matches_built '^Cargo\.(toml|lock)$' && lock_changed "$@"; then
    echo "the packages resolved for $* changed: running $part"; printf -v "$part" '.'
  fi
}
narrow() { _narrow test "$@"; }
narrow_release() { _narrow release "$@"; }
narrow_examples() { _narrow examples "$@"; }
narrow quickwit usnm-search usnm-ingest
narrow image usnm-api usnm-ingest
narrow e2e usnm-api
narrow_release ocr_worker usnm-ocr-worker
narrow_release ocr_dash usnm-ocr-dash
narrow_release ocr_dashboard usnm-ocr-dash
narrow_examples boinc_worker usnm-boinc
narrow_release boinc_qwen usnm-boinc
narrow_release boinc_surya usnm-boinc
narrow_examples boinc_ndlocr usnm-boinc usnm-ocr-adapter
for part in rust quickwit web image e2e fixtures ocr_worker ocr_dash ocr_dashboard boinc_worker boinc_qwen boinc_surya boinc_ndlocr; do
  if matches_built "${!part}" ||
    { [ "$part" = rust ] && matches '^fixtures/ocr-gold/bundle-v1\.lock\.json$'; } ||
    [[ " $forced " == *" $part "* ]]; then echo "$part=true" >> "$out"; else echo "$part=false" >> "$out"; fi
done
for part in page_language contrib ocr_gold; do
  if matches "${!part}" || [[ " $forced " == *" $part "* ]]; then echo "$part=true" >> "$out"; else echo "$part=false" >> "$out"; fi
done
echo "ops=$ops" >> "$out"
echo "markdown=$markdown" >> "$out"
echo "boinc_pages=$boinc_pages" >> "$out"
if [ -n "$files" ] || [[ " $forced " == *" code "* ]]; then echo "code=true" >> "$out"; else echo "code=false" >> "$out"; fi
# The infra job's Bicep, naming, workbook and script-test steps read nothing in
# the dashboard crate's src/ and tests/ (its manifest is read, with every
# crate's, by the selector's own tests), so a change confined to them skips
# those steps. Anything else, a crate's other Rust sources included, runs them.
infra_files=$(grep -Ev '^crates/usnm-ocr-dash/(src|tests)/' <<< "$files" || true)
if [ -n "$infra_files" ] || [[ " $forced " == *" infra "* ]]; then echo "infra=true" >> "$out"; else echo "infra=false" >> "$out"; fi
echo "provision=$provision" >> "$out"
