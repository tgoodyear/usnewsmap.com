mod aggregate;
mod beacon;
mod coverage;
mod hits;
mod meta;

pub use aggregate::aggregate;
pub(crate) use aggregate::aggregate_in;
pub use beacon::beacon;
pub(crate) use beacon::is_bot;
pub use coverage::coverage;
pub(crate) use coverage::coverage_in;
pub use hits::hits;
pub(crate) use meta::places_in;
pub use meta::{meta, places, readyz};

use std::future::Future;
use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::FutureExt;
use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use tokio::time::Instant;
use usnm_store::ObjectStore;

use crate::error::ApiError;
use crate::flights::{Entry, Interest, Landing};
use crate::telemetry::ServedVersion;
use crate::version::{self, Pinning};
use crate::{AppState, Snapshot};

/// What a version-scoped response is computed from. Visitors' requests use
/// the serving snapshot and the computation limit (`compute_cap`); a warm-up
/// (`crate::prewarm`) passes the version about to be published and its own
/// limit.
pub(crate) struct Ctx {
    pub snap: Arc<Snapshot>,
    /// Limit on the computation and on each backend call in it, including
    /// the wait for a backend permit. Not how long a visitor waits: that is
    /// [`visitor_wait`].
    pub timeout: Duration,
    /// A warm-up, not a visitor: always compute on an in-process miss (so the
    /// search engine's caches fill too), and leave the request metrics alone.
    pub warm_up: bool,
}

impl Ctx {
    pub(crate) fn serving(state: &AppState) -> Self {
        Self {
            snap: state.snapshot.load_full(),
            timeout: state.config.compute_cap,
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

/// How one cached response is computed and waited for.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Job {
    /// The endpoint, for metrics (`aggregate`, `hits`, ...).
    pub endpoint: &'static str,
    /// A warm-up, not a visitor: skip the persistent read (so the backend
    /// runs and its caches warm), wait for the result however long it takes
    /// (within `limit`), and leave the visitor metrics alone.
    pub warm_up: bool,
    /// A search: it takes one of the `compute_concurrency` slots while it
    /// runs. The reference-data responses (places, coverage) don't.
    pub search: bool,
    /// Limit on the whole computation, after which it is cancelled.
    pub limit: Duration,
}

impl Job {
    /// A search on the backend.
    pub(crate) fn search(endpoint: &'static str, warm_up: bool, limit: Duration) -> Self {
        Self {
            endpoint,
            warm_up,
            search: true,
            limit,
        }
    }

    /// A response computed from reference data alone.
    pub(crate) fn reference(endpoint: &'static str, warm_up: bool, limit: Duration) -> Self {
        Self {
            endpoint,
            warm_up,
            search: false,
            limit,
        }
    }
}

/// Seconds a client waits before asking again for a response still being computed.
pub(crate) const COMPUTING_RETRY_SECS: u64 = 2;

/// How long a visitor's request waits for a response before the `202`: the
/// search timeout, plus the allowance for a persistent-cache read when
/// there is a persistent cache.
pub(crate) fn visitor_wait(state: &AppState) -> Duration {
    match state.responses {
        Some(_) => state.config.search_timeout + PERSISTED_READ_TIMEOUT,
        None => state.config.search_timeout,
    }
}

/// `202 Accepted`: the response is still being computed; the same request
/// later gets it (06 §6.3.5). Never cached; carries nothing from the query.
fn computing(serving: &str) -> Response {
    let mut resp = (
        StatusCode::ACCEPTED,
        Json(json!({ "status": "computing", "retry_after": COMPUTING_RETRY_SECS })),
    )
        .into_response();
    let headers = resp.headers_mut();
    headers.insert(header::RETRY_AFTER, HeaderValue::from(COMPUTING_RETRY_SECS));
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp.extensions_mut()
        .insert(ServedVersion(serving.to_owned()));
    resp
}

/// Where a request's body comes from.
enum Source {
    Cached(Arc<Vec<u8>>),
    /// A computation; `started` when this request started it.
    Flight {
        entry: Entry,
        started: bool,
    },
}

/// Serve `compute()` through the in-process cache, then the persistent
/// cache, with version-aware headers. Concurrent identical requests share one
/// computation, which runs in a task of its own (see [`crate::flights`]). A
/// visitor waits at most [`visitor_wait`], whether it computes the body or
/// waits on an identical computation (such as a warm-up's): past that it gets
/// `202 Accepted` and the computation carries on, up to `job.limit` and as
/// long as someone keeps asking, so that asking again later finds it running
/// or done. A warm-up skips the persistent read, so the backend runs and its
/// caches warm; the result is still persisted if it was slow.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn cached<F, T>(
    state: &Arc<AppState>,
    job: Job,
    key: String,
    pinning: &Pinning,
    serving: &str,
    path: &str,
    canonical: &str,
    compute: F,
) -> Result<Response, ApiError>
where
    F: Future<Output = Result<T, ApiError>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    cached_body(state, job, key, pinning, serving, path, canonical, compute)
        .await
        .map(|(resp, _)| resp)
}

/// [`cached`], also returning the body the response was built from; `None`
/// when the response is the `202`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn cached_body<F, T>(
    state: &Arc<AppState>,
    job: Job,
    key: String,
    pinning: &Pinning,
    serving: &str,
    path: &str,
    canonical: &str,
    compute: F,
) -> Result<(Response, Option<Arc<Vec<u8>>>), ApiError>
where
    F: Future<Output = Result<T, ApiError>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let wait = (!job.warm_up).then(|| Instant::now() + visitor_wait(state));
    let persistent = state
        .responses
        .clone()
        .map(|store| (store, persisted_path(serving, &key)));
    let metrics = &state.metrics;
    let body = match find_or_start(state, job, &key, wait, persistent, compute).await {
        Source::Cached(body) => {
            if !job.warm_up {
                metrics.cache("memory", true);
            }
            body
        }
        Source::Flight { entry, started } => {
            if started && !job.warm_up {
                metrics.cache("memory", false);
            }
            // Keeps the computation from being abandoned while this waits.
            let _watching = entry.interest.watch();
            let result = match wait {
                None => entry.flight.await,
                Some(at) => match tokio::time::timeout_at(at, entry.flight).await {
                    Ok(r) => r,
                    // Giving up drops only this request's wait: the
                    // computation carries on in its own task.
                    Err(_) => return Ok((computing(serving), None)),
                },
            };
            // A request that waited on an identical one that failed counts as neither.
            if !started && !job.warm_up && result.is_ok() {
                metrics.cache("memory", true);
            }
            result?
        }
    };
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
    Ok((resp, Some(body)))
}

/// Join the computation running for `key`, take its cached body, or start
/// it. `wait` is when this request stops waiting (`None` for a warm-up).
async fn find_or_start<F, T>(
    state: &Arc<AppState>,
    job: Job,
    key: &str,
    wait: Option<Instant>,
    persistent: Option<(Arc<dyn ObjectStore>, String)>,
    compute: F,
) -> Source
where
    F: Future<Output = Result<T, ApiError>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let flights = &state.flights;
    if let Some(entry) = flights.get(key) {
        return Source::Flight {
            entry,
            started: false,
        };
    }
    if let Some(body) = state.cache.get(key).await {
        return Source::Cached(body);
    }
    loop {
        {
            let mut running = flights.lock();
            if let Some(entry) = running.get(key) {
                return Source::Flight {
                    entry: entry.clone(),
                    started: false,
                };
            }
            // A computation stores its body before it leaves the map, so
            // under the lock the body is either running or cached (unless
            // it failed or has been evicted, and then it's computed again).
            if !state.cache.contains_key(key) {
                let entry = start(state, job, key.to_owned(), wait, persistent, compute);
                running.insert(key.to_owned(), entry.clone());
                return Source::Flight {
                    entry,
                    started: true,
                };
            }
        }
        if let Some(body) = state.cache.get(key).await {
            return Source::Cached(body);
        }
    }
}

/// Spawn the computation for `key`. It ends at `job.limit` at the latest,
/// or once nobody has waited on it for `abandon_after` (not a warm-up's),
/// stores a successful body in the in-process cache, and leaves the map (see
/// [`Landing`]) after that. `wait` is when the request that started it stops
/// waiting: a search that can't get a slot by then fails as busy.
fn start<F, T>(
    state: &Arc<AppState>,
    job: Job,
    key: String,
    wait: Option<Instant>,
    persistent: Option<(Arc<dyn ObjectStore>, String)>,
    compute: F,
) -> Entry
where
    F: Future<Output = Result<T, ApiError>> + Send + 'static,
    T: Serialize + Send + 'static,
{
    let state = state.clone();
    let interest = Interest::new();
    let watched = interest.clone();
    let task = tokio::spawn(async move {
        let landing = Landing {
            state: state.clone(),
            key,
        };
        let started = Instant::now();
        let work = tokio::time::timeout(job.limit, produce(&state, job, wait, persistent, compute));
        let after = state.config.abandon_after;
        let abandoned = async {
            if job.warm_up {
                return std::future::pending().await;
            }
            let tick = (after / 4).clamp(Duration::from_millis(10), Duration::from_secs(1));
            loop {
                tokio::time::sleep(tick).await;
                if watched.abandoned(after) {
                    return;
                }
            }
        };
        let (result, outcome) = tokio::select! {
            r = work => match r {
                Ok(Ok(body)) => (Ok(body), "ok"),
                Ok(Err(ApiError::Busy)) => (Err(ApiError::Busy), "busy"),
                Ok(Err(ApiError::Timeout)) | Err(_) => (Err(ApiError::Timeout), "timeout"),
                Ok(Err(e)) => (Err(e), "error"),
            },
            () = abandoned => (Err(ApiError::Timeout), "abandoned"),
        };
        if let Ok(body) = &result {
            state.cache.insert(landing.key.clone(), body.clone()).await;
        }
        drop(landing);
        if !job.warm_up
            && job.search
            && (outcome == "busy" || started.elapsed() >= visitor_wait(&state))
        {
            state.metrics.slow_search(job.endpoint, outcome);
        }
        result
    });
    let flight = async move {
        task.await.unwrap_or_else(|e| {
            // Logged once here, not by every waiter.
            tracing::error!(panicked = e.is_panic(), "a response computation failed");
            Err(ApiError::Backend("the computation failed".into()))
        })
    }
    .boxed()
    .shared();
    Entry { flight, interest }
}

/// The body: from the persistent cache (visitors only), else computed, and
/// persisted when it was slow. A search computes only once it has a slot:
/// a visitor's waits until shortly before `wait` and is otherwise busy; the
/// warm-up uses its own slot.
async fn produce<F, T>(
    state: &AppState,
    job: Job,
    wait: Option<Instant>,
    persistent: Option<(Arc<dyn ObjectStore>, String)>,
    compute: F,
) -> Result<Arc<Vec<u8>>, ApiError>
where
    F: Future<Output = Result<T, ApiError>>,
    T: Serialize,
{
    if let Some((store, path)) = persistent.as_ref().filter(|_| !job.warm_up) {
        let found = read_persisted(store.as_ref(), path).await;
        state.metrics.cache("blob", found.is_some());
        if let Some(body) = found {
            return Ok(Arc::new(body));
        }
    }
    let _slot = if job.search {
        let slots = if job.warm_up {
            &state.flights.warm_up_slot
        } else {
            &state.flights.slots
        };
        let acquire = slots.clone().acquire_owned();
        let acquired = match wait {
            None => acquire.await,
            // A little before the request stops waiting, so it hears that
            // the API is busy rather than a 202 for a search that isn't running.
            Some(at) => {
                let margin = visitor_wait(state).min(Duration::from_secs(2)) / 4;
                tokio::time::timeout_at(at - margin, acquire)
                    .await
                    .map_err(|_| ApiError::Busy)?
            }
        };
        // The semaphores are never closed.
        Some(acquired.map_err(|e| ApiError::Backend(e.to_string()))?)
    } else {
        None
    };
    let started = Instant::now();
    let value = compute.await?;
    let body = serde_json::to_vec(&value)
        .map(Arc::new)
        .map_err(|e| ApiError::Backend(e.to_string()))?;
    if let Some((store, path)) = persistent {
        if started.elapsed() >= state.config.persist_after {
            persist(store, path, body.clone());
        }
    }
    Ok(body)
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

/// The pipeline status document (see [`crate::status`]).
pub async fn status(axum::extract::State(state): axum::extract::State<Arc<AppState>>) -> Response {
    let (body, version) = state.status.get(&state).await;
    let mut resp = body.as_ref().clone().into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=30"),
    );
    // The version the document describes, which may be older than the
    // snapshot serving now if it was built before a reload.
    resp.extensions_mut().insert(ServedVersion(version));
    resp
}
