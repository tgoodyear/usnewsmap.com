# US News Map Revival: Design Document

**Status:** living design record, kept current with the code · **First draft:** September 2026 · **Scope:** architecture and design of [usnewsmap.com](https://usnewsmap.com), first built on Azure and served on-prem behind Cloudflare since 31 October 2026.

US News Map (2015–2016, Georgia Tech Research Institute and eHistory.org at the University of Georgia) let anyone search the Library of Congress's _Chronicling America_ newspaper collection and watch where and when a word or phrase appeared across the United States. You typed a term, a map lit up with cities whose newspapers printed it, and a playback control showed coverage spreading over time. The site won a prize in the NEH _Chronicling America Data Challenge_, and the Washington Post, UGA/Newswise, the Library of Virginia and genealogy societies all wrote about it. It later went offline because no organization was left to support it.

This document set evaluates the legacy system ([`tgoodyear/usnewsmap`](https://github.com/tgoodyear/usnewsmap)) and describes its replacement, which one maintainer can run.

## Reading order

| #   | Document                                                             | What it covers                                                                                                                                                                                     |
| --- | -------------------------------------------------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 01  | [Background & legacy analysis](01-background-and-legacy-analysis.md) | Project history, press coverage, a walkthrough of the legacy code, why the system failed, security findings                                                                                        |
| 02  | [Product requirements](02-product-requirements.md)                   | Personas and use cases from the press coverage, the legacy feature list, new features, non-functional requirements                                                                                 |
| 03  | [Architecture overview](03-architecture-overview.md)                 | How it runs today, components, request and data flows, the main architectural decisions                                                                                                            |
| 04  | [Data sources & ingestion](04-data-sources-and-ingestion.md)         | Library of Congress data sources after the 2025 migration, the ingest pipeline, data lake layout, geocoding, baselines, re-indexing                                                                |
| 05  | [Search & document storage](05-search-and-storage.md)                | Evaluation of Azure AI Search, Quickwit, Cosmos DB, Elastic and PostgreSQL; sizing; index schemas; query semantics; aggregation strategy                                                           |
| 06  | [API design (Rust)](06-api-design.md)                                | Whether Rust is the right choice, API contract, crate layout, caching, guardrails                                                                                                                  |
| 07  | [Frontend design](07-frontend-design.md)                             | SPA stack, map rendering, client-side playback, timeline, accessibility, embed mode, wireframes                                                                                                    |
| 08  | [Azure infrastructure & delivery](08-azure-infrastructure.md)        | What Azure holds, identity, networking, sizing, IaC, CI/CD, portability                                                                                                                            |
| 09  | [Operations, security & cost](09-operations-security-cost.md)        | SLOs, observability, on-prem serving runbook, privacy, threat model, costs, backup/DR                                                                                                              |
| 10  | [Roadmap, risks & open questions](10-roadmap-and-risks.md)           | Where each phase stands, validation spikes, risk register, open questions for the maintainer                                                                                                       |
| 11  | [Geographic skew of search terms](11-term-geographic-skew.md)        | The relative-rate view (phase 1 built): comparing places with what their digitized pages predict (time-standardized lift with empirical-Bayes shrinkage), with measurements on the published index |
| 12  | [On-prem OCR pipeline](12-onprem-ocr-pipeline.md)                    | Re-reading badly OCR'd pages through BOINC (#296): what is built, the flows from queue to search, languages, next pull requests                                                                    |
| 13  | [OCR engines](13-ocr-engines.md)                                     | The contract between the on-prem OCR worker and its engines, how engines run and are routed, provenance, the survey's use of the same images, and an engine catalog                                |
| 14  | [Corpus data and processing flow](14-corpus-data-flow.md)            | Artifact locations, page/run metadata, reconciled status, bounded queries and the local pipeline dashboard                                                                                         |
| --  | [Image reuse](image-reuse.md)                                        | Phase 3 design: licensed models, leakage controls, calibrated duplicate matching and a zero-fetch offline evaluation boundary                                                                      |
| ADR | [Architecture decision records](adr)                                 | Short records of each major decision and the alternatives rejected                                                                                                                                 |

The [technical notes](../notes/README.md) are dated write-ups of the work, with its measurements; they record what was true at the time, while these documents describe how the system stands now.

Documents 15 and 16, the on-prem cold-storage plan and the `tank` disk replacement and scan staging plan, were dated plans and are now notes: [2026-10-18 Moving cold storage on-prem](../notes/2026-10-18-onprem-cold-storage.md) and [2026-10-19 `tank` replacement and scan access](../notes/2026-10-19-tank-replacement-and-scan-access.md). [ADR-0022](adr/0022-onprem-cold-storage.md) and [ADR-0023](adr/0023-onprem-scan-access.md) record their decisions.

## Executive summary

**What failed before.** The legacy system was a Python 2 / Flask API in front of a self-managed Solr cluster on university-internal hosts. It kept per-user session state in MongoDB and needed hand-run Python scripts to ingest data. Every part depended on one institution's servers and on individual people. When that support went away, nothing could keep it running. It also showed only a random sample of 500 hits per request, so the map was a sample rather than a full count. See [01](01-background-and-legacy-analysis.md).

**How it runs today.** The API, the site and the Quickwit searcher run in one on-prem container behind a Cloudflare Tunnel. A Cloudflare Worker sends each request there while it is healthy, and otherwise to a copy in Azure Container Apps kept at zero replicas. The ingest commands and index releases run on on-prem hosts, and OCR runs only through BOINC, whose server is on-prem. Azure keeps the shared state (the Blob lake, the index account, Cosmos DB, the OCR exchange), the registry, the Key Vaults and the monitoring. [03 §3.3](03-architecture-overview.md#33-deployment-today-november-2026) has the diagram, and ADRs [0027](adr/0027-onprem-serving-behind-cloudflare.md) and [0029](adr/0029-onprem-primary-azure-failover.md) record the move.

```
Browser (React + MapLibre/deck.gl SPA)
   │  https://usnewsmap.com, /v1/... same origin; also https://api.usnewsmap.com/v1/...
   ▼
Cloudflare: TLS, edge cache, health-check Worker
   │ while on-prem is ready                       │ when it isn't
   ▼                                              ▼
On-prem container (tunnel usnm-serving):         Azure Container Apps spare (zero replicas):
  Caddy → Rust API (blue/green) → Quickwit         API app → Quickwit app → index account
          index on local NVMe
   │                                              │
   └──────────────► Azure: Blob lake (reference data, search log, response cache), Cosmos DB, Application Insights
                            ▲
On-prem hosts: ingest, releases, OCR pickup ◄── LoC bulk OCR + loc.gov API;  BOINC server ◄─► volunteers
```

**Main decisions** (details and alternatives are in the ADRs):

1. **The server returns complete aggregates instead of a sample of hits.** The API returns counts for every matching page, grouped by place and time bucket, in one cacheable response. Playback, cumulative and trailing-window views are computed in the browser, with no per-user server state ([ADR-0003](adr/0003-stateless-aggregate-first-api.md)).
2. **Azure Blob Storage is the system of record.** The corpus is curated Parquet on Blob Storage, and every search index can be rebuilt from it ([ADR-0002](adr/0002-blob-data-lake-system-of-record.md)).
3. **Quickwit is the search engine.** For about 23 million pages and roughly 0.6 to 1 TB of text, Azure AI Search would cost about $2,800 to $5,600 a month; Quickwit runs on our own hardware and reads its index from local disk. It also supports nested _place × time_ aggregations in its generally available API ([ADR-0001](adr/0001-search-engine.md), [ADR-0013](adr/0013-quickwit-from-our-fork.md)).
4. **The API is written in Rust (axum + tokio)**, for fast cold starts, a small memory footprint and predictable tail latency ([ADR-0004](adr/0004-rust-api.md)).
5. **Cost and operations are hard requirements.** The target is under $80 a month, all in, with everything reproducible from the repository and no Azure VMs ([ADR-0005](adr/0005-sustainability-constraints.md)). Moving serving on-prem took Azure from about $427 a month (22 October 2026) to an estimated $105 (6 November 2026), still over the target ([09 §9.5](09-operations-security-cost.md#95-cost-model-monthly-usd-list-prices-confirm-with-the-azure-pricing-calculator)). The API serves the site from its own image ([ADR-0010](adr/0010-site-served-by-the-api.md)); all Azure infrastructure is Bicep, deployed by an operator, with GitHub Actions using OIDC to publish images.
6. **Every identity is an Entra identity.** No shared keys, SAS or connection strings anywhere; on-prem hosts use Azure Arc machine identities ([ADR-0009](adr/0009-entra-identity-only.md)).

**What users get beyond the original:** full counts of every matching page; shareable URLs; phrase, all-words, any-word and proximity search, and `~ocr` to match common OCR misreadings; filters by state, title and language; keyword-in-context snippets; a relative-rate view that compares places with what their digitized pages predict ([11](11-term-geographic-skew.md)); accessible table views with a CSV of the newspapers; and a documented public API with an OpenAPI description. Comparison of several terms, a first-appearance layer, a coverage layer, embed mode and fuzzy matching are in the requirements ([02](02-product-requirements.md)) but not built ([07 §7.4](07-frontend-design.md)).

## Sources consulted

The legacy source repository, [`tgoodyear/usnewsmap`](https://github.com/tgoodyear/usnewsmap), was read in full. For press and context, some sources could not be fetched directly from the build environment; for those we used search-engine summaries and syndicated copies:

- Newswise / UGA Today, _What going viral looked like 120 years ago_ (the "Cross of Gold" study)
- Washington Post, _The secret, pre-Internet history of "viral" memes_ (Caitlin Dewey, 17 March 2016)
- Library of Virginia, _The UncommonWealth_: _US News Map: Newspaper Coverage over Space and Time_ (20 April 2016)
- Rock Island County Illinois Genealogical Society (RICIGS): _Search newspapers with usnewsmap.com_
- Reddit r/history launch thread (March 2016)
- UGA Franklin College, _Saunt's USNewsMap wins NEH prize_; NEH press release of 25 July 2016; H-Net launch announcement; Jason A. Heppler's review in the _Journal of American History_ 104:1 (June 2017)
- Library of Congress: the Chronicling America migration to loc.gov (4 August 2025), the loc.gov JSON API tutorials, and the Chronicling America Datasets portal and NDNP-Open-OCR reprocessing announcement (The Signal, August 2026)
- Microsoft: Azure AI Search service limits (MicrosoftDocs/azure-ai-docs), the Azure SDK for Rust GA announcement (May 2026), Cosmos DB full-text search GA, and AI Search hierarchical facets (preview)
