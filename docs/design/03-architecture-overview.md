# 03: Architecture Overview

## 3.1 Architectural drivers

Ranked in order. Where drivers conflict, the higher one wins.

1. **Sustainability.** Low fixed cost, no servers to patch, reproducible from code. The system must not die again for lack of support.
2. **Correctness.** Complete aggregates, honest normalization, stable citations.
3. **Performance.** The map should feel instant, with smooth playback on commodity laptops and phones.
4. **Lean on Azure PaaS.** Use managed services for storage, compute, edge, identity and observability.
5. **Evolvability.** The search engine can be swapped, the article-level corpus added later, and semantic search layered on.

## 3.2 System context

```mermaid
flowchart TB
  U[Researchers · Public · Librarians · Genealogists]
  E[Embedding sites<br/>blogs, LibGuides, news]
  R[Researcher scripts<br/>public API]
  subgraph USNM["US News Map (Azure)"]
    W[Web app]
    API[Search API]
    ING[Ingest pipeline]
  end
  LOC[(Library of Congress<br/>Chronicling America<br/>loc.gov API · Datasets portal)]
  LOCV[LoC page viewer / IIIF / PDF]
  U --> W
  E -->|iframe| W
  R --> API
  W --> API
  ING -->|bulk OCR + metadata| LOC
  W -->|deep links| LOCV
```

## 3.3 Container view (target, lean profile)

```mermaid
flowchart LR
  subgraph Web
    SWA[Azure Static Web Apps · Free<br/>usnewsmap.com · TLS · global static CDN<br/>React + TypeScript SPA · MapLibre GL + deck.gl]
  end
  subgraph "Azure Container Apps environment (Consumption)"
    subgraph APP["ca-usnm · api.usnewsmap.com · min 1 / max 2 replicas"]
      API[api container<br/>Rust · axum]
      QW[quickwit container<br/>searcher + metastore<br/>localhost only]
    end
    JOBS[[Container Apps Jobs<br/>weekly: discover · batch · stats<br/>incremental index · prewarm]]
  end
  SPOT[[ACI Spot container groups · preview<br/>backfill + full re-index only<br/>created on demand, deleted after]]
  subgraph Storage["Azure Storage (ADLS Gen2, LRS)"]
    CUR[(curated/<br/>pages Parquet · Cool)]
    REF[(reference/<br/>titles · places · baselines · coverage)]
    IDX[(qw-index/<br/>Quickwit splits + metastore · Hot)]
    CACHE[(cache/<br/>persistent response cache)]
    Q[[Storage Queues<br/>ingest work items]]
  end
  TILES[(Blob: PMTiles basemap)]
  COS[(Cosmos DB · free tier<br/>document state: titles · batches<br/>issues · index_runs)]
  AI[Application Insights<br/>+ Log Analytics]

  SWA -. SPA calls GET /v1/* .-> API
  SWA -. map tiles .-> TILES
  API -->|localhost ES-compatible REST| QW
  API -->|startup load| REF
  API <--> CACHE
  QW --> IDX
  JOBS --> CUR & REF & Q
  JOBS <--> COS
  SPOT --> CUR & IDX
  SPOT <--> COS
  Q --> SPOT
  JOBS -->|ingest API| QW
  APP & JOBS --> AI
```

The **growth profile** adds Azure Front Door in front of both origins (edge cache, WAF), splits the API and search into separate apps with a 4 vCPU / 8 GiB searcher, and can swap Quickwit for Azure AI Search. All of these are Bicep parameters ([ADR-0006](adr/0006-lean-hosting-profile.md)).

### 3.3.1 Component responsibilities

| Component | Tech | Responsibility | Scales by |
|-----------|------|----------------|-----------|
| **Static Web Apps** | SWA Free | Serves the SPA at `usnewsmap.com` with TLS and global static distribution; route rules (SPA fallback, `/loc_api/*` → 410); PR preview environments | Managed |
| **Web app** | React 19 + TypeScript + Vite; MapLibre GL JS; deck.gl; uPlot; TanStack Query | UI, URL state, **client-side playback** from aggregate cubes, accessibility views | Static |
| **Search API** (`api` container) | Rust, axum/tokio, reqwest, moka | Parse and validate queries → backend DSL; run aggregate, hit and snippet queries; join with reference data; normalize; encode responses; **three cache layers** (browser, in-process, Blob); rate limiting; OpenAPI | Replicas (max 2) |
| **Search engine** (`quickwit` sidecar) | **Quickwit** (Rust, Apache-2.0); Azure AI Search in the growth profile | Inverted index with positions; phrase, proximity and fuzzy; nested *terms × histogram* aggregations; snippets | With its replica; splits on Blob |
| **Reference data** | Parquet/JSON artifacts on Blob | Titles (LCCN → name, place, dates, language), places (lat/lon, county, state), baselines (pages per place per day), coverage | Loaded into API memory (tens of MB) |
| **Weekly ingest** | Rust CLI in Container Apps Jobs; Storage Queues | Discover new LoC batches; curate; build reference data; incremental index; pre-warm | Queue length |
| **Backfill / re-index** | Same Rust image on **ACI Spot container groups** (preview) | Initial download and curation of ~3,000 batches; full index builds | Launcher creates N groups |
| **Data lake** | ADLS Gen2 (Blob) | System of record for the corpus and all derived artifacts | Managed |
| **Document state** | Cosmos DB for NoSQL, free tier | Per-title, per-batch, per-issue and per-index-run status; work leases; change feed driving incremental indexing. Not on the API request path | Free tier (1,000 RU/s) |
| **Observability** | App Insights via the managed OpenTelemetry agent; Log Analytics (free tier with daily cap) | Traces, metrics, logs, availability tests, alerts | Managed |

## 3.4 Key runtime flows

### 3.4.1 Search → map → playback (the main flow)

```mermaid
sequenceDiagram
  autonumber
  participant B as Browser
  participant API as Rust API
  participant SE as Search engine
  B->>API: GET api.usnewsmap.com/v1/aggregate?q="cross of gold"&from=1896-06-01&to=1896-12-31&bucket=week
  API->>API: parse → AST → validate → canonicalize (cache key)
  alt in-process or Blob cache hit
    API-->>B: 200 (cached, ~20–150 ms)
  else miss
    API->>SE: query + aggs: terms(place_id) → histogram(bucket) · histogram(bucket) · min(day) per place
    SE-->>API: nested buckets
    API->>API: join places / baselines, compute relative freq, first-appearance, build sparse cube
    API->>API: store in moka + Blob cache/{index_version}/
    API-->>B: 200 application/json (or ?format=arrow), Cache-Control: public, max-age=86400, ETag=index_version+query
  end
  B->>B: build per-bucket prefix sums, then compute playback, cumulative and trailing windows locally at 60 fps
  B->>API: GET /v1/hits?q=...&place=P123&cursor=...  (on marker click)
  API->>SE: (cache miss) top-N by date with highlights
  API-->>B: page list with KWIC snippets + LoC links
```

**Why this fixes the legacy problems:** there is one request per search instead of one per animation frame; there is no server session; responses can be cached by URL; and the counts are complete.

### 3.4.2 Ingestion (weekly and on demand)

```mermaid
sequenceDiagram
  autonumber
  participant CRON as Scheduled job: ingest-discover
  participant LOC as LoC Datasets / loc.gov API
  participant Q as Storage Queue
  participant W as ingest-batch workers (KEDA)
  participant L as Data lake (Blob)
  participant IX as index-build job
  participant SE as Search engine
  CRON->>LOC: list batches (+ versions) and OCR bulk files
  CRON->>L: upsert batch state in Cosmos DB
  CRON->>Q: enqueue new/changed batches
  W->>Q: dequeue batch
  W->>LOC: download bulk OCR (rate-limited, polite UA)
  W->>L: stream archive (not retained) → write curated/pages/batch=…/part.parquet
  W->>L: patch Cosmos batch + issue state (status=curated)
  IX->>L: read curated partitions changed since the last index version
  IX->>SE: ingest docs (Quickwit ingest API / AI Search push)
  IX->>L: rebuild reference/ (baselines, coverage) → publish new index_version
  Note over SE,L: The API reads index_version and reference data on startup and every 10 minutes
```

## 3.5 Cross-cutting design decisions

| Decision | Choice | ADR |
|----------|--------|-----|
| Search engine | Quickwit on Container Apps + Blob (recommended, the only option within budget); Azure AI Search (managed alternative, growth profile); both behind the Rust `SearchBackend` trait | [0001](adr/0001-search-engine.md) |
| System of record | Curated Parquet in ADLS Gen2; indexes are disposable | [0002](adr/0002-blob-data-lake-system-of-record.md) |
| Document state | Cosmos DB free tier (titles, batches, issues, index runs) | [0007](adr/0007-cosmos-document-state.md) |
| API shape | Stateless, GET, aggregate-first, CDN-cacheable; client-side temporal playback | [0003](adr/0003-stateless-aggregate-first-api.md) |
| API language | Rust (axum) | [0004](adr/0004-rust-api.md) |
| Operating constraints | < $80/mo, no VMs, IaC, single-maintainer operable | [0005](adr/0005-sustainability-constraints.md) |
| Hosting profile | No Front Door; SWA Free; one container app (API + Quickwit sidecar); ACI Spot for backfill | [0006](adr/0006-lean-hosting-profile.md) |
| Unit of retrieval | **Page** at launch (matches LoC URLs and the legacy); article level later (American Stories) | [04](04-data-sources-and-ingestion.md) |
| Geography | Place = title's place of publication (point), resolved from LoC metadata and GNIS; state and county from the same source | [04](04-data-sources-and-ingestion.md) |
| Cache invalidation | `index_version` is included in every ETag and cache key (browser, in-process, Blob `cache/{index_version}/`); a new index version means new URLs and a new cache prefix | [06](06-api-design.md) |
| Identity | Managed identities everywhere; no connection strings in app config | [08](08-azure-infrastructure.md) |

## 3.6 Repository layout (proposed for `usnewsmap.com`)

```
usnewsmap.com/
├─ docs/design/                 ← this document set
├─ web/                         ← React + TS SPA (Vite)
├─ crates/
│  ├─ usnm-core/               ← domain types, query AST/parser, normalization, cube encoding
│  ├─ usnm-search/             ← SearchBackend trait + quickwit + azure_ai_search adapters
│  ├─ usnm-api/                ← axum service (bin)
│  ├─ usnm-ingest/             ← ingest CLI: discover, batch, refdata, index (bin)
│  └─ usnm-refdata/            ← titles/places/baselines builders, gazetteer matching
├─ search/
│  ├─ quickwit/                ← index config YAML, node config
│  └─ azure-ai-search/         ← index JSON schema, analyzers
├─ infra/                      ← Bicep modules + azd environment files
├─ .github/workflows/          ← CI (fmt, clippy, test, build, scan), CD (azd deploy)
└─ ops/runbooks/               ← reindex, restore, incident, cost-spike
```
