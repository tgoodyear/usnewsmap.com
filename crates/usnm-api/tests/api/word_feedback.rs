//! `POST /v1/word-feedback` (06 §6.3.13, #2026): what a mark may hold, what
//! is refused, what is stored, and that nothing about the reader is.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::Utc;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::config::RateLimit;
use usnm_api::ocr_requests::staging_dir;
use usnm_api::word_feedback::WordFeedback;
use usnm_api::{app, AppState};
use usnm_store::{LocalStore, ObjectStore};

use super::common::{config, fixture_backend, refdata};

const DOC: &str = "sn99000001_1896-05-04_ed-1_seq-1";
const WORD: &str = "20261101T120000Z:0:1:2";

fn dir(name: &str) -> std::path::PathBuf {
    let d = std::env::temp_dir().join(format!("usnm-word-feedback-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn feedback(root: &std::path::Path) -> Arc<WordFeedback> {
    WordFeedback::start(
        Arc::new(LocalStore::new(root)),
        &opentelemetry::global::meter("test"),
    )
}

async fn state_with(feedback: Option<Arc<WordFeedback>>) -> Arc<AppState> {
    let state = AppState::new(config(), Arc::new(fixture_backend()), refdata().await);
    Arc::new(match feedback {
        Some(f) => state.with_word_feedback(f),
        None => state,
    })
}

fn request(body: &str) -> Request<Body> {
    Request::post("/v1/word-feedback")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body.to_owned()))
        .unwrap()
}

async fn send(
    state: &Arc<AppState>,
    req: Request<Body>,
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn mark(doc: &str, word: &str, verdict: &str) -> String {
    json!({"v": 1, "doc_id": doc, "word_id": word, "verdict": verdict}).to_string()
}

/// Every stored line under today's staging directory, once the writer has stopped.
async fn stored(root: &std::path::Path, fb: &WordFeedback) -> Vec<String> {
    fb.shutdown(Duration::from_secs(10)).await;
    let store = LocalStore::new(root);
    let mut lines = Vec::new();
    for path in store
        .list(&staging_dir(Utc::now().date_naive()))
        .await
        .unwrap()
    {
        let body = store.get(&path).await.unwrap().unwrap();
        lines.extend(String::from_utf8(body).unwrap().lines().map(str::to_owned));
    }
    lines
}

#[tokio::test]
async fn a_mark_is_accepted_and_stored_with_only_page_word_and_verdict() {
    let root = dir("stored");
    let fb = feedback(&root);
    let s = state_with(Some(fb.clone())).await;
    // Bare and qualified ids are the same page; the headers say who sent it.
    for (doc, verdict) in [(DOC.to_owned(), "wrong"), (format!("loc:{DOC}"), "right")] {
        let req = Request::post("/v1/word-feedback")
            .header(header::CONTENT_TYPE, "application/json")
            .header("user-agent", "Mozilla/5.0 secret-agent")
            .header("x-forwarded-for", "203.0.113.9")
            .header("cookie", "a=b")
            .body(Body::from(mark(&doc, WORD, verdict)))
            .unwrap();
        let (status, headers, body) = send(&s, req).await;
        assert_eq!(status, StatusCode::NO_CONTENT, "{doc}");
        assert_eq!(headers[header::CACHE_CONTROL], "no-store");
        assert_eq!(body, Value::Null);
    }
    let lines = stored(&root, &fb).await;
    assert_eq!(lines.len(), 2);
    let mut rows: Vec<Value> = lines
        .iter()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    rows.sort_by_key(|r| r["verdict"].as_str().unwrap().to_owned());
    assert_eq!(
        rows,
        vec![
            json!({"v": 1, "doc_id": DOC, "word_id": WORD, "verdict": "right"}),
            json!({"v": 1, "doc_id": DOC, "word_id": WORD, "verdict": "wrong"}),
        ]
    );
    let text = lines.join("\n");
    for private in ["secret-agent", "203.0.113.9", "a=b"] {
        assert!(!text.contains(private), "{private}");
    }
    // Create-only blobs under the day's directory, which names no time but the day.
    let store = LocalStore::new(&root);
    let paths = store
        .list(&staging_dir(Utc::now().date_naive()))
        .await
        .unwrap();
    assert_eq!(paths.len(), 1);
    assert!(paths[0].ends_with(".jsonl"), "{paths:?}");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn text_plain_bodies_are_read_like_a_beacons() {
    let root = dir("plain");
    let fb = feedback(&root);
    let s = state_with(Some(fb.clone())).await;
    let req = Request::post("/v1/word-feedback")
        .header(header::CONTENT_TYPE, "text/plain;charset=UTF-8")
        .body(Body::from(mark(DOC, WORD, "right")))
        .unwrap();
    assert_eq!(send(&s, req).await.0, StatusCode::NO_CONTENT);
    assert_eq!(stored(&root, &fb).await.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn invalid_bodies_are_refused_and_nothing_is_stored() {
    let root = dir("invalid");
    let fb = feedback(&root);
    let s = state_with(Some(fb.clone())).await;
    let long_stamp = format!("{}:0:0:0", "a".repeat(40));
    let bodies = [
        "".to_owned(),
        "[]".to_owned(),
        "null".to_owned(),
        "{".to_owned(),
        json!({"v": 1, "doc_id": DOC, "word_id": WORD}).to_string(),
        json!({"v": 1, "doc_id": DOC, "verdict": "right"}).to_string(),
        json!({"v": 2, "doc_id": DOC, "word_id": WORD, "verdict": "right"}).to_string(),
        mark(DOC, WORD, "maybe"),
        mark(DOC, WORD, "Right"),
        mark("", WORD, "right"),
        mark("not-a-page", WORD, "right"),
        mark(&format!("ghn:{DOC}"), WORD, "right"),
        mark(&format!("{DOC}/../x"), WORD, "right"),
        mark(DOC, "", "right"),
        mark(DOC, "20261101T120000Z:0:1", "right"),
        mark(DOC, "20261101T120000Z:0:1:2:3", "right"),
        mark(DOC, "20261101T120000Z:a:1:2", "right"),
        mark(DOC, "20261101T120000Z:00:1:2", "right"),
        mark(DOC, "2026 1101:0:1:2", "right"),
        mark(DOC, "../x:0:1:2", "right"),
        mark(DOC, &long_stamp, "right"),
        // A word's text, or anything else, has no field to go in.
        json!({"v": 1, "doc_id": DOC, "word_id": WORD, "verdict": "wrong", "text": "Liberty"})
            .to_string(),
        json!({"v": 1, "doc_id": DOC, "word_id": WORD, "verdict": "wrong", "note": "x"})
            .to_string(),
        format!(
            r#"{{"v":1,"doc_id":"{DOC}","word_id":"{WORD}","verdict":"wrong","verdict":"right"}}"#
        ),
    ];
    for body in bodies {
        let (status, headers, problem) = send(&s, request(&body)).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
        assert_eq!(headers[header::CONTENT_TYPE], "application/problem+json");
        assert_eq!(problem["type"], "/errors/bad-parameter", "{body}");
    }
    assert!(stored(&root, &fb).await.is_empty());
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn an_oversize_body_or_the_wrong_media_type_is_refused() {
    let s = state_with(Some(feedback(&dir("size")))).await;
    let big =
        json!({"v": 1, "doc_id": DOC, "word_id": WORD, "verdict": "right", "pad": "x".repeat(600)})
            .to_string();
    // Declared and undeclared (a chunked body has no Content-Length).
    let (status, _, problem) = send(&s, request(&big)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(problem["type"], "/errors/too-large");
    let stream = futures::stream::iter([Ok::<_, std::io::Error>(axum::body::Bytes::from(big))]);
    let chunked = Request::post("/v1/word-feedback")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from_stream(stream))
        .unwrap();
    assert_eq!(send(&s, chunked).await.0, StatusCode::PAYLOAD_TOO_LARGE);

    let form = Request::post("/v1/word-feedback")
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from(mark(DOC, WORD, "right")))
        .unwrap();
    let (status, _, problem) = send(&s, form).await;
    assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    assert_eq!(problem["type"], "/errors/unsupported-media-type");
}

#[tokio::test]
async fn only_post_is_routed() {
    let s = state_with(Some(feedback(&dir("method")))).await;
    let resp = app(s)
        .oneshot(
            Request::get("/v1/word-feedback")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::METHOD_NOT_ALLOWED);
}

#[tokio::test]
async fn unset_config_answers_503_with_a_problem() {
    let s = state_with(None).await;
    let (status, headers, problem) = send(&s, request(&mark(DOC, WORD, "right"))).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(headers[header::CONTENT_TYPE], "application/problem+json");
    assert_eq!(problem["type"], "/errors/unavailable");
    // Also under the same-origin prefix.
    let req = Request::post("/api/v1/word-feedback")
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(mark(DOC, WORD, "right")))
        .unwrap();
    assert_eq!(send(&s, req).await.0, StatusCode::SERVICE_UNAVAILABLE);
    // A bad body is still the client's error.
    assert_eq!(send(&s, request("{}")).await.0, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn do_not_track_does_not_stop_an_explicit_mark() {
    let root = dir("dnt");
    let fb = feedback(&root);
    let s = state_with(Some(fb.clone())).await;
    let req = Request::post("/v1/word-feedback")
        .header(header::CONTENT_TYPE, "application/json")
        .header("dnt", "1")
        .header("sec-gpc", "1")
        .body(Body::from(mark(DOC, WORD, "wrong")))
        .unwrap();
    assert_eq!(send(&s, req).await.0, StatusCode::NO_CONTENT);
    assert_eq!(stored(&root, &fb).await.len(), 1);
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_client_over_the_rate_limit_is_refused() {
    let mut cfg = config();
    cfg.rate_limit = Some(RateLimit {
        per_minute: NonZeroU32::new(1).unwrap(),
        burst: NonZeroU32::new(1).unwrap(),
    });
    cfg.trusted_proxy_hops = 1;
    let root = dir("limit");
    let state = Arc::new(
        AppState::new(cfg, Arc::new(fixture_backend()), refdata().await)
            .with_word_feedback(feedback(&root)),
    );
    let post = || {
        Request::post("/v1/word-feedback")
            .header(header::CONTENT_TYPE, "application/json")
            .header("x-forwarded-for", "203.0.113.9")
            .body(Body::from(mark(DOC, WORD, "right")))
            .unwrap()
    };
    assert_eq!(send(&state, post()).await.0, StatusCode::NO_CONTENT);
    let (status, headers, problem) = send(&state, post()).await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert_eq!(problem["type"], "/errors/rate-limited");
    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn a_mark_from_another_site_is_refused_and_one_without_origin_is_accepted() {
    let root = dir("origin");
    let fb = feedback(&root);
    let s = state_with(Some(fb.clone())).await;
    let own = format!("https://{}", s.config.site_host);
    let post = |origin: Option<&str>| {
        let mut req = Request::post("/v1/word-feedback").header(header::CONTENT_TYPE, "text/plain");
        if let Some(o) = origin {
            req = req.header(header::ORIGIN, o);
        }
        req.body(Body::from(mark(DOC, WORD, "right"))).unwrap()
    };
    let (status, _, _) = send(&s, post(Some("https://evil.example"))).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, _) = send(&s, post(Some(&own))).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    let (status, _, _) = send(&s, post(None)).await;
    assert_eq!(status, StatusCode::NO_CONTENT);
    // Only the two accepted marks were stored.
    assert_eq!(stored(&root, &fb).await.len(), 2);
}
