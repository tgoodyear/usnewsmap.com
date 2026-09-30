//! BlobStore and ManagedIdentity against a loopback server that checks the
//! headers Azure requires and emulates Get Blob, Get Blob Properties, Put
//! Blob and Append Block semantics.

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

/// Paths created as append blobs.
static APPEND_BLOBS: Mutex<Vec<String>> = Mutex::new(Vec::new());

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
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&h) {
        return StatusCode::FORBIDDEN.into_response();
    }
    if q.get("comp").map(String::as_str) == Some("appendblock") {
        let mut m = b.lock().unwrap();
        let Some(existing) = m.get_mut(&p) else {
            return (StatusCode::NOT_FOUND, [("x-ms-error-code", "BlobNotFound")]).into_response();
        };
        if !APPEND_BLOBS.lock().unwrap().contains(&p) {
            return (
                StatusCode::CONFLICT,
                [("x-ms-error-code", "InvalidBlobType")],
            )
                .into_response();
        }
        existing.extend_from_slice(&body);
        return StatusCode::CREATED.into_response();
    }
    if h.get("x-ms-blob-type").is_none() {
        return StatusCode::FORBIDDEN.into_response();
    }
    assert_eq!(h.get("if-none-match").unwrap(), "*");
    if h.get("x-ms-blob-type").unwrap() == "AppendBlob" {
        assert!(body.is_empty(), "an append blob is created empty");
        APPEND_BLOBS.lock().unwrap().push(p.clone());
    }
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
async fn append_creates_an_append_blob_then_adds_blocks() {
    let (base, blobs) = serve().await;
    let cred = Arc::new(Credential::new(StaticToken("secret-token".into())));
    let store = BlobStore::new(&format!("{base}/c"), cred).unwrap();
    assert!(!store.exists("log/2026-09-29.jsonl").await.unwrap());
    store
        .append(
            "log/2026-09-29.jsonl",
            b"a\n".to_vec(),
            "application/x-ndjson",
        )
        .await
        .unwrap();
    store
        .append(
            "log/2026-09-29.jsonl",
            b"b\n".to_vec(),
            "application/x-ndjson",
        )
        .await
        .unwrap();
    assert!(store.exists("log/2026-09-29.jsonl").await.unwrap());
    assert_eq!(
        blobs.lock().unwrap()["log/2026-09-29.jsonl"],
        b"a\nb\n".to_vec()
    );
    // A block blob at the path is an error, and so is a body over the limit.
    assert!(store
        .put_new("log/block.jsonl", b"x".to_vec(), "application/x-ndjson")
        .await
        .unwrap());
    let err = store
        .append("log/block.jsonl", b"y".to_vec(), "application/x-ndjson")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("409"), "{err}");
    let err = store
        .append(
            "log/big.jsonl",
            vec![b'x'; usnm_store::MAX_APPEND_BYTES + 1],
            "application/x-ndjson",
        )
        .await
        .unwrap_err();
    assert!(matches!(err, usnm_store::StoreError::TooLarge(_)), "{err}");
    // Wrong credentials are errors, not "missing".
    let bad = BlobStore::new(
        &format!("{base}/c"),
        Arc::new(Credential::new(StaticToken("wrong".into()))),
    )
    .unwrap();
    assert!(bad.exists("log/2026-09-29.jsonl").await.is_err());
}
