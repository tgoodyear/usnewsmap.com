# Shortening hosted CI's critical path

**Status:** implemented on a pull request, not merged. **Date:** 26 October 2026. **Measured on:** production CI run
[40389690376](https://github.com/tgoodyear/usnewsmap.com/actions/runs/40389690376).
**Code:** `.github/workflows/ci.yml`, the Rust/image setup actions,
`scripts/ci/run-unit-tests.py`, `boinc/images/ci.hcl`.
**Issue:** #1229. **Pull request:** #1232.

## The question

An on-prem comparison did not justify moving a broad set of CI jobs to
older shared hardware. Can the existing hosted runners finish the same
checks sooner by changing scheduling, artifact size and cache policy?

## What we measured first

The baseline was a successful production run, not a cold synthetic build.
Its Rust jobs hit existing caches: the workspace restored about 521 MiB
and Quickwit about 465 MiB; tool/cache setup took 26 seconds for the workspace
job. Adding another generic cache would not address the dominant work.

| Job                  | Baseline elapsed | Dominant steps                                              |
| -------------------- | ---------------: | ----------------------------------------------------------- |
| Quickwit             |          1,314 s | Fixture loading 161 s; parity 724 s; pipeline 396 s         |
| Server/bridge images |            858 s | Base image 229 s; bridge builds 511 s; live TLS checks 92 s |
| Infra                |            688 s | Python modules 479 s; Bicep lint 92 s; build 95 s           |
| Container images     |            681 s | Sequential API/ingest builds 269 s and 339 s                |
| Rust                 |            653 s | Workspace tests 513 s; clippy 43 s                          |

The workspace test step included about 174 seconds of compilation. The
slowest completed test binaries included prewarm (65 s), pipeline without
a real Quickwit binary (64 s), parity without a live URL (46 s), API (21 s)
and conformance (18 s). Some build the reference fixtures before an
individual live-service check can return early; these timings are not
equivalent to the live integration job. We did not remove tests, shorten
timeouts or replace real-service checks with mocks.

Within the Python step, the three largest modules took 185 seconds
(`test_changes.py`), 109 seconds (`test_boinc_scripts.py`) and 90 seconds
(`test_env_scripts.py`). The new sharding weights come from those
measurements, not a guessed equal file count.

## What changed

Quickwit parity and ingest pipeline run independently with the same pinned
Quickwit release; only parity loads the shared searcher fixture. Each runs
all its original tests. The original required check name is a fail-closed
aggregate of both suites, not a skipped job that could hide their failures.
API, ingest and Japanese OCR images similarly build and smoke-test in
parallel, with the original required image check aggregating every result.

The BOINC base/server chain and the bridge's Rust compiler image form a
Bake graph. BuildKit can schedule independent inputs concurrently, and the
bridge waits for both images. All original server, TLS and bridge smoke
checks remain. Each target has a distinct cache scope; no whole-image
artifact is shipped from one runner to another.

Python modules still execute one per process, preserving import isolation,
and every discovered non-excluded module belongs to exactly one of three
separate runner shards. New modules participate automatically. The runner
fails for test failures or zero-test collection and records module durations
even when tests fail. The required infra check waits for configuration
checks and all relevant shards.

CI's Rust setup exports `CARGO_PROFILE_DEV_DEBUG=0` and
`CARGO_PROFILE_TEST_DEBUG=0`; production manifests and release profiles are
unchanged. This removes compiler debug artifacts, not assertions, overflow
checks, or test coverage. A separate cache namespace prevents reuse of
incompatible old-profile artifacts. Stable libtest summaries retain
per-binary timings so a later test-sharding decision can use real data
without duplicating compilation indiscriminately.

At the cache audit snapshot, cache entries grouped as follows:

| Scope     |  BuildKit |      Rust |              npm |           Other |
| --------- | --------: | --------: | ---------------: | --------------: |
| PR/branch | 6.781 GiB | 2.255 GiB |        0.110 GiB |       0.273 GiB |
| Main      | 1.504 GiB | 1.157 GiB | none in snapshot | below 0.001 GiB |

These are a point-in-time inventory, not proof of a configured quota or
eviction rate. No cache was deleted or quota changed. Rust and build-check
image caches now save only on `main`; PRs can restore main's cache without
creating another per-PR copy. npm/browser caching is unchanged. This saves
storage/write traffic but means PR-only dependencies/profiles do not warm a
shared cache until merged. A first no-debug run must therefore be labelled
cold, not compared as though it had the baseline's warm cache.

Superseded PR runs are cancelled to stop obsolete work consuming slots;
main and workflow-dispatch runs retain independent concurrency groups.
No branch protection rule or deployment permission was changed.

## Hosted verification and remaining limits

The [PR measurement comment](https://github.com/tgoodyear/usnewsmap.com/pull/1232#issuecomment-6426524257)
records the completed hosted validation run, actual timings, cache state,
failures and any follow-up commits. No projected speedup above should be
read as a measured post-change result.

Larger hosted capacity was inspected but not provisioned: the repository's
hosted-runner management endpoint returned 404, so no usable larger-runner
configuration was established with current access. We did not create a
paid runner or select an unverified label that could leave jobs queued.
That is a separate capacity/cost decision, after profiling CPU-bound work.

One production sample cannot establish repeatability or total billed CPU
minutes. Extra parallel runners can reduce latency while increasing queue
pressure and duplicate compilation. Main's new caches will warm only after
merge. Future changes to debug profiles, Rust test sharding or test fixture
reuse need another measured comparison, not reduced coverage.
