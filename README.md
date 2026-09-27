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
| `crates/usnm-api` | The public search API (axum): `/v1/meta`, `/v1/places`, `/v1/aggregate`, `/v1/hits`, `/v1/coverage`, health probes |
| `fixtures/` | A small **synthetic** corpus for local development and tests (not real newspaper data) |
| `docs/design/` | The design document set |

Not yet built: the web app, the ingest pipeline, and the Bicep infrastructure (see the [roadmap](docs/design/10-roadmap-and-risks.md)).

## Local development

Requires a stable Rust toolchain (see `rust-toolchain.toml`).

```sh
cargo test --workspace                 # unit + API integration tests against the fixtures
cargo run -p usnm-api                  # serves the synthetic fixture corpus on :8080
curl 'localhost:8080/v1/aggregate?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31'
curl 'localhost:8080/v1/hits?q=%22cross+of+gold%22&place=P00001&limit=5'
```

### Configuration (environment variables)

| Variable | Default | Meaning |
|----------|---------|---------|
| `USNM_BIND` | `0.0.0.0:8080` | Listen address |
| `USNM_BACKEND` | `memory` | `memory` (JSONL indexes under `USNM_DATA_DIR/indexes`) or `quickwit` |
| `USNM_QUICKWIT_URL` | `http://127.0.0.1:7280` | Quickwit base URL (the localhost sidecar in production) |
| `USNM_DATA_DIR` | `fixtures/data` | Directory with `current.json` and the reference snapshot |
| `USNM_ALLOWED_ORIGINS` | `https://usnewsmap.com` | Comma-separated CORS origins (GET only) |
| `USNM_SEARCH_TIMEOUT_SECS` | `10` | Backend timeout; exceeded → `503` problem |
| `USNM_REFRESH_SECS` | `600` | How often `current.json` is re-read for a newly published version |
| `USNM_CACHE_MB` | `256` | In-process response cache size |
| `RUST_LOG` | `info` | Log filter (logs are JSON and never include query strings) |

## License

Apache-2.0 (to be confirmed by the project owner).
