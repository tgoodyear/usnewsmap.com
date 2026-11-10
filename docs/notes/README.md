# Technical notes

Dated write-ups of work on the index, the OCR, BOINC and the corpus's coverage: what we tried, what we measured, what we decided and what was left. Each note records what was true on its date, next to the code, so a number can be traced to the version, commit and script it came from.

The other kinds of document:

- The [design documents](../design/README.md) say how the system works now and are kept current.
- The [ADRs](../design/adr/README.md) record a decision and the alternatives rejected.
- The [reports](../reports) are longer studies with their own data.

A note isn't rewritten once merged. When its subject changes later (shipped, deployed, retired, superseded), the note gets an **Outcome:** line under its title or status line, dated and linked to the PR, the current document or the later note. When a note changes what a design document says, the design document changes in the same pull request and names the note.

Some notes started as plans, runbooks or experiment logs elsewhere in `docs/`: the BOINC pilots, the OCR benchmark audits, the on-prem storage plans that were design documents 15 and 16 (recorded in [ADR-0022](../design/adr/0022-onprem-cold-storage.md) and [ADR-0023](../design/adr/0023-onprem-scan-access.md)), and two experiment folders, which keep their names (`2026-10-10-searcher/`, `2026-10-14-buda/`). The scripts that only these notes and the reports cite are in [`scripts/archive/`](../../scripts/archive/README.md).

## Index and search

| Date       | Note                                                                                                           | Status on 10 November 2026                                |
| ---------- | -------------------------------------------------------------------------------------------------------------- | --------------------------------------------------------- |
| 2026-11-05 | [What the split count costs an uncached search](2026-11-05-split-count-latency.md)                             | Measured; nothing changed in production (#1430 open)      |
| 2026-10-25 | [Decade pruning: production acceptance](2026-10-25-decade-pruning-acceptance.md)                               | Accepted; #123 closed on 25 October 2026                  |
| 2026-10-10 | [Searcher experiments: Quickwit 0.9.1, Blob/NFS and the patched build](2026-10-10-searcher/README.md)          | #251 closed on 10 October 2026; dev environment torn down |
| 2026-10-05 | [Phrases with common words, and the index layout around them](2026-10-05-phrases-through-common-word-pairs.md) | Shipped; measured live in the `pages-v20261006-2` capture |
| 2026-10-05 | [Index versions: what built each one, and comparing them](2026-10-05-index-versions-and-what-built-them.md)    | Shipped (#162 to #164); scripts now in `scripts/ops/`     |

## Corpus coverage and search examples

| Date       | Note                                                                                                   | Status on 10 November 2026                              |
| ---------- | ------------------------------------------------------------------------------------------------------ | ------------------------------------------------------- |
| 2026-10-26 | [100 additional historical search examples](2026-10-26-100-more-historical-examples.md)                | Record of the additions; corrected by the #1360 audit   |
| 2026-10-05 | [Corpus coverage: what is searchable, what is counted, what is missing](2026-10-05-corpus-coverage.md) | Backfill complete; the other gaps listed are still open |
| 2026-10-01 | [Term skew: full output for seven searches](2026-10-01-term-skew-full-output.md)                       | Measured; summary figures in design 11                  |

## OCR engines and text

| Date       | Note                                                                                        | Status on 10 November 2026                                  |
| ---------- | ------------------------------------------------------------------------------------------- | ----------------------------------------------------------- |
| 2026-11-05 | [Reading our OCR by column where Tesseract read across](2026-11-05-column-reading-order.md) | Offline command merged (PR #1576); not in the worker (#926) |
| 2026-10-18 | [Serial Set OCR audit and pilot](2026-10-18-serialset-ocr-audit.md)                         | #814 closed; searchability is #922, open                    |
| 2026-10-05 | [Japanese pages: OCR of the pages LoC ships without text](2026-10-05-japanese-ocr.md)       | Shipped; searchable since `pages-v20261006-2`               |

## OCR benchmarks and reuse of LoC's OCR

| Date       | Note                                                                                                                            | Status on 10 November 2026                           |
| ---------- | ------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------- |
| 2026-11-05 | [Avoided-work accounting for a supersession list](2026-11-05-loc-supersession-accounting.md)                                    | Merged (PR #1577); no list in production             |
| 2026-11-05 | [Pinned supersession of bulk re-recognition by verified LoC OCR](2026-11-05-loc-alternate-supersession.md)                      | Merged (PR #1571); off by default                    |
| 2026-11-05 | [Binding LoC crop scores to the audited package](2026-11-05-loc-audit-binding.md)                                               | Merged (PR #1553)                                    |
| 2026-10-28 | [Reversible third-party OCR decision previews](2026-10-28-loc-alternate-decisions.md)                                           | Merged (PR #1287); preview only                      |
| 2026-10-28 | [Explicit third-party OCR staging](2026-10-28-loc-alternate-staging.md)                                                         | Merged (PR #1286)                                    |
| 2026-10-27 | [LoC reuse: safety fixtures and the ingestion boundary](2026-10-27-loc-reuse-import-contract.md)                                | Contract steps merged, all off; #811 open            |
| 2026-10-27 | [LoC re-OCR: Florida package and full repair-manifest join](2026-10-27-loc-package-audit.md)                                    | #809 closed on 27 October 2026                       |
| 2026-10-18 | [Latest LoC OCR: release inventory and the benchmark coverage gap](2026-10-18-loc-refreshed-baselines.md)                       | Inventory superseded by the package audit; #810 open |
| 2026-10-18 | [OCR benchmark source, licence and coverage audit](2026-10-18-ocr-bench-source-audit.md)                                        | v1.1.0 published on 18 October 2026; #738 open       |
| 2026-10-18 | [OCR candidate feasibility (EffOCR, CHURRO, Calamari)](2026-10-18-ocr-bench-candidate-feasibility.md)                           | #812 open; Surya app retired on 2 November 2026      |
| 2026-10-17 | [English Gold trust calibration on issue-disjoint BLN600 excerpts](2026-10-17-gold-english-trust-calibration.md)                | Promoted 17 October 2026; trust format v5 since      |
| 2026-10-16 | [Gold bundle v1 on the 80bf worker: offline, Linux and Apple Silicon measurements](2026-10-16-gold-bundle-80bf-measurements.md) | Measured; no promotion came from it                  |

## BOINC server, releases and volunteers

| Date       | Note                                                                                                        | Status on 10 November 2026                                      |
| ---------- | ----------------------------------------------------------------------------------------------------------- | --------------------------------------------------------------- |
| 2026-11-05 | [The feeder's restart bound and a stale-slot check](2026-11-05-feeder-restart-bound.md)                     | Merged (PR #1552); deployment not confirmed                     |
| 2026-10-26 | [Initial Intel and AMD BOINC acceptance follow-through](2026-10-26-boinc-gpu-initial-probes.md)             | Acceptance record of 26 October 2026; GPU classes changed since |
| 2026-10-26 | [BOINC GPU release follow-through](2026-10-26-boinc-gpu-release.md)                                         | Release record of 26 October 2026                               |
| 2026-10-21 | [Moving the BOINC server on-prem](2026-10-21-boinc-server-onprem-cutover.md)                                | Done; prod's server on-prem since 21 October 2026               |
| 2026-10-19 | [First AMD Vulkan release of usnm_qwen (#885)](2026-10-19-amd-vulkan-release.md)                            | Superseded by the 26 October GPU release                        |
| 2026-10-18 | [BOINC production drift reconciliation](2026-10-18-boinc-prod-drift-reconciliation.md)                      | Record; the Azure server was replaced on 21 October 2026        |
| 2026-10-17 | [First current-worker OCR pages through BOINC](2026-10-17-boinc-first-current-worker-pages.md)              | Record; superseded by the batch controller                      |
| 2026-10-16 | [BOINC beside the OCR worker on the nodes](2026-10-16-boinc-ocr-node-pilot.md)                              | Superseded; the direct worker is retired (#966)                 |
| 2026-10-15 | [BUDA ARM Mac evaluation and `Run_Podman` state-query fix](2026-10-15-boinc-cross-platform-distribution.md) | Merged (PR #544); Mac versions released 18 October 2026         |
| 2026-10-14 | [Native ARM64 BOINC worker on the Mac](2026-10-14-buda-native-arm64.md)                                     | Shipped as `usnm_ocr` 1.5 on 18 October 2026                    |
| 2026-10-14 | [BUDA Mac tests without a project server](2026-10-14-buda-mac-offline-tests.md)                             | PR #503 merged; BUDA not adopted (ADR-0020)                     |
| 2026-10-14 | [BUDA evaluation: validation, worker identity and Mac support](2026-10-14-buda-evaluation.md)               | Decided not to adopt BUDA (ADR-0020)                            |
| 2026-10-14 | [BUDA spike procedure (#481)](2026-10-14-buda/README.md)                                                    | Never run; kept as a reference                                  |
| 2026-10-14 | [Native BOINC worker builds and Mac release tooling](2026-10-14-boinc-native-arm-build.md)                  | Released on 18 October 2026 (#476)                              |
| 2026-10-14 | [Rootless BOINC volunteer readiness on an SELinux host](2026-10-14-boinc-volunteer-preflight.md)            | Startup gate shipped (PR #505)                                  |
| 2026-10-13 | [Stock BOINC batch pilot](2026-10-13-boinc-pilot.md)                                                        | Became the project's OCR path                                   |

## BOINC app pilots

| Date       | Note                                                                                                       | Status on 10 November 2026                          |
| ---------- | ---------------------------------------------------------------------------------------------------------- | --------------------------------------------------- |
| 2026-10-27 | [Iris performance experiment: no-change decision](2026-10-27-iris-performance-decision.md)                 | `usnm_iris` retired on 2 November 2026              |
| 2026-10-27 | [Iris Xe crop pilot: evaluation and source-only staging](2026-10-27-iris-pilot-closeout.md)                | #703 closed; `usnm_iris` retired on 2 November 2026 |
| 2026-10-19 | [The quality wave's page JPEG cache](2026-10-19-quality-wave-jpeg-cache.md)                                | Retired; the on-prem feed derives page JPEGs        |
| 2026-10-16 | [BOINC OCR quality and vision-language crop pilot and calibration](2026-10-16-boinc-quality-qwen-pilot.md) | Both apps in production; see the quality page       |
| 2026-10-16 | [Host onboarding and OCR spot checks](2026-10-16-boinc-host-trust-followups.md)                            | Reference for `usnm-boinc host-trust`               |
| 2026-10-16 | [Iris Xe OCR pilot](2026-10-16-boinc-iris-pilot.md)                                                        | Closed 27 October 2026; retired on 2 November 2026  |
| 2026-10-16 | [Transcription post-training pilot (`usnm_qwen_train`)](2026-10-16-boinc-transcription.md)                 | Trials never ran; retired on 2 November 2026        |
| 2026-10-16 | [JP2 against IIIF JPEG on BOINC (`usnm_assess`)](2026-10-16-boinc-assessment.md)                           | Never ran; retired on 2 November 2026               |
| 2026-10-16 | [Japanese NDLOCR-Lite on BOINC: first full-page validation](2026-10-16-ndlocr-lite-boinc-validation.md)    | Shipped as `usnm_ndlocr` 1.1                        |
| 2026-10-15 | [Historical-newspaper transcription pilot](2026-10-15-transcription-pilot.md)                              | Baseline published; GPU trials never ran            |

## Qwen agreement, cost and dispatch

| Date       | Note                                                                                                    | Status on 10 November 2026                             |
| ---------- | ------------------------------------------------------------------------------------------------------- | ------------------------------------------------------ |
| 2026-11-05 | [Stopping Yiddish v1 tie-breakers](2026-11-05-qwen-yiddish-tiebreak-policy.md)                          | Merged (PR #1549); not turned on (#1477)               |
| 2026-11-05 | [Whether Qwen dispatch leaves the Intel host fair work](2026-11-05-qwen-intel-dispatch.md)              | Intel took general admission on 8 November 2026        |
| 2026-11-05 | [What bounds the quality feed's supply](2026-11-05-quality-supply.md)                                   | Measured; #1475 open                                   |
| 2026-11-04 | [Strict Qwen agreement on crops with no words](2026-11-04-qwen-wordless-crops.md)                       | Pair log marks them (PR #1504); decision open on #1463 |
| 2026-11-04 | [What Qwen's extra replicas cost](2026-11-04-qwen-replica-cost.md)                                      | Measured; followed by the tie-breaker policy           |
| 2026-11-04 | [Sample page ids on the Qwen wire](2026-11-04-page-id-wire-inventory.md)                                | Merged (PR #1510); #983 open                           |
| 2026-11-04 | [The crop rule's floor against human transcriptions](2026-11-04-crop-consensus-floor-check.md)          | Floor raised to 0.7 (PR #1491); rule still off         |
| 2026-11-01 | [Why Qwen replicas come back inconclusive](2026-11-01-qwen-cross-vendor-validation.md)                  | Measured; crop rule and loop guard off                 |
| 2026-10-18 | [Qwen 8B on Intel and NVIDIA GPUs: first measurements](2026-10-18-qwen-8b-intel-nvidia-measurements.md) | Shipped as `intel_vulkan_8b` and `nvidia_vulkan_8b`    |

## Holding results from publishing

| Date       | Note                                                                                              | Status on 10 November 2026                      |
| ---------- | ------------------------------------------------------------------------------------------------- | ----------------------------------------------- |
| 2026-11-07 | [Serbian pages read by two engines and held from publishing](2026-11-07-serbian-trust-hold.md)    | Live since 7 November 2026                      |
| 2026-11-05 | [Holding German BOINC results until Fraktur trust is calibrated](2026-11-05-german-trust-hold.md) | Live since 6 November 2026 (#835)               |
| 2026-10-18 | [German Fraktur cutover diagnostic](2026-10-18-german-fraktur-diagnostic.md)                      | Measured; threshold failed, German results held |

The two policies, [`2026-11-05-german-trust-hold.json`](2026-11-05-german-trust-hold.json) and [`2026-11-07-serbian-trust-hold.json`](2026-11-07-serbian-trust-hold.json), are read by the `usnm-boinc` trust tests and the BOINC CI workflows' path filters. Don't move or edit them without changing those.

## Operations and storage

| Date       | Note                                                                                               | Status on 10 November 2026                                                   |
| ---------- | -------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------- |
| 2026-10-28 | [Bounded JPEG-cache replenishment, before deployment](2026-10-28-jpeg-cache-replenishment.md)      | Never deployed; #911 closed another way                                      |
| 2026-10-26 | [Shortening hosted CI's critical path](2026-10-26-hosted-ci-critical-path.md)                      | Merged (PR #1232) on 26 October 2026                                         |
| 2026-10-19 | [`tank` disk replacement options and scan staging](2026-10-19-tank-replacement-and-scan-access.md) | Design only (formerly design document 16)                                    |
| 2026-10-18 | [Moving cold storage on-prem: plan](2026-10-18-onprem-cold-storage.md)                             | Sample check and restore drill done; #895 open (formerly design document 15) |

## Data files

- [`2026-11-01-native-ocr-parity.json`](2026-11-01-native-ocr-parity.json): per-page parity of the container and native `usnm_ocr` builds, cited by [Native OCR](../boinc/apps/native-ocr.md).
- The two trust-hold policies, above.

## Writing one

Copy [`template.md`](template.md) to `YYYY-MM-DD-slug.md`, fill it in, and add a row to the right topic table above (newest first). Then:

- **Name what was measured.** Every number names the index version, the commit or the data it was measured on, and the script or command that produced it, so someone can run it again. Prefer the repository's own tooling: `scripts/ops/compare-versions.py` for a version's search results, `/v1/status` and `/v1/versions` for the pipeline, the captures under `ops/version-snapshots/`, the Japanese OCR scoring in `ocr/ja-ocr/tools/`.
- **Numbers go in tables**, with the environment they were taken in (the job's 2 vCPU, a dev container, cold or warm caches).
- **Point, don't repeat.** The current description of a mechanism lives in the design documents and runbooks; the note keeps the story and the measurements, and links the section (`05 §5.5.3`) rather than copying it. Link the issue and pull request numbers.
- **Say what is left.** A note ends with what the work didn't settle and what the next measurement should be.
- **The same rules as [CONTRIBUTING](../../CONTRIBUTING.md).** No real search queries from the search log, no production logs with identifiers, no secrets. Aggregate counts from `/v1/status`, the benchmark searches (05 §5.8), the home page examples and the fixtures are all fine.

The notes are plain Markdown, rendered by GitHub. The site is served by the API from its own image ([ADR-0010](../design/adr/0010-site-served-by-the-api.md)), so publishing them on usnewsmap.com would mean building them into the web app; nothing here assumes that.
