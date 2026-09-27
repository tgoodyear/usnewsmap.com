//! BlobStore and ManagedIdentity against a loopback server that checks the
//! headers Azure requires and emulates Get Blob / Put Blob semantics.

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

async fn put_blob(
    State(b): State<Blobs>,
    Path(p): Path<String>,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    if !authorized(&h) || h.get("x-ms-blob-type").is_none() {
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
        .route("/c/{*p}", get(get_blob).put(put_blob))
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
