//! The searcher sidecar's cache metrics (#125): what reaches Application
//! Insights (a fake ingestion endpoint) from each scrape, and what is logged
//! while the sidecar is down.

use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use opentelemetry::metrics::MeterProvider as _;
use serde_json::Value;
use tracing_subscriber::layer::SubscriberExt;
use usnm_api::searcher_caches::{self, Instruments, Scraper, HEARTBEAT};
use usnm_api::telemetry::SERVICE;
use usnm_search::cache_metrics::{Cache, CacheReport, CacheStats};
use usnm_search::quickwit::QuickwitBackend;
use usnm_telemetry::testing::{fake_ingestion, Plain, Seen};

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

impl Captured {
    fn text(&self) -> String {
        String::from_utf8(self.0.lock().unwrap().clone()).unwrap()
    }
    fn count(&self, message: &str) -> usize {
        self.text()
            .lines()
            .filter(|l| serde_json::from_str::<Value>(l).unwrap()["fields"]["message"] == message)
            .count()
    }
}

fn subscriber(console: &Captured) -> impl tracing::Subscriber + Send + Sync {
    tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::new("info"))
        .with(tracing_subscriber::fmt::layer().json().with_writer({
            let c = console.clone();
            move || c.clone()
        }))
}

/// The searcher metrics in uploads `from..`: `(name, cache, value)`, sorted.
fn exported(seen: &Seen, from: usize) -> Vec<(String, String, f64)> {
    let mut out: Vec<_> = seen.uploads()[from..]
        .iter()
        .flat_map(|u| match serde_json::from_str::<Value>(&u.json).unwrap() {
            Value::Array(items) => items,
            other => vec![other],
        })
        .filter(|e| e["data"]["baseType"] == "MetricData")
        .flat_map(|e| {
            let d = &e["data"]["baseData"];
            let cache = d["properties"]["cache"].as_str().unwrap_or("").to_owned();
            d["metrics"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|m| {
                    m["name"]
                        .as_str()
                        .unwrap()
                        .starts_with("api.searcher_cache_")
                })
                .map(|m| {
                    (
                        m["name"].as_str().unwrap().to_owned(),
                        cache.clone(),
                        m["value"].as_f64().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        })
        .collect();
    out.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    out
}

fn row(name: &str, cache: &str, value: f64) -> (String, String, f64) {
    (
        format!("api.searcher_cache_{name}"),
        cache.to_owned(),
        value,
    )
}

fn report(entries: &[(Cache, CacheStats)]) -> CacheReport {
    entries.iter().copied().collect()
}

fn s(bytes: u64, items: u64, hits: u64, misses: u64, evictions: u64) -> CacheStats {
    CacheStats {
        bytes,
        items,
        hits,
        misses,
        evictions,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scrapes_become_a_few_metrics_and_quiet_logs() {
    let (endpoint, seen) = fake_ingestion().await;
    let conn = format!(
        "InstrumentationKey=00000000-0000-0000-0000-000000000000;IngestionEndpoint={endpoint}"
    );
    let (tracer, meter_provider) =
        usnm_telemetry::providers(&conn, Plain::with_token("tok"), SERVICE).unwrap();
    let meter_provider = Arc::new(meter_provider);
    let meter = meter_provider.meter(SERVICE.name);
    let console = Captured::default();
    let guard = tracing::subscriber::set_default(subscriber(&console));
    // Exports what was recorded since the last flush; returns the uploads
    // seen before it.
    let flush = || {
        let p = meter_provider.clone();
        let before = seen.uploads().len();
        async move {
            tokio::task::spawn_blocking(move || p.force_flush().unwrap())
                .await
                .unwrap();
            before
        }
    };

    let mut scraper = Scraper::new(Instruments::new(&meter));
    let t0 = Instant::now();
    let a = report(&[
        (Cache::SplitFooter, s(17_220, 2, 4, 2, 0)),
        (Cache::FastField, s(663, 4, 0, 4, 0)),
    ]);

    // The first scrape: both gauges of each cache, and everything since the
    // searcher started. Zero counts aren't sent.
    scraper.observe(Ok(a.clone()), t0);
    let from = flush().await;
    assert_eq!(
        exported(&seen, from),
        vec![
            row("bytes", "fast_field", 663.0),
            row("bytes", "split_footer", 17_220.0),
            row("hits", "split_footer", 4.0),
            row("items", "fast_field", 4.0),
            row("items", "split_footer", 2.0),
            row("misses", "fast_field", 4.0),
            row("misses", "split_footer", 2.0),
        ]
    );

    // Nothing changed: nothing is sent.
    scraper.observe(Ok(a.clone()), t0 + Duration::from_secs(60));
    let from = flush().await;
    assert_eq!(exported(&seen, from), vec![]);

    // The sidecar goes away for a few scrapes: one warning, nothing sent.
    for i in 2..6 {
        scraper.observe(
            Err("connection refused".into()),
            t0 + Duration::from_secs(60 * i),
        );
    }
    assert_eq!(scraper.failures(), 4);
    let from = flush().await;
    assert_eq!(exported(&seen, from), vec![]);
    assert_eq!(
        console.count("searcher cache metrics unavailable; retrying quietly"),
        1
    );

    // Back, with the footer cache full: its gauges and increments; the fast
    // field cache didn't change. The first footer eviction is logged once,
    // as what was seen: an eviction alone doesn't say the footers don't fit.
    let b = report(&[
        (Cache::SplitFooter, s(268_000_000, 790, 10, 802, 12)),
        (Cache::FastField, s(663, 4, 0, 4, 0)),
    ]);
    let t1 = t0 + Duration::from_secs(360);
    scraper.observe(Ok(b.clone()), t1);
    let c = report(&[
        (Cache::SplitFooter, s(268_000_000, 790, 11, 803, 13)),
        (Cache::FastField, s(663, 4, 0, 4, 0)),
    ]);
    scraper.observe(Ok(c.clone()), t1 + Duration::from_secs(60));
    let from = flush().await;
    assert_eq!(
        exported(&seen, from),
        vec![
            row("bytes", "split_footer", 268_000_000.0),
            row("evictions", "split_footer", 13.0),
            row("hits", "split_footer", 7.0),
            row("items", "split_footer", 790.0),
            row("misses", "split_footer", 801.0),
        ]
    );
    assert_eq!(scraper.failures(), 0);
    assert_eq!(console.count("searcher cache metrics available again"), 1);
    assert_eq!(console.count("split footer cache evictions seen"), 1);

    // Quiet for the heartbeat: the gauges again, so a quiet replica still
    // reports what its caches hold.
    scraper.observe(Ok(c.clone()), t1 + HEARTBEAT);
    let from = flush().await;
    assert_eq!(
        exported(&seen, from),
        vec![
            row("bytes", "fast_field", 663.0),
            row("bytes", "split_footer", 268_000_000.0),
            row("items", "fast_field", 4.0),
            row("items", "split_footer", 790.0),
        ]
    );

    // The searcher restarted, and only the footer cache's counters went
    // down: the fast field cache's 6 misses, more than before, are also all
    // since the restart, not 2 more.
    let d = report(&[
        (Cache::SplitFooter, s(8_000, 1, 0, 1, 0)),
        (Cache::FastField, s(663, 4, 0, 6, 0)),
    ]);
    scraper.observe(Ok(d), t1 + HEARTBEAT + Duration::from_secs(60));
    let from = flush().await;
    assert_eq!(
        exported(&seen, from),
        vec![
            row("bytes", "split_footer", 8_000.0),
            row("items", "split_footer", 1.0),
            row("misses", "fast_field", 6.0),
            row("misses", "split_footer", 1.0),
        ]
    );
    assert_eq!(
        console.count("the searcher restarted; its cache counters start again from 0"),
        1
    );
    // A new searcher run logs its first footer eviction again.
    let e = report(&[
        (Cache::SplitFooter, s(8_000, 1, 0, 2, 1)),
        (Cache::FastField, s(663, 4, 0, 6, 0)),
    ]);
    scraper.observe(Ok(e), t1 + HEARTBEAT + Duration::from_secs(120));
    assert_eq!(console.count("split footer cache evictions seen"), 2);

    drop(guard);
    tokio::task::spawn_blocking(move || {
        tracer.shutdown().unwrap();
        meter_provider.shutdown().unwrap();
    })
    .await
    .unwrap();
}

/// The scrape loop against a sidecar that isn't there: it keeps running and
/// logs one line, however many scrapes fail.
#[tokio::test]
async fn a_missing_sidecar_is_logged_once_and_the_loop_keeps_going() {
    let console = Captured::default();
    let _guard = tracing::subscriber::set_default(subscriber(&console));
    // Nothing listens on port 9 (discard) of localhost.
    let qw = Arc::new(QuickwitBackend::new("http://127.0.0.1:9", Duration::from_secs(1)).unwrap());
    let calls = Arc::new(Mutex::new(0u32));
    let handle = searcher_caches::spawn(
        Duration::from_millis(20),
        &opentelemetry::global::meter("test"),
        {
            let calls = calls.clone();
            move || {
                let qw = qw.clone();
                *calls.lock().unwrap() += 1;
                async move { qw.cache_metrics().await.map_err(|e| e.to_string()) }
            }
        },
    );
    for _ in 0..200 {
        if *calls.lock().unwrap() >= 5 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(*calls.lock().unwrap() >= 5, "the loop stopped scraping");
    assert!(!handle.is_finished(), "the loop ended");
    handle.abort();
    assert_eq!(
        console.count("searcher cache metrics unavailable; retrying quietly"),
        1,
        "{}",
        console.text()
    );
}

/// The scrape loop against a sidecar that answers with Quickwit 0.9.1's
/// metrics text.
#[tokio::test]
async fn reads_a_sidecar_over_http() {
    let text = include_str!("../../usnm-search/tests/data/quickwit-0.9.1-metrics.txt");
    let app =
        axum::Router::new().route("/metrics", axum::routing::get(move || async move { text }));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let qw = QuickwitBackend::new(&format!("http://{addr}"), Duration::from_secs(1)).unwrap();
    let r = qw.cache_metrics().await.unwrap();
    assert_eq!(r[&Cache::SplitFooter], s(17_220, 2, 4, 2, 0));
    // Something else on the port: an error, not a panic.
    let other = QuickwitBackend::new(&format!("http://{addr}/nope"), Duration::from_secs(1))
        .unwrap()
        .cache_metrics()
        .await
        .unwrap_err()
        .to_string();
    assert!(other.contains("404"), "{other}");
}
