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

/// Staged blocks, committed blobs and their tiers, for Put Block and Put
/// Block List.
/// A committed blob: its bytes, access tier and content type.
type Committed = (Vec<u8>, Option<String>, Option<String>);

#[derive(Default)]
struct Blocks {
    staged: HashMap<(String, String), Vec<u8>>,
    committed: HashMap<String, Committed>,
}

type SharedBlocks = Arc<Mutex<Blocks>>;

async fn put_block_or_list(
    State(b): State<SharedBlocks>,
    Path(p): Path<String>,
    Query(q): Query<HashMap<String, String>>,
    h: HeaderMap,
    body: Bytes,
) -> StatusCode {
    if !authorized(&h) {
        return StatusCode::FORBIDDEN;
    }
    let mut b = b.lock().unwrap();
    match q.get("comp").map(String::as_str) {
        Some("block") => {
            let id = q.get("blockid").unwrap().clone();
            b.staged.insert((p, id), body.to_vec());
            StatusCode::CREATED
        }
        Some("blocklist") => {
            let xml = String::from_utf8(body.to_vec()).unwrap();
            let mut data = Vec::new();
            for id in xml.split("<Latest>").skip(1) {
                let id = id.split("</Latest>").next().unwrap().to_owned();
                data.extend(b.staged.get(&(p.clone(), id)).unwrap());
            }
            let header = |k: &str| h.get(k).map(|v| v.to_str().unwrap().to_owned());
            let entry = (
                data,
                header("x-ms-access-tier"),
                header("x-ms-blob-content-type"),
            );
            b.committed.insert(p, entry);
            StatusCode::CREATED
        }
        _ => StatusCode::BAD_REQUEST,
    }
}

async fn serve_blocks() -> (String, SharedBlocks) {
    let blocks: SharedBlocks = Arc::default();
    let app = Router::new()
        .route("/c/big/object", get(chunked))
        .route("/c/{*p}", axum::routing::put(put_block_or_list))
        // 8 MiB blocks, past axum's 2 MB default.
        .layer(axum::extract::DefaultBodyLimit::disable())
        .with_state(blocks.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (format!("http://127.0.0.1:{}", addr.port()), blocks)
}

/// A streamed upload is cut into blocks and committed, in its tier, only
/// when told to; an abandoned one leaves nothing visible.
#[tokio::test]
async fn streamed_uploads_commit_blocks_in_their_tier() {
    let (base, blocks) = serve_blocks().await;
    let cred = Arc::new(Credential::new(StaticToken("secret-token".into())));
    let store = BlobStore::new(&format!("{base}/c"), cred).unwrap();
    let data: Vec<u8> = (0..(usnm_store::blob::BLOCK_BYTES * 2 + 12345))
        .map(|i| (i % 251) as u8)
        .collect();
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let (done, commit) = tokio::sync::oneshot::channel();
    let upload = {
        let store = BlobStore::new(
            &format!("{base}/c"),
            Arc::new(Credential::new(StaticToken("secret-token".into()))),
        )
        .unwrap();
        tokio::spawn(async move {
            store
                .put_stream(
                    "raw/b/b.tar.bz2",
                    "application/x-bzip2",
                    Some("Cold"),
                    rx,
                    commit,
                )
                .await
        })
    };
    for c in data.chunks(1_000_003) {
        tx.send(bytes::Bytes::copy_from_slice(c)).await.unwrap();
    }
    drop(tx);
    done.send(true).unwrap();
    assert_eq!(upload.await.unwrap().unwrap(), data.len() as u64);
    {
        let b = blocks.lock().unwrap();
        let (body, tier, ctype) = &b.committed["raw/b/b.tar.bz2"];
        assert_eq!(body, &data);
        assert_eq!(tier.as_deref(), Some("Cold"));
        assert_eq!(ctype.as_deref(), Some("application/x-bzip2"));
        assert_eq!(b.staged.len(), 3);
    }

    // Abandoned: the sender goes away without a commit.
    let (tx, rx) = tokio::sync::mpsc::channel(4);
    let (done, commit) = tokio::sync::oneshot::channel::<bool>();
    tx.send(bytes::Bytes::from_static(b"partial"))
        .await
        .unwrap();
    drop(tx);
    drop(done);
    let err = store
        .put_stream("raw/x/x.tar.bz2", "application/x-bzip2", None, rx, commit)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("abandoned"), "{err}");
    assert!(!blocks
        .lock()
        .unwrap()
        .committed
        .contains_key("raw/x/x.tar.bz2"));

    // A streamed read has no size limit.
    use futures::StreamExt;
    let mut s = store.get_stream("big/object").await.unwrap().unwrap();
    let mut n = 0u64;
    while let Some(c) = s.next().await {
        n += c.unwrap().len() as u64;
    }
    assert!(n > usnm_store::MAX_OBJECT_BYTES);
}
