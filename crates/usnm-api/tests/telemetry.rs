//! Request telemetry: what reaches Application Insights (a fake ingestion
//! endpoint) and the console log, and what never does.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use opentelemetry::metrics::MeterProvider as _;
use serde_json::Value;
use tower::ServiceExt;
use tracing_subscriber::layer::SubscriberExt;
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::telemetry::{observe_index_version, Metrics, SERVICE};
use usnm_api::{app, AppState};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;
use usnm_search::memory::MemoryBackend;
use usnm_search::{
    Capabilities, CubeCell, HitsPage, HitsQuery, IndexSet, SearchBackend, SearchError, Summary,
};
use usnm_store::LocalStore;
use usnm_telemetry::testing::{fake_ingestion, Plain};

/// Search text in the tests' query strings. None of it may be exported or logged.
const SECRETS: [&str; 4] = ["zebrasecret", "parensecret", "pathsecret", "q="];

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.rate_limit = None;
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

async fn refdata() -> RefData {
    RefData::load(&LocalStore::new(data_dir())).await.unwrap()
}

/// A backend whose every search fails, for a 503.
struct Broken;

#[async_trait]
impl SearchBackend for Broken {
    fn capabilities(&self) -> Capabilities {
        MemoryBackend::new().capabilities()
    }
    async fn summary(
        &self,
        _: &IndexSet,
        _: &Node,
        _: &Filters,
        _: &BucketSpec,
    ) -> Result<Summary, SearchError> {
        Err(SearchError::Backend("engine down".into()))
    }
    async fn cube(
        &self,
        _: &IndexSet,
        _: &Node,
        _: &Filters,
        _: &BucketSpec,
        _: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        Err(SearchError::Backend("engine down".into()))
    }
    async fn hits(
        &self,
        _: &IndexSet,
        _: &Node,
        _: &Filters,
        _: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        Err(SearchError::Backend("engine down".into()))
    }
    async fn health(&self) -> Result<(), SearchError> {
        Ok(())
    }
}

async fn status_of(state: &Arc<AppState>, uri: &str) -> StatusCode {
    let resp = app(state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    resp.into_body().collect().await.unwrap();
    status
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

fn temp_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-telemetry-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Whether a persisted response (`*.zst`) is anywhere under `dir`.
fn has_zst(dir: &std::path::Path) -> bool {
    std::fs::read_dir(dir).unwrap().any(|e| {
        let p = e.unwrap().path();
        if p.is_dir() {
            has_zst(&p)
        } else {
            p.extension().is_some_and(|x| x == "zst")
        }
    })
}

/// Every envelope in every upload.
fn envelopes(json: &[String]) -> Vec<Value> {
    json.iter()
        .flat_map(|j| match serde_json::from_str::<Value>(j).unwrap() {
            Value::Array(items) => items,
            other => vec![other],
        })
        .collect()
}

/// Summed values of metric `name` whose properties include all of `attrs`.
fn metric_total(envs: &[Value], name: &str, attrs: &[(&str, &str)]) -> f64 {
    envs.iter()
        .filter(|e| e["data"]["baseType"] == "MetricData")
        .map(|e| &e["data"]["baseData"])
        .filter(|d| {
            attrs
                .iter()
                .all(|(k, v)| d["properties"][*k].as_str() == Some(*v))
        })
        .flat_map(|d| d["metrics"].as_array().unwrap().iter())
        .filter(|m| m["name"] == name)
        .map(|m| m["value"].as_f64().unwrap())
        .sum()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn requests_are_exported_by_route_template_without_search_text() {
    let (endpoint, seen) = fake_ingestion().await;
    let conn = format!(
        "InstrumentationKey=00000000-0000-0000-0000-000000000000;IngestionEndpoint={endpoint}"
    );
    let (tracer, meter_provider) =
        usnm_telemetry::providers(&conn, Plain::with_token("tok"), SERVICE).unwrap();
    let meter = meter_provider.meter(SERVICE.name);
    let console = Captured::default();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(tracing_subscriber::fmt::layer().json().with_writer({
            let c = console.clone();
            move || c.clone()
        }))
        .with(usnm_telemetry::otel_layer(&tracer, SERVICE));
    let guard = tracing::subscriber::set_default(subscriber);

    let site = temp_dir("site");
    std::fs::write(site.join("index.html"), "<!doctype html><title>t</title>").unwrap();
    let blobs = temp_dir("blobs");
    let store = Arc::new(LocalStore::new(&blobs));
    let mut cfg = config();
    cfg.site_dir = Some(site);
    cfg.persist_after = Duration::ZERO;
    let state = Arc::new(
        AppState::new(cfg.clone(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(store.clone())
            .with_metrics(Metrics::new(&meter)),
    );
    observe_index_version(&state, &meter);

    let search = "/v1/aggregate?q=zebrasecret&from=1896-01-01&to=1896-12-31";
    assert_eq!(status_of(&state, "/healthz").await, StatusCode::OK);
    assert_eq!(status_of(&state, "/readyz").await, StatusCode::OK);
    // Computed (both caches miss), then served from the in-process cache.
    assert_eq!(status_of(&state, search).await, StatusCode::OK);
    assert_eq!(status_of(&state, search).await, StatusCode::OK);
    assert_eq!(
        status_of(&state, "/v1/aggregate?q=text:parensecret").await,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        status_of(&state, "/v1/pathsecret?q=zebrasecret").await,
        StatusCode::NOT_FOUND
    );
    assert_eq!(status_of(&state, "/api/v1/meta").await, StatusCode::OK);
    assert_eq!(status_of(&state, "/").await, StatusCode::OK);

    // A second replica: empty in-process cache, the first one's persisted body.
    for _ in 0..100 {
        if has_zst(&blobs) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(has_zst(&blobs), "the first response was persisted");
    let replica = Arc::new(
        AppState::new(cfg, Arc::new(fixture_backend()), refdata().await)
            .with_response_store(store)
            .with_metrics(Metrics::new(&meter)),
    );
    assert_eq!(status_of(&replica, search).await, StatusCode::OK);

    // The search engine fails: a 5xx, the only kind of failed request.
    let broken = Arc::new(
        AppState::new(config(), Arc::new(Broken), refdata().await)
            .with_metrics(Metrics::new(&meter)),
    );
    assert_eq!(
        status_of(&broken, search).await,
        StatusCode::SERVICE_UNAVAILABLE
    );

    drop(guard);
    tokio::task::spawn_blocking(move || {
        tracer.shutdown().unwrap();
        meter_provider.shutdown().unwrap();
    })
    .await
    .unwrap();

    // Uploads are signed and go to the Entra ingestion path.
    let uploads = seen.uploads();
    assert!(!uploads.is_empty(), "nothing was exported");
    for u in &uploads {
        assert_eq!(u.path, "/v2.1/track");
        assert_eq!(u.authorization.as_deref(), Some("Bearer tok"));
    }
    let all = seen.all_json();
    let envs = envelopes(&uploads.iter().map(|u| u.json.clone()).collect::<Vec<_>>());

    // One request per call except the probes, named by route template.
    let mut requests: Vec<(String, String, bool)> = envs
        .iter()
        .filter(|e| e["data"]["baseType"] == "RequestData")
        .map(|e| {
            assert_eq!(e["tags"]["ai.cloud.role"], "usnm-api", "{e}");
            let d = &e["data"]["baseData"];
            assert!(d.get("url").is_none(), "no URL is exported: {d}");
            (
                d["name"].as_str().unwrap().to_owned(),
                d["responseCode"].as_str().unwrap().to_owned(),
                d["success"].as_bool().unwrap(),
            )
        })
        .collect();
    requests.sort();
    let expect = |n: &str, c: &str, ok: bool| (n.to_owned(), c.to_owned(), ok);
    let mut expected = vec![
        expect("GET /v1/aggregate", "200", true),
        expect("GET /v1/aggregate", "200", true),
        expect("GET /v1/aggregate", "200", true),
        expect("GET /v1/aggregate", "400", true),
        expect("GET /v1/aggregate", "503", false),
        expect("GET (no route)", "404", true),
        expect("GET /api/v1/meta", "200", true),
        expect("GET (site)", "200", true),
    ];
    expected.sort();
    assert_eq!(requests, expected, "{all}");

    // Neither search text nor raw paths nor probes appear in any trace.
    for secret in SECRETS {
        assert!(!all.contains(secret), "`{secret}` was exported: {all}");
    }
    let traces: String = envs
        .iter()
        .filter(|e| e["data"]["baseType"] != "MetricData")
        .map(Value::to_string)
        .collect();
    assert!(!traces.contains("healthz") && !traces.contains("readyz"));

    // Metrics.
    let total = |name, attrs: &[(&str, &str)]| metric_total(&envs, name, attrs);
    assert_eq!(
        total(
            "api.cache_lookups",
            &[("layer", "memory"), ("result", "hit")]
        ),
        1.0
    );
    assert_eq!(
        total("api.cache_lookups", &[("layer", "blob"), ("result", "hit")]),
        1.0
    );
    assert!(
        total(
            "api.cache_lookups",
            &[("layer", "blob"), ("result", "miss")]
        ) >= 1.0
    );
    assert_eq!(
        total(
            "api.requests",
            &[("route", "/healthz"), ("status_class", "2xx")]
        ),
        1.0
    );
    assert_eq!(
        total(
            "api.requests",
            &[("route", "/v1/aggregate"), ("status_class", "5xx")]
        ),
        1.0
    );
    assert_eq!(total("api.rejected_queries", &[("reason", "syntax")]), 1.0);
    assert!(all.contains("api.backend_duration_seconds"), "{all}");
    assert!(all.contains("api.request_duration_seconds"), "{all}");
    assert_eq!(
        total("api.index_version", &[("index_version", "fixture-v1")]),
        1.0
    );

    // One console line per request, the same fields, no probes, no search text.
    let console = String::from_utf8(console.0.lock().unwrap().clone()).unwrap();
    for secret in SECRETS {
        assert!(
            !console.contains(secret),
            "`{secret}` was logged: {console}"
        );
    }
    let mut lines: Vec<(String, String, u64)> = console
        .lines()
        .map(|l| serde_json::from_str::<Value>(l).unwrap())
        .filter(|l| l["fields"]["message"] == "request")
        .map(|l| {
            assert_eq!(l["level"], "INFO");
            assert!(l.get("span").is_none(), "{l}");
            assert!(l["fields"]["ms"].is_u64(), "{l}");
            (
                l["fields"]["method"].as_str().unwrap().to_owned(),
                l["fields"]["route"].as_str().unwrap().to_owned(),
                l["fields"]["status"].as_u64().unwrap(),
            )
        })
        .collect();
    lines.sort();
    let mut expected: Vec<(String, String, u64)> = [
        ("/v1/aggregate", 200),
        ("/v1/aggregate", 200),
        ("/v1/aggregate", 200),
        ("/v1/aggregate", 400),
        ("/v1/aggregate", 503),
        ("(no route)", 404),
        ("/api/v1/meta", 200),
        ("(site)", 200),
    ]
    .into_iter()
    .map(|(r, s)| ("GET".to_owned(), r.to_owned(), s))
    .collect();
    expected.sort();
    assert_eq!(lines, expected, "{console}");
}
