//! BlobStore and ManagedIdentity against a loopback server that checks the
//! headers Azure requires and emulates Get Blob, Get Blob Properties, Put
//! Blob and List Blobs semantics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::{Body, Bytes};
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use usnm_store::credential::{Credential, ManagedIdentity, StaticToken};
use usnm_store::{BlobStore, ObjectStore};

type Blobs = Arc<Mutex<HashMap<String, Vec<u8>>>>;

fn authorized(h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some("Bearer secret-token")
        && h.get("x-ms-version").is_some()
        && h.get("x-ms-date").is_some()
}

async fn get_blob(
    State(b): State<Blobs>,
    Path(p): Path<String>,
    h: HeaderMap,
) -> Result<Vec<u8>, StatusCode> {
    if !authorized(&h) {
        return Err(StatusCode::FORBIDDEN);
    }
    b.lock()
        .unwrap()
        .get(&p)
        .cloned()
        .ok_or(StatusCode::NOT_FOUND)
}

/// List Blobs, two names per page.
async fn list_blobs(
    State(b): State<Blobs>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
) -> Response {
    if !authorized(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    assert_eq!(q.get("restype").map(String::as_str), Some("container"));
    assert_eq!(q.get("comp").map(String::as_str), Some("list"));
    let prefix = q.get("prefix").cloned().unwrap_or_default();
    let mut names: Vec<String> = b
        .lock()
        .unwrap()
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect();
    names.sort();
    let start: usize = q.get("marker").map_or(0, |m| m.parse().unwrap());
    let page: String = names
        .iter()
        .skip(start)
        .take(2)
        .map(|n| format!("<Blob><Name>{n}</Name></Blob>"))
        .collect();
    let next = if start + 2 < names.len() {
        format!("<NextMarker>{}</NextMarker>", start + 2)
    } else {
        "<NextMarker />".to_owned()
    };
    format!("<?xml version=\"1.0\"?><EnumerationResults><Blobs>{page}</Blobs>{next}</EnumerationResults>")
        .into_response()
}

async fn head_blob(State(b): State<Blobs>, Path(p): Path<String>, h: HeaderMap) -> StatusCode {
    if !authorized(&h) {
        return StatusCode::FORBIDDEN;
    }
    if b.lock().unwrap().contains_key(&p) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

async fn put_blob(
    State(b): State<Blobs>,
    Path(p): Path<String>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if h.get("x-ms-blob-type").is_none() {
        return StatusCode::FORBIDDEN.into_response();
    }
    assert_eq!(h.get("if-none-match").unwrap(), "*");
    if p.starts_with("leased/") {
        return (
            StatusCode::CONFLICT,
            [("x-ms-error-code", "LeaseIdMissing")],
        )
            .into_response();
    }
    let mut m = b.lock().unwrap();
    if m.contains_key(&p) {
        return (
            StatusCode::CONFLICT,
            [("x-ms-error-code", "BlobAlreadyExists")],
        )
            .into_response();
    }
    m.insert(p, body.to_vec());
    StatusCode::CREATED.into_response()
}

async fn chunked(h: HeaderMap) -> Response {
    if !authorized(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    // No Content-Length: the body is streamed in chunks past the limit.
    let chunk = Bytes::from(vec![b'x'; 1024 * 1024]);
    let n = usnm_store::MAX_OBJECT_BYTES / chunk.len() as u64 + 2;
    let stream = futures::stream::iter((0..n).map(move |_| Ok::<_, std::io::Error>(chunk.clone())));
    Response::new(Body::from_stream(stream))
}

async fn identity(Query(q): Query<HashMap<String, String>>, h: HeaderMap) -> (StatusCode, String) {
    if h.get("x-identity-header").and_then(|v| v.to_str().ok()) != Some("hdr")
        || q.get("resource").map(String::as_str) != Some("https://storage.azure.com/")
        || q.get("client_id").map(String::as_str) != Some("cid")
    {
        return (StatusCode::BAD_REQUEST, String::new());
    }
    (
        StatusCode::OK,
        r#"{"access_token":"secret-token","expires_on":"4102444800"}"#.into(),
    )
}

async fn serve() -> (String, Blobs) {
    let blobs: Blobs = Arc::default();
    let app = Router::new()
        .route("/msi/token", get(identity))
        .route("/c/big/object", get(chunked))
        .route("/c", get(list_blobs))
        .route("/c/{*p}", get(get_blob).put(put_blob).head(head_blob))
        .with_state(blobs.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{}", addr.port()), blobs)
}

#[tokio::test]
async fn get_and_create_only_put_with_managed_identity() {
    let (base, blobs) = serve().await;
    let cred = Arc::new(Credential::new(ManagedIdentity::new(
        format!("{base}/msi/token"),
        "hdr".into(),
        Some("cid".into()),
    )));
    let store = BlobStore::new(&format!("{base}/c"), cred).unwrap();
    assert_eq!(store.get("v1/x.json").await.unwrap(), None);
    assert!(store
        .put_new("v1/x.json", b"1".to_vec(), "application/json")
        .await
        .unwrap());
    assert!(!store
        .put_new("v1/x.json", b"2".to_vec(), "application/json")
        .await
        .unwrap());
    assert_eq!(store.get("v1/x.json").await.unwrap().unwrap(), b"1");
    assert_eq!(blobs.lock().unwrap().len(), 1);
    // Other conflicts are errors, not "already exists".
    let err = store
        .put_new("leased/x.json", b"1".to_vec(), "application/json")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("409"), "{err}");
}

#[tokio::test]
async fn oversized_streams_stop_at_the_limit() {
    let (base, _) = serve().await;
    let cred = Arc::new(Credential::new(StaticToken("secret-token".into())));
    let store = BlobStore::new(&format!("{base}/c"), cred).unwrap();
    let err = store.get("big/object").await.unwrap_err();
    assert!(matches!(err, usnm_store::StoreError::TooLarge(_)), "{err}");
}

#[tokio::test]
async fn auth_failures_are_errors_not_misses() {
    let (base, _) = serve().await;
    let cred = Arc::new(Credential::new(StaticToken("wrong".into())));
    let store = BlobStore::new(&format!("{base}/c"), cred).unwrap();
    let err = store.get("current.json").await.unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
}

#[tokio::test]
async fn list_follows_markers_and_strips_the_store_prefix() {
    let (base, _) = serve().await;
    let cred = Arc::new(Credential::new(StaticToken("secret-token".into())));
    let store = BlobStore::new(&format!("{base}/c/sub"), cred.clone()).unwrap();
    for p in [
        "log/d/3.jsonl",
        "log/d/1.jsonl",
        "log/d/2.jsonl",
        "logs/x.jsonl",
    ] {
        assert!(store
            .put_new(p, b"1".to_vec(), "application/x-ndjson")
            .await
            .unwrap());
    }
    assert!(!store.exists("log/d/4.jsonl").await.unwrap());
    assert!(store.exists("log/d/1.jsonl").await.unwrap());
    assert_eq!(
        store.list("log").await.unwrap(),
        ["log/d/1.jsonl", "log/d/2.jsonl", "log/d/3.jsonl"]
    );
    assert!(store.list("none").await.unwrap().is_empty());
    // Wrong credentials are errors, not "missing" or "empty".
    let bad = BlobStore::new(
        &format!("{base}/c/sub"),
        Arc::new(Credential::new(StaticToken("wrong".into()))),
    )
    .unwrap();
    assert!(bad.exists("log/d/1.jsonl").await.is_err());
    assert!(bad.list("log").await.is_err());
}
