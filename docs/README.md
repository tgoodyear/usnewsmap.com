# Documentation

## Guides

| Guide                                               | For                                                                                                                                 |
| --------------------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------- |
| [Development](development.md)                       | Running and testing locally: the API and the site on the synthetic corpus, Quickwit, the ingest pipeline, the local Azure stand-ins |
| [Configuration](configuration.md)                   | The API's environment variables                                                                                                     |
| [Operations](operations.md)                         | Running the ingest pipeline in Azure: the backfill, releases, full rebuilds, pruning old index versions, logs and the search log    |
| [Infrastructure](../infra/README.md)                | What the Bicep deploys, standing an environment up, settings, DNS, the container registry                                           |
| [Web app](../web/README.md)                         | The front end: scripts, build settings, how it works                                                                                |
| [Contributor compute](../contrib-compute/README.md) | Running OCR on other people's machines with BOINC: Azure hosting for the stock BOINC server that #441's pilot submits to            |
| [Fixtures](../fixtures/README.md)                   | The synthetic corpus the tests and local development use                                                                            |
| [Contributing](../CONTRIBUTING.md)                  | The checks to run and the repository's rules                                                                                        |

## Design

The [design documents](design/README.md) (01–14) record the background, requirements and architecture, and the reasoning behind each part of the system; the [architecture decision records](design/adr/README.md) record each major decision and the alternatives rejected. References such as "05 §5.5.1" in the code and docs point into them.

## Notes

The [technical notes](notes/README.md) are dated write-ups of work on the index, the OCR and the corpus's coverage: what was tried, what was measured, what was decided and what is left. The design documents say how the system stands now; a note says what was true when. The BOINC pilot write-ups (assessment, Iris, transcription, host trust), the OCR benchmark audits and trials, the Serial Set audit, the on-prem storage plans (formerly design 15 and 16) and the former experiments directories are notes too; the scripts only they cite are in [`scripts/archive/`](../scripts/archive/README.md).

- [BOINC batch pilot](boinc.md): opt-in cached-page volunteer processing and recovery.
- [Linux BOINC quick start](boinc-linux.md): install, join, set limits and check, in five short steps, with the headless attachment helper for cluster nodes.
- [Windows BOINC quick start](boinc-windows.md): the one-script setup for a Windows PC, in five short steps.
- [macOS BOINC quick start](boinc-macos.md): the same five steps for an Apple Silicon or Intel Mac.
