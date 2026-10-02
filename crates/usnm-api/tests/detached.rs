//! Searches that outlast a visitor's wait (06 §6.3.5, §6.5): `202` while a
//! search computes in its own task, one computation per key however many
//! requests ask, a hard limit per computation, a cap on how many run at
//! once, errors never cached, and a polled search logged once.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::searchlog::{self, LogConfig, Record, SearchLog};
use usnm_api::{app, AppState};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;
use usnm_search::memory::MemoryBackend;
use usnm_search::{
    Capabilities, CubeCell, HitsPage, HitsQuery, IndexSet, SearchBackend, SearchError, Summary,
};
use usnm_store::LocalStore;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

fn fixture_backend() -> MemoryBackend {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        let path = data_dir().join("indexes").join(format!("{id}.jsonl"));
        let docs: Vec<_> = std::fs::read_to_string(path)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str::<usnm_search::PageDoc>(l).unwrap())
            .collect();
        backend.add_index(id, docs);
    }
    backend
}

/// A visitor waits 100 ms; a computation may run 5 s.
fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.rate_limit = None;
    c.search_timeout = Duration::from_millis(100);
    c.compute_cap = Duration::from_secs(5);
    c
}

/// The fixture corpus behind a counter, with a delay per call and an
/// optional failure, both adjustable while the test runs.
struct Slow {
    inner: MemoryBackend,
    calls: AtomicUsize,
    /// Calls that have returned (or failed).
    done: AtomicUsize,
    delay_ms: AtomicU64,
    fail: AtomicBool,
}

impl Slow {
    fn new(delay: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: fixture_backend(),
            calls: AtomicUsize::new(0),
            done: AtomicUsize::new(0),
            delay_ms: AtomicU64::new(delay.as_millis() as u64),
            fail: AtomicBool::new(false),
        })
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    fn set_delay(&self, delay: Duration) {
        self.delay_ms
            .store(delay.as_millis() as u64, Ordering::SeqCst);
    }

    async fn enter(&self) -> Result<(), SearchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let delay = Duration::from_millis(self.delay_ms.load(Ordering::SeqCst));
        tokio::time::sleep(delay).await;
        self.done.fetch_add(1, Ordering::SeqCst);
        if self.fail.load(Ordering::SeqCst) {
            return Err(SearchError::Backend("engine down".into()));
        }
        Ok(())
    }
}

#[async_trait]
impl SearchBackend for Slow {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }
    async fn summary(
        &self,
        i: &IndexSet,
        q: &Node,
        f: &Filters,
        s: &BucketSpec,
    ) -> Result<Summary, SearchError> {
        self.enter().await?;
        self.inner.summary(i, q, f, s).await
    }
    async fn cube(
        &self,
        i: &IndexSet,
        q: &Node,
        f: &Filters,
        s: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        self.enter().await?;
        self.inner.cube(i, q, f, s, shards).await
    }
    async fn hits(
        &self,
        i: &IndexSet,
        q: &Node,
        f: &Filters,
        p: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        self.enter().await?;
        self.inner.hits(i, q, f, p).await
    }
    async fn health(&self) -> Result<(), SearchError> {
        Ok(())
    }
}

async fn state_with(cfg: Config, backend: Arc<Slow>) -> Arc<AppState> {
    let refdata = RefData::load(&LocalStore::new(data_dir())).await.unwrap();
    Arc::new(AppState::new(cfg, backend, refdata))
}

const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

/// A request as the site's page sends it.
fn from_site(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::USER_AGENT, CHROME)
        .header("sec-fetch-site", "same-origin")
        .body(Body::empty())
        .unwrap()
}

async fn send(state: &Arc<AppState>, req: Request<Body>) -> (StatusCode, HeaderMap, Value) {
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn get(state: &Arc<AppState>, uri: &str) -> (StatusCode, HeaderMap, Value) {
    send(state, from_site(uri)).await
}

/// Poll `uri` as the web app does until it isn't a `202`.
async fn poll(state: &Arc<AppState>, uri: &str) -> (StatusCode, Value, usize) {
    let mut accepted = 0;
    loop {
        let (status, _, body) = get(state, uri).await;
        if status != StatusCode::ACCEPTED {
            return (status, body, accepted);
        }
        accepted += 1;
        assert!(accepted < 200, "still computing");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Wait until no computation is in flight.
async fn settled(state: &Arc<AppState>) {
    for _ in 0..200 {
        if state.flights.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("computations still in flight: {}", state.flights.len());
}

const GOLD: &str = "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31";

#[tokio::test]
async fn a_slow_search_answers_202_then_its_result() {
    let backend = Slow::new(Duration::from_millis(400));
    let state = state_with(config(), backend.clone()).await;

    let started = Instant::now();
    let (status, headers, body) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::ACCEPTED, "{body}");
    assert!(started.elapsed() < Duration::from_millis(350));
    assert_eq!(headers[header::RETRY_AFTER], "2");
    assert_eq!(headers[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        headers[header::CONTENT_TYPE].to_str().unwrap(),
        "application/json"
    );
    // A status and a wait: nothing from the query.
    assert_eq!(
        body,
        serde_json::json!({ "status": "computing", "retry_after": 2 })
    );
    assert!(!body.to_string().contains("gold"));
    assert_eq!(backend.calls(), 1);

    // Asking again joins the same computation instead of starting another.
    let (status, _, _) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(backend.calls(), 1);

    let (status, body, _) = poll(&state, GOLD).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["total"]["hits"].as_u64().unwrap() > 0);
    settled(&state).await;
    let calls = backend.calls();
    // Cached now: no backend call, and an immediate answer.
    let started = Instant::now();
    let (status, headers, _) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::OK);
    assert!(started.elapsed() < Duration::from_millis(100));
    assert!(headers.contains_key(header::CONTENT_LOCATION));
    assert_eq!(backend.calls(), calls);
    assert_eq!(state.flights.free_slots(), state.config.compute_concurrency);
}

#[tokio::test]
async fn identical_requests_share_one_computation() {
    let backend = Slow::new(Duration::from_millis(150));
    let state = state_with(config(), backend.clone()).await;
    let requests: Vec<_> = (0..8)
        .map(|_| {
            let state = state.clone();
            tokio::spawn(async move { poll(&state, GOLD).await })
        })
        .collect();
    let mut bodies = Vec::new();
    for r in requests {
        let (status, body, _) = r.await.unwrap();
        assert_eq!(status, StatusCode::OK);
        bodies.push(body["total"].clone());
    }
    assert!(bodies.windows(2).all(|w| w[0] == w[1]));
    let one = backend.calls();
    // One computation's worth of calls: a fresh state computing it alone
    // makes as many.
    let alone = Slow::new(Duration::ZERO);
    let fresh = state_with(config(), alone.clone()).await;
    assert_eq!(poll(&fresh, GOLD).await.0, StatusCode::OK);
    assert_eq!(one, alone.calls());
}

#[tokio::test]
async fn a_visitor_who_leaves_does_not_cancel_the_search() {
    let backend = Slow::new(Duration::from_millis(300));
    let state = state_with(config(), backend.clone()).await;
    // The client hangs up after 20 ms.
    let gave_up = tokio::time::timeout(
        Duration::from_millis(20),
        app(state.clone()).oneshot(from_site(GOLD)),
    )
    .await;
    assert!(gave_up.is_err());
    settled(&state).await;
    let calls = backend.calls();
    assert!(calls >= 2, "the search carried on: {calls}");
    let (status, _, _) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(backend.calls(), calls, "served from the cache");
}

#[tokio::test]
async fn a_search_past_the_cap_fails_and_is_not_cached() {
    let backend = Slow::new(Duration::from_secs(30));
    let mut cfg = config();
    cfg.search_timeout = Duration::from_secs(5);
    cfg.compute_cap = Duration::from_millis(200);
    let state = state_with(cfg, backend.clone()).await;

    let started = Instant::now();
    let (status, headers, body) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["type"], "/errors/backend-timeout");
    assert!(headers.contains_key(header::RETRY_AFTER));
    assert!(started.elapsed() < Duration::from_secs(2));
    settled(&state).await;
    assert_eq!(state.flights.free_slots(), state.config.compute_concurrency);

    // Nothing was cached: the next request computes again, and succeeds.
    backend.set_delay(Duration::ZERO);
    let calls = backend.calls();
    let (status, _, _) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::OK);
    assert!(backend.calls() > calls);
}

#[tokio::test]
async fn errors_reach_every_waiter_and_are_not_cached() {
    let backend = Slow::new(Duration::from_millis(50));
    backend.fail.store(true, Ordering::SeqCst);
    let mut cfg = config();
    cfg.search_timeout = Duration::from_secs(2);
    let state = state_with(cfg, backend.clone()).await;
    let a = tokio::spawn({
        let state = state.clone();
        async move { get(&state, GOLD).await.0 }
    });
    let b = tokio::spawn({
        let state = state.clone();
        async move { get(&state, GOLD).await.0 }
    });
    assert_eq!(a.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(b.await.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(backend.calls(), 1, "one computation for both");
    settled(&state).await;

    backend.fail.store(false, Ordering::SeqCst);
    let (status, _, _) = get(&state, GOLD).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn searches_beyond_the_slots_get_503_busy() {
    let backend = Slow::new(Duration::from_millis(600));
    let mut cfg = config();
    cfg.compute_concurrency = 1;
    let state = state_with(cfg, backend.clone()).await;

    assert_eq!(get(&state, GOLD).await.0, StatusCode::ACCEPTED);
    assert_eq!(state.flights.free_slots(), 0);
    // Another search waits for the slot as long as a visitor waits, then
    // hears the API is busy.
    let started = Instant::now();
    let other = "/v1/aggregate?q=silver&from=1896-01-01&to=1896-12-31";
    let (status, headers, body) = get(&state, other).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["type"], "/errors/busy");
    assert_eq!(headers[header::RETRY_AFTER], "5");
    assert!(started.elapsed() >= Duration::from_millis(50));
    assert!(!body.to_string().contains("silver"));
    // The running search needs no slot to be asked about again.
    assert_eq!(get(&state, GOLD).await.0, StatusCode::ACCEPTED);
    // Cached results need none either.
    assert_eq!(poll(&state, GOLD).await.0, StatusCode::OK);
    settled(&state).await;
    assert_eq!(state.flights.free_slots(), 1);
    // A queued search gets the slot when it frees up within its wait.
    backend.set_delay(Duration::from_millis(30));
    let mut cfg = config();
    cfg.compute_concurrency = 1;
    cfg.search_timeout = Duration::from_secs(3);
    let state = state_with(cfg, backend.clone()).await;
    let first = tokio::spawn({
        let state = state.clone();
        async move { get(&state, GOLD).await.0 }
    });
    tokio::time::sleep(Duration::from_millis(5)).await;
    assert_eq!(get(&state, other).await.0, StatusCode::OK);
    assert_eq!(first.await.unwrap(), StatusCode::OK);
}

#[tokio::test]
async fn hits_are_detached_too() {
    let backend = Slow::new(Duration::from_millis(300));
    let state = state_with(config(), backend.clone()).await;
    let (_, _, meta) = get(&state, "/v1/meta").await;
    let v = meta["index_version"].as_str().unwrap();
    backend.set_delay(Duration::ZERO);
    let (_, _, agg) = get(&state, &format!("{GOLD}&v={v}")).await;
    let place = agg["places"]["id"][0].as_str().unwrap().to_owned();
    backend.set_delay(Duration::from_millis(300));
    let hits = format!("/v1/hits?q=gold&from=1896-01-01&to=1896-12-31&place={place}&v={v}");
    assert_eq!(get(&state, &hits).await.0, StatusCode::ACCEPTED);
    let (status, body, _) = poll(&state, &hits).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["total"].as_u64().unwrap() > 0);
}

#[tokio::test]
async fn a_polled_search_is_logged_once() {
    let dir = std::env::temp_dir().join(format!("usnm-detached-log-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let log = SearchLog::start(
        Arc::new(LocalStore::new(&dir)),
        LogConfig {
            flush_interval: Duration::from_secs(3600),
            ..LogConfig::default()
        },
        &opentelemetry::global::meter("test"),
    );
    let backend = Slow::new(Duration::from_millis(300));
    let refdata = RefData::load(&LocalStore::new(data_dir())).await.unwrap();
    let state = Arc::new(AppState::new(config(), backend, refdata).with_search_log(log.clone()));

    let (status, _, accepted) = poll(&state, GOLD).await;
    assert_eq!(status, StatusCode::OK);
    assert!(accepted >= 2, "polled {accepted} times");
    log.shutdown(Duration::from_secs(5)).await;
    let day = chrono::Utc::now().date_naive();
    let records: Vec<Record> = std::fs::read_dir(dir.join(searchlog::staging_dir(day)))
        .unwrap()
        .flat_map(|e| {
            std::fs::read_to_string(e.unwrap().path())
                .unwrap()
                .lines()
                .map(|l| serde_json::from_str(l).unwrap())
                .collect::<Vec<Record>>()
        })
        .collect();
    assert_eq!(records.len(), 1, "{records:?}");
    assert_eq!(records[0].q, "gold");
    assert!(records[0].pages.unwrap() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_fixture_switch_slows_matching_searches_only() {
    let mut cfg = config();
    cfg.fixture_slow = Some(("gold".into(), Duration::from_millis(300)));
    let backend = Slow::new(Duration::ZERO);
    let state = state_with(cfg, backend).await;
    assert_eq!(get(&state, GOLD).await.0, StatusCode::ACCEPTED);
    let silver = "/v1/aggregate?q=silver&from=1896-01-01&to=1896-12-31";
    assert_eq!(get(&state, silver).await.0, StatusCode::OK);
    assert_eq!(poll(&state, GOLD).await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_search_nobody_asks_about_any_more_is_cancelled() {
    let backend = Slow::new(Duration::from_secs(30));
    let mut cfg = config();
    cfg.compute_concurrency = 1;
    cfg.abandon_after = Duration::from_millis(300);
    let state = state_with(cfg, backend.clone()).await;
    assert_eq!(get(&state, GOLD).await.0, StatusCode::ACCEPTED);
    assert_eq!(state.flights.free_slots(), 0);
    // Asking again keeps it alive past the abandon time.
    for _ in 0..4 {
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert_eq!(get(&state, GOLD).await.0, StatusCode::ACCEPTED);
    }
    assert_eq!(state.flights.len(), 1);
    // The visitor stops asking: the search is cancelled and its slot freed.
    settled(&state).await;
    assert_eq!(state.flights.free_slots(), 1);
    assert_eq!(backend.calls(), 1, "never restarted");
}

#[tokio::test]
async fn persisted_results_need_no_slot() {
    let dir = std::env::temp_dir().join(format!("usnm-detached-blob-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store: Arc<dyn usnm_store::ObjectStore> = Arc::new(LocalStore::new(&dir));
    let silver = "/v1/aggregate?q=silver&from=1896-01-01&to=1896-12-31";
    // One replica computes silver and persists it.
    let mut cfg = config();
    cfg.persist_after = Duration::ZERO;
    let backend = Slow::new(Duration::ZERO);
    let first = Arc::new(
        AppState::new(
            cfg.clone(),
            backend.clone(),
            RefData::load(&LocalStore::new(data_dir())).await.unwrap(),
        )
        .with_response_store(store.clone()),
    );
    assert_eq!(get(&first, silver).await.0, StatusCode::OK);
    for _ in 0..100 {
        if std::fs::read_dir(&dir).map(|d| d.count()).unwrap_or(0) > 0 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    // Another, with its one slot taken by a slow search, still serves it.
    cfg.compute_concurrency = 1;
    let backend = Slow::new(Duration::from_secs(5));
    let second = Arc::new(
        AppState::new(
            cfg,
            backend.clone(),
            RefData::load(&LocalStore::new(data_dir())).await.unwrap(),
        )
        .with_response_store(store),
    );
    let slow = tokio::spawn({
        let state = second.clone();
        async move { get(&state, GOLD).await.0 }
    });
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(second.flights.free_slots(), 0);
    let (status, _, body) = get(&second, silver).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        backend.calls(),
        1,
        "only the slow search reached the backend"
    );
    slow.abort();
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn the_warm_up_has_its_own_slot() {
    let backend = Slow::new(Duration::from_millis(20));
    let mut cfg = config();
    cfg.compute_concurrency = 1;
    let state = state_with(cfg, backend.clone()).await;
    // A visitor's slow search holds the only slot.
    backend.set_delay(Duration::from_secs(5));
    assert_eq!(get(&state, GOLD).await.0, StatusCode::ACCEPTED);
    backend.set_delay(Duration::ZERO);
    let report = usnm_api::prewarm::run(
        &state,
        state.snapshot.load_full(),
        usnm_api::prewarm::Trigger::Startup,
    )
    .await;
    assert_eq!(report.ok, report.queries, "{report:?}");
}
