# 04: Data Sources & Ingestion

## 4.1 Source landscape (as of September 2026)

The legacy crawler targeted `chroniclingamerica.loc.gov` JSON endpoints (`/batches/…json`, `/lccn/{sn}/{date}/ed-{n}.json`, `/…/ocr.txt`, `/lccn/{sn}.json`). **Those endpoints are gone.** On **4 August 2025** the Library of Congress moved Chronicling America onto loc.gov. The legacy domain now permanently redirects there, and the collection is reachable **only through the loc.gov API**. Since then LoC has launched a **Chronicling America Datasets portal** with batch data and **bulk OCR downloads for 3,000+ batches and 23M+ pages**. It is also re-OCRing pre-2012 content with its open-source **NDNP-Open-OCR** pipeline (Tesseract with layout detection; 425k+ pages reprocessed as of August 2026).

| Source | What we take | Access pattern | Notes |
|--------|--------------|----------------|-------|
| **Chronicling America Datasets portal** (`loc.gov/collections/chronicling-america/datasets`) | Batch list and versions; **bulk OCR text** per batch | Bulk download, rate-limited (historically 10 bulk requests per 10 min per IP) | **Primary corpus source.** One archive per batch is far cheaper for LoC and for us than per-page fetches. |
| **loc.gov JSON API** (`?fo=json`) | **Title metadata** (LCCN, title, place, county, state, language, ethnicity, dates); page `resource` URLs; small top-ups | Paginated JSON; requests limited; searches over 100k results must be split by facets | Used for reference data and for reconciling page URLs. Not used to pull text for the bulk corpus. |
| **NDNP-Open-OCR outputs** | Replacement OCR for reprocessed pages | Show up as new batch versions / dataset updates | Triggers re-ingestion of affected pages (§4.7). |
| **USGS GNIS Domestic Names** (public domain) + **Census TIGER** county/state shapes | Coordinates for places of publication; county centroids as fallback; state polygons | One-time download, refreshed yearly | Replaces the legacy Google Places geocoding and its committed key. |
| **Legacy `town_ref.csv`** | 1,795 LCCN → city/state/lat/lon rows | One-time | **Cross-check only** for geocoding QA. |
| *American Stories* (Dell et al., Harvard; Hugging Face) | Article-level segmentation of ~20M scans, 438M articles, 1774–1963 | Parquet download | **Phase 3** option for article-level results and reprint detection (F-33/F-34). |
| *Mirrors* (e.g. the `biglam/chronicling-america-bulk-ocr` bucket on Hugging Face) | Same bulk OCR | S3-style bulk | Optional **backfill accelerator** to reduce load on LoC. Check provenance and checksums against LoC manifests before use. |

### 4.1.1 Spike S-1 findings (September 2026)

- **The batch list is machine-readable.** The collection JSON (`https://www.loc.gov/collections/chronicling-america/?fo=json&c=1`) carries a `datasets` array: one entry per batch with `batch` (e.g. `vi_elgar_ver02`), `url` (`https://chroniclingamerica.loc.gov/data/ocr/{batch}.tar.bz2`), `sha256`, `size`, `page_count`, `issue_count` and `lccns`. `usnm-ingest enqueue` reads it directly (its default `--list`). The Datasets HTML page itself answers scripted clients with 403, so the JSON is the interface.
- **Size:** 2,997 batches, **23.7M pages**, **2.47 TB of `tar.bz2`** (largest 3.8 GB). Versions run `_ver01` (2,307) to `_ver08`.
- **Archive layout:** `{lccn}/{yyyy}/{mm}/{dd}/ed-{n}/seq-{n}/ocr.txt` plus ALTO `ocr.xml`, exactly the historical layout the reader expected. The XML is ~35× the text and is skipped, so most of the download is discarded.
- **Text volume:** a 1900s weekly batch (`vi_elgar_ver02`: 1,605 pages) averages **33.7 KB of text per page**; its curated Parquet is 3.2× smaller than the raw text. That fits the 450–800 GB raw / 150–300 GB curated estimate in §4.3.1.
- **Throughput:** the download ran at ~80 MB/s. Curation is bound by single-threaded bzip2 at **~3.9 MB/s of archive per worker** (54 s for 208 MB), so the full 2.47 TB is ~175 worker-hours: about 22 hours with 8 workers. Run one worker process per vCPU.
- **Scratch disk:** the worker downloads the whole archive before parsing (to verify the sha256 first), so its disk must hold the largest archive (3.8 GB).
- **Titles:** `https://www.loc.gov/item/{lccn}/?fo=json` returns the title record with `location_city/county/state`, **`latlong`**, `dates_of_publication`, `number_first_issue`/`number_last_issue` and `languages`. LoC's own coordinates can seed `geocode`, with GNIS as the cross-check.
- **Access:** everything above is anonymous HTTPS. A real archive (`dlc_zurich_ver04`, 58 KB) is checked into `crates/usnm-ingest/tests/data/` as a layout regression test.

**Good-citizen policy:** identify ourselves with a descriptive User-Agent and contact address; prefer bulk over per-page requests; schedule off-peak; back off exponentially on 429/5xx; never parallelize beyond published limits; cache everything in our lake so we fetch each object **once**.

## 4.2 Canonical identifiers

- **Page key** (human-readable and stable): `{lccn}/{yyyy-mm-dd}/ed-{n}/seq-{n}`, e.g. `sn84026749/1896-07-10/ed-1/seq-1`. This is the same tuple the legacy code used (`sn, date, ed, seq`) and the one LoC uses for resource paths.
- **Doc id** (engine key): the page key itself, with `/` replaced by `_` (e.g. `sn84026749_1896-07-10_ed-1_seq-1`). It is **collision-free by construction**, reversible, and uses only characters valid in AI Search keys (letters, digits, `_`, `-`). A 64-bit hash was rejected: at 23M pages the chance of at least one collision is ~1.4×10⁻⁵, and a collision would silently merge two pages and break exact counts.
- **Place id**: `P` + a stable integer assigned once and never reused (e.g. `P00412`). Ids stay stable across reference snapshots; the registry is carried forward from one `reference/{index_version}/places` snapshot to the next. One place can serve many titles, since cities had several papers.
- **Index version**: `pages-v{YYYYMMDD}-{n}`. It is recorded in `reference/current.json` and appears in every API ETag.

## 4.3 Data lake layout (Blob Storage, flat namespace, one storage account)

```
raw/                                   (lean profile: NOT retained; LoC is the source of record)
  loc-api/titles/{date}/…json.zst       raw title metadata snapshots only (small)
curated/                               (Cool; the system of record; versioned + soft delete)
  pages/{batch}/v{NN}/{attempt}/part-{nnnn}.parquet
                                        written once, never overwritten; the Cosmos `batches` item
                                        points at the committed attempt (the atomic "commit")
  pages/{batch}/v{NN}/{attempt}/counts.json
                                        pages per (lccn, day), all text statuses: baselines are
                                        built from these without re-reading the parts
reference/                             (Hot; small; loaded by API; IMMUTABLE per index version)
  catalog/titles.json, places.json      the working catalog (titles-sync + geocode; hand-written
                                        until those land). Titles and places carry stable ordinals
  {index_version}/                      = pages-v{YYYYMMDD}-{n}
    titles.json  places.json            the catalog as published with this version (only titles
                                        and places that have pages in it)
    baselines.json                      pages published per place: {place_id: [[day, pages], …]}
    manifest.json                       { index_version, files: [{path, sha256, bytes}], built_from: {batches} }
  raw/titles.json                       the fields titles-sync keeps from each LoC title record
  current.json                          { index_version, backend, indexes: [base, delta…] (sealed), reference: "{index_version}", bounds, previous_version, published_at, synthetic }
(document state, meaning per-title, per-batch, per-issue and per-index-run status, lives in Cosmos DB, not here; see 05 §5.9.1)
cache/{index_version}/                 (Hot) persistent API response cache, zstd JSON keyed by canonical-query hash
qw-index/                              (Hot; Quickwit splits + file-backed metastore)
```

### 4.3.1 Curated page schema (Parquet, zstd)

| Column | Type | Notes |
|--------|------|-------|
| `doc_id` | string | `page_key` with `/`→`_` (unique) |
| `page_key` | string | `lccn/date/ed-n/seq-n` |
| `lccn` | string | |
| `date` | date32 | Publication date. Pre-1970 dates are normal; ~1756 onward. |
| `year`, `month` | int16, int8 | Denormalized for partition pruning |
| `edition`, `seq` | int16 | |
| `front_page` | bool | `seq == 1` |

| `batch`, `batch_version` | string, int16 | Provenance |
| `ocr_source` | enum | `ndnp-original` \| `ndnp-open-ocr` |
| `text_status` | enum | `ok` \| `short` \| `empty` (§4.5) |
| `text` | large_string (nullable) | Normalized OCR text (§4.5); null unless `text_status = ok` |
| `text_chars`, `word_count` | int32 | QA and baselines |
| `text_sha256` | fixed_binary(32) | Change detection |
| `resource_url` | string | Canonical loc.gov page URL |
| `ingested_at` | timestamp | |

Curated rows hold only **immutable page facts**. Title-derived attributes (`place_id`, `state`, `language`) are **not stored here**. The `index` and `stats` jobs join them from the versioned titles snapshot, so a corrected title record never leaves stale copies in the system of record.

**Size estimate.** The legacy sample of 1879 *Daily Review* (Towanda, PA) pages averages about **10.6 KB** of OCR text per page. Later eight-column broadsheets carry 2–5× more. We plan for **20–35 KB per page average × ~23M pages ≈ 450–800 GB of raw text**, or about **150–300 GB as zstd Parquet**. This is validated in Spike S-1 ([10](10-roadmap-and-risks.md)).

## 4.4 Pipeline

All stages are subcommands of one Rust binary (`usnm-ingest`, in `crates/usnm-ingest`), packaged as one container image (`Dockerfile.ingest`: the binary on the pinned Quickwit image).

**Implemented so far:** `enqueue` (from LoC's collection JSON `datasets` listing by default, or any batch list), `curate` (the `batch` stage), and `release` (the `index` stage, with the reference snapshot built from each batch's `counts.json`; `stats` is folded in). `run` chains all three for the weekly job. State lives in Cosmos over its REST API, or in a local JSON file for development. `titles-sync` and `geocode` build the catalog from LoC's title records (§4.6), and `run` calls `titles-sync` before releasing, so every curated title has a catalog entry. **Still to come:** garbage collection of uncommitted attempts and unpublished indexes (the janitor), and the GNIS/Census cross-check in `geocode`. The ingest and backfill Container Apps Jobs are in `infra/modules/ingestjobs.bicep` (08 §8.4). `scripts/fixture-batches.py` turns the synthetic fixtures into batch archives, and CI checks that the pipeline reproduces the fixture indexes and reference snapshot exactly. **Weekly incremental** work runs as **Azure Container Apps Jobs**. The **initial backfill** runs the same image as a manual Container Apps Job with N parallel curation replicas, inside the VNet (see [08 §8.4](08-azure-infrastructure.md#84-compute-sizing-notes); ACI Spot remains a documented alternative). Work is split into **one Cosmos `batches` item per batch version** (about 3,000 batches). Workers claim items with lease patches, so work parallelizes naturally and retries are idempotent. Cosmos is the work queue; there is no Storage Queue.

```mermaid
flowchart LR
  D[discover<br/>scheduled weekly] -->|status=queued| Q[(Cosmos batches)]
  Q -->|lease claim| B[batch worker ×N<br/>Jobs or ACI Spot]
  B -. streamed, not retained .-> RAW[(LoC bulk archive)]
  B --> CUR[(curated/pages)]
  B --> MAN[(Cosmos DB: document state)]
  T[titles-sync<br/>weekly] --> REFT[(Cosmos titles + reference/v/titles)]
  G[geocode<br/>on titles change] --> REFP[(reference/v/places)]
  CUR --> S[stats<br/>baselines + coverage] --> REFB[(reference/v/baselines, coverage)]
  CUR --> I[index<br/>incremental or full] --> SE[(search engine)]
  I --> CURJ[(reference/current.json)]
```

| Stage | Trigger | Does | Idempotency |
|-------|---------|------|-------------|
| `discover` (`enqueue`) | Cron (weekly) + manual | Reads the batch list from LoC's collection JSON `datasets` array (§4.1.1), or any batch list given with `--list`; upserts `batches` items in Cosmos and marks new or changed versions `queued`. Never downgrades a version | Only marks `queued` if the status ≠ curated for that version; failed batches are re-queued |
| `batch` | A `queued` batch is available to claim | Streams the bulk OCR for the batch (checksum verified; not retained); parses per-page text; normalizes; writes curated Parquet (~256 MB row groups); updates the manifest | Claims the batch with a conditional Cosmos patch (lease + ETag); writes to a **new attempt path** (never overwriting); then **upserts the issue items first** (idempotent, keyed by lccn/date/edition). The **final commit marker** is one conditional patch on the batch item, made only while the worker still holds the lease: `curated_path` → that attempt, plus sha256, pages and `status=curated`. If anything fails before that patch, the batch stays un-curated, its lease expires, and it's retried; `discover` also re-queues any batch not `curated`. Re-upserting issues is harmless. Uncommitted attempts are garbage-collected after 7 days |
| `titles-sync` | Each `run` (weekly), before release; manual | Fetches `loc.gov/item/{lccn}/?fo=json` for titles with pages that aren't cached or catalogued yet; caches the fields used in `reference/raw/titles.json`; then runs `geocode` | Paced under LoC's 20 requests/minute limit; stops on a 429/CAPTCHA; failures are retried next run; the cache is saved as it goes |
| `geocode` | After `titles-sync`; manual (e.g. after editing overrides) | Resolves each title's place of publication (§4.6): override, else LoC's `latlong`, else state centroid; writes `catalog/places.json` then `catalog/titles.json`, only if they changed | Deterministic, no network; stable ordinals and ids |
| `stats` | After batches are curated | Aggregates baselines (pages per place/day and per title/month) and coverage (state/year) from curated Parquet joined with the titles snapshot, using DataFusion or Polars. Writes a **new** `reference/{index_version}/` snapshot (never overwrites) | Full recompute (cheap: one scan of the ids and dates columns) |
| `index` (`release`) | After curation, or manual | Takes the Cosmos `ops/quickwit-writer` lease (the single metastore writer). Runs the Quickwit writer node as a child process for the length of the release. **Incremental:** writes newly curated rows with `text_status = ok` into a **new delta index**. **Compaction/full:** writes all rows into a **new base index**. Both join the titles snapshot for `place_id`/`state`/`language` and compute `place_shard`. Never writes an index a published version lists. Publishes by writing `current.json` last, naming the index set and its reference snapshot | Deterministic `doc_id` (the page key); each new index is built from scratch, so a retry discards the unpublished index and starts again |

**Throughput plan for the initial backfill.** 23M pages at a conservative 2,000 pages/s per indexer is about 3.2 hours of pure indexing per indexer. In practice LoC download politeness dominates (days, not hours), which is why the curated lake matters: **after the first backfill we never re-download to re-index.** If the mirror is validated, the backfill can pull from it instead. S-1 measured curation at ~175 worker-hours for the whole corpus (§4.1.1): about 22 hours with 8 Container Apps Job replicas, ~$15–20. A worker that dies only delays the work, because each batch is idempotent and becomes claimable again when its lease expires. The jobs run inside the VNet, so the data services stay private throughout.

## 4.5 Text normalization (applied at curation time)

1. Unicode NFC; strip control characters; turn the long s `ſ` into `s`; normalize ligatures (`ﬁ` → `fi`).
2. **Rejoin line-break hyphenation**: `indus-\ntry` → `industry`, but only when the joined token is alphabetic. This substantially improves phrase recall on OCR text.
3. Collapse runs of whitespace; keep paragraph breaks as `\n`.
4. **Keep every digitized page as a curated row**, even when the normalized text is empty or shorter than 20 characters. Such rows get `text_status = empty | short` and a null `text`. They are **excluded from the search index only**; baselines and coverage count them, so relative frequencies and the "no digitized newspapers" layer stay correct. (The legacy code dropped them entirely.)
5. Keep the text case-sensitive in storage; lowercasing and ASCII folding happen in the engine's analyzer.

## 4.6 Geography

- **Unit:** a title's **place of publication**, as recorded by LoC (city, county, state). All pages of a title inherit its place. Titles that moved have date ranges for their place in `titles.places[]`; when there are more than one, the page's date selects the right one.
- **Resolution order (implemented):** (1) manual override (`catalog/overrides/places.json` in git, compiled into the ingest image: `[{city, state, lat, lon, precision?}]`) → (2) LoC's own `latlong` on the title record (precision `city`) → (3) state centroid (precision `state`). Each place stores `precision ∈ {city, county, state}` and the UI shows lower precision with a hollow marker.
  - A title's place is the city in its LoC title, e.g. "(North Platte, Neb.)", in the state LoC records; when a title lists several states, the one its place label names wins. Titles in the same city and state share one place, at the coordinates of the first of them (by LCCN). The label is kept on the title as `place_of_publication`, so historical names ("Dakota Territory") survive.
  - `titles-sync` caches the fields it uses from each record in `reference/raw/titles.json`, fetching only titles that are neither cached nor in the catalog (`--refresh` re-fetches). Requests go one at a time, every 3.5 s: loc.gov's JSON API allows 20 a minute and blocks for an hour beyond that. A 429 or CAPTCHA page stops the run early, and the next run continues from the cache. The first sync of ~4,400 titles takes about 4.5 hours; weekly runs fetch only new titles. `geocode` rebuilds the catalog from that cache with no network. Ordinals and place ids are stable, and nothing is ever removed.
  - **Planned refinement:** GNIS populated places (normalized name + state, county to break ties) and Census county centroids as a cross-check on LoC's coordinates and a better fallback than the state centroid.
- **QA:** diff against legacy `town_ref.csv`, flag any place that is more than 25 km from the legacy coordinates for review, and have CI check that every title with pages resolves to a place.
- **Historical context:** territories (e.g. Dakota Territory, Indian Territory) keep their historical label in `titles`. Historical state and territory boundary overlays are F-32 (Phase 3).

## 4.7 Updates, reprocessing and re-indexing

| Change | Detection | Action |
|--------|-----------|--------|
| **New batch** | `discover` finds an unseen batch | Curate → `stats` builds the new `reference/{index_version}/` snapshot → `index` writes the new pages into a **new sealed delta index** → **publish last**: write `current.json` listing base + deltas only when both the index set and its reference snapshot are complete |
| **New batch version** (e.g. `_ver02`, or NDNP-Open-OCR reprocessing) | Version increment or dataset update | Curate the new version and mark the batch `pending_replacement` in Cosmos. Published indexes are sealed, so the replacement takes effect at the next **compaction** (a full rebuild into a new base index, monthly or sooner), which publishes a new `index_version`. Until then the previous OCR keeps serving. |
| **Title metadata change** (e.g. corrected place) | `titles-sync` diff (recorded on the Cosmos `titles` item) | Record the change in Cosmos as pending. At the next **compaction**, the new titles snapshot drives both the rebuilt base index (via the join) and the matching reference snapshot, published together as one `index_version`. No re-curation is needed because curated rows carry no title-derived fields. |
| **Analyzer or schema change** | Code change | **Full rebuild** (compaction) into a new base index from curated Parquet, then flip `current.json` atomically. Indexes listed by the previous version are kept for 7 days for rollback. |

The API re-reads `reference/current.json` every 10 minutes and loads the reference snapshot it names, verifying checksums. Snapshots are immutable and kept for at least the current and previous versions (7 days minimum), so **rolling back `current.json` restores the exact index + reference pair** that was published together. Because `index_version` is part of every cache key, caches never mix versions.
