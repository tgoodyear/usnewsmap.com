# Documentation

| Section                                   | What's in it                                                                                                                                          |
| ----------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------- |
| [Development](guides/development.md)      | Running and testing locally: the API and the site on the synthetic corpus, Quickwit, the ingest pipeline, the local Azure stand-ins                   |
| [Configuration](guides/configuration.md)  | The API's environment variables                                                                                                                       |
| [MCP server](guides/mcp.md)               | Searching the corpus from an AI agent through the public MCP endpoint                                                                                 |
| [Operations](operations/README.md)        | Running the pipeline and the site: environments and deploys, releases, full rebuilds, search serving and the Azure spare, on-prem OCR, the search log |
| [BOINC](boinc/README.md)                  | All OCR runs on volunteer computers through BOINC: volunteer setup, the apps and their releases, and the on-prem server and its steps                 |
| [OCR evaluation](ocr/README.md)           | Benchmark releases, the gold set, scoring an engine, text agreement and LoC's OCR words                                                               |
| [Design](design/README.md)                | Design documents 01–14 and the architecture decision records                                                                                          |
| [Notes](notes/README.md)                  | Dated write-ups: what was tried, measured and decided, and when                                                                                       |
| [Reports](reports/README.md)              | Research reports with their scripts and data                                                                                                          |
| [Infrastructure](../infra/README.md)      | What the Bicep deploys, standing an environment up, settings, DNS, the container registry                                                             |
| [Web app](../web/README.md)               | The front end: scripts, build settings, how it works                                                                                                  |
| [Contributor compute](../boinc/README.md) | The BOINC code: apps, server images, scripts and Bicep                                                                                                |
| [Fixtures](../fixtures/README.md)         | The synthetic corpus the tests and local development use                                                                                              |
| [Contributing](../CONTRIBUTING.md)        | The checks to run and the repository's rules                                                                                                          |

The design documents say how the system stands now; a note or report says what was true when it was written. References such as "05 §5.5.1" in the code and docs point into the design documents.
