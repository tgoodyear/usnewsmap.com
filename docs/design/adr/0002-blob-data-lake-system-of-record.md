# ADR-0002: Blob data lake as the system of record

- **Status:** Accepted
- **Date:** 2026-09

## Context

The legacy system treated Solr as the only copy of the processed corpus and fetched OCR page by page from an API that no longer exists. Rebuilding meant re-crawling LoC. Search engines and their schemas will change over the life of the project.

## Decision

Store the corpus in **Azure Blob Storage** (StorageV2, flat namespace; HNS/ADLS Gen2 is deliberately *not* enabled because it rules out blob versioning) in three layers:

- **raw**: only small title-metadata snapshots in the lean profile. Bulk archives are streamed and not retained (LoC is the source of record); the growth profile may retain them in Cold
- **curated**: normalized page-level Parquet, one row per page, partitioned by year and batch
- **reference**: titles, places, baselines, coverage, and `current.json`

**Every index is derived and disposable.** It is rebuilt only from `curated`.

## Alternatives

- *Engine as the only store* (legacy): rejected, because rebuilds depend on the source staying available and engine migrations become painful.
- *Cosmos DB as the document store*: rejected. Paying RU and storage costs for a read-mostly corpus of about 1 TB that is scanned in bulk makes no sense.
- *Azure SQL / PostgreSQL*: rejected. Bulk columnar scans (for baselines and rebuilds) are cheaper and simpler with Parquet.

## Consequences

- Fetching from LoC happens **once**; after that, re-indexing and engine swaps are local.
- The curated lake also serves offline research (DuckDB, DataFusion, Fabric/Synapse) and could be published as a derived open dataset.
- Storage cost is about $15–25/month (index Hot, curated Cool).
- Cosmos DB was considered for document state and rejected for the corpus (~600 GB uncompressed JSON ≈ $150/month in storage alone). Cosmos DB is used for **document state** instead ([ADR-0007](0007-cosmos-document-state.md)).
