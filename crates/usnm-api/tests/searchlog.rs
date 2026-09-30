//! The anonymous search log at the HTTP boundary (06 §6.8): which requests
//! are recorded, exactly once, what a record holds, batching and flushing,
//! the day files, and that search text never reaches the console log.

use std::collections::BTreeSet;
use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use chrono::{NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::searchlog::{self, LogConfig, Record, SearchLog};
use usnm_api::{app, prewarm, AppState};
use usnm_search::memory::MemoryBackend;
use usnm_search::PageDoc;
use usnm_store::{LocalStore, ObjectStore, StoreError};

const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

fn fixture_backend() -> MemoryBackend {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        let path = data_dir().join("indexes").join(format!("{id}.jsonl"));
        let docs: Vec<PageDoc> = std::io::BufReader::new(std::fs::File::open(path).unwrap())
            .lines()
            .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
            .collect();
        backend.add_index(id, docs);
    }
    backend
}

fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.cache_bytes = 16 * 1024 * 1024;
    c.rate_limit = None;
    c
}

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-searchlog-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn meter() -> opentelemetry::metrics::Meter {
    opentelemetry::global::meter("test")
}

/// An app whose search log writes to `store`.
async fn app_with(store: Arc<dyn ObjectStore>, log: LogConfig) -> (Arc<AppState>, Arc<SearchLog>) {
    let refdata = RefData::load(&LocalStore::new(data_dir())).await.unwrap();
    let log = SearchLog::start(store, log, &meter());
    let state = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata).with_search_log(log.clone()),
    );
    (state, log)
}

fn hour() -> LogConfig {
    LogConfig {
        flush_interval: Duration::from_secs(3600),
        ..LogConfig::default()
    }
}

/// A request as the site's page sends it: same origin, a browser.
fn from_site(uri: &str) -> Request<Body> {
    Request::builder()
        .uri(uri)
        .header(header::USER_AGENT, CHROME)
        .header("sec-fetch-site", "same-origin")
        .body(Body::empty())
        .unwrap()
}

async fn send(state: &Arc<AppState>, req: Request<Body>) -> (StatusCode, Value) {
    let resp = app(state.clone()).oneshot(req).await.unwrap();
    let status = resp.status();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(Value::Null),
    )
}

fn today() -> NaiveDate {
    Utc::now().date_naive()
}

/// The records in today's staging file (none if it doesn't exist).
fn staged(dir: &std::path::Path) -> Vec<Record> {
    let path = dir.join(searchlog::staging_path(today()));
    match std::fs::read_to_string(path) {
        Ok(text) => text
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        Err(_) => Vec::new(),
    }
}

async fn version(state: &Arc<AppState>) -> String {
    let (_, meta) = send(state, from_site("/v1/meta")).await;
    meta["index_version"].as_str().unwrap().to_owned()
}

#[tokio::test]
async fn one_search_on_the_site_is_one_record() {
    let dir = temp_dir("once");
    let (state, log) = app_with(Arc::new(LocalStore::new(&dir)), hour()).await;
    let v = version(&state).await;

    // What the site requests for one search: places, the aggregate, the
    // coverage cube it links to, and a page of hits for a place.
    send(&state, from_site(&format!("/v1/places?v={v}"))).await;
    let search = format!("/v1/aggregate?q=gold&mode=phrase&from=1896-01-01&to=1896-12-31&v={v}");
    let (status, agg) = send(&state, from_site(&search)).await;
    assert_eq!(status, StatusCode::OK);
    let coverage = agg["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert_eq!(send(&state, from_site(&coverage)).await.0, StatusCode::OK);
    let place = agg["places"]["id"][0].as_str().unwrap().to_owned();
    let hits = format!(
        "/v1/hits?q=gold&mode=phrase&from=1896-01-01&to=1896-12-31&place={place}&limit=20&v={v}"
    );
    assert_eq!(send(&state, from_site(&hits)).await.0, StatusCode::OK);
    assert_eq!(send(&state, from_site("/healthz")).await.0, StatusCode::OK);

    log.shutdown(Duration::from_secs(5)).await;
    let records = staged(&dir);
    assert_eq!(records.len(), 1, "{records:?}");
    let r = &records[0];
    assert_eq!(r.q, "gold");
    assert_eq!(r.mode.as_deref(), Some("phrase"));
    assert_eq!(r.source, "api");
    assert_eq!(r.day, today());
    assert_eq!(r.pages, agg["total"]["hits"].as_u64());
    assert_eq!(r.index_version.as_deref(), Some(v.as_str()));
    // Only the staging file: the day file is written after the day ends.
    assert!(!dir.join(searchlog::day_path(today())).exists());
}

#[tokio::test]
async fn cached_responses_count_and_failures_and_redirects_do_not() {
    let dir = temp_dir("cache");
    let (state, log) = app_with(Arc::new(LocalStore::new(&dir)), hour()).await;
    let v = version(&state).await;
    let search = format!("/v1/aggregate?q=gold&v={v}");
    // Computed, then from the in-process cache: two visitors, two searches.
    assert_eq!(send(&state, from_site(&search)).await.0, StatusCode::OK);
    assert_eq!(send(&state, from_site(&search)).await.0, StatusCode::OK);
    // A stale version is redirected; the request the browser then makes counts.
    let resp = app(state.clone())
        .oneshot(from_site("/v1/aggregate?q=silver&v=old"))
        .await
        .unwrap();
    assert!(resp.status().is_redirection(), "{}", resp.status());
    let location = resp.headers()[header::LOCATION]
        .to_str()
        .unwrap()
        .to_owned();
    assert_eq!(send(&state, from_site(&location)).await.0, StatusCode::OK);
    // Invalid searches get no results and aren't recorded.
    assert_eq!(
        send(&state, from_site("/v1/aggregate?q=text:gold")).await.0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        send(&state, from_site("/v1/aggregate?q=gold&from=x"))
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    log.shutdown(Duration::from_secs(5)).await;
    let records = staged(&dir);
    assert_eq!(records.len(), 3, "{records:?}");
    let gold: Vec<&Record> = records.iter().filter(|r| r.q == "gold").collect();
    assert_eq!(gold.len(), 2);
    assert_eq!(gold[0].pages, gold[1].pages);
    assert_eq!(records.iter().filter(|r| r.query == "silver").count(), 1);
}

#[tokio::test]
async fn opt_outs_bots_other_origins_and_the_warm_up_are_not_recorded() {
    let dir = temp_dir("excluded");
    let (state, log) = app_with(Arc::new(LocalStore::new(&dir)), hour()).await;
    let v = version(&state).await;
    let uri = format!("/v1/aggregate?q=gold&v={v}");
    let with = |extra: &[(&str, &str)]| {
        let mut b = Request::builder()
            .uri(&uri)
            .header(header::USER_AGENT, CHROME)
            .header("sec-fetch-site", "same-origin");
        for (k, v) in extra {
            b = b.header(*k, *v);
        }
        b.body(Body::empty()).unwrap()
    };
    for req in [
        with(&[("dnt", "1")]),
        with(&[("sec-gpc", "1")]),
        with(&[("origin", "https://example.com")]),
        Request::builder()
            .uri(&uri)
            .header(header::USER_AGENT, CHROME)
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap(),
        Request::builder()
            .uri(&uri)
            .header(
                header::USER_AGENT,
                "Mozilla/5.0 (compatible; Googlebot/2.1)",
            )
            .header("sec-fetch-site", "same-origin")
            .body(Body::empty())
            .unwrap(),
        // A script: no browser headers at all.
        Request::builder()
            .uri(&uri)
            .header(header::USER_AGENT, "curl/8.7.1")
            .body(Body::empty())
            .unwrap(),
    ] {
        assert_eq!(send(&state, req).await.0, StatusCode::OK);
    }
    // The warm-up runs the home page's example searches through the same
    // handler code.
    let report = prewarm::run(
        &state,
        state.snapshot.load_full(),
        prewarm::Trigger::Startup,
    )
    .await;
    assert!(report.ok > 1, "{report:?}");

    log.shutdown(Duration::from_secs(5)).await;
    assert!(staged(&dir).is_empty(), "{:?}", staged(&dir));
}

#[tokio::test]
async fn nothing_is_recorded_without_a_search_log() {
    let refdata = RefData::load(&LocalStore::new(data_dir())).await.unwrap();
    let state = Arc::new(AppState::new(
        config(),
        Arc::new(fixture_backend()),
        refdata,
    ));
    let v = version(&state).await;
    let (status, _) = send(&state, from_site(&format!("/v1/aggregate?q=gold&v={v}"))).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn batches_are_appended_on_the_interval_and_when_full() {
    // On the interval, without a shutdown.
    let dir = temp_dir("interval");
    let (state, log) = app_with(
        Arc::new(LocalStore::new(&dir)),
        LogConfig {
            flush_interval: Duration::from_millis(50),
            ..LogConfig::default()
        },
    )
    .await;
    let v = version(&state).await;
    send(&state, from_site(&format!("/v1/aggregate?q=gold&v={v}"))).await;
    let mut found = 0;
    for _ in 0..100 {
        found = staged(&dir).len();
        if found == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(found, 1);
    log.shutdown(Duration::from_secs(5)).await;

    // A full batch goes at once; the rest waits for the interval or shutdown.
    let dir = temp_dir("full");
    let (state, log) = app_with(
        Arc::new(LocalStore::new(&dir)),
        LogConfig {
            max_batch: 2,
            ..hour()
        },
    )
    .await;
    for q in ["gold", "silver", "cotton"] {
        send(&state, from_site(&format!("/v1/aggregate?q={q}&v={v}"))).await;
    }
    let mut found = 0;
    for _ in 0..100 {
        found = staged(&dir).len();
        if found >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!((found, staged(&dir).len()), (2, 2));
    log.shutdown(Duration::from_secs(5)).await;
    let qs: BTreeSet<String> = staged(&dir).into_iter().map(|r| r.q).collect();
    assert_eq!(qs, ["cotton", "gold", "silver"].map(String::from).into());
}

#[tokio::test]
async fn a_full_queue_drops_instead_of_waiting() {
    // A store that never answers: the writer is stuck on its first append.
    #[derive(Debug)]
    struct Stuck;
    #[async_trait]
    impl ObjectStore for Stuck {
        async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }
        async fn put_new(&self, _: &str, _: Vec<u8>, _: &str) -> Result<bool, StoreError> {
            Ok(true)
        }
        async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
        async fn append(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), StoreError> {
            std::future::pending().await
        }
    }
    let (state, log) = app_with(
        Arc::new(Stuck),
        LogConfig {
            queue: 1,
            max_batch: 1,
            ..hour()
        },
    )
    .await;
    let v = version(&state).await;
    let search = format!("/v1/aggregate?q=gold&v={v}");
    let started = std::time::Instant::now();
    for _ in 0..20 {
        assert_eq!(send(&state, from_site(&search)).await.0, StatusCode::OK);
    }
    assert!(started.elapsed() < Duration::from_secs(5));
    log.shutdown(Duration::from_millis(100)).await;
}

/// Console output captured from the JSON log layer.
#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[tokio::test]
async fn write_errors_are_logged_without_search_text_and_retried() {
    /// Fails the first append, then works.
    #[derive(Debug, Default)]
    struct Flaky {
        failed: Mutex<bool>,
        written: Mutex<Vec<u8>>,
    }
    #[async_trait]
    impl ObjectStore for Flaky {
        async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, StoreError> {
            Ok(None)
        }
        async fn put_new(&self, _: &str, _: Vec<u8>, _: &str) -> Result<bool, StoreError> {
            Ok(true)
        }
        async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), StoreError> {
            Ok(())
        }
        async fn append(&self, path: &str, body: Vec<u8>, _: &str) -> Result<(), StoreError> {
            let mut failed = self.failed.lock().unwrap();
            if !*failed {
                *failed = true;
                return Err(StoreError::Http {
                    op: "append",
                    path: path.into(),
                    status: 403,
                });
            }
            self.written.lock().unwrap().extend_from_slice(&body);
            Ok(())
        }
    }

    let console = Captured::default();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("trace"))
        .with(tracing_subscriber::fmt::layer().json().with_writer({
            let c = console.clone();
            move || c.clone()
        }));
    let _guard = tracing::subscriber::set_default(subscriber);

    let store = Arc::new(Flaky::default());
    let refdata = RefData::load(&LocalStore::new(data_dir())).await.unwrap();
    let log = SearchLog::start(
        store.clone(),
        LogConfig {
            max_batch: 1,
            ..hour()
        },
        &meter(),
    );
    let state = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata).with_search_log(log.clone()),
    );
    let v = version(&state).await;
    let (status, _) = send(
        &state,
        from_site(&format!("/v1/aggregate?q=zebrasecret+OR+gold&v={v}")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // The first append fails; shutdown tries again and succeeds.
    for _ in 0..100 {
        if *store.failed.lock().unwrap() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    log.shutdown(Duration::from_secs(5)).await;

    let written = String::from_utf8(store.written.lock().unwrap().clone()).unwrap();
    assert_eq!(written.lines().count(), 1, "{written}");
    assert!(written.contains("zebrasecret"));
    let console = String::from_utf8(console.0.lock().unwrap().clone()).unwrap();
    assert!(
        console.contains("search log append failed"),
        "the failure is logged: {console}"
    );
    assert!(console.contains("staging/"), "{console}");
    assert!(!console.contains("zebrasecret"), "{console}");
}

#[tokio::test]
async fn a_day_file_holds_the_staged_lines_once() {
    let dir = temp_dir("close");
    let store = LocalStore::new(&dir);
    let day = NaiveDate::from_ymd_opt(2026, 9, 28).unwrap();
    // Nothing staged: nothing written.
    searchlog::close_day(&store, day).await.unwrap();
    assert!(!store.exists(&searchlog::day_path(day)).await.unwrap());

    let lines: Vec<String> = (0..200).map(|i| format!("{{\"n\":{i}}}")).collect();
    for chunk in lines.chunks(50) {
        store
            .append(
                &searchlog::staging_path(day),
                format!("{}\n", chunk.join("\n")).into_bytes(),
                "application/x-ndjson",
            )
            .await
            .unwrap();
    }
    searchlog::close_day(&store, day).await.unwrap();
    let written =
        String::from_utf8(store.get(&searchlog::day_path(day)).await.unwrap().unwrap()).unwrap();
    let got: Vec<&str> = written.lines().collect();
    assert_ne!(got, lines, "the day file is shuffled");
    let mut sorted = got.clone();
    sorted.sort_unstable();
    let mut want: Vec<&str> = lines.iter().map(String::as_str).collect();
    want.sort_unstable();
    assert_eq!(sorted, want);

    // Written once: a second close (another replica) leaves it alone.
    store
        .append(
            &searchlog::staging_path(day),
            b"{\"n\":\"late\"}\n".to_vec(),
            "application/x-ndjson",
        )
        .await
        .unwrap();
    searchlog::close_day(&store, day).await.unwrap();
    let again = store.get(&searchlog::day_path(day)).await.unwrap().unwrap();
    assert_eq!(again, written.as_bytes());
}
