# 05: Search & Document Storage

This is the most consequential decision in the design. The legacy system's single Solr core did two jobs: it was the document store and it was the aggregation engine. Here we separate **storage** (Azure Blob Storage, see [04](04-data-sources-and-ingestion.md)) from **retrieval** (a full-text engine whose index can be rebuilt from the lake), and evaluate the retrieval options.

## 5.1 Workload characterization

| Query | Description | Share of traffic | Shape |
|-------|-------------|------------------|-------|
| **Q1 Aggregate** | Text query + date range (+ filters) → total hits; counts per place × time bucket; counts per time bucket; first match date per place | ~70% | Full-text match, then **nested aggregation** over *all* matches: `terms(place_id, size≤5,000) → histogram(bucket)` and `min(day)` |
| **Q2 Hits** | Same query restricted to one place (or title), sorted by date, paginated 50 at a time, with highlighted snippets | ~25% | Top-N by a sort field + snippet generation |
| **Q3 Lookup** | Page metadata by `doc_id` | ~5% | Point read (usually served from Parquet or reference data, not the engine) |
| **Ingest** | Append-mostly; periodic batch replacement (OCR reprocessing); rare full rebuilds | Offline | Bulk |

**Characteristics that drive the choice:**

- **Relevance ranking barely matters.** Results are sorted by **date**, and the map needs **counts**. What matters is how fast the engine can *aggregate over millions of matches*, not BM25 quality.
- **Phrase and proximity are essential.** The index must store term **positions**.
- **Queries are deterministic and data changes weekly at most**, so caching by URL is extremely effective.
- **Traffic is low with spikes.** A fixed-cost engine sized for spikes is wasted most of the month.
- **Corpus size:** ~23M pages; **~450–800 GB raw text**; the index with positions is expected to be **0.5–1.2× raw text** depending on the engine. We budget **~1 TB**.

## 5.2 Requirements matrix

| # | Requirement | Weight |
|---|-------------|--------|
| R1 | Phrase and proximity search with positions over ~1 TB | Must |
| R2 | Nested aggregation *place × time bucket* over all matches, GA (not preview) | Must |
| R3 | Date range filtering that prunes efficiently | Must |
| R4 | Sort by date + paginate + highlight snippets | Must |
| R5 | Fuzzy term matching (OCR tolerance) | Should |
| R6 | Steady-state cost that fits the **< $80/mo total** budget (search compute + index storage ideally ≤ $50) | Must |
| R7 | **No IaaS VMs** (no VMs, VM Scale Sets, AKS or Batch pools); managed or containerized on Azure PaaS; low operational effort | Must |
| R8 | Re-index from the lake in ≤ 24 h | Should |
| R9 | Azure-native support and SLA | Nice |
| R10 | Path to vector/semantic search | Nice |

## 5.3 Options evaluated

### Option A: Azure AI Search (fully managed PaaS)

- **Fit:** Mature managed service with full Lucene syntax (phrase, proximity `"a b"~5`, fuzzy `term~1`, boolean), highlighting, facets, a native Blob indexer, vector and semantic search. SLA with 2 or more replicas. Managed-identity RBAC.
- **Capacity:** For services created after May 2024, partition storage is **S1 160 GB, S2 512 GB, S3 1 TB, L1 2 TB, L2 4 TB**. A ~1 TB index fits in **1 × L1** or **1–2 × S3**.
- **Aggregations (R2):** GA facets give counts per field (`place_id,count:5000`) and date interval facets (`date,interval:year`). They do **not** nest. **Hierarchical facets** (`place_id > year`) and facet metrics exist only in **preview** APIs (2025-03-01-preview and later), with no GA timeline and no SLA. GA-only workarounds:
  1. a precomputed composite facet field `place_year` (`"P00412|1896"`) and `place_month`, requested with large `count` values. This works, but bloats the index, and large facet counts are slow;
  2. fan out one facet query per time bucket (up to about 200 queries for yearly buckets), which is too expensive;
  3. accept the preview API for this one call.
- **Cost (list price, pay-as-you-go, East US; confirm in the Azure pricing calculator):**

  | Config | Storage | Approx. monthly |
  |--------|---------|-----------------|
  | L1 × 1 partition × 1 replica (no SLA) | 2 TB | **~$2,800** |
  | L1 × 1 × 2 replicas (read SLA) | 2 TB | **~$5,600** |
  | S3 × 1 × 1 (tight fit at 1 TB) | 1 TB | ~$1,960 |
  | S3 × 2 × 1 | 2 TB | ~$3,900 |

  Storage-optimized (L) tiers also have **higher query latency** by design, which matters for large aggregations.
- **Preview features are acceptable** to the owner, so the preview hierarchical facets would satisfy R2. Cost is the blocker: the cheapest tier that fits the index (S3 or L1) is about $2–2.8k/month, **25–35× the lean budget**. The preview **Serverless Developer** tier caps an index at 1 GB, so it can't hold the corpus.
- **Verdict:** Functionally strong and the least operations work, but far outside the budget. **This is the recommended alternative if funding allows** (e.g. a grant or institutional sponsor covering about $35–70k per year), or if semantic search (R10) becomes a priority.

### Option B: Quickwit on Azure Container Apps, index on Azure Blob ✅ *recommended*

- **What:** Quickwit is a Rust search engine built on **Tantivy**, relicensed to **Apache-2.0** after Datadog acquired it in January 2025. It **decouples compute from storage**: index *splits* live on **Azure Blob Storage**, and stateless searchers read them directly, using hotcache footers and a local split cache.
- **Fit:** Elasticsearch-compatible aggregations (**terms → histogram/date_histogram nesting, min/max, cardinality** are GA). Phrase queries with slop (positions have to be enabled per field). Boolean queries, snippets and sorting on fast fields. Time-partitioned splits prune date-range queries well, and publication date is a natural timestamp. Ingest API, delete tasks, and a file-backed metastore on Blob (no database needed). The file-backed metastore allows **one writer only**; serving searchers open it read-only and poll for changes ([08 §8.4.1](08-azure-infrastructure.md#841-quickwit-metastore-one-writer-many-readers)).
- **Azure PaaS usage:** Container Apps (managed, KEDA autoscaling, managed identity, managed OTel agent), Blob Storage (Hot), and Container Apps Jobs for indexing. There are no VMs and no Kubernetes to operate.
- **Cost:** Index on Blob Hot at about **$20 per TB-month**. The lean profile runs Quickwit as a sidecar next to the API in one always-warm Container Apps replica. It started at 1 vCPU / 2 GiB (roughly **$15–45 per month** at idle rates) and moved to 2 vCPU / 4 GiB in October 2026 so that uncached searches across the full date range finish sooner. The growth profile uses a 4 vCPU / 8 GiB searcher at about $100–300. Ingest compute is paid only while jobs run, on Spot for the backfill.
- **Risks:**
  1. Quickwit is tuned for logs and traces. Documents averaging 20–35 KB and very high-frequency terms need benchmarking (Spike S-2).
  2. Pre-1970 timestamps need validation. The mitigation is integer bucket fields (`day`, `ym`, `year`), which the design uses anyway (§5.5).
  3. Project cadence after the acquisition. The mitigation is that it's Apache-2.0 and Tantivy is independently maintained; the `SearchBackend` trait keeps it swappable.
  4. Fuzzy matching through the Quickwit query DSL needs validation.
  5. Container Apps ephemeral disk limits the size of the split cache. Mitigation: a Dedicated workload profile if the cache misses hurt latency.
- **Verdict:** Meets R1–R4, R6 and R7 and is Rust-native. **Recommended**, subject to Spike S-2.

### Option C: Azure Cosmos DB for NoSQL with full-text search

- Full-text search and BM25 (keyword and phrase) are **GA**, with hybrid vector search. Excellent for operational data.
- **No good fit for R2.** Aggregating millions of matching documents with `GROUP BY` costs request units that grow with the number of matches, and multi-partition aggregation over roughly 1 TB would be slow and expensive. Phrase search on 30 KB OCR bodies at this scale is untested.
- **Verdict:** Not the search backend. **Reserved for future user features** (saved searches, collections) as Cosmos serverless.

### Option D: Elastic Cloud on Azure (Azure Native ISV Service)

- Has every feature we need (nested aggregations, highlighting, fuzzy, vectors). **Searchable snapshots on Azure Blob** (frozen tier) could make ~1 TB cheap to store, but frozen-tier aggregations over millions of documents take multiple seconds. A hot tier of ~1 TB runs about **$1,500–3,000 per month**.
- **Verdict:** A credible fallback if Quickwit fails S-2 and AI Search is unaffordable. It is a fully managed SaaS offering billed through the Azure Marketplace; Elastic runs the infrastructure and we manage no VMs. A self-hosted Elasticsearch/OpenSearch cluster on VMs is **not** an option.

### Option E: Azure Database for PostgreSQL Flexible Server (tsvector / GIN)

- Handles phrase search (`<->`) and SQL aggregation well at small scale. However, a **`tsvector` records at most 16,383 positions**, and word positions above that are clamped. Dense broadsheet pages exceed that, so **phrase matches silently fail** on the ends of long pages. Also, `ts_headline` over 30 KB pages is slow, and GIN over ~1 TB needs a large, always-on server (~$1k+/mo).
- **Verdict:** Rejected (fails R1 at our document sizes).

### Option F: Embedded Tantivy inside the Rust API

- Fastest and cheapest per query (in-process, no network hop). But it needs a **~1 TB local index on a persistent disk**, which Container Apps doesn't provide at that size. It would mean VMs or AKS, which violates R7.
- **Verdict:** Rejected for the hosted system, because it would require IaaS VMs. It remains useful **outside the cloud architecture** as an offline research build: a single binary plus an index on a researcher's own machine.

### Scoring summary

| | R1 | R2 | R3 | R4 | R5 | R6 cost | R7 ops | R9 | R10 | Result |
|---|---|---|---|---|---|---|---|---|---|---|
| A AI Search | ✅ | ✅ (preview OK) | ✅ | ✅ | ✅ | ❌ $2–5.6k | ✅✅ | ✅ | ✅ | Growth-profile alternative |
| **B Quickwit/ACA/Blob** | ✅ | ✅ | ✅ | ✅ | ⚠️ validate | ✅ ~$25–65 | ✅ | ⚠️ OSS | ⚠️ | **Recommended** |
| C Cosmos DB | ⚠️ | ❌ | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅✅ | ✅ | ✅ | Future user data |
| D Elastic on Azure | ✅ | ✅ | ✅ | ✅ | ✅ | ❌/⚠️ | ✅ | ✅ | ✅ | Fallback |
| E PostgreSQL | ❌ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅ | ✅ | ✅ | Rejected |
| F Embedded Tantivy | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ | ⚠️ | Offline only |

**Decision gate (Spike S-2):** index a **1M-page sample** into Quickwit running at the lean size (1 vCPU / 2 GiB sidecar), and repeat at 2 vCPU / 4 GiB. Run the benchmark query set (§5.8) and project the results to 23M pages. **Accept the lean size if** p95 for Q1 is ≤ 2 s on typical queries and ≤ 15 s on the high-frequency set, and the correctness checks pass. **Otherwise** move to 2 vCPU / 4 GiB (still under $80), and if that fails, revisit the budget. A comparison run on AI Search S1 is optional; it is informative, but unaffordable at full scale.

## 5.4 Backend abstraction

The Rust API depends on a narrow trait, so the engine choice is reversible:

```rust
#[async_trait]
pub trait SearchBackend: Send + Sync {
    async fn aggregate(&self, q: &CompiledQuery, spec: &AggSpec) -> Result<AggResult, SearchError>;
    async fn hits(&self, q: &CompiledQuery, page: &HitPage) -> Result<HitsResult, SearchError>;
    async fn health(&self) -> Result<BackendHealth, SearchError>;
    fn capabilities(&self) -> Capabilities; // fuzzy, proximity max slop, nested aggs, max buckets
}
```

`CompiledQuery` is produced from the backend-neutral AST (see [06](06-api-design.md)) by a per-backend **translator**. Both translators are covered by the same golden-file tests.

## 5.5 Index schema

The same logical fields exist in both engines. **Integer bucket fields** are used for all time aggregation. This avoids calendar edge cases before 1970 and makes week, month and year bucketing exact and engine-independent.

| Field | Type | Indexed | Fast/Facet/Sort | Stored | Purpose |
|-------|------|---------|-----------------|--------|---------|
| `doc_id` | keyword | key | ✅ | ✅ | Identity |
| `text` | text (positions) | ✅ analyzer `usnm_text` | – | ✅ (for snippets) | Search |
| `date` | date | ✅ | ✅ | ✅ | Quickwit timestamp field (pre-1970 confirmed in S-2) |
| `day` | u32 | ✅ | ✅ sort | ✅ | Days since 1700-01-01; day/week buckets; range filter; hit order |
| `sort_key` | u64 | – | ✅ sort | – | `title ordinal << 32 \| edition << 16 \| seq`: stable hit order within a day (Quickwit can't sort on text, §5.5.1) |
| `ym` | u32 | ✅ | ✅ | – | `year*12 + (month-1)`; month buckets |
| `year` | u16 | ✅ | ✅ | – | Year buckets |
| `place_id` | keyword | ✅ | ✅ facet | ✅ | Map aggregation |
| `place_shard` | u8 | ✅ | ✅ | – | `place ordinal mod 8`, used to shard large cube aggregations (§5.7) |
| `lccn` | keyword | ✅ | ✅ | ✅ | Title filter and hits |
| `state` | keyword | ✅ | ✅ | – | Filter and choropleth |
| `language` | keyword[] | ✅ | ✅ | – | Filter |
| `front_page` | bool | ✅ | ✅ | – | Filter |
| `edition`, `seq` | u16 | – | – | ✅ | Link building |
| `batch` | keyword | ✅ | – | – | Delete-by-batch on reprocessing |

**Analyzer `usnm_text`:** a Unicode word tokenizer, then lowercase, then ASCII folding, then removal of tokens longer than 40 characters (OCR garbage). **No stemming and no stop words.** Historical exactness matters, and stop words are needed for phrases such as "cross of gold".

### 5.5.1 Quickwit index config (validated in S-2)

The config lives in [`infra/quickwit/pages-index.yaml`](../../infra/quickwit/pages-index.yaml). It is a template: the tooling substitutes `${INDEX_ID}` (`pages-base-20261001`, `pages-delta-20261008-1`, …) and `${INDEX_URI}` (`azure://qw-index/{id}`), so every base and delta shares one doc mapping. It has been checked against **Quickwit 0.9.1** (the pinned version) on the synthetic fixtures:

```yaml
doc_mapping:
  mode: strict
  timestamp_field: date
  tokenizers:
    - { name: usnm_text, type: simple, filters: [remove_long, lower_caser, ascii_folding] }
  field_mappings:
    - { name: doc_id,     type: text,     tokenizer: raw, fast: true, stored: true }
    - { name: text,       type: text,     tokenizer: usnm_text, record: position, stored: true, fieldnorms: false }
    - { name: date,       type: datetime, input_formats: ["%Y-%m-%d"], output_format: "%Y-%m-%d", fast: true, stored: true }
    - { name: day,        type: u64,      fast: true, indexed: true, stored: true }
    - { name: sort_key,   type: u64,      fast: true, indexed: false }   # hit order within a day
    # ym, year, place_id, place_shard, lccn, state, language, front_page, edition, seq, batch as in §5.5
indexing_settings:
  commit_timeout_secs: 30           # batch ingestion: cut splits at the 1 GiB heap or 30 s, not every 10 s
  split_num_docs_target: 1000000    # splits stop merging at a million pages (about 23 GB)
  merge_policy:
    type: limit_merge
    merge_factor: 10
    max_merge_factor: 12
    max_merge_ops: 4
    maturation_period: 7d
    max_finalize_merge_operations: 5
  resources: { heap_size: 1GiB }
```

**What spike S-2 settled, and how it's checked.** CI's `quickwit` job loads the fixture base and delta into Quickwit 0.9.1, stops the writer, and serves them from a read-only polling searcher configured like production. `crates/usnm-search/tests/quickwit_parity.rs` then compares the Quickwit backend with the in-memory reference backend. It covers 11 query shapes × 6 filter sets × 4 bucket units: summaries, full cubes, place-shard partitions of the cube, hit pages and snippets.

| Item | Finding on 0.9.1 | Consequence |
|------|------------------|-------------|
| Tokenizer | `type: simple` plus `remove_long`, `lower_caser` and `ascii_folding` filters is accepted as written | As §5.5 |
| Pre-1970 dates | `datetime` with `%Y-%m-%d` input stores and returns 1890s dates correctly, and `date` works as the timestamp field | Buckets still use the integer fields |
| Multi-index search | One request over base + delta returns exact counts and aggregations | Versions are explicit index lists (08 §8.4.1) |
| Nested aggregations | `terms(place_id) > histogram(day / ym / year)` and the `place_shard` split match the reference exactly | §5.7 as designed |
| Sorting | Quickwit **can't sort on text fields**, so `doc_id` can't be the tiebreak. A leading `-` in `sort_by` means **ascending** | Hits sort by `-day,-sort_key`. `sort_key` = `title ordinal << 32 \| edition << 16 \| seq`, a numeric stand-in for (title, edition, page) |
| Snippets | `snippet_fields` is a comma-separated string, not an array. Fragments come back HTML-escaped with `<b>` highlights | The API unescapes the text, then re-escapes it and emits only `<mark>` |
| Phrases, slop, prefix | Exact phrases (stop words included), `"a b"~n` and `word*` match the reference | As §5.6 |
| **Fuzzy terms** | **Not supported.** `term~1` parses but silently matches nothing, and the Elasticsearch-compatible API has no fuzzy query either | The Quickwit backend reports `fuzzy: false` and returns 422 for fuzzy queries rather than wrong counts. F-21 needs another approach; see [10 R-15](10-roadmap-and-risks.md#103-risk-register) |

Still open in S-2: the 1M-page benchmark (§5.8, latency and memory at the sidecar size, split cache) and managed-identity Blob auth against a real account (08 §8.2).

**Splits and merges.** A cold search opens every split of every index it names, with several Blob round trips per split (the footer, then term dictionary, postings and fast-field ranges), so its fixed cost grows with the number of splits, whatever the match count. The first prod releases (September 2026) committed every 10 s, so the writer cut a split about every 10 s while it ingested; each release published about 30 s after ingest stopped and the job exited, so background merges never caught up. Two deltas also filled the writer's disk ("No space left on device"), which killed indexers and failed merges. Searches against them had a fixed cost of about 4.5 to 5 s while the 1 vCPU searcher peaked at 65% CPU.

The settings above, and the release, now make each index a few large splits:

- Splits are cut when the indexing heap fills or after 30 s (about 20,000 pages at the writer's 20 MB/s), then merged 10 at a time (`limit_merge`) until they hold `split_num_docs_target` pages. At about 23 KB of index per page, a million pages is about 23 GB, and the largest merge needs its inputs and its output on disk at once, about 60 GB in all. The writer runs one merge at a time on a scratch share sized for that (08 §8.4).
- The release waits for the merges before it publishes (`crates/usnm-ingest/src/merges.rs`): first until the merge planner has nothing running or queued and the split list is stable, then it disables the index's ingest source, which shuts its merge pipeline down after up to 5 final merges of the small splits that are left. That also seals the index: later writer runs start no pipelines for it. The wait is bounded (90 minutes by default), and the release fails rather than publish if it runs out, if the writer reports a full disk or a failed merge, if the splits don't hold exactly the documents sent, or if there are more splits than the policy leaves (`docs / 1,000,000 + 5 + 2`).
- Each release logs an `index layout` line per index of the new version (splits, documents, size, smallest and largest split) and records the same in the version's `manifest.json` under `indexes`, so the result is visible without access to the index storage.

Indexes built before this keep their splits: they are older than any maturation period, so no merge policy touches them again. A full release rebuilds them as one new base ([infra/README](../../infra/README.md#ingest-jobs)).

### 5.5.2 Azure AI Search index (sketch)

```json
{
  "name": "pages-v20261001-1",
  "fields": [
    { "name": "doc_id", "type": "Edm.String", "key": true, "filterable": true },
    { "name": "text", "type": "Edm.String", "searchable": true, "analyzer": "usnm_text", "retrievable": true },
    { "name": "date", "type": "Edm.DateTimeOffset", "filterable": true, "sortable": true, "facetable": true },
    { "name": "day",  "type": "Edm.Int32", "filterable": true, "sortable": true },
    { "name": "ym",   "type": "Edm.Int32", "filterable": true, "facetable": true },
    { "name": "year", "type": "Edm.Int32", "filterable": true, "facetable": true },
    { "name": "place_id", "type": "Edm.String", "filterable": true, "facetable": true },
    { "name": "place_year",  "type": "Edm.String", "facetable": true, "retrievable": false },
    { "name": "place_ym",    "type": "Edm.String", "facetable": true, "retrievable": false },
    { "name": "lccn", "type": "Edm.String", "filterable": true, "facetable": true },
    { "name": "state", "type": "Edm.String", "filterable": true, "facetable": true },
    { "name": "language", "type": "Collection(Edm.String)", "filterable": true, "facetable": true },
    { "name": "front_page", "type": "Edm.Boolean", "filterable": true },
    { "name": "edition", "type": "Edm.Int32" },
    { "name": "seq", "type": "Edm.Int32" },
    { "name": "batch", "type": "Edm.String", "filterable": true }
  ],
  "analyzers": [{
    "name": "usnm_text",
    "@odata.type": "#Microsoft.Azure.Search.CustomAnalyzer",
    "tokenizer": "standard_v2",
    "tokenFilters": ["lowercase", "asciifolding", "usnm_len"]
  }],
  "tokenFilters": [{ "name": "usnm_len", "@odata.type": "#Microsoft.Azure.Search.LengthTokenFilter", "min": 1, "max": 40 }]
}
```

`place_year` and `place_ym` are the GA workaround for R2 (composite facets). With the preview API, `facets=["place_id > (year)"]` replaces them.

## 5.6 Query semantics

The user-facing syntax is simple and matches LoC's modes. The API parses it into an AST (see [06](06-api-design.md)), then translates:

| User intent | UI control / syntax | AST | Quickwit | AI Search (`queryType=full`) |
|-------------|--------------------|-----|----------|------------------------------|
| Exact phrase | mode = phrase, or `"cross of gold"` | `Phrase([cross,of,gold], slop 0)` | `text:"cross of gold"` | `text:"cross of gold"` |
| All words | mode = all | `And[Term…]` | `text:a AND text:b` | `+a +b` |
| Any word | mode = any | `Or[Term…]` | `text:a OR text:b` | `a b` (searchMode any) |
| Near | mode = near, n = 5 | `Phrase(slop 5, unordered)` | `text:"a b"~5` | `"a b"~5` |
| Exclude | `-word` | `Not(Term)` | `-text:word` | `-word` |
| Fuzzy (OCR) | toggle "OCR-tolerant" | `Fuzzy(term, d=1 or 2)` | not supported in 0.9 (§5.5.1); refused | `term~1` |
| Prefix | `word*` (≥ 3 chars) | `Prefix` | `text:word*` | `word*` |

Filters (`from`, `to`, `state`, `lccn`, `language`, `front`) compile to range and term filters on `day`, `state`, etc. They are never free text.

## 5.7 Aggregation strategy

**Bucket choice.** The API chooses the finest bucket that keeps the result cube bounded, unless the client pins one:

| Date span | Default bucket | Max buckets |
|-----------|----------------|-------------|
| > 30 years | year | ~210 |
| 3–30 years | month | 360 |
| 4 months – 3 years | week | 157 |
| ≤ 4 months | day | 122 |

**Queries issued for Q1** (Quickwit; AI Search in the growth profile issues the equivalent facet requests):

1. **Summary query** (always one call): `histogram(bucket_field)` for the national series, plus `terms(place_id, size = 5,000) → min(day)` for the first appearance per place. This also yields **P**, the number of places with at least one hit. It needs at most ~3,000 + 360 buckets.
2. **Cube query:** `terms(place_id) → histogram(bucket_field, interval)` gives the sparse cube `[place][bucket] = pages`. Its upper bound is **P × B** buckets (B = number of time buckets).
   - If P × B ≤ **150,000**, it's a single call.
   - Otherwise the API **shards** it by `place_shard`, a fast field holding `place ordinal mod 8` that is written at index time. It issues ⌈P × B / 150,000⌉ sub-queries (at most 8), two at a time, each filtered to its shard, and merges the results. The worst case (~633k cells) takes 5 sub-queries.
3. `max_hits = 0` on every call, so no documents are returned.

**Engine limits, set explicitly.** Quickwit caps nested aggregations with `searcher.aggregation_bucket_limit` (**default 65,000**) and `searcher.aggregation_memory_limit` (default 500 MB). The sidecar config sets **`aggregation_bucket_limit: 200000`** and **`aggregation_memory_limit: 768MB`**, which fits the 4 GiB sidecar, so a 150k-bucket shard has headroom. S-2 benchmarks memory and latency at exactly these settings. If memory is tight, the shard target drops (e.g. to 60k buckets, ≤ 11 sub-queries) rather than raising the limits. Typical queries therefore take two calls (summary + one cube), and the largest take up to 1 + 8.

**Normalization** happens in the API from `reference/{index_version}/baselines_place_day` (pages *published* per place and day, rolled up to any bucket in memory):
`rel[place][bucket] = hits / baseline`, and nationally `rel[bucket] = Σhits / Σbaseline`. This replaces the legacy `globalFreq` table, stays correct as the corpus grows, and gives honest per-place rates.

**High-frequency guardrails.** Terms such as `the` match almost every page. Protections:

- (a) The response cache and CDN absorb repeats.
- (b) A visitor's request waits at most 10 s for a search (12 s when there is a persistent cache). The API answers a slower search with `202 Accepted` and keeps computing it for up to 2 minutes while the browser asks again (06 §6.3.5); past that the API returns `503` with a hint to narrow the date range or filters, and the timeout is not cached.
- (c) The UI discourages queries made only of stop words (warns before sending).
- (d) Example searches are pre-warmed after each index-version bump.

## 5.8 Benchmark query set (Spike S-2)

| Class | Examples | Expected hits (23M corpus) |
|-------|----------|----------------------------|
| Rare phrase | `"cross of gold"` 1896; `"yellow jack"` 1878 | 10³–10⁴ |
| Mid-frequency term | `scalawag`, `miscegenation`, `influenza` 1918 | 10⁴–10⁵ |
| High-frequency term | `railroad`, `lincoln`, `war` | 10⁶–10⁷ |
| Proximity | `"gold silver"~5` | 10⁵ |
| Filtered | `influenza` state=GA front=true | 10³ |
| Pathological | `the`, `a*` (rejected), 6-term OR | – |

Metrics: p50, p95 and p99 latency (cold and warm); index size ÷ raw text; ingest pages/s; cost per 1,000 queries; count correctness against a DataFusion brute-force scan of the curated Parquet sample (must match exactly).

## 5.9 Document storage and document state

The design separates **document content** from **document state**:

- **Content** is the page text and immutable metadata: large, written once per batch version, and read in bulk.
- **State** is small and mutable: where each title, batch and page is in the pipeline, its errors, versions, index membership, geocoding review status, and coverage summaries. It changes often, needs atomic updates, and people ask questions of it ("which titles failed curation?", "which batches aren't in index v7?").

| Data | Store | Why |
|------|-------|-----|
| **Page text + immutable metadata** (the corpus) | **Curated Parquet on Blob Storage (flat namespace), Cool tier** (~150–300 GB compressed; immutable attempt paths; versioning + soft delete) | The system of record for content ([ADR-0002](adr/0002-blob-data-lake-system-of-record.md)). Read in bulk for rebuilds, baselines and offline research. About $2–3/month |
| **Document state** (per LCCN, per batch, per issue, per index run) | **Azure Cosmos DB for NoSQL, free tier** | Queryable, atomic partial updates (patch), optimistic concurrency, change feed. **$0** within the free tier ([ADR-0007](adr/0007-cosmos-document-state.md)) |
| **Serving copy of text** (snippets) and **stored fields** | Inside the search index (Quickwit docstore on Blob Hot) | Hits and snippets come back in the same engine call |
| **Reference data for the API** (titles, places, baselines, coverage) | Blob Hot, Parquet + zstd JSON, loaded into API memory. Built by the `stats` job from Parquet and the Cosmos `titles` container | The API's read path has no database dependency, so the site keeps serving if Cosmos is unavailable |
| **Response cache** | Blob Hot `cache/{index_version}/` | Survives restarts |
| **User data** (future: saved searches, collections) | Same Cosmos account (new containers) | Reuses the existing account and identity |

### 5.9.1 Cosmos DB state model

One account (**free tier**, provisioned throughput, NoSQL API, `disableLocalAuth: true`, Entra ID data-plane RBAC), one database `usnm` with **shared throughput of 1,000 RU/s** (the free-tier allowance), and these containers:

| Container | Partition key | Item (one per…) | Approx. count | Key fields |
|-----------|---------------|-----------------|---------------|-----------|
| `titles` | `/lccn` | newspaper title | ~4–5k | name, place history (date-ranged), `place_id`, geocode `{method, precision, review_status, override}`, languages, first/last issue, page counts by `ocr_source`, `status ∈ {discovered, curated, indexed, error}`, `last_error`, `updated_at` |
| `batches` | `/batch` | LoC batch | ~3k | `versions_seen[]`, `current_version`, `status ∈ {discovered, queued, downloading, curated, indexed, failed}`, `attempts`, `source_sha256`, `pages`, `lccns[]`, `curated_path`, `indexed_in[]`, `lease {owner, until}` |
| `issues` | `/lccn` | issue (lccn + date + edition) | ~3–4M | page count, empty-OCR pages, `batch`, `batch_version`, `ocr_source`, `indexed_in` (index version). Enables "what's missing for this title?" checks |
| `index_runs` | `/index_version` | index build | tens | `status`, `full`, `indexes`, `new_index`, doc and page counts, `batch_count`, `batch_list` (the path of `reference/{index_version}/batches.json`), `started_at`, `published_at`, `previous_version`, `last_error` |
| `ops` | `/kind` | misc | few | `current` pointer (mirrors `reference/current.json`), locks, the LoC download pacer, the running release's progress (`release-progress`), schedules |

**Index run items stay small.** A version's batch list carries every batch's curation record, about 500–650 bytes each, so ~3,000 batches come to ~1.5–2 MB, and Cosmos DB refuses items over 2 MB. The list is in the version's reference snapshot (`batches.json`, immutable and checksummed in the manifest like the rest), and the run item has only `batch_count` and `batch_list`, a few hundred bytes whatever the corpus size. Runs written before this carry the list inline as `batches`; the release reads either form, and the status page reads `batch_count` or the inline list's length, never the list.

**Why issues and not pages?** Page-level state (23M items, ~12 GB) would fit the free tier's 25 GB, but writing it costs about 140M RU: roughly 38 hours of the whole free throughput. It would also duplicate what the Parquet files already record. Issue-level state (~3–4M items, ~2 GB) answers the practical questions (coverage gaps, reprocessing status) at about a sixth of the write cost. Page-level detail is always available by scanning Parquet.

**Usage patterns:**
- **Work claiming.** Batch workers (Container Apps Jobs or ACI Spot groups) take a batch with a conditional **patch** on the batch item (`lease.until < now` → set the owner, `status=downloading`), using its ETag. This also makes Cosmos the work queue: `queued` batches are claimed in order, and an expired lease makes a batch claimable again. No Storage Queue is needed, which saves a private endpoint.
- **Commit order.** Cosmos transactions don't span containers, so a worker writes dependent items first (issue upserts, which are idempotent) and makes the batch's `status=curated` patch the **last** write, conditional on its lease. A crash at any point leaves the batch un-curated and retryable, never marked done with incomplete issue state.
- **Progress and retries.** Every state transition is one patch (~10 RU). Failures record `last_error` and `attempts`, and a failed batch can be re-queued with a single query plus patch.
- **Change feed.** The `index` job reads the `batches` change feed (pull model) to find batches that became `curated` since its last continuation token. This is a natural driver for incremental indexing. The Rust SDK added change-feed pull in 0.37.
- **Operator queries.** `SELECT * FROM b WHERE b.status = 'failed'`, "titles with county-precision geocodes awaiting review", "issues not in the current index". These run from the Data Explorer or a tiny admin CLI subcommand.

**Throughput budget.**
- A backfill writes ~3k batch transitions × ~5 plus ~3.5M issue upserts at ~8 RU each, about **30M RU in total**. At a throttled ~600 RU/s that takes ~14 hours, well within the ~4-day download-bound backfill.
- Weekly increments use a few thousand RU.
- The SDK retries 429s, and workers keep a client-side RU budget so the free throughput isn't exceeded.

**Cost:** **$0** within the free tier. Beyond it: ~$0.25/GB-month plus provisioned RU/s. If the free tier is already used in the subscription, **serverless** costs roughly $1–3/month at this volume.

**Client:** the pipeline talks to the Cosmos **REST API** directly (`crates/usnm-ingest/src/cosmos.rs`) with Entra ID tokens: point read, create, **replace with `If-Match`** (for claims, commits and locks), upsert and a one-field query, retrying 429s. That is the whole surface the pipeline needs, and it avoids depending on the beta Rust SDK. Finding newly curated batches uses a status query rather than the change feed; at ~3k batch items the query costs little.

**Resilience:** the API doesn't read Cosmos on the search path. Only the status page (`/v1/status`, [06 §6.3.6](06-api-design.md#636-get-v1status)) reads it, read-only, at most once a minute per API replica and three queries at a time. If Cosmos is throttled or down, the site keeps serving, the status page shows its last reading as stale, and only the pipeline pauses. Continuous backup (7-day, free tier) covers mistakes, and state can be rebuilt from Parquet plus the LoC batch list if ever lost.
