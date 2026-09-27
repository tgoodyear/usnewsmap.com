//! End-to-end tests of the API against the synthetic fixture corpus.

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use usnm_api::config::{BackendKind, Config};
use usnm_api::refdata::RefData;
use usnm_api::{app, memory_snapshot, reload_if_changed, AppState, Reloader};
use usnm_core::query::parse;
use usnm_core::text::tokenize;
use usnm_core::time::day_number;
use usnm_search::memory::{eval, MemoryBackend};
use usnm_search::PageDoc;

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
    Config {
        bind: "127.0.0.1:0".into(),
        backend: BackendKind::Memory,
        data_dir: data_dir(),
        allowed_origins: vec!["https://usnewsmap.com".into()],
        search_timeout: Duration::from_secs(10),
        refresh_interval: Duration::from_secs(600),
        cache_bytes: 16 * 1024 * 1024,
        max_cells: usnm_core::cube::MAX_CELLS,
    }
}

fn state_with_cells(max_cells: usize) -> Arc<AppState> {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        backend.add_index(id, load_docs(id));
    }
    let refdata = RefData::load(&data_dir()).unwrap();
    let mut cfg = config();
    cfg.max_cells = max_cells;
    Arc::new(AppState::new(cfg, Arc::new(backend), refdata))
}

fn state_with(indexes: Option<Vec<String>>) -> Arc<AppState> {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        backend.add_index(id, load_docs(id));
    }
    let mut refdata = RefData::load(&data_dir()).unwrap();
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
    let s = state_with(None);
    let (status, _, _) = get(&s, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["index_version"], "fixture-v1");
    assert_eq!(meta["synthetic"], true);
    assert_eq!(meta["places"], 6);
}

#[tokio::test]
async fn aggregate_counts_match_brute_force() {
    let s = state_with(None);
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
    let s = state_with(None);
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
    let full = state_with(None);
    let base_only = state_with(Some(vec!["pages-base-fixture".into()]));
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
    let s = state_with(None);
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
    let s = state_with(None);
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
    let s = state_with(None);
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
    let s = state_with(None);
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
    let s = state_with(None);
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
    let s = state_with_cells(240);
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
    let tiny = state_with_cells(5);
    let (status, _, body) = get(&tiny, "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"], "/errors/query-too-broad");
}

#[tokio::test]
async fn places_geojson_is_version_pinned() {
    let s = state_with(None);
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

#[tokio::test]
async fn hot_reload_swaps_reference_data_and_backend_together() {
    let dir = temp_data_dir("reload");
    let mut cfg = config();
    cfg.data_dir = dir.clone();
    let d = dir.clone();
    let reloader: Reloader = Arc::new(move || memory_snapshot(&d));
    let s = Arc::new(AppState::with_reloader(
        cfg,
        reloader().unwrap(),
        Some(reloader),
    ));
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
