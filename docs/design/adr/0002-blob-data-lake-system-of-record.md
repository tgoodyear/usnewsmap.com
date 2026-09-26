# ADR-0002: Blob data lake as the system of record

- **Status:** Accepted
- **Date:** 2026-09

## Context

The legacy system treated Solr as the only copy of the processed corpus and fetched OCR page by page from an API that no longer exists. Rebuilding meant re-crawling LoC. Search engines and their schemas will change over the life of the project.

## Decision

Store the corpus in **Azure Data Lake Storage Gen2** in three layers:

- **raw**: immutable LoC bulk archives, tiered to Cool/Cold
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
- Storage cost is about $30–50/month.
