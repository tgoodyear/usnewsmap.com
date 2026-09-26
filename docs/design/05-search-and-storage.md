# 05: Search & Document Storage

This is the most consequential decision in the design. The legacy system's single Solr core did two jobs: it was the document store and it was the aggregation engine. Here we separate **storage** (Azure Blob / ADLS Gen2, see [04](04-data-sources-and-ingestion.md)) from **retrieval** (a full-text engine whose index can be rebuilt from the lake), and evaluate the retrieval options.

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
| R6 | Steady-state cost that fits the **≤ $500/mo total** budget | Must |
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
- **Verdict:** Functionally strong and the least operations work, but it is **5–10× over the total budget**, and the one query shape we need most (R2) is preview-only. **This is the recommended alternative if funding allows** (e.g. a grant or institutional sponsor covering about $35–70k per year), or if semantic search (R10) becomes a priority.

### Option B: Quickwit on Azure Container Apps, index on Azure Blob ✅ *recommended*

- **What:** Quickwit is a Rust search engine built on **Tantivy**, relicensed to **Apache-2.0** after Datadog acquired it in January 2025. It **decouples compute from storage**: index *splits* live on **Azure Blob Storage**, and stateless searchers read them directly, using hotcache footers and a local split cache.
- **Fit:** Elasticsearch-compatible aggregations (**terms → histogram/date_histogram nesting, min/max, cardinality** are GA). Phrase queries with slop (positions have to be enabled per field). Boolean queries, snippets and sorting on fast fields. Time-partitioned splits prune date-range queries well, and publication date is a natural timestamp. Ingest API, delete tasks, and a file-backed metastore on Blob (no database needed).
- **Azure PaaS usage:** Container Apps (managed, KEDA autoscaling, managed identity, managed OTel agent), Blob Storage (Hot), and Container Apps Jobs for indexing. There are no VMs and no Kubernetes to operate.
- **Cost:** Index on Blob Hot at about **$20 per TB-month**, plus 1 always-on searcher (4 vCPU / 8 GiB on Consumption; idle-rate billing between requests) at roughly **$100–300 per month**. Ingest compute is paid only while jobs run.
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
| A AI Search | ✅ | ⚠️ preview | ✅ | ✅ | ✅ | ❌ $2.8–5.6k | ✅✅ | ✅ | ✅ | Alternative |
| **B Quickwit/ACA/Blob** | ✅ | ✅ | ✅ | ✅ | ⚠️ validate | ✅ $0.1–0.4k | ✅ | ⚠️ OSS | ⚠️ | **Recommended** |
| C Cosmos DB | ⚠️ | ❌ | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅✅ | ✅ | ✅ | Future user data |
| D Elastic on Azure | ✅ | ✅ | ✅ | ✅ | ✅ | ❌/⚠️ | ✅ | ✅ | ✅ | Fallback |
| E PostgreSQL | ❌ | ✅ | ✅ | ⚠️ | ⚠️ | ⚠️ | ✅ | ✅ | ✅ | Rejected |
| F Embedded Tantivy | ✅ | ✅ | ✅ | ✅ | ✅ | ✅ | ❌ | ❌ | ⚠️ | Offline only |

**Decision gate (Spike S-2):** index the same **1M-page sample** into Quickwit (on Container Apps) and Azure AI Search (S1, since 1M pages is about 30 GB). Run the benchmark query set (§5.8). **Choose Quickwit if** its p95 for Q1 is ≤ 1.5 s on typical queries and ≤ 4 s on the high-frequency set, and it passes the correctness checks. **Otherwise** pick AI Search at L1 if funding is secured, or Elastic.

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
| `doc_id` | keyword | key | – | ✅ | Identity |
| `text` | text (positions) | ✅ analyzer `usnm_text` | – | ✅ (for snippets) | Search |
| `date` | date | ✅ | ✅ sort | ✅ | Range and sort; Quickwit timestamp field (if S-2 confirms pre-1970) |
| `day` | u32 | ✅ | ✅ | – | Days since 1700-01-01; day/week buckets; range filter |
| `ym` | u32 | ✅ | ✅ | – | `year*12 + (month-1)`; month buckets |
| `year` | u16 | ✅ | ✅ | – | Year buckets |
| `place_id` | keyword | ✅ | ✅ facet | ✅ | Map aggregation |
| `lccn` | keyword | ✅ | ✅ | ✅ | Title filter and hits |
| `state` | keyword | ✅ | ✅ | – | Filter and choropleth |
| `language` | keyword[] | ✅ | ✅ | – | Filter |
| `front_page` | bool | ✅ | ✅ | – | Filter |
| `edition`, `seq` | u16 | – | – | ✅ | Link building |
| `batch` | keyword | ✅ | – | – | Delete-by-batch on reprocessing |

**Analyzer `usnm_text`:** a Unicode word tokenizer, then lowercase, then ASCII folding, then removal of tokens longer than 40 characters (OCR garbage). **No stemming and no stop words.** Historical exactness matters, and stop words are needed for phrases such as "cross of gold".

### 5.5.1 Quickwit index config (sketch)

```yaml
version: 0.9
index_id: pages-v20261001-1
index_uri: azure://qw-index/pages-v20261001-1
doc_mapping:
  mode: strict
  timestamp_field: date          # validate pre-1970 in S-2; bucket fields don't depend on it
  tokenizers:
    - name: usnm_text
      type: simple            # unicode word split
      filters: [lower_caser, ascii_folding, remove_long]
  field_mappings:
    - { name: doc_id,     type: text,     tokenizer: raw, stored: true }
    - { name: text,       type: text,     tokenizer: usnm_text, record: position, stored: true, fieldnorms: false }
    - { name: date,       type: datetime, input_formats: ["%Y-%m-%d"], fast: true, stored: true }
    - { name: day,        type: u64,      fast: true, indexed: true }
    - { name: ym,         type: u64,      fast: true, indexed: true }
    - { name: year,       type: u64,      fast: true, indexed: true }
    - { name: place_id,   type: text,     tokenizer: raw, fast: true, stored: true }
    - { name: lccn,       type: text,     tokenizer: raw, fast: true, stored: true }
    - { name: state,      type: text,     tokenizer: raw, fast: true }
    - { name: language,   type: array<text>, tokenizer: raw, fast: true }
    - { name: front_page, type: bool,     fast: true, indexed: true }
    - { name: edition,    type: u64,      stored: true, indexed: false }
    - { name: seq,        type: u64,      stored: true, indexed: false }
    - { name: batch,      type: text,     tokenizer: raw }
indexing_settings:
  split_num_docs_target: 2000000
  commit_timeout_secs: 60
search_settings:
  default_search_fields: [text]
```

The exact syntax for custom tokenizers is to be confirmed against the Quickwit version pinned in S-2. `fieldnorms: false` saves space because we don't rank by BM25 length normalization.

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
| Fuzzy (OCR) | toggle "OCR-tolerant" | `Fuzzy(term, d=1 or 2)` | validate in S-2 | `term~1` |
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

**Queries issued for Q1** (one round-trip in Quickwit; two or three in parallel for AI Search):

1. `terms(place_id, size = #places) → histogram(bucket_field, interval)` gives the sparse cube `[place][bucket] = pages`.
2. `histogram(bucket_field)` over all hits gives the national series (this could also be derived from 1; computing it directly guards against truncation).
3. `terms(place_id) → min(day)` gives the first appearance per place.
4. `max_hits = 0`, so no documents are returned.

**Normalization** happens in the API from `reference/baselines_place_day` (pages *published* per place and day, rolled up to any bucket in memory):
`rel[place][bucket] = hits / baseline`, and nationally `rel[bucket] = Σhits / Σbaseline`. This replaces the legacy `globalFreq` table, stays correct as the corpus grows, and gives honest per-place rates.

**High-frequency guardrails.** Terms such as `the` match almost every page. Protections:

- (a) The response cache and CDN absorb repeats.
- (b) There is a hard server timeout of 10 s. On timeout the API returns `503` with a hint to narrow the date range or filters, and nothing expensive is retried.
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

## 5.9 Document storage summary

| Data | Store | Why |
|------|-------|-----|
| Raw LoC archives | Blob **Cool→Cold** | Immutable provenance; cheap |
| Curated page corpus | ADLS Gen2 **Hot**, Parquet | System of record; rebuilds indexes; enables offline analytics (DuckDB/DataFusion/Fabric) |
| Search index | Quickwit splits on Blob **Hot** (or AI Search managed storage) | Disposable, versioned |
| Reference data | Blob Hot (Parquet + zstd JSON), in API memory | Small; read-mostly |
| Operational/user data (future) | Cosmos DB serverless | Only if accounts or saved searches are added |
