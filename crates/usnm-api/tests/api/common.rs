//! The fixture corpus, the servers started over it, and the request and
//! assertion helpers the test modules share.

use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::{app, AppState};
use usnm_core::query::parse_with;
use usnm_core::text::{tokenize, Analyzer};
use usnm_core::time::day_number;
use usnm_search::memory::{eval, MemoryBackend};
use usnm_search::PageDoc;
use usnm_store::LocalStore;

pub(crate) fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

pub(crate) fn load_docs(index_id: &str) -> Vec<PageDoc> {
    let path = data_dir().join("indexes").join(format!("{index_id}.jsonl"));
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

pub(crate) fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.cache_bytes = 16 * 1024 * 1024;
    // Tests without a client address share one bucket; rate limiting has its own test.
    c.rate_limit = None;
    c
}

pub(crate) async fn refdata() -> RefData {
    RefData::load(&LocalStore::new(data_dir())).await.unwrap()
}

pub(crate) fn fixture_backend() -> MemoryBackend {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        backend.add_index(id, load_docs(id));
    }
    backend
}

pub(crate) async fn state_with_cells(max_cells: usize) -> Arc<AppState> {
    let mut cfg = config();
    cfg.max_cells = max_cells;
    Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ))
}

pub(crate) async fn state_with(indexes: Option<Vec<String>>) -> Arc<AppState> {
    let backend = fixture_backend();
    let mut refdata = refdata().await;
    if let Some(ids) = indexes {
        refdata.current.indexes = ids;
    }
    Arc::new(AppState::new(config(), Arc::new(backend), refdata))
}

pub(crate) async fn get(
    state: &Arc<AppState>,
    uri: &str,
) -> (StatusCode, axum::http::HeaderMap, Value) {
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

pub(crate) fn header_str(h: &axum::http::HeaderMap, name: header::HeaderName) -> &str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// The fixture version's analyzer: its `current.json` has no
/// `common_grams`, as versions built before analyzer versions (#168).
pub(crate) const FIXTURE: Analyzer = Analyzer::V1;

/// [`usnm_core::query::parse`] as the API parses for the fixture version.
pub(crate) fn parse(q: &str) -> Result<usnm_core::query::Node, usnm_core::query::QueryError> {
    parse_with(q, FIXTURE)
}

/// Independent brute-force count over the fixture files.
pub(crate) fn oracle_count(q: &str, from: &str, to: &str, indexes: &[&str]) -> u64 {
    let node = parse(q).unwrap();
    let (f, t) = (
        day_number(chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap()),
        day_number(chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").unwrap()),
    );
    indexes
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text, FIXTURE)))
        .count() as u64
}

/// Copy the fixture data dir to a fresh temp dir the test can modify.
pub(crate) fn temp_data_dir(name: &str) -> PathBuf {
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

pub(crate) fn persisted_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("fixture-v1/f8")) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "zst"))
        .collect()
}

/// The fixture state with the Japanese pages' index (#139) published, and its
/// synthetic title in the catalog.
pub(crate) async fn ja_state() -> Arc<AppState> {
    ja_state_with(config(), false).await
}

/// [`ja_state`] with `cfg`, on a version built with American Stories' text
/// or not.
pub(crate) async fn ja_state_with(cfg: Config, american_stories: bool) -> Arc<AppState> {
    let mut backend = fixture_backend();
    backend.add_index("pages-ja-fixture", load_docs("pages-ja-fixture"));
    let mut refdata = refdata().await;
    refdata.current.ja = Some(usnm_api::refdata::JaIndexes {
        indexes: vec!["pages-ja-fixture".into()],
        fold: usnm_core::ja::fold_version(FIXTURE),
        pages: 52,
    });
    if american_stories {
        refdata.current.american_stories = Some(usnm_core::american_stories::VERSION);
    }
    refdata.titles.insert(
        "sn99000901".into(),
        serde_json::from_value(json!({
            "lccn": "sn99000901", "name": "Fixture Shimpo (Japanese)",
            "place_id": "P00003", "state": "CA", "languages": ["eng", "jpn"],
        }))
        .unwrap(),
    );
    Arc::new(AppState::new(cfg, Arc::new(backend), refdata))
}
