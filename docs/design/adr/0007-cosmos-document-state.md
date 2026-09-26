# ADR-0007: Cosmos DB (free tier) for document state

- **Status:** Accepted
- **Date:** 2026-09
- **Amends:** the Blob-manifest pipeline state proposed earlier in 04/05

## Context

The pipeline has to track per-title (LCCN), per-batch, per-issue and per-index-run state: discovery, curation, indexing, errors, retries, versions and geocode review. The owner suggested Cosmos DB for this *state*, explicitly not for full text. The first draft used JSON manifests on Blob with ETag concurrency.

## Decision

Store document state in **Azure Cosmos DB for NoSQL on the free tier**: provisioned 1,000 RU/s shared across one database, 25 GB, Entra ID RBAC only. Containers are `titles`, `batches`, `issues`, `index_runs` and `ops` (see [05 §5.9.1](../05-search-and-storage.md#591-cosmos-db-state-model)). Page text stays in Parquet on ADLS. The API's request path doesn't depend on Cosmos.

## Alternatives

| Option | Why not |
|--------|---------|
| Blob JSON manifests (previous draft) | Works, but isn't queryable. "Which titles failed?" means listing and reading thousands of blobs; no partial updates; no change feed |
| Azure Table Storage | Cheap and in the same account, but queries are limited (no secondary indexes), there is no GA Rust SDK, and there is no change feed |
| Azure SQL Database (serverless / free offer) | Relational and queryable, but auto-pause wake-ups add latency and it's heavier to operate; its free offer has monthly vCore-second limits |
| Page-level state in Cosmos | ~23M items and ~140M RU to write. It duplicates Parquet; issue-level granularity answers the practical questions |

## Consequences

- $0/month within the free tier (one free-tier account per subscription, which must be enabled at account creation). The same account later holds user data (saved searches, collections).
- It adds a dependency on the beta Rust Cosmos SDK (0.37). The operations used are minimal, and the REST API is a fallback.
- Backfill writes are throttled to the free-tier RU/s, which is harmless because the backfill is bound by download speed.
- The pipeline pauses (the site doesn't) if Cosmos is unavailable.
