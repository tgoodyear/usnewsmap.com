# Index versions: what built each one, and comparing them

**Status:** shipped (#162, #163, #164); the first version to record its own build will be the next one released from a commit at or after #162 · **Date:** October 2026 · **Measured on:** the nine index runs `pages-v20260928-1` to `pages-v20261005-1` (`ops/index-history.json`; their outcomes from the `/v1/status` reading in #161), and the capture of `pages-v20261003-1` · **Code:** `crates/usnm-ingest/src/release.rs` (the build record), `crates/usnm-api/src/versions.rs` (`/v1/versions`), `scripts/reconstruct-index-history.py`, `scripts/compare-versions.py`, `ops/index-history.json`, `ops/version-snapshots/` · **Issues:** #161, #162, #163, #164

## The question

Nine index runs started in a week (28 September to 5 October 2026), five of which published a version, while the analyzer, the templates and the release itself were changing under them. Nothing recorded which commit, which Quickwit, which index template or which feature versions a given version was built with, so when a search's result or its speed changed between versions there was no way to say what changed it. And the API serves one version at a time, so there was no way to put two versions side by side.

## What we did

**Each release records what built it** (#162). The run's commit (`USNM_GIT_SHA`, passed by CI into the ingest image), the ingest crate version, `quickwit --version` as the writer reports it, full or delta, the feature versions (`common_grams`, `ja_fold`) and each index template the run applied. The version's manifest holds the templates in full; the `index_runs` item in Cosmos holds their checksums, so it stays small.

**The versions before that were reconstructed** (`scripts/reconstruct-index-history.py`, `ops/index-history.json`). Every ingest execution ran the `usnewsmap-ingest:main` image, which CI pushes with a tag per commit, so a run's commit is the last tag pushed before the execution it ran in started (a run can start hours into its execution, after `titles-sync`). Everything else comes from that commit in git: the ingest version, the Quickwit the Dockerfile pins, the feature versions in `usnm-core`, and the templates' checksums. Each entry carries the run's start time, and the API hands a reconstructed record only to the run with that exact start, since version names repeat across environments. The file is compiled into the API.

**`GET /v1/versions`** (#163) lists every index run from the pipeline state, newest first, from the same cached reading as `/v1/status`, each with its build record, recorded or reconstructed.

**Versions are compared through captures** (#164). `scripts/compare-versions.py capture` asks the live version a fixed set of 63 searches, as the web app asks them (waiting out `202` and `503 busy`, up to 150 s each): the ten benchmark searches of 05 §5.8, the home page's 50 examples, and three Japanese ones. It records, per search, the HTTP status, the totals (hits, places, papers, days, baseline pages, first and last day), the per-bucket hits, hits by language, the backend's timing and the wall time. `diff` puts two captures side by side and flags searches whose hits changed by more than 1%, or whose status or query changed. One capture per version is kept under `ops/version-snapshots/`; a capture has to be taken while its version is live, before the next release replaces it.

## What we measured

The nine runs. Build records from `ops/index-history.json` (all reconstructed; `common_grams` and `ja_fold` are the feature versions); outcomes, pages and publish times from the `/v1/status` reading recorded in #161 on 5 October 2026 at 17:04 UTC, when `pages-v20261005-1` was still building. A failed run published nothing; its version name was never served.

| Run               | Started (UTC)    | Kind  | Commit  | Features                  |      Pages | Outcome                    |
| ----------------- | ---------------- | ----- | ------- | ------------------------- | ---------: | -------------------------- |
| pages-v20260928-1 | 2026-09-28 22:36 | full  | d8b9c30 | none                      |  2,747,479 | failed                     |
| pages-v20260928-2 | 2026-09-28 22:49 | full  | d8b9c30 | none                      |  2,747,479 | failed                     |
| pages-v20260929-1 | 2026-09-29 00:06 | full  | 4d2ddc5 | none                      |  2,747,479 | published 2026-09-29 00:42 |
| pages-v20260929-2 | 2026-09-29 01:46 | delta | c6a2e34 | none                      |  2,819,396 | published 2026-09-29 01:48 |
| pages-v20260929-3 | 2026-09-29 09:09 | delta | 5394beb | none                      |  5,263,258 | published 2026-09-29 10:10 |
| pages-v20260929-4 | 2026-09-29 13:48 | delta | 6d3e8d3 | none                      |  6,557,925 | published 2026-09-29 14:23 |
| pages-v20261002-1 | 2026-10-02 14:09 | full  | 6792a15 | none                      |  7,850,528 | failed                     |
| pages-v20261003-1 | 2026-10-03 13:33 | full  | bfa2baf | none                      | 23,722,885 | published 2026-10-04 00:46 |
| pages-v20261005-1 | 2026-10-05 17:00 | full  | 307beb5 | common_grams 1, ja_fold 1 | 23,691,405 | building at that reading   |

All of them ran Quickwit 0.9.1 and ingest 0.1.0. The `pages` template's checksum changed four times in nine runs (at `pages-v20260929-1`, `pages-v20261002-1`, `pages-v20261003-1` and `pages-v20261005-1`), which is the kind of change the record exists to show. `pages-v20261005-1` is the first with a `pages-ja` template too, and the first with common-word pairs and the Japanese fold, so it is the first that can answer a Japanese query.

The first capture, `pages-v20261003-1`, taken 5 October 2026 at 17:34 UTC:

|                                                        |                                                                                                       |
| ------------------------------------------------------ | ----------------------------------------------------------------------------------------------------- |
| Searches                                               | 63: 60 answered `200`, the three Japanese ones `422` (no Japanese index in that version)              |
| Pages in the baseline over the benchmark's full window | 23,722,885 (the `yellow fever` benchmark, 1736-09-03 to 1963-12-31)                                   |
| Slowest searches                                       | `"cross of gold"` 1896 57.6 s, standard-oil 50.8 s, cross-of-gold 44.8 s, civil-service-reform 32.3 s |
| Typical example search                                 | 4–15 s backend time                                                                                   |

The timing in a capture is the backend's own, recorded when the result was computed, so a search that a cache answered shows the time of an earlier computation.

## What is left

- **A second capture**, of `pages-v20261005-1`, and the first `diff`. That is the before/after for the [common-word pairs note](2026-10-05-phrases-through-common-word-pairs.md) and the first version where the Japanese searches should answer `200`.
- **Captures cost a quiet hour.** A search no cache holds is computed in full, one at a time; 63 of them at 4–60 s each is most of an hour on the live searcher. Whether to capture every version or only those whose build record differs in a way that could change results (template, features, analyzer) is a judgment the build record now makes possible.
- **Reconstructed records are an inference.** The commit is the last image pushed before the execution started; an execution that was started between a push and CI's image build would be attributed to the previous commit. Recorded builds replace the inference from the next release on.
