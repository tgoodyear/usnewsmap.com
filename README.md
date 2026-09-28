# usnewsmap.com

Revival of **US News Map**: search two centuries of American newspapers from the Library of Congress's [Chronicling America](https://www.loc.gov/collections/chronicling-america/) collection and watch where and when words and stories spread across the country.

The original site (2015–2016, Georgia Tech Research Institute and eHistory.org at the University of Georgia) is preserved at [`tgoodyear/usnewsmap`](https://github.com/tgoodyear/usnewsmap).

## Design

Start with the **[design document](docs/design/README.md)**: background, requirements, architecture, data and ingestion, search and storage, API, frontend, Azure infrastructure, operations and cost, the roadmap, and the [architecture decision records](docs/design/adr/README.md).

## Repository layout

| Path | What |
|------|------|
| `crates/usnm-core` | Domain types: page keys and doc ids, day/bucket math, the query language parser, canonical request parameters, text normalization, the sparse cube |
| `crates/usnm-search` | `SearchBackend` trait, the Quickwit translator and client, an in-memory reference backend, and the sharded aggregate planner |
| `crates/usnm-store` | Object storage: Azure Blob over REST with managed identity (Entra ID only), or a local directory with the same layout |
| `crates/usnm-api` | The public search API (axum): `/v1/meta`, `/v1/places`, `/v1/aggregate`, `/v1/hits`, `/v1/coverage`, health probes |
| `crates/usnm-ingest` | The ingest pipeline (`usnm-ingest`): enqueue LoC batches, curate them to Parquet, build sealed indexes and reference snapshots, publish. State in Cosmos DB |
| `web/` | The single-page app: React + MapLibre + deck.gl (see [`web/README.md`](web/README.md)) |
| `infra/` | Bicep + `azd` for the lean Azure profile (see [`infra/README.md`](infra/README.md)) |
| `fixtures/` | A small **synthetic** corpus for local development and tests (not real newspaper data) |
| `docs/design/` | The design document set |

Not yet built: `titles-sync` and `geocode`, and the ingest jobs in Azure (see [04 §4.4](docs/design/04-data-sources-and-ingestion.md#44-pipeline)). Until the first index is published, the infrastructure runs the API on synthetic fixtures (`USNM_SEARCH_BACKEND=fixtures`); the Quickwit sidecar is ready behind `USNM_SEARCH_BACKEND=quickwit`.

## Local development

Requires a stable Rust toolchain (see `rust-toolchain.toml`).

```sh
cargo test --workspace                 # unit + API integration tests against the fixtures
cargo run -p usnm-api                  # serves the synthetic fixture corpus on :8080
curl 'localhost:8080/v1/aggregate?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31'
curl 'localhost:8080/v1/hits?q=%22cross+of+gold%22&place=P00001&limit=5'
```

### Against Quickwit

The same fixtures can be served by [Quickwit 0.9.1](https://github.com/quickwit-oss/quickwit/releases/tag/v0.9.1), the pinned version, set up the way production runs it. `scripts/quickwit-fixtures.sh` loads the fixture indexes with a writer node, stops it, and starts a read-only searcher that polls the metastore:

```sh
url=$(QUICKWIT_BIN=/path/to/quickwit scripts/quickwit-fixtures.sh /tmp/qw)
QUICKWIT_URL=$url cargo test -p usnm-search --test quickwit_parity   # Quickwit vs the reference backend
USNM_BACKEND=quickwit cargo run -p usnm-api
kill "$(cat /tmp/qw/quickwit.pid)"
```

Without `QUICKWIT_URL`, the parity tests skip; CI runs them in the `quickwit` job. Quickwit has no fuzzy term queries, so on this backend `/v1/meta` reports `"fuzzy": false` and fuzzy queries return 422 (05 §5.5.1).

### Ingest pipeline

`usnm-ingest` turns LoC batch archives into a published version: `enqueue` records a batch list, `curate` claims queued batches and writes curated Parquet, and `release` builds a new index (a delta, or a base with `--full`) and its reference snapshot, then writes `current.json` last. `run` does all three. To try it on the synthetic corpus, rebuilt as two batch archives:

```sh
python3 scripts/fixture-batches.py /tmp/usnm-ingest          # archives, batches.json, reference/catalog/
cd /tmp/usnm-ingest
cargo run --manifest-path ~/src/usnewsmap.com/Cargo.toml -p usnm-ingest -- \
  --state-file state.json --curated curated --reference reference \
  run --list batches.json --synthetic --index-dir reference/indexes
USNM_DATA_DIR=/tmp/usnm-ingest/reference cargo run --manifest-path ~/src/usnewsmap.com/Cargo.toml -p usnm-api
```

(Adjust `~/src/usnewsmap.com` to your checkout.) To index into Quickwit instead, replace `--index-dir …` with `--quickwit-bin /path/to/quickwit --quickwit-metastore file:///tmp/usnm-ingest/qw --quickwit-index-root file:///tmp/usnm-ingest/qw`; the pipeline runs its own writer node for the release.

In Azure the same binary runs from the `usnewsmap-ingest` image with `--cosmos https://{account}.documents.azure.com/`, Blob URLs for `--curated` and `--reference`, and `--quickwit-metastore azure://qw-index --quickwit-index-root azure://qw-index`. The store, state and index-target options also read environment variables (`USNM_CURATED_URL`, `USNM_REFERENCE_URL`, `USNM_COSMOS_ENDPOINT`, `USNM_QUICKWIT_*`, …; see `usnm-ingest --help`). `enqueue` with no `--list` reads LoC's own batch listing; add `--batches name_ver01,…` to take only some.

### Configuration (environment variables)

| Variable | Default | Meaning |
|----------|---------|---------|
| `USNM_BIND` | `0.0.0.0:8080` | Listen address |
| `USNM_BACKEND` | `memory` | `memory` (JSONL indexes under `USNM_DATA_DIR/indexes`) or `quickwit` |
| `USNM_QUICKWIT_URL` | `http://127.0.0.1:7280` | Quickwit base URL (the localhost sidecar in production) |
| `USNM_DATA_DIR` | `fixtures/data` | Local data directory: the memory backend's `indexes/`, and the default reference location |
| `USNM_REFERENCE_URL` | `USNM_DATA_DIR` | Where `current.json` and the reference snapshots live: a Blob container URL (`https://{account}.blob.core.windows.net/reference`) or a local directory. Every file is checked against the snapshot's `manifest.json` |
| `USNM_RESPONSE_CACHE_URL` | unset | Persistent response cache (`https://{account}.blob.core.windows.net/cache` or a directory). Entries are `{index_version}/f{format}/{sha256}.json.zst` |
| `USNM_PERSIST_AFTER_MS` | `500` | Only responses that took at least this long to compute are persisted |
| `USNM_ALLOWED_ORIGINS` | `https://usnewsmap.com` | Comma-separated CORS origins (GET only) |
| `USNM_SEARCH_TIMEOUT_SECS` | `10` | Backend timeout; exceeded → `503` problem |
| `USNM_REFRESH_SECS` | `600` | How often `current.json` is re-read for a newly published version |
| `USNM_CACHE_MB` | `256` | In-process response cache size |
| `USNM_RATE_PER_MIN` | `120` | Per-client token bucket refill rate on `/v1` (`0` disables) |
| `USNM_RATE_BURST` | `40` | Per-client bucket size |
| `USNM_TRUSTED_PROXY_HOPS` | `1` | Proxies that append to `X-Forwarded-For` (1 = Container Apps ingress, 2 = a proxy such as Front Door + ingress, 0 = use the peer address) |
| `USNM_BACKEND_CONCURRENCY` | `8` | Requests that may query the search backend at once; waiting counts against the timeout |
| `IDENTITY_ENDPOINT`, `IDENTITY_HEADER`, `AZURE_CLIENT_ID` | set by Container Apps | Managed identity for Blob; `AZURE_CLIENT_ID` selects the user-assigned identity. Without them, the Azure CLI login is used (`az login`) |
| `RUST_LOG` | `info` | Log filter (logs are JSON and never include query strings) |

## License

Apache-2.0 (to be confirmed by the project owner).
