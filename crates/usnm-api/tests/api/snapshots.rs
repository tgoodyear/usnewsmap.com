//! Which version is served: what it was built with, pinning, loading and
//! reloading.

use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::refdata::RefData;
use usnm_api::{app, reload_if_changed, AppState, Engine, Loader};
use usnm_core::text::Analyzer;
use usnm_store::LocalStore;

use crate::common::{
    config, fixture_backend, get, header_str, oracle_count, refdata, state_with, temp_data_dir,
};

/// A version laid out by decade (05 §5.5.5) names its decades for searches
/// to skip the other decades' splits: only at the API's own decade version,
/// over the decades of the version's bounds.
#[tokio::test]
async fn searches_name_their_decades_only_on_a_version_laid_out_with_them() {
    let mut rd = refdata().await;
    assert_eq!(rd.index_set().decades(), None);
    rd.current.decades = Some(usnm_core::decade::VERSION + 1);
    assert_eq!(rd.index_set().decades(), None);
    rd.current.decades = Some(usnm_core::decade::VERSION);
    // The fixtures run from 1895 to 1897.
    assert_eq!(rd.index_set().decades(), Some(&(1890..=1890)));
    // `current.json` written without the key reads as off, and the key
    // round-trips.
    let v = serde_json::to_value(&rd.current).unwrap();
    assert_eq!(v["decades"], usnm_core::decade::VERSION);
    // Searches return the same pages either way.
    let on = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));
    let off = state_with(None).await;
    for q in [
        "/v1/aggregate?q=gold&from=1896-01-01&to=1896-06-30",
        "/v1/aggregate?q=gold",
    ] {
        let (status, _, a) = get(&on, q).await;
        assert_eq!(status, StatusCode::OK, "{a}");
        let (_, _, b) = get(&off, q).await;
        assert_eq!(a["total"], b["total"], "{q}");
    }
}

/// Queries are folded with the analyzer the served version was built with
/// (#168): on a version with common-word pairs 2, `½` is the word `½`, as
/// Quickwit's `text` has it; on one with pairs 1, or none, it is `1⁄2`, as
/// those versions' pairs have it. Phrases search the pairs on either, and
/// the Japanese pages fold as their own version says.
#[tokio::test]
async fn queries_fold_as_the_served_version_was_built() {
    use usnm_api::refdata::JaIndexes;
    use usnm_core::params::RawParams;
    use usnm_core::text::Analyzers;
    let canonical = |rd: &RefData, q: &str| {
        let raw = RawParams::parse(&format!("q={q}")).unwrap();
        rd.search_request(&raw).unwrap().query.to_string()
    };
    let mut rd = refdata().await;
    // The fixture version predates the pairs: version 1, phrases in `text`.
    assert_eq!(rd.analyzers(), Analyzers::all(Analyzer::V1));
    assert!(!rd.index_set().common_grams());
    assert_eq!(canonical(&rd, "%C2%BD"), "1\u{2044}2");
    rd.current.common_grams = Some(1);
    assert!(rd.index_set().common_grams());
    assert_eq!(rd.index_set().analyzer(), Analyzer::V1);
    assert_eq!(canonical(&rd, "%C2%BD"), "1\u{2044}2");
    let v1 = rd.current.clone();
    rd.current.common_grams = Some(2);
    assert!(rd.index_set().common_grams());
    assert_eq!(rd.index_set().analyzer(), Analyzer::V2);
    assert_eq!(canonical(&rd, "%C2%BD"), "½");
    let v2 = rd.current.clone();
    // Pairs this API doesn't know: phrases stay in `text`.
    rd.current.common_grams = Some(usnm_core::common_grams::VERSION + 1);
    assert!(!rd.index_set().common_grams());
    assert_eq!(rd.index_set().analyzer(), Analyzer::LATEST);
    // The Japanese pages fold as `ja.fold` says.
    rd.current = v1.clone();
    rd.current.ja = Some(JaIndexes {
        indexes: vec!["pages-ja-fixture".into()],
        fold: 1,
        pages: 52,
    });
    assert_eq!(rd.ja_index_set().unwrap().analyzer(), Analyzer::V1);
    rd.current.ja.as_mut().unwrap().fold = 2;
    assert_eq!(rd.analyzers().ja, Analyzer::V2);
    assert_eq!(rd.ja_index_set().unwrap().analyzer(), Analyzer::V2);

    // Through the API, on the memory backend, which reads pages and
    // queries as Quickwit does: `½` finds the four market reports that print
    // it with version 2, and with version 1 (`1⁄2`, the phrase "1 2" to the
    // engine) the four that print `1/2`, as before #168. `1/2` finds those
    // on either.
    for (current, half) in [(v1, "1\u{2044}2"), (v2, "½")] {
        let mut rd = refdata().await;
        rd.current = current;
        let s = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));
        for (q, want) in [("%C2%BD", 4), ("1%2F2", 4)] {
            let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["total"]["hits"], want, "{q} {half}");
        }
        let (_, _, body) = get(&s, "/v1/aggregate?q=%C2%BD").await;
        assert_eq!(body["query"]["ast"], half, "{body}");
    }
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
        congress_search: true,
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
    assert_eq!((lang.rows[0].titles, lang.rows[0].pages), (4, None));
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

/// A snapshot from before releases recorded each title's page dates still
/// loads, and its titles list them as `null`; one whose file doesn't match
/// its manifest doesn't load.
#[tokio::test]
async fn a_snapshot_without_title_dates_still_loads() {
    let dir = temp_data_dir("no-title-dates");
    let store = LocalStore::new(&dir);
    std::fs::remove_file(dir.join("fixture-v1/title_dates.json")).unwrap();
    let manifest = dir.join("fixture-v1/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["files"]
        .as_array_mut()
        .unwrap()
        .retain(|f| f["path"] != "title_dates.json");
    std::fs::write(&manifest, m.to_string()).unwrap();
    let rd = RefData::load(&store).await.unwrap();
    assert!(rd.title_dates.is_none());
    assert!(rd.title_pages.is_some());
    let s = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));
    let (status, _, body) = get(&s, "/v1/titles").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let titles = body["titles"].as_array().unwrap();
    assert_eq!(titles.len(), 6);
    assert!(titles
        .iter()
        .all(|t| t["first_page_date"].is_null() && t["last_page_date"].is_null()));
    assert!(titles.iter().all(|t| t["pages"].is_u64()));
    let (_, _, one) = get(&s, "/v1/titles/sn99000001").await;
    assert!(one["first_page_date"].is_null() && one["last_page_date"].is_null());
    let _ = std::fs::remove_dir_all(&dir);

    // Listed in the manifest but altered: the version doesn't load.
    let dir = temp_data_dir("bad-title-dates");
    let store = LocalStore::new(&dir);
    let path = dir.join("fixture-v1/title_dates.json");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replacen("1895-01-05", "1895-01-06", 1);
    std::fs::write(&path, text).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("title_dates.json does not match"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A GET's status and body as text.
async fn get_text(state: &Arc<AppState>, uri: &str) -> (StatusCode, String) {
    let resp = app(state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

#[tokio::test]
async fn readyz_fails_with_one_body_whatever_the_reason() {
    // A searcher that refuses connections: its error names its address.
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let url = format!("http://127.0.0.1:{port}");
    let backend =
        usnm_search::quickwit::QuickwitBackend::new(&url, Duration::from_secs(1)).unwrap();
    let s = Arc::new(AppState::new(config(), Arc::new(backend), refdata().await));
    let (status, body) = get_text(&s, "/readyz").await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    );
    s.warming.store(true, std::sync::atomic::Ordering::Relaxed);
    let (status, body) = get_text(&s, "/readyz").await;
    assert_eq!(
        (status, body.as_str()),
        (StatusCode::SERVICE_UNAVAILABLE, "not ready")
    );

    let s = state_with(None).await;
    let (status, body) = get_text(&s, "/readyz").await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "ready"));
}

#[tokio::test]
async fn a_publish_naming_an_index_the_engine_lacks_keeps_serving_and_can_fail_readyz() {
    let dir = temp_data_dir("missing-index");
    let state = |requires: bool| {
        let dir = dir.clone();
        async move {
            let loader = Loader {
                reference: Arc::new(LocalStore::new(&dir)),
                engine: Engine::Memory {
                    indexes_dir: dir.join("indexes"),
                },
                congress_search: true,
            };
            let snapshot = loader.snapshot().await.unwrap();
            let mut cfg = config();
            cfg.ready_requires_published_indexes = requires;
            Arc::new(AppState::with_loader(cfg, snapshot, Some(loader)))
        }
    };
    let (on, off) = (state(true).await, state(false).await);
    let readyz = |s: Arc<AppState>| async move { get_text(&s, "/readyz").await.0 };
    let v1: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();

    // A version with a delta this engine never got (published elsewhere).
    snapshot_copy(&dir, "fixture-v1", "fixture-v2");
    let mut v2 = v1.clone();
    v2["index_version"] = "fixture-v2".into();
    v2["reference"] = "fixture-v2".into();
    v2["indexes"] = json!(["pages-base-fixture", "pages-delta-fixture-2"]);
    std::fs::write(dir.join("current.json"), v2.to_string()).unwrap();
    for s in [&on, &off] {
        let err = reload_if_changed(s).await.unwrap_err();
        assert!(err.contains("no index `pages-delta-fixture-2`"), "{err}");
        assert_eq!(s.snapshot.load().refdata.version(), "fixture-v1");
        assert_eq!(
            s.missing_index.load().as_deref().map(String::as_str),
            Some("pages-delta-fixture-2")
        );
        let (status, _, body) = get(s, "/v1/aggregate?q=fever").await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["index_version"], "fixture-v1");
    }
    assert_eq!(readyz(on.clone()).await, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(readyz(off.clone()).await, StatusCode::OK);

    // A load that fails for another reason doesn't clear it.
    let mut v3 = v2.clone();
    v3["index_version"] = "fixture-v3".into();
    v3["reference"] = "fixture-v3".into();
    std::fs::write(dir.join("current.json"), v3.to_string()).unwrap();
    assert!(reload_if_changed(&on).await.is_err());
    assert_eq!(readyz(on.clone()).await, StatusCode::SERVICE_UNAVAILABLE);

    // Taking the publish back clears it.
    std::fs::write(dir.join("current.json"), v1.to_string()).unwrap();
    assert!(!reload_if_changed(&on).await.unwrap());
    assert!(on.missing_index.load().is_none());
    assert_eq!(readyz(on.clone()).await, StatusCode::OK);

    // So does the index arriving.
    std::fs::write(dir.join("current.json"), v2.to_string()).unwrap();
    assert!(reload_if_changed(&on).await.is_err());
    assert_eq!(readyz(on.clone()).await, StatusCode::SERVICE_UNAVAILABLE);
    std::fs::copy(
        dir.join("indexes/pages-delta-fixture-1.jsonl"),
        dir.join("indexes/pages-delta-fixture-2.jsonl"),
    )
    .unwrap();
    assert!(reload_if_changed(&on).await.unwrap());
    assert_eq!(on.snapshot.load().refdata.version(), "fixture-v2");
    assert!(on.missing_index.load().is_none());
    assert_eq!(readyz(on.clone()).await, StatusCode::OK);
    let _ = std::fs::remove_dir_all(&dir);
}
