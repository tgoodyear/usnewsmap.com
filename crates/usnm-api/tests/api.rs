//! End-to-end tests of the API against the synthetic fixture corpus.

use std::collections::HashMap;
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::{app, reload_if_changed, AppState, Engine, Loader};
use usnm_core::query::parse;
use usnm_core::text::tokenize;
use usnm_core::time::day_number;
use usnm_search::memory::{eval, MemoryBackend};
use usnm_search::PageDoc;
use usnm_store::LocalStore;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

fn load_docs(index_id: &str) -> Vec<PageDoc> {
    let path = data_dir().join("indexes").join(format!("{index_id}.jsonl"));
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.cache_bytes = 16 * 1024 * 1024;
    // Tests without a client address share one bucket; rate limiting has its own test.
    c.rate_limit = None;
    c
}

async fn refdata() -> RefData {
    RefData::load(&LocalStore::new(data_dir())).await.unwrap()
}

fn fixture_backend() -> MemoryBackend {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        backend.add_index(id, load_docs(id));
    }
    backend
}

async fn state_with_cells(max_cells: usize) -> Arc<AppState> {
    let mut cfg = config();
    cfg.max_cells = max_cells;
    Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ))
}

async fn state_with(indexes: Option<Vec<String>>) -> Arc<AppState> {
    let backend = fixture_backend();
    let mut refdata = refdata().await;
    if let Some(ids) = indexes {
        refdata.current.indexes = ids;
    }
    Arc::new(AppState::new(config(), Arc::new(backend), refdata))
}

async fn get(state: &Arc<AppState>, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app(state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

fn header_str(h: &axum::http::HeaderMap, name: header::HeaderName) -> &str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// Independent brute-force count over the fixture files.
fn oracle_count(q: &str, from: &str, to: &str, indexes: &[&str]) -> u64 {
    let node = parse(q).unwrap();
    let (f, t) = (
        day_number(chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap()),
        day_number(chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").unwrap()),
    );
    indexes
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text)))
        .count() as u64
}

#[tokio::test]
async fn health_and_meta() {
    let s = state_with(None).await;
    let (status, _, _) = get(&s, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["index_version"], "fixture-v1");
    assert_eq!(meta["synthetic"], true);
    assert_eq!(meta["places"], 6);
    // Every page in the version: the sum of the baselines.
    let baselines: HashMap<String, Vec<(u32, u32)>> = serde_json::from_slice(
        &std::fs::read(data_dir().join("fixture-v1/baselines.json")).unwrap(),
    )
    .unwrap();
    let pages: u64 = baselines
        .values()
        .flatten()
        .map(|&(_, n)| u64::from(n))
        .sum();
    assert!(pages > 0);
    assert_eq!(meta["pages"], pages);
}

#[tokio::test]
async fn aggregate_counts_match_brute_force() {
    let s = state_with(None).await;
    let uri = "/v1/aggregate?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31";
    let (status, headers, body) = get(&s, uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = oracle_count(
        r#""cross of gold""#,
        "1896-06-01",
        "1896-12-31",
        &["pages-base-fixture", "pages-delta-fixture-1"],
    );
    assert!(expected > 0);
    assert_eq!(body["total"]["hits"], expected);
    assert_eq!(body["bucket"]["unit"], "week");
    let series_sum: u64 = body["series"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    let cube_sum: u64 = body["cube"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    let places_sum: u64 = body["places"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(series_sum, expected);
    assert_eq!(cube_sum, expected);
    assert_eq!(places_sum, expected);
    assert!(
        body["series"]["baseline"].as_array().unwrap().len()
            == body["bucket"]["count"].as_u64().unwrap() as usize
    );

    // The synthetic speech spreads east before west: P00001 first, P00006 last.
    let ids: Vec<&str> = body["places"]["id"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let first: Vec<u64> = body["places"]["first_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    let fd = |id: &str| first[ids.iter().position(|x| *x == id).unwrap()];
    assert!(fd("P00001") < fd("P00003") && fd("P00003") < fd("P00006"));

    // Unversioned: short cache, Content-Location pins the version.
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=300"
    );
    assert!(header_str(&headers, header::CONTENT_LOCATION).ends_with("&v=fixture-v1"));
}

#[tokio::test]
async fn version_pinning_and_redirects() {
    let s = state_with(None).await;
    let (status, headers, _) = get(&s, "/v1/aggregate?q=gold&v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));

    let (status, headers, _) = get(&s, "/v1/aggregate?q=Gold&v=old-version").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    let location = header_str(&headers, header::LOCATION);
    assert!(
        location.starts_with("/v1/aggregate?bucket=week&from=1895-01-01&q=gold&to=1897-12-31"),
        "{location}"
    );
    assert!(location.ends_with("&v=fixture-v1"));
}

#[tokio::test]
async fn index_set_isolation() {
    // A version naming only the base index must not see pages in the delta.
    let full = state_with(None).await;
    let base_only = state_with(Some(vec!["pages-base-fixture".into()])).await;
    let uri = "/v1/aggregate?q=fever&from=1895-01-01&to=1897-12-31";
    let (_, _, a) = get(&full, uri).await;
    let (_, _, b) = get(&base_only, uri).await;
    assert_eq!(
        a["total"]["hits"],
        oracle_count(
            "fever",
            "1895-01-01",
            "1897-12-31",
            &["pages-base-fixture", "pages-delta-fixture-1"]
        )
    );
    assert_eq!(
        b["total"]["hits"],
        oracle_count("fever", "1895-01-01", "1897-12-31", &["pages-base-fixture"])
    );
    assert!(a["total"]["hits"].as_u64() > b["total"]["hits"].as_u64());
}

#[tokio::test]
async fn problems_for_bad_requests() {
    let s = state_with(None).await;
    let (status, headers, body) = get(&s, "/v1/aggregate?q=text:gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/problem+json"
    );
    assert_eq!(body["type"], "/errors/query-syntax");
    assert_eq!(body["position"], 4);
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold&cachebust=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/bad-parameter");
    let (status, _, _) = get(&s, "/v1/hits?q=gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&s, "/v1/places?q=gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&s, "/v1/hits?q=gold&place=P99999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn hits_are_sorted_marked_linked_and_paginated() {
    let s = state_with(None).await;
    let base = "/v1/hits?q=%22cross+of+gold%22&from=1896-01-01&to=1896-12-31&place=P00001&limit=5";
    let (status, _, page1) = get(&s, base).await;
    assert_eq!(status, StatusCode::OK, "{page1}");
    let items = page1["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let dates: Vec<&str> = items.iter().map(|i| i["date"].as_str().unwrap()).collect();
    let mut sorted = dates.clone();
    sorted.sort_unstable();
    assert_eq!(dates, sorted);
    let first = &items[0];
    assert!(first["snippets"][0].as_str().unwrap().contains("<mark>"));
    assert!(first["links"]["viewer"]
        .as_str()
        .unwrap()
        .starts_with("https://www.loc.gov/resource/sn99000001/1896-"));
    assert!(first["links"]["viewer"]
        .as_str()
        .unwrap()
        .ends_with("&q=cross+of+gold"));
    assert_eq!(page1["place"]["id"], "P00001");

    let cursor = page1["next_cursor"].as_str().unwrap();
    let (_, _, page2) = get(&s, &format!("{base}&cursor={cursor}")).await;
    let second_first = page2["items"][0]["doc_id"].as_str().unwrap();
    assert!(items.iter().all(|i| i["doc_id"] != second_first));
}

#[tokio::test]
async fn hits_by_title_uses_lccn() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/v1/hits?q=gold&lccn=sn99000002&limit=3").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"]["lccn"], "sn99000002");
    assert!(body["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["lccn"] == "sn99000002"));
    let (status, _, _) = get(&s, "/v1/hits?q=gold&lccn=sn99000001,sn99000002").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&s, "/v1/hits?q=gold&lccn=sn99999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // With `place`, `lccn` is just a filter.
    let (status, _, body) = get(&s, "/v1/hits?q=gold&place=P00001&lccn=sn99000002").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
}

#[tokio::test]
async fn coverage_matches_aggregate_baseline() {
    let s = state_with(None).await;
    let (_, _, agg) = get(
        &s,
        "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31&bucket=month",
    )
    .await;
    let baseline_ref = agg["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert!(baseline_ref.ends_with("&v=fixture-v1"));
    let (status, _, cov) = get(&s, &baseline_ref).await;
    assert_eq!(status, StatusCode::OK, "{cov}");
    let pages: u64 = cov["pages"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(Some(pages), agg["total"]["baseline_pages"].as_u64());
    // Baselines include empty-OCR pages, so they exceed the indexed page count.
    assert!(pages > 0);
}

#[tokio::test]
async fn front_filter_disables_inexact_baseline_and_api_alias_works() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/api/v1/aggregate?q=gold&front=true").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["series"]["baseline"].is_null());
    assert!(body["cube"]["baseline_ref"].is_null());
    // Links built on the /api/v1 mount stay on it.
    let (_, _, body) = get(&s, "/api/v1/aggregate?q=gold").await;
    let link = body["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert!(link.starts_with("/api/v1/coverage?"), "{link}");
    let (status, _, _) = get(&s, &link).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn coarsens_before_querying_an_oversized_cube() {
    // 6 places × 366 days and × 53 weeks exceed 240 cells; 6 × 12 months fit.
    let s = state_with_cells(240).await;
    let (status, _, body) = get(
        &s,
        "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31&bucket=day",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["bucket"]["unit"], "month");
    assert_eq!(body["coarsened"], true);
    let cube_sum: u64 = body["cube"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(cube_sum, body["total"]["hits"].as_u64().unwrap());
    // Even by year the cube would exceed the budget → 422, not a backend failure.
    let tiny = state_with_cells(5).await;
    let (status, _, body) = get(&tiny, "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"], "/errors/query-too-broad");
}

#[tokio::test]
async fn places_geojson_is_version_pinned() {
    let s = state_with(None).await;
    let (status, headers, _) = get(&s, "/v1/places?v=old").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_str(&headers, header::LOCATION),
        "/v1/places?v=fixture-v1"
    );
    let (status, headers, _) = get(&s, "/v1/places?v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    let (_, headers, body) = get(&s, "/v1/places").await;
    assert_eq!(
        header_str(&headers, header::CONTENT_LOCATION),
        "/v1/places?v=fixture-v1"
    );
    assert_eq!(body["type"], "FeatureCollection");
    assert_eq!(body["features"].as_array().unwrap().len(), 6);
    assert_eq!(body["features"][0]["geometry"]["type"], "Point");
}

/// Copy the fixture data dir to a fresh temp dir the test can modify.
fn temp_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["indexes", "fixture-v1"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
        for entry in std::fs::read_dir(data_dir().join(sub)).unwrap() {
            let path = entry.unwrap().path();
            std::fs::copy(&path, dir.join(sub).join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::copy(data_dir().join("current.json"), dir.join("current.json")).unwrap();
    dir
}

/// Copy a reference snapshot under a new version, as the stats job would publish it.
fn snapshot_copy(dir: &std::path::Path, from: &str, to: &str) {
    std::fs::create_dir_all(dir.join(to)).unwrap();
    for entry in std::fs::read_dir(dir.join(from)).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, dir.join(to).join(path.file_name().unwrap())).unwrap();
    }
    let manifest = dir.join(to).join("manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["index_version"] = to.into();
    std::fs::write(manifest, m.to_string()).unwrap();
}

#[tokio::test]
async fn hot_reload_swaps_reference_data_and_backend_together() {
    let dir = temp_data_dir("reload");
    let loader = Loader {
        reference: Arc::new(LocalStore::new(&dir)),
        engine: Engine::Memory {
            indexes_dir: dir.join("indexes"),
        },
    };
    let snapshot = loader.snapshot().await.unwrap();
    let s = Arc::new(AppState::with_loader(config(), snapshot, Some(loader)));
    assert!(
        !reload_if_changed(&s).await.unwrap(),
        "unchanged version must not reload"
    );

    // Publish a new version whose index set adds a delta the old snapshot never loaded.
    std::fs::copy(
        dir.join("indexes/pages-delta-fixture-1.jsonl"),
        dir.join("indexes/pages-delta-fixture-2.jsonl"),
    )
    .unwrap();
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    current["index_version"] = "fixture-v2".into();
    current["indexes"] = serde_json::json!(["pages-base-fixture", "pages-delta-fixture-2"]);

    // A version whose reference snapshot is still v1's is refused.
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();
    let err = reload_if_changed(&s).await.unwrap_err();
    assert!(err.contains("is for `fixture-v1`"), "{err}");
    assert_eq!(s.snapshot.load().refdata.version(), "fixture-v1");

    // With its own snapshot, it publishes.
    snapshot_copy(&dir, "fixture-v1", "fixture-v2");
    current["reference"] = "fixture-v2".into();
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();

    assert!(reload_if_changed(&s).await.unwrap());
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(meta["index_version"], "fixture-v2");
    let (status, _, body) = get(&s, "/v1/aggregate?q=fever").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["index_version"], "fixture-v2");
    assert!(body["total"]["hits"].as_u64().unwrap() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

fn persisted_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("fixture-v1/f1")) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "zst"))
        .collect()
}

#[tokio::test]
async fn slow_responses_persist_and_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("usnm-respcache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalStore::new(&dir));
    let mut cfg = config();
    cfg.persist_after = Duration::ZERO;
    let first = Arc::new(
        AppState::new(cfg.clone(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(store.clone()),
    );
    let uri = "/v1/aggregate?q=fever&v=fixture-v1";
    let (status, _, body) = get(&first, uri).await;
    assert_eq!(status, StatusCode::OK);
    let mut files = Vec::new();
    for _ in 0..100 {
        files = persisted_files(&dir);
        if !files.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(files.len(), 1, "one entry under {{version}}/f1/");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        !name.contains("fever"),
        "search text must not appear in paths"
    );

    // A fresh replica with an empty in-process cache and an empty index still
    // answers from the persisted entry: the body is identical.
    let restarted = Arc::new(
        AppState::new(cfg, Arc::new(MemoryBackend::new()), refdata().await)
            .with_response_store(store),
    );
    let (status, headers, again) = get(&restarted, uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, body);
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));

    // An entry that decompresses but isn't JSON is ignored and recomputed.
    std::fs::write(
        &files[0],
        zstd::encode_all(&b"{\"truncated"[..], 3).unwrap(),
    )
    .unwrap();
    let recomputing = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(Arc::new(LocalStore::new(&dir))),
    );
    let (status, _, mut fresh) = get(&recomputing, uri).await;
    assert_eq!(status, StatusCode::OK);
    // Recomputed, so only its timing may differ from the original.
    let mut original = body.clone();
    for v in [&mut fresh, &mut original] {
        v.as_object_mut().unwrap().remove("timing_ms");
    }
    // Serving the invalid entry would have parsed as `null` here.
    assert_eq!(fresh, original);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fast_responses_are_not_persisted() {
    let dir = std::env::temp_dir().join(format!("usnm-respfast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = config();
    cfg.persist_after = Duration::from_secs(3600);
    let s = Arc::new(
        AppState::new(cfg, Arc::new(fixture_backend()), refdata().await)
            .with_response_store(Arc::new(LocalStore::new(&dir))),
    );
    let (status, _, _) = get(&s, "/v1/aggregate?q=fever&v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(persisted_files(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

async fn get_from(
    state: &Arc<AppState>,
    uri: &str,
    xff: &str,
) -> (StatusCode, axum::http::HeaderMap) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-forwarded-for", xff)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (resp.status(), resp.headers().clone())
}

#[tokio::test]
async fn clients_over_their_rate_get_429() {
    let mut cfg = config();
    cfg.rate_limit = Some(usnm_api::config::RateLimit {
        per_minute: 1.try_into().unwrap(),
        burst: 2.try_into().unwrap(),
    });
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    for _ in 0..2 {
        assert_eq!(
            get_from(&s, "/v1/meta", "203.0.113.7").await.0,
            StatusCode::OK
        );
    }
    let (status, headers) = get_from(&s, "/api/v1/meta", "198.51.100.1, 203.0.113.7").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/problem+json"
    );
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    let retry: u64 = header_str(&headers, header::RETRY_AFTER).parse().unwrap();
    assert!((1..=60).contains(&retry));
    // Other clients and health probes are unaffected.
    assert_eq!(
        get_from(&s, "/v1/meta", "203.0.113.8").await.0,
        StatusCode::OK
    );
    assert_eq!(
        get_from(&s, "/healthz", "203.0.113.7").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn reference_files_must_match_the_manifest() {
    let dir = temp_data_dir("manifest");
    let store = LocalStore::new(&dir);
    assert!(RefData::load(&store).await.is_ok());

    let places = dir.join("fixture-v1/places.json");
    let mut text = std::fs::read_to_string(&places).unwrap();
    text = text.replacen("Fixture", "Fixturf", 1);
    std::fs::write(&places, text).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("manifest"), "{err}");

    // Ids from current.json become object paths, so they must be plain segments.
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    current["reference"] = "../fixture-v1".into();
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("not a valid id"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A built site: an index, a hashed asset with a precompressed copy, a favicon.
/// Each test names its own, since tests run in parallel.
fn site_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-site-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><div id=root></div>").unwrap();
    std::fs::write(dir.join("assets/index-abc.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("assets/index-abc.js.gz"), b"gzipped").unwrap();
    std::fs::write(dir.join("favicon.svg"), "<svg/>").unwrap();
    dir
}

async fn get_site(
    state: &Arc<AppState>,
    uri: &str,
    encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(e) = encoding {
        req = req.header(header::ACCEPT_ENCODING, e);
    }
    let resp = app(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn serves_the_site_alongside_the_api() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("serve"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));

    // The app shell, revalidated on every load, with the security headers.
    let (status, h, body) = get_site(&s, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("id=root"));
    assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
    assert!(header_str(&h, header::CONTENT_SECURITY_POLICY).contains("connect-src 'self'"));
    assert_eq!(header_str(&h, header::X_CONTENT_TYPE_OPTIONS), "nosniff");

    // The app's pages get the shell too, so a reload keeps working. The file
    // itself is served with or without a trailing slash (web/src/route.ts).
    for uri in ["/status", "/status/", "/index.html", "/index.html/"] {
        let (status, h, body) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(body.contains("id=root"), "{uri}");
        assert!(header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
    }

    // Other paths get the shell, where the app shows a not-found page, with a 404.
    for uri in ["/search/gold", "/does-not-exist", "/statuses"] {
        let (status, h, body) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(body.contains("id=root"), "{uri}");
        assert!(header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache", "{uri}");
    }

    // HEAD on an app route: GET's headers and status, no body.
    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let len = std::fs::metadata(s.config.site_dir.as_ref().unwrap().join("index.html"))
        .unwrap()
        .len();
    assert_eq!(
        header_str(resp.headers(), header::CONTENT_LENGTH),
        len.to_string()
    );
    assert!(header_str(resp.headers(), header::CONTENT_TYPE).starts_with("text/html"));
    assert!(resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());

    // Hashed assets: cached for a year, the precompressed copy when accepted.
    let (status, h, body) = get_site(&s, "/assets/index-abc.js", Some("gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "gzipped");
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "gzip");
    assert_eq!(
        header_str(&h, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    let (_, h, body) = get_site(&s, "/assets/index-abc.js", None).await;
    assert_eq!(body, "console.log(1)");
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "");

    let (status, h, _) = get_site(&s, "/favicon.svg", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&h, header::CACHE_CONTROL),
        "public, max-age=3600"
    );

    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(header_str(resp.headers(), header::CONTENT_TYPE).starts_with("text/html"));
    assert_eq!(
        header_str(resp.headers(), header::CACHE_CONTROL),
        "no-cache"
    );

    // Missing files are 404s, not the shell.
    for uri in ["/assets/gone-123.js", "/gone.txt"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "", "{uri}");
    }

    // The API is unchanged, and its unknown paths stay problem details.
    let (status, _, body) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["index_version"].is_string());
    let (status, h, _) = get(&s, "/v1/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        header_str(&h, header::CONTENT_TYPE),
        "application/problem+json"
    );
    let (status, _, _) = get(&s, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn search_permalinks_and_status_are_noindex() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("noindex"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let robots = header::HeaderName::from_static("x-robots-tag");
    for uri in [
        "/?q=x",
        "/?q=%22cross+of+gold%22&from=1896-06-01",
        "/status",
    ] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, robots.clone()), "noindex", "{uri}");
    }
    for uri in ["/", "/?t=1896-07-01", "/favicon.svg"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, robots.clone()), "", "{uri}");
    }
}

async fn get_host(state: &Arc<AppState>, host: &str, uri: &str) -> axum::response::Response {
    app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn www_redirects_to_the_apex() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("www"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let cases = [
        ("www.usnewsmap.com", "/", "https://usnewsmap.com/"),
        (
            "WWW.usnewsmap.com:443",
            "/?q=%22cross+of+gold%22&from=1896-06-01",
            "https://usnewsmap.com/?q=%22cross+of+gold%22&from=1896-06-01",
        ),
        (
            "www.usnewsmap.com",
            "/status",
            "https://usnewsmap.com/status",
        ),
        (
            "www.usnewsmap.com",
            "/v1/meta",
            "https://usnewsmap.com/v1/meta",
        ),
    ];
    for (host, uri, location) in cases {
        let resp = get_host(&s, host, uri).await;
        assert_eq!(resp.status(), StatusCode::MOVED_PERMANENTLY, "{host}{uri}");
        assert_eq!(header_str(resp.headers(), header::LOCATION), location);
        assert_eq!(
            header_str(resp.headers(), header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert!(header_str(resp.headers(), header::CONTENT_SECURITY_POLICY).contains("default-src"));
    }
    // HTTP/2 puts the host in the URI.
    let resp = get_site(&s, "https://www.usnewsmap.com/status?x=1", None).await;
    assert_eq!(resp.0, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        header_str(&resp.1, header::LOCATION),
        "https://usnewsmap.com/status?x=1"
    );
    // Other hosts are served as they are.
    for host in [
        "usnewsmap.com",
        "api.usnewsmap.com",
        "www.example.com",
        "127.0.0.1:8080",
    ] {
        let resp = get_host(&s, host, "/").await;
        assert_eq!(resp.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn without_a_site_only_the_api_answers() {
    let s = state_with(None).await;
    let (status, _, _) = get_site(&s, "/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get_site(&s, "/v1/meta", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn status_without_pipeline_state() {
    let state = state_with(None).await;
    for uri in ["/v1/status", "/api/v1/status"] {
        let (status, headers, body) = get(&state, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(
            header_str(&headers, header::CACHE_CONTROL),
            "public, max-age=30"
        );
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/json"
        );
        assert_eq!(body["schema"], 1);
        assert_eq!(body["published"]["index_version"], "fixture-v1");
        assert_eq!(body["published"]["indexes"].as_array().unwrap().len(), 2);
        assert_eq!(body["backfill"]["available"], false);
        assert_eq!(body["indexing"]["available"], false);
    }
}

#[tokio::test]
async fn status_has_pages_by_state_and_language() {
    let state = state_with(None).await;
    let (_, _, body) = get(&state, "/v1/status").await;
    let p = &body["published"];
    // Six fixture places, one per state, 312 pages each: ties go by name.
    let states = p["by_state"].as_array().unwrap();
    assert_eq!(states.len(), 6);
    assert_eq!(
        states[0],
        json!({"state": "CA", "name": "California", "places": 1, "titles": 1,
               "pages": 312, "percent": 16.7})
    );
    let names: Vec<&str> = states.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "California",
            "Georgia",
            "Illinois",
            "Nebraska",
            "New York",
            "South Carolina"
        ]
    );
    let total: u64 = states.iter().map(|r| r["pages"].as_u64().unwrap()).sum();
    assert_eq!(total, p["pages"].as_u64().unwrap());
    assert_eq!(
        p["by_language"],
        json!({"pages_known": true, "multilingual_titles": 0, "multilingual_pages": 0,
               "rows": [{"code": "eng", "name": "English", "titles": 6, "pages": 1872,
                         "percent": 100.0}]})
    );
}

#[tokio::test]
async fn a_snapshot_without_pages_per_title_still_loads() {
    let dir = temp_data_dir("no-title-pages");
    let store = LocalStore::new(&dir);
    // A snapshot from before releases wrote the file: not in the manifest.
    std::fs::remove_file(dir.join("fixture-v1/title_pages.json")).unwrap();
    let manifest = dir.join("fixture-v1/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["files"]
        .as_array_mut()
        .unwrap()
        .retain(|f| f["path"] != "title_pages.json");
    std::fs::write(&manifest, m.to_string()).unwrap();
    let rd = RefData::load(&store).await.unwrap();
    assert!(rd.title_pages.is_none());
    let lang = usnm_api::status::assemble::by_language(&rd);
    assert!(!lang.pages_known);
    assert_eq!((lang.rows[0].titles, lang.rows[0].pages), (6, None));
    assert_eq!(usnm_api::status::assemble::by_state(&rd).len(), 6);
    let _ = std::fs::remove_dir_all(&dir);

    // Listed in the manifest but altered: the version doesn't load.
    let dir = temp_data_dir("bad-title-pages");
    let store = LocalStore::new(&dir);
    let path = dir.join("fixture-v1/title_pages.json");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replacen("312", "313", 1);
    std::fs::write(&path, text).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("title_pages.json does not match"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}
