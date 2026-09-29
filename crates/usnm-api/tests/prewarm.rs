//! Cache warm-up before a version serves: at a reload and at start.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use usnm_api::config::Config;
use usnm_api::prewarm::{self, Trigger};
use usnm_api::{app, reload_if_changed, spawn_startup_warm_up, AppState, Engine, Loader};
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

fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.rate_limit = None;
    c.prewarm_retry_first = Duration::from_millis(10);
    c
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

/// The fixture corpus behind a counter, optionally slow or failing.
struct Counting {
    inner: MemoryBackend,
    calls: AtomicUsize,
    delay: Duration,
    fail: bool,
    /// Calls that fail before the backend starts answering (a searcher that
    /// is still starting).
    fail_first: AtomicUsize,
    /// Every call is refused as a bad request (a Quickwit 4xx).
    reject: bool,
}

impl Counting {
    fn new(delay: Duration, fail: bool) -> Arc<Self> {
        Arc::new(Self {
            inner: fixture_backend(),
            calls: AtomicUsize::new(0),
            delay,
            fail,
            fail_first: AtomicUsize::new(0),
            reject: false,
        })
    }

    fn rejecting() -> Arc<Self> {
        Arc::new(Self {
            inner: fixture_backend(),
            calls: AtomicUsize::new(0),
            delay: Duration::ZERO,
            fail: false,
            fail_first: AtomicUsize::new(0),
            reject: true,
        })
    }

    fn failing_first(n: usize) -> Arc<Self> {
        let b = Self::new(Duration::ZERO, false);
        b.fail_first.store(n, Ordering::SeqCst);
        b
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }

    async fn enter(&self) -> Result<(), SearchError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(self.delay).await;
        if self.fail {
            return Err(SearchError::Backend("engine down".into()));
        }
        if self.reject {
            return Err(SearchError::Rejected(
                "quickwit returned 400 Bad Request".into(),
            ));
        }
        let starting = self
            .fail_first
            .fetch_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok();
        if starting {
            return Err(SearchError::Backend("quickwit returned 500".into()));
        }
        Ok(())
    }
}

#[async_trait]
impl SearchBackend for Counting {
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

/// A fresh copy of the fixture reference data the test can publish into.
fn temp_reference(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-prewarm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("fixture-v1")).unwrap();
    for entry in std::fs::read_dir(data_dir().join("fixture-v1")).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(
            &path,
            dir.join("fixture-v1").join(path.file_name().unwrap()),
        )
        .unwrap();
    }
    std::fs::copy(data_dir().join("current.json"), dir.join("current.json")).unwrap();
    dir
}

/// Publish `fixture-v2`: the same indexes with its own reference snapshot.
fn publish_v2(dir: &Path) {
    std::fs::create_dir_all(dir.join("fixture-v2")).unwrap();
    for entry in std::fs::read_dir(dir.join("fixture-v1")).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(
            &path,
            dir.join("fixture-v2").join(path.file_name().unwrap()),
        )
        .unwrap();
    }
    let manifest = dir.join("fixture-v2/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["index_version"] = "fixture-v2".into();
    std::fs::write(manifest, m.to_string()).unwrap();
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    current["index_version"] = "fixture-v2".into();
    current["reference"] = "fixture-v2".into();
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();
}

/// A state serving `fixture-v1` from `dir`, with `backend` shared across versions.
async fn reloading_state(dir: &Path, cfg: Config, backend: Arc<Counting>) -> AppState {
    let loader = Loader {
        reference: Arc::new(LocalStore::new(dir)),
        engine: Engine::Shared(backend),
    };
    let snapshot = loader.snapshot().await.unwrap();
    AppState::with_loader(cfg, snapshot, Some(loader))
}

async fn get(state: &Arc<AppState>, uri: &str) -> (StatusCode, Value) {
    let resp = app(state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

async fn readyz(state: &Arc<AppState>) -> StatusCode {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .uri("/readyz")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    resp.status()
}

/// Everything the web app loads for each example at `version`: the places,
/// the search and the coverage it links to. Returns the backend calls it cost
/// and how many distinct responses it loaded.
async fn load_examples(state: &Arc<AppState>, backend: &Counting, version: &str) -> (usize, usize) {
    let before = backend.calls();
    let mut coverages = std::collections::HashSet::new();
    let (status, _) = get(state, &format!("/v1/places?v={version}")).await;
    assert_eq!(status, StatusCode::OK);
    for ex in prewarm::examples() {
        let (status, body) = get(
            state,
            &format!("/v1/aggregate?{}&v={version}", ex.aggregate),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{}: {body}", ex.id);
        assert_eq!(body["index_version"], version);
        assert!(body["total"]["hits"].as_u64().unwrap() > 0, "{}", ex.id);
        let coverage = body["cube"]["baseline_ref"].as_str().unwrap().to_owned();
        let (status, body) = get(state, &coverage).await;
        assert_eq!(status, StatusCode::OK, "{}: {body}", ex.id);
        coverages.insert(coverage);
    }
    let distinct = 1 + prewarm::examples().len() + coverages.len();
    (backend.calls() - before, distinct)
}

async fn visit_examples(state: &Arc<AppState>, backend: &Counting, version: &str) -> usize {
    load_examples(state, backend, version).await.0
}

fn persisted(dir: &Path, version: &str) -> usize {
    std::fs::read_dir(dir.join(version).join("f1"))
        .map(|d| d.count())
        .unwrap_or(0)
}

#[tokio::test]
async fn a_new_version_is_warmed_before_it_is_swapped_in() {
    let dir = temp_reference("publish");
    let cache = dir.join("cache");
    let mut cfg = config();
    cfg.persist_after = Duration::ZERO;
    let backend = Counting::new(Duration::ZERO, false);
    let state = Arc::new(
        reloading_state(&dir, cfg, backend.clone())
            .await
            .with_response_store(Arc::new(LocalStore::new(&cache))),
    );

    publish_v2(&dir);
    assert!(reload_if_changed(&state).await.unwrap());
    assert_eq!(state.snapshot.load().refdata.version(), "fixture-v2");
    assert!(backend.calls() > 0, "the warm-up searched the new version");

    // What the web app loads for each example is already cached for v2: no
    // backend call.
    let (calls, distinct) = load_examples(&state, &backend, "fixture-v2").await;
    assert_eq!(calls, 0);
    // A request without `v` (a shared link) has the same cache key.
    let before = backend.calls();
    let ex = &prewarm::examples()[0];
    let (status, body) = get(&state, &format!("/v1/aggregate?{}", ex.aggregate)).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["index_version"], "fixture-v2");
    assert_eq!(backend.calls(), before);

    // The slow ones (all of them, with `persist_after` = 0) were persisted
    // under the new version, for replicas that start later.
    for _ in 0..100 {
        if persisted(&cache, "fixture-v2") == distinct {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(persisted(&cache, "fixture-v2"), distinct);
    assert_eq!(persisted(&cache, "fixture-v1"), 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn warm_up_reports_what_it_ran() {
    let dir = temp_reference("report");
    let backend = Counting::new(Duration::ZERO, false);
    let state = Arc::new(reloading_state(&dir, config(), backend.clone()).await);
    let snap = state.snapshot.load_full();
    let report = prewarm::run(&state, snap.clone(), Trigger::Startup).await;
    let queries = 1 + 2 * prewarm::examples().len();
    assert_eq!(report.queries, queries);
    assert_eq!(report.ok, queries, "{report:?}");
    assert_eq!(visit_examples(&state, &backend, "fixture-v1").await, 0);

    // Again: everything is in the in-process cache already.
    let calls = backend.calls();
    let again = prewarm::run(&state, snap, Trigger::Startup).await;
    assert_eq!(again.ok, queries);
    assert_eq!(backend.calls(), calls);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_slow_backend_holds_the_swap_for_the_budget_at_most() {
    let dir = temp_reference("slow");
    let mut cfg = config();
    cfg.prewarm_query_timeout = Duration::from_millis(200);
    cfg.prewarm_budget = Duration::from_millis(500);
    let backend = Counting::new(Duration::from_secs(30), false);
    let state = Arc::new(reloading_state(&dir, cfg, backend.clone()).await);

    // Each search runs into the per-query limit, and the last ones into the budget.
    let started = Instant::now();
    let report = prewarm::run(&state, state.snapshot.load_full(), Trigger::Startup).await;
    assert!(started.elapsed() < Duration::from_secs(2), "{report:?}");
    assert_eq!(
        report.ok, 1,
        "only the places, which need no search: {report:?}"
    );
    assert!(report.timed_out >= 2, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(
        report.ok + report.timed_out + report.skipped,
        1 + prewarm::examples().len(),
        "no coverage without its search: {report:?}"
    );

    // A publish swaps once the budget is spent, and v1 serves meanwhile.
    publish_v2(&dir);
    let started = Instant::now();
    let reload = {
        let state = state.clone();
        tokio::spawn(async move { reload_if_changed(&state).await })
    };
    tokio::time::sleep(Duration::from_millis(100)).await;
    let (_, meta) = get(&state, "/v1/meta").await;
    assert_eq!(
        meta["index_version"], "fixture-v1",
        "v1 serves during the warm-up"
    );
    assert!(reload.await.unwrap().unwrap());
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_eq!(state.snapshot.load().refdata.version(), "fixture-v2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_searcher_that_is_still_starting_is_retried() {
    let dir = temp_reference("starting");
    let backend = Counting::failing_first(2);
    let state = Arc::new(reloading_state(&dir, config(), backend.clone()).await);
    let report = prewarm::run(&state, state.snapshot.load_full(), Trigger::Startup).await;
    let queries = 1 + 2 * prewarm::examples().len();
    assert_eq!(report.ok, queries, "{report:?}");
    assert_eq!(report.failed, 0, "{report:?}");
    assert_eq!(visit_examples(&state, &backend, "fixture-v1").await, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn slow_failures_share_one_limit_per_query() {
    let dir = temp_reference("slow-failing");
    let backend = Counting::new(Duration::from_millis(100), true);
    let mut cfg = config();
    let limit = Duration::from_millis(350);
    cfg.prewarm_query_timeout = limit;
    cfg.prewarm_budget = Duration::from_secs(30);
    let state = Arc::new(reloading_state(&dir, cfg, backend).await);
    let started = std::time::Instant::now();
    let report = prewarm::run(&state, state.snapshot.load_full(), Trigger::Startup).await;
    let n = prewarm::examples().len();
    // Every example got its turn: none used up the run's budget.
    assert_eq!(report.skipped, 0, "{report:?}");
    assert_eq!(report.failed + report.timed_out, n, "{report:?}");
    // Each gave up at about its own limit. Resetting the limit on every
    // attempt would take about three times as long here.
    let bound = limit * n as u32 + Duration::from_millis(800);
    assert!(
        started.elapsed() < bound,
        "{:?} >= {bound:?}",
        started.elapsed()
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_rejected_request_is_not_retried() {
    let dir = temp_reference("rejecting");
    let backend = Counting::rejecting();
    let state = Arc::new(reloading_state(&dir, config(), backend.clone()).await);
    let before = backend.calls();
    let report = prewarm::run(&state, state.snapshot.load_full(), Trigger::Startup).await;
    assert_eq!(report.failed, prewarm::examples().len(), "{report:?}");
    // One call per example search: none of them was tried again.
    assert_eq!(backend.calls() - before, prewarm::examples().len());
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_failing_backend_does_not_block_the_swap() {
    let dir = temp_reference("failing");
    let backend = Counting::new(Duration::ZERO, true);
    let state = Arc::new(reloading_state(&dir, config(), backend).await);
    let report = prewarm::run(&state, state.snapshot.load_full(), Trigger::Startup).await;
    assert_eq!(report.ok, 1, "{report:?}");
    assert_eq!(report.failed, prewarm::examples().len(), "{report:?}");

    publish_v2(&dir);
    assert!(reload_if_changed(&state).await.unwrap());
    assert_eq!(state.snapshot.load().refdata.version(), "fixture-v2");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn not_ready_until_warm() {
    let dir = temp_reference("ready");
    let backend = Counting::new(Duration::from_millis(20), false);
    let state = Arc::new(reloading_state(&dir, config(), backend.clone()).await);
    assert_eq!(readyz(&state).await, StatusCode::OK);

    let warming = spawn_startup_warm_up(state.clone());
    assert_eq!(readyz(&state).await, StatusCode::SERVICE_UNAVAILABLE);
    warming.await.unwrap();
    assert_eq!(readyz(&state).await, StatusCode::OK);
    // Ready means warm.
    assert_eq!(visit_examples(&state, &backend, "fixture-v1").await, 0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn ready_at_the_cap_even_if_still_warming() {
    let dir = temp_reference("cap");
    let mut cfg = config();
    cfg.ready_cap = Duration::from_millis(300);
    cfg.search_timeout = Duration::from_millis(200);
    let backend = Counting::new(Duration::from_secs(30), false);
    let state = Arc::new(reloading_state(&dir, cfg, backend.clone()).await);

    let started = Instant::now();
    let warming = spawn_startup_warm_up(state.clone());
    assert_eq!(readyz(&state).await, StatusCode::SERVICE_UNAVAILABLE);
    warming.await.unwrap();
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_millis(300) && waited < Duration::from_secs(2),
        "{waited:?}"
    );
    assert_eq!(readyz(&state).await, StatusCode::OK);
    // The warm-up carries on past the cap: the first search is still running.
    assert_eq!(backend.calls(), 1);

    // A visitor asking for that search waits on the warm-up's computation,
    // but only up to the visitor's own limit (plus the 2 s allowance for a
    // persistent-cache read); the warm-up keeps going.
    let first = &prewarm::examples()[0];
    let started = Instant::now();
    let (status, body) = get(&state, &format!("/v1/aggregate?{}", first.aggregate)).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(
        backend.calls(),
        1,
        "the visitor joined the warm-up's search"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
