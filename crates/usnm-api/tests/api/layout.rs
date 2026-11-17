//! `GET /v1/pages/{doc_id}/layout` (06 §6.3.12, #2026) against the
//! synthetic curated store in `fixtures/curated`.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::{app, AppState};
use usnm_store::LocalStore;

use crate::common::{
    config, curated_dir, fixture_backend, get, header_str, layout_state, refdata, LAYOUT_DOC,
};

const STAMP: &str = "20261101T120000Z";
/// The fixture reference data's index version, the last part of every ETag.
const VERSION: &str = "fixture-v1";

fn uri(doc: &str) -> String {
    format!("/v1/pages/{doc}/layout")
}

/// A curated store in a temp directory holding `files` (name, bytes) under
/// `ocr-local/alto/`, served by a state with the fixture reference data.
async fn state_over(name: &str, files: &[(String, Vec<u8>)]) -> Arc<AppState> {
    let dir = std::env::temp_dir().join(format!("usnm-layout-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let alto = dir.join("ocr-local/alto");
    std::fs::create_dir_all(&alto).unwrap();
    for (file, bytes) in files {
        std::fs::write(alto.join(file), bytes).unwrap();
    }
    Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata().await)
            .with_alto_store(Arc::new(LocalStore::new(dir))),
    )
}

fn fixture_xml() -> Vec<u8> {
    let path = curated_dir()
        .join("ocr-local/alto")
        .join(format!("{LAYOUT_DOC}.node1.{STAMP}.xml"));
    std::fs::read(path).unwrap()
}

#[tokio::test]
async fn a_page_with_stored_alto_has_its_words_and_boxes() {
    let s = layout_state().await;
    let (status, headers, body) = get(&s, &uri(LAYOUT_DOC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/json"
    );
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    assert_eq!(
        header_str(&headers, header::ETAG),
        format!("\"node1.{STAMP}.{VERSION}\"")
    );
    assert_eq!(
        body,
        json!({
            "doc_id": format!("loc:{LAYOUT_DOC}"),
            "engine": null,
            "stamp": STAMP,
            "node": "node1",
            "width": 1000,
            "height": 1400,
            "granularity": "word",
            "image": null,
            "lines": [
                {"b": 0, "l": 0, "rect": [100, 100, 800, 60], "words": [
                    {"i": 0, "t": "Fixture", "rect": [100, 100, 300, 60], "c": 0.93},
                    {"i": 1, "t": "Gazette", "rect": [420, 100, 480, 60], "c": 0.5},
                ]},
                {"b": 0, "l": 1, "rect": [100, 180, 400, 60], "words": [
                    {"i": 0, "t": "gold", "rect": [100, 180, 400, 60], "c": null},
                ]},
                // Block 1 is the picture, which has no lines.
                {"b": 2, "l": 0, "rect": [100, 800, 800, 60], "words": [
                    {"i": 0, "t": "silver", "rect": [100, 800, 800, 60], "c": 1.0},
                ]},
            ],
        })
    );
}

#[tokio::test]
async fn bare_and_qualified_ids_name_the_same_page() {
    let s = layout_state().await;
    let (_, _, bare) = get(&s, &uri(LAYOUT_DOC)).await;
    let (status, _, qualified) = get(&s, &uri(&format!("loc:{LAYOUT_DOC}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bare, qualified);
    // The colon may be percent-encoded.
    let (status, _, encoded) = get(&s, &uri(&format!("loc%3A{LAYOUT_DOC}"))).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(bare, encoded);
}

#[tokio::test]
async fn a_page_without_alto_is_a_404_problem() {
    let s = layout_state().await;
    // Another page, a longer sequence number that starts like the stored one,
    // and an id of another source.
    for doc in [
        "sn99000001_1896-05-04_ed-1_seq-2",
        "sn99000001_1896-05-04_ed-1_seq-10",
        "ghn:sn99000001_1896-05-04_ed-1_seq-1",
        "lcg:lcg-globe-1_p3",
    ] {
        let resp = raw(&s, &uri(doc), None).await;
        assert_eq!(resp.0, StatusCode::NOT_FOUND, "{doc}");
        assert_eq!(
            header_str(&resp.1, header::CONTENT_TYPE),
            "application/problem+json"
        );
        assert_eq!(resp.2["type"], "/errors/not-found", "{doc}");
        assert_eq!(resp.2["status"], 404);
    }
    // Without a curated store every page is a 404.
    let none = Arc::new(AppState::new(
        config(),
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let (status, _, body) = get(&none, &uri(LAYOUT_DOC)).await;
    assert_eq!(status, StatusCode::NOT_FOUND, "{body}");
}

#[tokio::test]
async fn a_malformed_or_oversize_id_is_a_400() {
    let s = layout_state().await;
    let long = format!("sn99000001_1896-05-04_ed-1_seq-{}", "1".repeat(300));
    for doc in [
        "not-a-page",
        "sn99000001_1896-13-40_ed-1_seq-1",
        "nosuch:sn99000001_1896-05-04_ed-1_seq-1",
        "loc:",
        long.as_str(),
    ] {
        let (status, headers, body) = get(&s, &uri(doc)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{doc}: {body}");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/problem+json"
        );
        assert_eq!(body["type"], "/errors/bad-parameter");
    }
}

#[tokio::test]
async fn the_newest_stamp_is_served_and_old_ones_are_left_alone() {
    let xml = fixture_xml();
    let newer = String::from_utf8(xml.clone())
        .unwrap()
        .replace("\"Fixture\"", "\"Reread\"")
        .into_bytes();
    let s = state_over(
        "newest",
        &[
            (format!("{LAYOUT_DOC}.node2.20260101T000000Z.xml"), xml),
            (format!("{LAYOUT_DOC}.node1.20270101T000000Z.xml"), newer),
        ],
    )
    .await;
    let (status, headers, body) = get(&s, &uri(LAYOUT_DOC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["stamp"], "20270101T000000Z");
    assert_eq!(body["node"], "node1");
    assert_eq!(body["lines"][0]["words"][0]["t"], "Reread");
    assert_eq!(
        header_str(&headers, header::ETAG),
        format!("\"node1.20270101T000000Z.{VERSION}\"")
    );
}

#[tokio::test]
async fn the_etag_answers_a_conditional_request_with_a_304() {
    let s = layout_state().await;
    let etag = format!("\"node1.{STAMP}.{VERSION}\"");
    let (status, headers, bytes) = raw_bytes(&s, &uri(LAYOUT_DOC), Some(&etag)).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
    assert!(bytes.is_empty());
    assert_eq!(header_str(&headers, header::ETAG), etag);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    // Another file's ETag gets the body.
    let (status, ..) = raw_bytes(&s, &uri(LAYOUT_DOC), Some("\"node1.other\"")).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn the_engine_is_the_one_the_served_version_holds() {
    use usnm_state::ocr_local::{doc_hash, Engine, Entry, OcrLocalPages};
    let mut rd = refdata().await;
    rd.ocr_local = OcrLocalPages {
        engines: vec![Engine {
            source: "usnm-tesseract".into(),
            engine: "tesseract/5 fixture".into(),
        }],
        entries: vec![Entry {
            hash: doc_hash(LAYOUT_DOC),
            engine: 0,
            batch: 0,
        }],
    };
    let s = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), rd)
            .with_alto_store(Arc::new(LocalStore::new(curated_dir()))),
    );
    let (status, _, body) = get(&s, &uri(LAYOUT_DOC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["engine"], "usnm-tesseract");
}

#[tokio::test]
async fn a_new_version_gets_its_own_cache_entry_and_etag() {
    use usnm_api::Snapshot;
    use usnm_state::ocr_local::{doc_hash, Engine, Entry, OcrLocalPages};
    let s = layout_state().await;
    let (_, headers, body) = get(&s, &uri(LAYOUT_DOC)).await;
    assert_eq!(body["engine"], Value::Null);
    let old = header_str(&headers, header::ETAG);
    // A reload publishes the page's OCR under a new version, with the ALTO
    // file unchanged.
    let mut rd = refdata().await;
    rd.current.index_version = "fixture-v2".into();
    rd.ocr_local = OcrLocalPages {
        engines: vec![Engine {
            source: "usnm-tesseract".into(),
            engine: "tesseract/5 fixture".into(),
        }],
        entries: vec![Entry {
            hash: doc_hash(LAYOUT_DOC),
            engine: 0,
            batch: 0,
        }],
    };
    s.snapshot.store(Arc::new(Snapshot {
        refdata: rd,
        backend: Arc::new(fixture_backend()),
        congress_dropped: false,
    }));
    // The old ETag no longer matches, and the body has the engine.
    let (status, headers, bytes) = raw_bytes(&s, &uri(LAYOUT_DOC), Some(old)).await;
    assert_eq!(status, StatusCode::OK);
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["engine"], "usnm-tesseract");
    let new = header_str(&headers, header::ETAG);
    assert_ne!(new, old);
    assert_eq!(new, format!("\"node1.{STAMP}.fixture-v2\""));
    let (status, ..) = raw_bytes(&s, &uri(LAYOUT_DOC), Some(new)).await;
    assert_eq!(status, StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn a_line_level_stored_file_reports_its_granularity() {
    use usnm_alto::{Block, BlockKind, Description, Granularity, Line, Page, Rect, Step, Word};
    let line = Rect::new(10.0, 10.0, 90.0, 30.0);
    let word = |t: &str| Word {
        rect: line,
        text: t.into(),
        conf: None,
    };
    let page = Page {
        width: 100.0,
        height: 100.0,
        granularity: Granularity::Line,
        blocks: vec![Block {
            kind: BlockKind::Text,
            rect: line,
            lang: None,
            lines: vec![Line {
                rect: line,
                words: vec![word("one"), word("two")],
                hyphen: None,
            }],
        }],
    };
    let desc = Description {
        ocr: Step {
            settings: vec![("granularity".into(), "line".into())],
            ..Step::default()
        },
        ..Description::default()
    };
    let xml = usnm_alto::write(&page, &desc, "eng");
    let s = state_over(
        "line-level",
        &[(format!("{LAYOUT_DOC}.node1.{STAMP}.xml"), xml.into_bytes())],
    )
    .await;
    let (status, _, body) = get(&s, &uri(LAYOUT_DOC)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["granularity"], "line");
}

#[tokio::test]
async fn a_page_over_the_caps_is_a_422_problem() {
    // More words than a layout holds.
    let words = String::from_utf8(fixture_xml()).unwrap().replace(
        "<String ID=\"ST3\"",
        &"<String ID=\"W\" HPOS=\"1\" VPOS=\"1\" WIDTH=\"1\" HEIGHT=\"1\" CONTENT=\"w\"/>"
            .repeat(150_001),
    );
    // A file over the read cap: padding in a comment, after the root.
    let big = format!(
        "{}<!--{}-->",
        String::from_utf8(fixture_xml()).unwrap(),
        " ".repeat(17 << 20)
    );
    let s = state_over(
        "caps",
        &[
            (
                format!("{LAYOUT_DOC}.node1.20261101T120000Z.xml"),
                words.into_bytes(),
            ),
            (
                "sn99000001_1896-05-04_ed-1_seq-2.node1.20261101T120000Z.xml".into(),
                big.into_bytes(),
            ),
        ],
    )
    .await;
    for doc in [LAYOUT_DOC, "sn99000001_1896-05-04_ed-1_seq-2"] {
        let (status, headers, body) = get(&s, &uri(doc)).await;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{doc}: {body}");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/problem+json"
        );
        assert_eq!(body["type"], "/errors/page-too-large");
    }
}

#[tokio::test]
async fn a_stored_file_that_does_not_parse_is_a_503_not_a_page() {
    let s = state_over(
        "broken",
        &[(
            format!("{LAYOUT_DOC}.node1.{STAMP}.xml"),
            b"not xml".to_vec(),
        )],
    )
    .await;
    let (status, _, body) = get(&s, &uri(LAYOUT_DOC)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
}

/// A store that counts reads of each path and how many run at once.
#[derive(Debug)]
struct CountingStore {
    inner: LocalStore,
    reads: std::sync::Mutex<Vec<String>>,
    now: std::sync::atomic::AtomicUsize,
    peak: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl usnm_store::ObjectStore for CountingStore {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        self.inner.get(path).await
    }
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, usnm_store::StoreError> {
        self.inner.put_new(path, body, content_type).await
    }
    async fn put(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), usnm_store::StoreError> {
        self.inner.put(path, body, content_type).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        self.inner.list(prefix).await
    }
    async fn list_stem(&self, stem: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        self.inner.list_stem(stem).await
    }
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<Option<usnm_store::ByteStream>, usnm_store::StoreError> {
        use std::sync::atomic::Ordering::SeqCst;
        self.reads.lock().unwrap().push(path.to_owned());
        let now = self.now.fetch_add(1, SeqCst) + 1;
        self.peak.fetch_max(now, SeqCst);
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        self.now.fetch_sub(1, SeqCst);
        self.inner.get_stream(path).await
    }
}

async fn counting_state(name: &str, docs: &[String]) -> (Arc<AppState>, Arc<CountingStore>) {
    let dir = std::env::temp_dir().join(format!("usnm-layout-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let alto = dir.join("ocr-local/alto");
    std::fs::create_dir_all(&alto).unwrap();
    for doc in docs {
        std::fs::write(alto.join(format!("{doc}.node1.{STAMP}.xml")), fixture_xml()).unwrap();
    }
    let store = Arc::new(CountingStore {
        inner: LocalStore::new(dir),
        reads: Default::default(),
        now: Default::default(),
        peak: Default::default(),
    });
    let state = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata().await)
            .with_alto_store(store.clone()),
    );
    (state, store)
}

#[tokio::test]
async fn concurrent_misses_for_one_page_share_one_read() {
    let (s, store) = counting_state("single-flight", &[LAYOUT_DOC.to_owned()]).await;
    let calls = (0..8).map(|_| async { get(&s, &uri(LAYOUT_DOC)).await });
    for (status, ..) in futures::future::join_all(calls).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(store.reads.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn builds_of_different_pages_run_a_few_at_a_time() {
    let docs: Vec<String> = (1..=6)
        .map(|n| format!("sn99000001_1896-05-04_ed-1_seq-{n}"))
        .collect();
    let (s, store) = counting_state("bounded", &docs).await;
    let calls = docs.iter().map(|d| async { get(&s, &uri(d)).await });
    for (status, ..) in futures::future::join_all(calls).await {
        assert_eq!(status, StatusCode::OK);
    }
    assert_eq!(store.reads.lock().unwrap().len(), 6);
    let peak = store.peak.load(std::sync::atomic::Ordering::SeqCst);
    assert!((1..=2).contains(&peak), "{peak} reads at once");
}

async fn raw(
    s: &Arc<AppState>,
    uri: &str,
    if_none_match: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let (status, headers, bytes) = raw_bytes(s, uri, if_none_match).await;
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn raw_bytes(
    s: &Arc<AppState>,
    uri: &str,
    if_none_match: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    let mut req = Request::builder().uri(uri);
    if let Some(tag) = if_none_match {
        req = req.header(header::IF_NONE_MATCH, tag);
    }
    let resp = app(s.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

const HITS: &str =
    "/v1/hits?q=%22cross+of+gold%22&from=1896-01-01&to=1896-12-31&place=P00001&limit=5";

/// The `links.layout` of each item of the hits, by doc id.
async fn layout_links(s: &Arc<AppState>) -> Vec<(String, Option<String>)> {
    let (status, _, body) = get(s, HITS).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| {
            (
                i["doc_id"].as_str().unwrap().to_owned(),
                i["links"]["layout"].as_str().map(str::to_owned),
            )
        })
        .collect()
}

#[tokio::test]
async fn a_hit_links_its_layout_only_when_the_page_has_stored_alto() {
    // The ids the fixture's search finds; the store gets the first one's ALTO.
    let plain = crate::common::state_with(None).await;
    let ids = layout_links(&plain).await;
    assert!(ids.len() > 1);
    let doc = ids[0].0.clone();

    let s = state_over(
        "links",
        &[(format!("{doc}.node1.{STAMP}.xml"), fixture_xml())],
    )
    .await;
    // Nothing is listed yet: no page has a link.
    assert!(layout_links(&s).await.iter().all(|(_, l)| l.is_none()));
    // The response above is cached without links, so ask a state that lists first.
    let s = state_over(
        "links",
        &[(format!("{doc}.node1.{STAMP}.xml"), fixture_xml())],
    )
    .await;
    s.layout_ids
        .refresh(s.alto.as_deref().unwrap())
        .await
        .unwrap();
    let with: Vec<_> = layout_links(&s)
        .await
        .into_iter()
        .filter(|(_, l)| l.is_some())
        .collect();
    assert_eq!(
        with,
        [(doc.clone(), Some(format!("/v1/pages/{doc}/layout")))]
    );
}

#[tokio::test]
async fn without_a_curated_store_no_hit_has_a_layout_link() {
    let s = crate::common::state_with(None).await;
    assert!(s.alto.is_none());
    assert!(layout_links(&s).await.iter().all(|(_, l)| l.is_none()));
    assert!(!s.layout_ids.loaded());
}

#[tokio::test]
async fn a_refresh_picks_up_a_new_file_and_drops_a_removed_one() {
    let s = state_over("refresh", &[]).await;
    let store = s.alto.clone().unwrap();
    assert_eq!(s.layout_ids.refresh(store.as_ref()).await.unwrap(), 0);
    assert!(s.layout_ids.loaded());
    assert!(!s.layout_ids.has(LAYOUT_DOC));

    let dir = std::env::temp_dir().join(format!("usnm-layout-refresh-{}", std::process::id()));
    let file = dir
        .join("ocr-local/alto")
        .join(format!("{LAYOUT_DOC}.node1.{STAMP}.xml"));
    std::fs::write(&file, fixture_xml()).unwrap();
    // Still the old listing until the next refresh.
    assert!(!s.layout_ids.has(LAYOUT_DOC));
    assert_eq!(s.layout_ids.refresh(store.as_ref()).await.unwrap(), 1);
    assert!(s.layout_ids.has(LAYOUT_DOC));

    std::fs::remove_file(&file).unwrap();
    s.layout_ids.refresh(store.as_ref()).await.unwrap();
    assert!(!s.layout_ids.has(LAYOUT_DOC));
}
