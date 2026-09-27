//! BlobStore and ManagedIdentity against a loopback server that checks the
//! headers Azure requires and emulates Get Blob / Put Blob semantics.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
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
) -> StatusCode {
    if !authorized(&h) || h.get("x-ms-blob-type").is_none() {
        return StatusCode::FORBIDDEN;
    }
    assert_eq!(h.get("if-none-match").unwrap(), "*");
    let mut m = b.lock().unwrap();
    if m.contains_key(&p) {
        return StatusCode::CONFLICT;
    }
    m.insert(p, body.to_vec());
    StatusCode::CREATED
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
}

#[tokio::test]
async fn auth_failures_are_errors_not_misses() {
    let (base, _) = serve().await;
    let cred = Arc::new(Credential::new(StaticToken("wrong".into())));
    let store = BlobStore::new(&format!("{base}/c"), cred).unwrap();
    let err = store.get("current.json").await.unwrap_err();
    assert!(err.to_string().contains("403"), "{err}");
}
