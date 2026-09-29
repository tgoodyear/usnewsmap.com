mod aggregate;
mod coverage;
mod hits;
mod meta;

pub use aggregate::aggregate;
pub(crate) use aggregate::aggregate_in;
pub use coverage::coverage;
pub(crate) use coverage::coverage_in;
pub use hits::hits;
pub(crate) use meta::places_in;
pub use meta::{meta, places, readyz};

use std::future::Future;
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use sha2::{Digest, Sha256};
use usnm_store::ObjectStore;

use crate::error::ApiError;
use crate::telemetry::ServedVersion;
use crate::version::{self, Pinning};
use crate::{AppState, Snapshot};

/// What a version-scoped response is computed from. Visitors' requests use
/// the serving snapshot and the request timeout; a warm-up (`crate::prewarm`)
/// passes the version about to be published and its own, longer timeout.
pub(crate) struct Ctx {
    pub snap: Arc<Snapshot>,
    /// Limit on each backend call, including the wait for a permit.
    pub timeout: Duration,
    /// A warm-up, not a visitor: always compute on an in-process miss (so the
    /// search engine's caches fill too), and leave the request metrics alone.
    pub warm_up: bool,
}

impl Ctx {
    pub(crate) fn serving(state: &AppState) -> Self {
        Self {
            snap: state.snapshot.load_full(),
            timeout: state.config.search_timeout,
            warm_up: false,
        }
    }
}

/// Bump when a response body's shape changes, so a new release never serves
/// persisted bodies written by an older one for the same index version.
pub(crate) const RESPONSE_FORMAT: u32 = 1;

/// A persistent-cache read slower than this is abandoned and the response computed.
const PERSISTED_READ_TIMEOUT: Duration = Duration::from_secs(2);

/// Largest decompressed body read back from the persistent cache.
const MAX_PERSISTED_BYTES: u64 = 64 * 1024 * 1024;

/// Object path for a cache key: `{index_version}/f{format}/{sha256(key)}.json.zst`.
/// Keys contain search text, so only their hash appears in the path.
pub(crate) fn persisted_path(serving: &str, key: &str) -> String {
    let digest: String = Sha256::digest(key.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{serving}/f{RESPONSE_FORMAT}/{digest}.json.zst")
}

async fn read_persisted(store: &dyn ObjectStore, path: &str) -> Option<Vec<u8>> {
    let bytes = match tokio::time::timeout(PERSISTED_READ_TIMEOUT, store.get(path)).await {
        Ok(Ok(found)) => found?,
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "persistent cache read failed");
            return None;
        }
        Err(_) => {
            tracing::warn!("persistent cache read timed out");
            return None;
        }
    };
    let mut body = Vec::new();
    let decoded = zstd::stream::read::Decoder::new(bytes.as_slice())
        .and_then(|d| d.take(MAX_PERSISTED_BYTES + 1).read_to_end(&mut body));
    match decoded {
        // Only a complete JSON document counts as a hit.
        Ok(n)
            if (n as u64) <= MAX_PERSISTED_BYTES
                && serde_json::from_slice::<serde::de::IgnoredAny>(&body).is_ok() =>
        {
            Some(body)
        }
        Ok(_) => {
            tracing::warn!("persistent cache entry is invalid; recomputing");
            None
        }
        Err(e) => {
            tracing::warn!(error = %e, "persistent cache entry is corrupt");
            None
        }
    }
}

/// Compress and store a computed body in the background; failures only log.
fn persist(store: Arc<dyn ObjectStore>, path: String, body: Arc<Vec<u8>>) {
    tokio::spawn(async move {
        let compressed =
            tokio::task::spawn_blocking(move || zstd::encode_all(body.as_slice(), 3)).await;
        let result = match compressed {
            Ok(Ok(bytes)) => store
                .put_new(&path, bytes, "application/zstd")
                .await
                .map_err(|e| e.to_string()),
            Ok(Err(e)) => Err(e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        if let Err(e) = result {
            tracing::warn!(error = %e, "persistent cache write failed");
        }
    });
}

/// Serve `compute()` through the in-process cache, then the persistent
/// cache, with version-aware headers. Concurrent identical requests share one
/// computation. A visitor waits at most the request timeout (plus the
/// persistent read's allowance), whether it computes the body or waits on an
/// identical computation, such as a warm-up's with its longer limit. A
/// warm-up skips the persistent read, so the backend runs and its caches
/// warm; the result is still persisted if it was slow.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn cached<F, T>(
    state: &AppState,
    warm_up: bool,
    key: String,
    pinning: &Pinning,
    serving: &str,
    path: &str,
    canonical: &str,
    compute: F,
) -> Result<Response, ApiError>
where
    F: Future<Output = Result<T, ApiError>>,
    T: Serialize,
{
    let persistent = state
        .responses
        .clone()
        .map(|store| (store, persisted_path(serving, &key)));
    let persist_after = state.config.persist_after;
    let metrics = &state.metrics;
    // Set when this request fills the in-process entry; otherwise the body
    // came from the cache (or from an identical request computing it).
    let filled = AtomicBool::new(false);
    let lookup = state.cache.try_get_with(key, async {
        filled.store(true, Ordering::Relaxed);
        if let Some((store, path)) = persistent.as_ref().filter(|_| !warm_up) {
            let found = read_persisted(store.as_ref(), path).await;
            metrics.cache("blob", found.is_some());
            if let Some(body) = found {
                return Ok(Arc::new(body));
            }
        }
        let started = Instant::now();
        let value = compute.await?;
        let body = serde_json::to_vec(&value)
            .map(Arc::new)
            .map_err(|e| ApiError::Backend(e.to_string()))?;
        if let Some((store, path)) = persistent {
            if started.elapsed() >= persist_after {
                persist(store, path, body.clone());
            }
        }
        Ok(body)
    });
    // Giving up drops only this request's part: when it was waiting on an
    // identical computation (a warm-up's, say), that computation carries on.
    let body = if warm_up {
        lookup.await
    } else {
        let deadline = state.config.search_timeout + PERSISTED_READ_TIMEOUT;
        tokio::time::timeout(deadline, lookup)
            .await
            .unwrap_or_else(|_| Err(Arc::new(ApiError::Timeout)))
    }
    .map_err(|e: Arc<ApiError>| Arc::try_unwrap(e).unwrap_or_else(|e| e.as_ref().clone()));
    // A request that waited on an identical one that failed counts as neither.
    let filled = filled.load(Ordering::Relaxed);
    if !warm_up && (filled || body.is_ok()) {
        metrics.cache("memory", !filled);
    }
    let body = body?;
    let mut resp = body.as_ref().clone().into_response();
    let headers = resp.headers_mut();
    headers.extend(version::cache_headers(
        pinning, serving, path, canonical, &body,
    ));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    resp.extensions_mut()
        .insert(ServedVersion(serving.to_owned()));
    Ok(resp)
}

pub(crate) async fn with_timeout<T>(
    state: &AppState,
    timeout: Duration,
    fut: impl Future<Output = Result<T, usnm_search::SearchError>>,
) -> Result<T, ApiError> {
    // Waiting for a permit counts against the timeout, so a saturated
    // backend sheds load as 503s instead of queueing without bound.
    let run = async {
        let _permit = state.permits.acquire().await;
        fut.await
    };
    match tokio::time::timeout(timeout, run).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(_) => Err(ApiError::Timeout),
    }
}

/// The mount prefix of a request path, e.g. `/api/v1` for `/api/v1/aggregate`.
pub(crate) fn mount_prefix<'a>(path: &'a str, endpoint: &str) -> &'a str {
    path.strip_suffix(endpoint).unwrap_or("/v1")
}

pub(crate) fn uses_fuzzy(node: &usnm_core::query::Node) -> bool {
    use usnm_core::query::Node;
    match node {
        Node::Term(t) => t.fuzzy > 0,
        Node::Phrase { .. } => false,
        Node::And(c) | Node::Or(c) => c.iter().any(uses_fuzzy),
        Node::Not(n) => uses_fuzzy(n),
    }
}
