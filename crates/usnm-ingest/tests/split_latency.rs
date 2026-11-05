//! A measurement, not a check (#1430, 05 §5.5.1): how a search's latency
//! depends on the number of splits a version has. It builds the synthetic
//! corpus (`synthetic`) into one index per split target, laid out by decade
//! and sent a decade at a time as a release does, then serves them all from
//! one read-only searcher set up like production's
//! (`infra/quickwit/searcher.yaml`, with the 4 search threads
//! `scripts/onprem/serving/gen-env.sh` gives it). It plans each kind of
//! search with `plan::aggregate`, as `/v1/aggregate` does, on each index in
//! turn: after one round to load the caches, each round's window ends a day
//! earlier, so Quickwit's partial-request cache never answers.
//!
//! For each index it reports the splits, and for each kind of search (a
//! word on no page, a rare word, two common ones and a rare phrase with a
//! common word in it) the median over the rounds of the whole plan's time,
//! its first pass's (the summary and the cube) and the earliest and latest
//! page lookups', and the mean of the searcher's CPU time (from `/proc`,
//! Linux only), which a busy host inflates less than the elapsed times.
//!
//! ```sh
//! QUICKWIT_BIN=/path/to/quickwit cargo test --release -p usnm-ingest \
//!   --test split_latency -- --ignored --nocapture
//! ```
//!
//! `USNM_MEASURE_SPLITS` sets the split targets (default `1500,5000,15000`),
//! `USNM_MEASURE_DOCS` the corpus size (default 200,000 pages),
//! `USNM_MEASURE_WORDS` the mean words a page (default 600),
//! `USNM_MEASURE_ROUNDS` the recorded rounds (default 5),
//! `USNM_MEASURE_THREADS` the searcher's threads (default 4) and
//! `USNM_MEASURE_PORT` the first of the ports it listens on (default 7440;
//! two for each writer, then two for the searcher).
//! `USNM_MEASURE_DIR` keeps the indexes in that directory rather than a
//! temporary one, and a later run searches them again rather than building
//! them; it refuses an index built from another page count or page length
//! (delete the directory to start over).
//! `USNM_WRITER_HEAP` and `USNM_WRITER_COMMIT_SECS` tune the writer as in a
//! release.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use chrono::NaiveDate;
use serde_json::Value;
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::{BucketSpec, BucketUnit};
use usnm_ingest::decade_order::DecadeOrder;
use usnm_ingest::merges::{IndexLayout, MergeWait};
use usnm_ingest::sink::{Decades, IndexSink, MainLayout, QuickwitNode, QuickwitSink};
use usnm_search::plan::{self, Planned};
use usnm_search::quickwit::QuickwitBackend;
use usnm_search::{
    Capabilities, CubeCell, HitsPage, HitsQuery, IndexSet, SearchBackend, SearchError, Summary,
};

mod synthetic;

/// A made-up word on about 1% of pages; with [`COMMON`] after it, on about
/// 0.03%: a rare phrase with a common word in it.
const RARE: &str = "qzphren";
/// A word on no page: what each split costs a search that matches nothing.
const ABSENT: &str = "qzabsent";
/// Words of the vocabulary by rank: at about 600 words a page, [`COMMON`]
/// is on about a sixth of the pages ("society" is on about 3 in 10 in
/// production) and [`VERY_COMMON`] on about 2 in 5.
const COMMON: usize = 300;
const VERY_COMMON: usize = 100;

const SEARCHER_YAML: &str = include_str!("../../../infra/quickwit/searcher.yaml");

fn env<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

/// Each kind of search: its name and query.
fn kinds() -> Vec<(&'static str, String)> {
    let common = synthetic::word(COMMON);
    vec![
        ("word on no page", ABSENT.to_owned()),
        ("rare word", RARE.to_owned()),
        ("common word", common.clone()),
        ("very common word", synthetic::word(VERY_COMMON)),
        ("rare phrase, common word", format!("\"{RARE} {common}\"")),
    ]
}

/// Where the indexes are: the file-backed metastore and the index root.
struct Store {
    meta: String,
    root: String,
}

/// Index the corpus as `id`, `target` pages a split, on a writer node of
/// its own in `work`, and stop it.
async fn build(
    bin: &Path,
    store: &Store,
    work: &Path,
    port: u16,
    id: &str,
    target: u64,
    corpus: synthetic::Corpus,
) -> IndexLayout {
    std::fs::create_dir_all(work).unwrap();
    let node = QuickwitNode::start(bin, work, port, &store.meta, &store.root)
        .await
        .unwrap();
    let mut sink = QuickwitSink::new(&node.url, &store.root)
        .unwrap()
        .watching(&node)
        .merges(MergeWait {
            poll: Duration::from_secs(1),
            stable_polls: 5,
            timeout: Duration::from_secs(4 * 3600),
            finalize_grace: Duration::from_secs(10),
            finalize_stall: Duration::from_secs(120),
            report: Default::default(),
        });
    let layout = MainLayout {
        split_docs: Some(target),
        ..MainLayout::from(Decades::Partitioned)
    };
    sink.create(id, layout).await.unwrap();
    let started = Instant::now();
    let mut order = DecadeOrder::new(work, target, None, None).unwrap();
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(2_000);
    let generator = std::thread::spawn(move || {
        synthetic::corpus(&corpus, |d| {
            let _ = tx.blocking_send(d);
        })
    });
    let mut sent = 0u64;
    while let Some(d) = rx.recv().await {
        order.add(&mut sink, &d).await.unwrap();
        sent += 1;
    }
    generator.join().unwrap();
    order.finish(&mut sink).await.unwrap();
    sink.finish(sent).await.unwrap();
    let layout = sink.layout(&[id.to_owned()]).await.unwrap().remove(0);
    eprintln!(
        "{id}: {sent} pages in {} splits (largest {} pages, {:.1} GB), by decade {:?}; built in {:.0} s",
        layout.splits,
        layout.largest_split_docs,
        layout.bytes as f64 / 1e9,
        layout.splits_by_decade,
        started.elapsed().as_secs_f64(),
    );
    node.stop().await.unwrap();
    layout
}

/// A read-only searcher on the indexes, configured as production's.
struct Searcher {
    child: tokio::process::Child,
    url: String,
    ticks_per_sec: Option<u64>,
}

impl Searcher {
    async fn start(bin: &Path, store: &Store, dir: &Path, port: u16, ids: &[String]) -> Self {
        let threads = env("USNM_MEASURE_THREADS", 4u32).to_string();
        let searcher: Vec<&str> = SEARCHER_YAML
            .lines()
            .skip_while(|l| *l != "searcher:")
            .take_while(|l| *l == "searcher:" || l.starts_with(' ') || l.is_empty())
            .collect();
        assert!(
            searcher.len() > 1,
            "infra/quickwit/searcher.yaml has no searcher section"
        );
        let data = dir.join("searcher-data");
        std::fs::create_dir_all(&data).unwrap();
        let config = format!(
            "version: 0.8\ncluster_id: usnm-measure\nnode_id: searcher\n\
             enabled_services: [searcher, metastore]\nlisten_address: 127.0.0.1\n\
             rest:\n  listen_port: {port}\ngrpc_listen_port: {}\ndata_dir: {}\n\
             metastore_uri: {}#polling_interval=5s\ndefault_index_root_uri: {}\n{}\n",
            port + 1,
            data.display(),
            store.meta,
            store.root,
            searcher.join("\n"),
        );
        let path = dir.join("searcher.yaml");
        std::fs::write(&path, config).unwrap();
        let log_path = dir.join("searcher.log");
        let log = std::fs::File::create(&log_path).unwrap();
        let child = tokio::process::Command::new(bin)
            .args(["run", "--config"])
            .arg(&path)
            .env("QW_DISABLE_TELEMETRY", "1")
            .env("QW_NUM_CPUS", &threads)
            .env("RAYON_NUM_THREADS", &threads)
            .env("QW_TOKIO_RUNTIME_NUM_THREADS", &threads)
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let url = format!("http://127.0.0.1:{port}");
        let http = reqwest::Client::new();
        // Ready, and listing every index.
        let deadline = Instant::now() + Duration::from_secs(120);
        loop {
            let listed = match http.get(format!("{url}/api/v1/indexes")).send().await {
                Ok(r) if r.status().is_success() => r.json::<Value>().await.ok(),
                _ => None,
            };
            let has = |id: &String| {
                listed.as_ref().and_then(Value::as_array).is_some_and(|l| {
                    l.iter()
                        .any(|i| i["index_config"]["index_id"].as_str() == Some(id.as_str()))
                })
            };
            if ids.iter().all(has) {
                break;
            }
            if Instant::now() > deadline {
                let log = std::fs::read_to_string(&log_path).unwrap_or_default();
                let tail: Vec<&str> = log.lines().rev().take(20).collect();
                panic!(
                    "the searcher doesn't list the indexes; its log ends:\n{}",
                    tail.into_iter().rev().collect::<Vec<_>>().join("\n")
                );
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        Self {
            child,
            url,
            ticks_per_sec: ticks_per_sec(),
        }
    }

    /// The searcher's CPU time so far, in ms: user and system ticks from
    /// `/proc/<pid>/stat`, at `getconf CLK_TCK` a second. `None` elsewhere.
    fn cpu_ms(&self) -> Option<u64> {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id()?)).ok()?;
        // The fields after the command name, which is in parentheses:
        // utime and stime are the 12th and 13th.
        let fields: Vec<&str> = stat[stat.rfind(')')? + 1..].split_whitespace().collect();
        let ticks = fields.get(11)?.parse::<u64>().ok()? + fields.get(12)?.parse::<u64>().ok()?;
        Some(ticks * 1000 / self.ticks_per_sec?)
    }
}

/// The clock ticks a second in which Linux's `/proc` counts CPU time
/// (`USER_HZ`, 100 on most systems).
fn ticks_per_sec() -> Option<u64> {
    let out = std::process::Command::new("getconf")
        .arg("CLK_TCK")
        .output()
        .ok()?;
    let hz: u64 = String::from_utf8(out.stdout).ok()?.trim().parse().ok()?;
    (hz > 0).then_some(hz)
}

/// The calls `plan::aggregate` makes, timed.
struct Timed {
    inner: QuickwitBackend,
    calls: Mutex<Vec<(&'static str, Instant, Instant)>>,
}

impl Timed {
    fn record(&self, call: &'static str, start: Instant) {
        self.calls
            .lock()
            .unwrap()
            .push((call, start, Instant::now()));
    }

    /// The first pass's time, and the edges', of the calls since the last
    /// `take`.
    fn take(&self) -> (Duration, Duration) {
        let calls = std::mem::take(&mut *self.calls.lock().unwrap());
        let span = |hits: bool| {
            let mut c = calls.iter().filter(|(n, ..)| (*n == "hits") == hits);
            let first = c.next();
            first.map_or(Duration::ZERO, |&(_, s, e)| {
                let (s, e) = c.fold((s, e), |(s, e), &(_, s2, e2)| (s.min(s2), e.max(e2)));
                e - s
            })
        };
        (span(false), span(true))
    }
}

#[async_trait]
impl SearchBackend for Timed {
    fn capabilities(&self) -> Capabilities {
        self.inner.capabilities()
    }

    async fn summary(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
    ) -> Result<Summary, SearchError> {
        let start = Instant::now();
        let out = self.inner.summary(indexes, query, filters, spec).await;
        self.record("summary", start);
        out
    }

    async fn cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        let start = Instant::now();
        let out = self.inner.cube(indexes, query, filters, spec, shards).await;
        self.record("cube", start);
        out
    }

    async fn summary_with_cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
    ) -> Result<Option<(Summary, Vec<CubeCell>)>, SearchError> {
        let start = Instant::now();
        let out = self
            .inner
            .summary_with_cube(indexes, query, filters, spec)
            .await;
        self.record("summary_with_cube", start);
        out
    }

    async fn hits(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        page: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        let start = Instant::now();
        let out = self.inner.hits(indexes, query, filters, page).await;
        self.record("hits", start);
        out
    }

    async fn health(&self) -> Result<(), SearchError> {
        self.inner.health().await
    }
}

#[derive(Default)]
struct Samples {
    hits: u64,
    total: Vec<f64>,
    first_pass: Vec<f64>,
    edges: Vec<f64>,
    cpu: Vec<f64>,
}

fn median(v: &[f64]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    let mut v = v.to_vec();
    v.sort_by(f64::total_cmp);
    let n = v.len();
    let m = if n % 2 == 1 {
        v[n / 2]
    } else {
        (v[n / 2 - 1] + v[n / 2]) / 2.0
    };
    format!("{m:.0}")
}

/// `port` and the gRPC port after it, checked free, so that a clash fails
/// here with a clear message rather than in Quickwit's startup.
fn free_pair(port: u16) -> u16 {
    for p in [port, port + 1] {
        if let Err(e) = std::net::TcpListener::bind(("127.0.0.1", p)) {
            panic!("port {p} is taken ({e}): set USNM_MEASURE_PORT to another base");
        }
    }
    port
}

/// The CPU time is counted in clock ticks (10 ms at the usual 100 a
/// second), too coarse for a median of a few rounds, so it's the mean.
fn mean(v: &[f64]) -> String {
    if v.is_empty() {
        return "-".into();
    }
    format!("{:.0}", v.iter().sum::<f64>() / v.len() as f64)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement: run with --ignored and QUICKWIT_BIN"]
async fn measure_split_latency() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    let bin = Path::new(&bin);
    let mut targets: Vec<u64> = std::env::var("USNM_MEASURE_SPLITS")
        .unwrap_or_else(|_| "1500,5000,15000".into())
        .split(',')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .map(|t| {
            t.parse()
                .unwrap_or_else(|_| panic!("USNM_MEASURE_SPLITS: {t:?} isn't a page count"))
        })
        .collect();
    targets.sort_unstable();
    targets.dedup();
    assert!(!targets.is_empty(), "USNM_MEASURE_SPLITS names no target");
    let base_port = env("USNM_MEASURE_PORT", 7440u16);
    let docs = env("USNM_MEASURE_DOCS", 200_000u64);
    let words_per_page = env("USNM_MEASURE_WORDS", 600u64);
    let rounds = env("USNM_MEASURE_ROUNDS", 5usize);
    let common = synthetic::word(COMMON);

    let temp = tempfile::tempdir().unwrap();
    let qw = std::env::var_os("USNM_MEASURE_DIR")
        .map_or_else(|| temp.path().to_owned(), PathBuf::from)
        .join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let store = Store {
        meta: format!("file://{}/meta", qw.display()),
        root: format!("file://{}/indexes", qw.display()),
    };
    let mut indexes = Vec::new();
    for (i, &target) in targets.iter().enumerate() {
        let id = format!("pages-split-{target}");
        let corpus = synthetic::Corpus {
            docs,
            words_per_page,
            with_decade: true,
            // About 100 to 400 pages a title, so the summary counts about
            // as many places as production's, up to the 2,000 place ids.
            title_pages: (100, 400),
            planted: vec![(RARE.to_owned(), 97), (format!("{RARE} {common}"), 2_999)],
        };
        // An index built by an earlier run in USNM_MEASURE_DIR is searched
        // again, if it was built from the same corpus.
        let built = qw.join(format!("{id}.layout.json"));
        let corpus_settings = serde_json::json!({ "docs": docs, "words_per_page": words_per_page });
        let layout: Value = match std::fs::read(&built) {
            Ok(b) => {
                let mut was: Value = serde_json::from_slice(&b).unwrap();
                assert_eq!(
                    was["corpus"], corpus_settings,
                    "{id} in USNM_MEASURE_DIR was built from another corpus: delete it to rebuild"
                );
                was["layout"].take()
            }
            Err(_) => {
                let work = qw.join(format!("writer-{target}"));
                let port = free_pair(base_port + 2 * i as u16);
                let layout = build(bin, &store, &work, port, &id, target, corpus).await;
                let layout = serde_json::to_value(layout).unwrap();
                let record = serde_json::json!({ "corpus": corpus_settings, "layout": layout });
                std::fs::write(&built, record.to_string()).unwrap();
                layout
            }
        };
        let decades: Vec<u16> = layout["splits_by_decade"]
            .as_object()
            .unwrap()
            .keys()
            .map(|d| d.parse().unwrap())
            .collect();
        let span = *decades.iter().min().unwrap()..=*decades.iter().max().unwrap();
        let set = IndexSet::new(vec![id.clone()])
            .with_common_grams(true)
            .with_decades(Some(span));
        indexes.push((target, layout["splits"].as_u64().unwrap(), set));
    }

    let ids: Vec<String> = targets.iter().map(|t| format!("pages-split-{t}")).collect();
    let port = free_pair(base_port + 2 * targets.len() as u16);
    let searcher = Searcher::start(bin, &store, &qw, port, &ids).await;
    let backend = Timed {
        inner: QuickwitBackend::new(&searcher.url, Duration::from_secs(120)).unwrap(),
        calls: Mutex::default(),
    };
    let kinds = kinds();
    let mut samples: BTreeMap<(usize, u64), Samples> = BTreeMap::new();
    // Round 0 loads the caches and isn't recorded.
    for round in 0..=rounds {
        for (k, (_, q)) in kinds.iter().enumerate() {
            let query = usnm_core::query::parse(q).unwrap();
            // Each index in turn, starting one further along each round, so
            // the host's load drifting during a round falls on each alike.
            for n in 0..indexes.len() {
                let (target, _, set) = &indexes[(n + round) % indexes.len()];
                let filters = Filters {
                    from: NaiveDate::from_ymd_opt(1800, 1, 1).unwrap(),
                    to: NaiveDate::from_ymd_opt(1999, 12, 31).unwrap()
                        - chrono::Duration::days(round as i64),
                    states: Vec::new(),
                    lccns: Vec::new(),
                    langs: Vec::new(),
                    front_only: false,
                    series: Vec::new(),
                };
                let spec = BucketSpec::new(BucketUnit::Year, filters.from, filters.to);
                let cpu = searcher.cpu_ms();
                let start = Instant::now();
                let planned = plan::aggregate(
                    &backend,
                    set,
                    &query,
                    &filters,
                    &spec,
                    usnm_core::cube::MAX_CELLS,
                )
                .await
                .unwrap();
                let total = start.elapsed();
                let cpu = cpu.zip(searcher.cpu_ms()).map(|(a, b)| b - a);
                let (first_pass, edges) = backend.take();
                let Planned::Complete(agg) = planned else {
                    panic!("{q}: over the cell budget");
                };
                if round == 0 {
                    continue;
                }
                let s = samples.entry((k, *target)).or_default();
                s.hits = agg.summary.total_hits;
                s.total.push(total.as_secs_f64() * 1e3);
                s.first_pass.push(first_pass.as_secs_f64() * 1e3);
                s.edges.push(edges.as_secs_f64() * 1e3);
                s.cpu.extend(cpu.map(|c| c as f64));
            }
        }
    }

    eprintln!(
        "\n{docs} pages, about {words_per_page} words each; medians of {rounds} rounds (the searcher's CPU time: the mean), ms\n"
    );
    eprintln!("| Search | Pages matched | Split target | Splits | Total | First pass | Edges | Searcher CPU |");
    eprintln!("| --- | --: | --: | --: | --: | --: | --: | --: |");
    for ((k, target), s) in &samples {
        let splits = indexes.iter().find(|(t, ..)| t == target).unwrap().1;
        eprintln!(
            "| {} | {} | {target} | {splits} | {} | {} | {} | {} |",
            kinds[*k].0,
            s.hits,
            median(&s.total),
            median(&s.first_pass),
            median(&s.edges),
            mean(&s.cpu),
        );
    }
    drop(searcher);
}
