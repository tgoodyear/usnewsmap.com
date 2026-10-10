# ADR-0004: Rust for the API and ingest

- **Status:** Accepted
- **Date:** 2026-09

## Context

The maintainer asked whether a more efficient language like Rust should be used for the API. The API is I/O-bound fan-out to the search engine, plus CPU-heavy post-processing of large aggregation results. It runs on per-second-billed, scale-to-zero Container Apps.

## Decision

Use **Rust** (axum, tokio, reqwest, serde, moka, utoipa, tracing/OpenTelemetry) for the **API** and the **ingest CLI**, in one Cargo workspace with shared domain types.

## Rationale

- Cold start is **milliseconds** and idle memory is **~10–30 MB**. We can run the smallest billable replica and scale non-production to zero without user-visible cold starts.
- There is no GC, so p99 is predictable during aggregation post-processing (3,000 × 360 cubes, joins, normalization).
- Parsing about 23M pages of OCR in the ingest CLI is CPU-bound, and the same code and types serve the API and the pipeline.
- Quickwit and Tantivy are Rust, which makes deeper integration possible (e.g. an embedded Tantivy offline build).
- The **Azure SDK for Rust reached GA in May 2026** for Core, Identity, Key Vault, Blob and Queues, which covers everything we need except AI Search. That one is a thin REST client with `azure_identity` tokens.

## Alternatives

| Option             | Assessment                                                                                                                                                            |
| ------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| .NET 10 Native AOT | Close second: best Azure SDKs and a larger maintainer pool. Cold start and memory are good with AOT. Choose this if the maintainers are .NET developers.              |
| Go                 | Good performance and a simple language, but no shared ecosystem with Quickwit/Tantivy, and weaker data tooling (Arrow/Parquet) than Rust.                             |
| Python (FastAPI)   | Best data ecosystem, but slow cold starts, higher memory, and slow post-processing without native extensions. It is still fine for ad-hoc notebooks against the lake. |
| Node/TypeScript    | Could share types with the SPA, but is weaker on CPU-heavy aggregation and has higher memory use.                                                                     |

## Consequences

- The contributor pool is smaller. Mitigated by a small codebase (roughly 3–5k lines for the API), the OpenAPI contract, golden tests and a documented rewrite path.
- The Cosmos Rust SDK is still beta (0.37), and Cosmos **is** on the launch ingestion path (document state and work leases, [ADR-0007](0007-cosmos-document-state.md)). This is an accepted beta dependency: the maintainer allows preview/beta components; the operations used are few (point read, upsert, conditional patch, query, change-feed pull); the REST API is the fallback; and the API's request path doesn't depend on Cosmos.
