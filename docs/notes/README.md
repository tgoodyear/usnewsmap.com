# Technical notes

Short, dated write-ups of work on the index, the OCR and the corpus's coverage: what we tried, what we measured, what we decided and what is left. They are the project's engineering log, kept next to the code so a number can always be traced to the version, commit and script it came from.

They sit between the other two kinds of document:

- The [design documents](../design/README.md) say how the system works now. They are kept current.
- The [ADRs](../design/adr/README.md) record a decision and the alternatives rejected.
- A **note** records a piece of work at a point in time, with its measurements. Once written it isn't rewritten: a later note supersedes it and links back. When a note changes what a design document says, the design document changes in the same pull request and names the note.

## Notes

| Date       | Note                                                                                                            | Topic                | Measured on                                                         |
| ---------- | --------------------------------------------------------------------------------------------------------------- | -------------------- | ------------------------------------------------------------------- |
| 2026-10-19 | [`tank` disk replacement options and scan staging](2026-10-19-tank-replacement-and-scan-access.md)              | operations           | Read-only `pve-big` pool and SMART inventory; design only           |
| 2026-10-18 | [Moving cold storage on-prem: plan](2026-10-18-onprem-cold-storage.md)                                          | operations           | Azure storage inventory of 18 October; read-only on-prem inventory  |
| 2026-10-18 | [OCR benchmark candidate feasibility (EffOCR, CHURRO, Calamari)](2026-10-18-ocr-bench-candidate-feasibility.md) | ocr                  | `ocr-bench-v1.0.0` smoke subsets, local CPU                         |
| 2026-10-18 | [OCR benchmark source, licence and coverage audit](2026-10-18-ocr-bench-source-audit.md)                        | ocr, coverage        | Published v1.0.0 and staged v1.1.0 manifests                        |
| 2026-10-18 | [Serial Set OCR audit and pilot](2026-10-18-serialset-ocr-audit.md)                                             | ocr, coverage        | `ocr/bench/serialset-pilot/` (GovInfo PDFs, Tesseract)              |
| 2026-10-18 | [Latest LoC OCR inventory and fixed benchmark coverage gap](2026-10-18-loc-refreshed-baselines.md)              | ocr, coverage        | Public LoC guide, 675-page archive, immutable benchmark             |
| 2026-10-16 | [BOINC OCR quality and vision-language crop pilot and calibration](2026-10-16-boinc-quality-qwen-pilot.md)      | ocr, operations      | 5 BOINC quality waves, Overproof-CA/BLN600/Fraktur cal              |
| 2026-10-16 | [Host onboarding and OCR spot checks (BOINC host trust)](2026-10-16-boinc-host-trust-followups.md)              | ocr, operations      | `usnm-boinc host-trust` controller commands                         |
| 2026-10-16 | [Iris Xe OCR pilot (Intel GPU, `usnm_iris`)](2026-10-16-boinc-iris-pilot.md)                                    | ocr, operations      | Single-host Intel Iris Xe evaluation                                |
| 2026-10-16 | [Transcription post-training pilot (`usnm_qwen_train`)](2026-10-16-boinc-transcription.md)                      | ocr, operations      | Public-gold LoRA trials on volunteer NVIDIA GPUs                    |
| 2026-10-16 | [JP2 against IIIF JPEG on BOINC (`usnm_assess`)](2026-10-16-boinc-assessment.md)                                | ocr, operations      | The #435 report's scripts; written and unit-tested, no campaign run |
| 2026-10-15 | [Historical-newspaper transcription pilot](2026-10-15-transcription-pilot.md)                                   | ocr, operations      | Synthetic fixtures and public Overproof-CA preparation              |
| 2026-10-14 | [Native ARM64 BOINC worker on the Mac](2026-10-14-buda-native-arm64.md)                                         | ocr, operations      | Native ARM engine/worker; wrapper 22                                |
| 2026-10-14 | [BUDA Mac tests without a project server](2026-10-14-buda-mac-offline-tests.md)                                 | ocr, operations      | BOINC 8.2.15, Podman 5.5.2, wrapper 22                              |
| 2026-10-14 | [BUDA evaluation: validation, worker identity and Mac support](2026-10-14-buda-evaluation.md)                   | ocr, operations      | Pinned BOINC source; Mac wrapper 22 launch                          |
| 2026-10-14 | [BUDA spike procedure (#481), kept as a reference](2026-10-14-buda/README.md)                                   | operations           | Not executed against a BOINC server                                 |
| 2026-10-10 | [Searcher experiments: Quickwit 0.9.1, Blob/NFS and the patched build (#251)](2026-10-10-searcher/README.md)    | indexing, operations | Dev environment's Log Analytics, 9 and 10 October                   |
| 2026-10-05 | [Corpus coverage: what is searchable, what is counted, what is missing](2026-10-05-corpus-coverage.md)          | coverage             | pages-v20260929-4, pages-v20261003-1                                |
| 2026-10-05 | [Japanese pages: OCR of the pages LoC ships without text](2026-10-05-japanese-ocr.md)                           | ocr                  | sample of 23 crops, 1942–45                                         |
| 2026-10-05 | [Phrases with common words, and the index layout around them](2026-10-05-phrases-through-common-word-pairs.md)  | indexing             | 90,745 pages (11 batches); pages-v20261003-1                        |
| 2026-10-05 | [Index versions: what built each one, and comparing them](2026-10-05-index-versions-and-what-built-them.md)     | indexing, operations | pages-v20260928-1 to pages-v20261005-1                              |

Dated plans, pilot write-ups and experiment reports that used to sit loose in `docs/` live here too: the BOINC pilots and assessments, the OCR benchmark audits and feasibility trials, the on-prem storage plans that were design documents 15 and 16 (decided in [ADR-0022](../design/adr/0022-onprem-cold-storage.md) and [ADR-0023](../design/adr/0023-onprem-scan-access.md)), and the former experiments directories, which keep their names (`2026-10-10-searcher/`, `2026-10-14-buda/`). The scripts that only these notes and the [reports](../reports/) cite are in [`scripts/archive/`](../../scripts/archive/README.md).

Topics: `indexing` (index schema, splits, the search engine), `ocr` (text we produce ourselves), `coverage` (what is in the corpus, the baselines and the no-data layer), `operations` (the pipeline, deploys, cost).

## Writing one

Copy [`template.md`](template.md) to `YYYY-MM-DD-slug.md`, fill it in, and add a row to the table above (newest first). Then:

- **Name what was measured.** Every number names the index version, the commit or the data it was measured on, and the script or command that produced it, so someone can run it again. Prefer the repository's own tooling: `scripts/compare-versions.py` for a version's search results, `/v1/status` and `/v1/versions` for the pipeline, the captures under `ops/version-snapshots/`, the Japanese OCR scoring in `scripts/ja-ocr/`.
- **Numbers go in tables**, with the environment they were taken in (the job's 2 vCPU, a laptop, cold or warm caches).
- **Point, don't repeat.** The current description of a mechanism lives in the design documents; the note tells the story and keeps the measurements, and links the section (`05 §5.5.3`) rather than copying it. Link the issue and pull request numbers.
- **Say what is left.** A note ends with what the work didn't settle and what the next measurement should be.
- **The same rules as [CONTRIBUTING](../../CONTRIBUTING.md).** No real search queries from the search log, no production logs with identifiers, no secrets. Aggregate counts from `/v1/status`, the benchmark searches (05 §5.8), the home page examples and the fixtures are all fine.

The notes are plain Markdown, rendered by GitHub. The site is served by the API from its own image ([ADR-0010](../design/adr/0010-site-served-by-the-api.md)), so publishing them on usnewsmap.com would mean building them into the web app; nothing here assumes that.

- [Stock BOINC batch pilot](2026-10-13-boinc-pilot.md): cached-page transport, native ingestion, and validation boundaries.

- [Rootless BOINC volunteer readiness on an SELinux host](2026-10-14-boinc-volunteer-preflight.md): slot labels, kernel limits, pause and restart, and a startup gate.

- [Native BOINC worker builds and Mac release tooling](2026-10-14-boinc-native-arm-build.md): native Podman round trip and the remaining release and client checks.

- [Cross-platform BOINC distribution and Mac `Run_Podman` state-query fix](2026-10-15-boinc-cross-platform-distribution.md): native distribution of `usnm_quality` and `usnm_qwen`, and the `Run_Podman` pipe-quoting patch.
