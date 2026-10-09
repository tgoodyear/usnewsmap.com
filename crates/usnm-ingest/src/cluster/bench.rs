//! The benchmark searches against the cluster's root, the way the API runs
//! a search (`/v1/aggregate`, 05 §5.7): `usnm_search::plan::aggregate` on a
//! `QuickwitBackend`, coarsening the buckets when the cube would be too
//! large, as `crates/usnm-api/src/routes/aggregate.rs` does. The API, its
//! caches and its slots are left out: what is timed is the search engine.
//!
//! The searches are those of `scripts/bench-cold-searches.py` (three whole
//! corpus searches by month) and `scripts/load-cold-searches.py` (ten, the
//! benchmark classes of 05 §5.8). Passes:
//!
//! - `first`: every search once, one at a time. On nodes that just started,
//!   their caches are cold; otherwise footers and fast fields may be warm.
//! - `warm`: the same requests again, which Quickwit's partial request cache
//!   answers per split.
//! - one pass per concurrency level (1, 2, 4 and 10 by default): every
//!   search with its date window shifted back one more day than the pass
//!   before, so no request repeats and the partial request cache can't
//!   answer it (the load test's "cold for the API", 06 §6.5), drained by
//!   that many workers.
//!
//! Each pass records every search's time and the Quickwit requests it made,
//! the pass's median, p90 and throughput, and from each node's metrics the
//! leaf searches and splits it served: how the root spread the work.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use futures::stream::{self, StreamExt};
use serde::Serialize;
use usnm_core::params::{Filters, RawParams, SearchRequest};
use usnm_core::query::Node;
use usnm_core::time::{BucketSpec, BucketUnit};
use usnm_search::plan::{self, Planned};
use usnm_search::quickwit::QuickwitBackend;
use usnm_search::{
    Capabilities, CubeCell, HitsPage, HitsQuery, IndexSet, SearchBackend, SearchError, Summary,
};

use super::members::{self, Member, NodeCounters};

/// A date window, `from` and `to`; `None` is the whole corpus.
pub type Window = Option<(&'static str, &'static str)>;

/// The benchmark searches: name, query string and date window.
pub const SEARCHES: &[(&str, &str, Window)] = &[
    // scripts/bench-cold-searches.py
    ("radio (month)", "q=radio&mode=phrase&bucket=month", None),
    (
        "television (month)",
        "q=television&mode=phrase&bucket=month",
        None,
    ),
    (
        "yellow fever (month)",
        "q=yellow+fever&mode=phrase&bucket=month",
        None,
    ),
    // scripts/load-cold-searches.py
    (
        "cross of gold (1896)",
        "q=cross+of+gold&mode=phrase",
        Some(("1896-01-01", "1896-12-31")),
    ),
    (
        "yellow jack (1878)",
        "q=yellow+jack&mode=phrase",
        Some(("1878-01-01", "1878-12-31")),
    ),
    ("scalawag", "q=scalawag&mode=phrase", None),
    ("miscegenation", "q=miscegenation&mode=phrase", None),
    (
        "influenza (1918)",
        "q=influenza&mode=phrase",
        Some(("1918-01-01", "1918-12-31")),
    ),
    ("railroad", "q=railroad&mode=phrase", None),
    ("lincoln", "q=lincoln&mode=phrase", None),
    ("gold silver near 5", "q=gold+silver&mode=near&near=5", None),
    (
        "influenza GA front",
        "q=influenza&mode=phrase&state=GA&front=true",
        None,
    ),
    (
        "yellow fever (month), load",
        "q=yellow+fever&mode=phrase&bucket=month",
        None,
    ),
];

/// The API's cell budget (`USNM_MAX_CELLS`'s default).
const MAX_CELLS: usize = usnm_core::cube::MAX_CELLS;

/// One Quickwit request a search made: its kind and seconds.
#[derive(Debug, Clone, Serialize)]
pub struct Call {
    pub kind: &'static str,
    pub secs: f64,
}

/// A [`SearchBackend`] that records each call's time.
struct Timed<'a> {
    inner: &'a QuickwitBackend,
    calls: Mutex<Vec<Call>>,
}

impl Timed<'_> {
    async fn time<T>(
        &self,
        kind: &'static str,
        f: impl std::future::Future<Output = Result<T, SearchError>>,
    ) -> Result<T, SearchError> {
        let t = std::time::Instant::now();
        let r = f.await;
        self.calls
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(Call {
                kind,
                secs: t.elapsed().as_secs_f64(),
            });
        r
    }
}

#[async_trait]
impl SearchBackend for Timed<'_> {
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
        self.time("summary", self.inner.summary(i, q, f, s)).await
    }
    async fn cube(
        &self,
        i: &IndexSet,
        q: &Node,
        f: &Filters,
        s: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        self.time("cube", self.inner.cube(i, q, f, s, shards)).await
    }
    async fn hits(
        &self,
        i: &IndexSet,
        q: &Node,
        f: &Filters,
        p: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        self.time("hits", self.inner.hits(i, q, f, p)).await
    }
    async fn american_stories_only(
        &self,
        i: &IndexSet,
        q: &Node,
        f: &Filters,
    ) -> Result<u64, SearchError> {
        self.time(
            "american_stories_only",
            self.inner.american_stories_only(i, q, f),
        )
        .await
    }
    async fn health(&self) -> Result<(), SearchError> {
        self.inner.health().await
    }
}

/// One search's request for a pass: its query string with the window
/// shifted back `shift` days, clamped to the corpus `bounds`.
pub fn request(
    query: &str,
    window: Option<(&str, &str)>,
    bounds: (NaiveDate, NaiveDate),
    shift: u32,
) -> anyhow::Result<SearchRequest> {
    let (lo, hi) = bounds;
    let (mut start, mut end) = match window {
        None => (lo, hi),
        Some((a, b)) => (
            NaiveDate::parse_from_str(a, "%Y-%m-%d")?,
            NaiveDate::parse_from_str(b, "%Y-%m-%d")?,
        ),
    };
    start = start.max(lo);
    end = end.min(hi);
    if end < start + chrono::Duration::days(30) {
        // The window is outside this corpus (the fixtures cover 1895-1897).
        (start, end) = (lo, hi);
    }
    let shift = chrono::Duration::days(i64::from(shift));
    let start = (start - shift).max(lo);
    let end = (end - shift).max(start);
    let qs = format!("{query}&from={start}&to={end}");
    let raw = RawParams::parse(&qs).map_err(|e| anyhow::anyhow!("{qs}: {e}"))?;
    SearchRequest::from_raw(&raw, bounds).map_err(|e| anyhow::anyhow!("{qs}: {e}"))
}

fn coarser(unit: BucketUnit) -> Option<BucketUnit> {
    match unit {
        BucketUnit::Day => Some(BucketUnit::Week),
        BucketUnit::Week => Some(BucketUnit::Month),
        BucketUnit::Month => Some(BucketUnit::Year),
        BucketUnit::Year => None,
    }
}

/// One search's result in a pass.
#[derive(Debug, Clone, Serialize)]
pub struct SearchResult {
    pub name: String,
    pub secs: f64,
    /// Matching pages (`None` on an error).
    pub pages: Option<u64>,
    pub places: Option<usize>,
    pub cube_calls: Option<u8>,
    pub coarsened: bool,
    pub calls: Vec<Call>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Run one search as the API would.
pub async fn search(
    backend: &QuickwitBackend,
    indexes: &IndexSet,
    name: &str,
    req: &SearchRequest,
) -> SearchResult {
    let timed = Timed {
        inner: backend,
        calls: Mutex::new(Vec::new()),
    };
    let t = std::time::Instant::now();
    let mut spec = req.bucket_spec();
    let mut coarsened = false;
    let outcome = loop {
        match plan::aggregate(&timed, indexes, &req.query, &req.filters, &spec, MAX_CELLS).await {
            Ok(Planned::Complete(agg)) => break Ok(agg),
            Ok(Planned::TooManyCells { upper_bound }) => match coarser(spec.unit) {
                Some(unit) => {
                    spec = BucketSpec::new(unit, spec.from, spec.to);
                    coarsened = true;
                }
                None => break Err(format!("too broad: about {upper_bound} cells by year")),
            },
            Err(e) => break Err(e.to_string()),
        }
    };
    let secs = t.elapsed().as_secs_f64();
    let calls = timed.calls.into_inner().unwrap_or_else(|e| e.into_inner());
    match outcome {
        Ok(agg) => SearchResult {
            name: name.to_owned(),
            secs,
            pages: Some(agg.summary.total_hits),
            places: Some(agg.summary.places.len()),
            cube_calls: Some(agg.cube_calls),
            coarsened,
            calls,
            error: None,
        },
        Err(e) => SearchResult {
            name: name.to_owned(),
            secs,
            pages: None,
            places: None,
            cube_calls: None,
            coarsened,
            calls,
            error: Some(e),
        },
    }
}

/// One pass over the searches.
#[derive(Debug, Clone, Serialize)]
pub struct Pass {
    pub name: String,
    pub concurrency: usize,
    pub shift_days: u32,
    pub started_at: DateTime<Utc>,
    pub wall_secs: f64,
    /// Searches completed per second.
    pub throughput: f64,
    pub median_secs: f64,
    pub p90_secs: f64,
    pub max_secs: f64,
    pub failed: usize,
    pub searches: Vec<SearchResult>,
    /// Each node's leaf searches, splits and split seconds in the pass, and
    /// its main runtime's busy time and Blob downloads (#251).
    pub nodes: BTreeMap<String, NodeCounters>,
    /// Each node's search pool, sampled through the pass.
    pub sampled: BTreeMap<String, Sampled>,
    /// Every ready searcher served leaf searches in the pass.
    pub all_searchers_used: bool,
}

/// A node's search pool through a pass, from its metrics every
/// [`SAMPLE_EVERY`] (#251): whether split searches waited for its threads.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Sampled {
    pub samples: u32,
    pub search_ongoing_max: f64,
    pub search_pending_max: f64,
    /// Samples with split searches queued for the pool.
    pub samples_pending: u32,
}

impl Sampled {
    fn add(&mut self, c: &NodeCounters) {
        self.samples += 1;
        self.search_ongoing_max = self.search_ongoing_max.max(c.search_ongoing);
        self.search_pending_max = self.search_pending_max.max(c.search_pending);
        self.samples_pending += u32::from(c.search_pending > 0.0);
    }
}

/// How often a pass reads each node's search pool.
pub const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// Read every node's search pool each [`SAMPLE_EVERY`] until `stop`.
async fn sample_pools(
    http: reqwest::Client,
    members_now: Vec<Member>,
    mut stop: tokio::sync::oneshot::Receiver<()>,
) -> BTreeMap<String, Sampled> {
    let mut out: BTreeMap<String, Sampled> = BTreeMap::new();
    loop {
        tokio::select! {
            _ = &mut stop => return out,
            _ = tokio::time::sleep(SAMPLE_EVERY) => {}
        }
        for (id, c) in members::counters(&http, &members_now, None).await {
            out.entry(id).or_default().add(&c);
        }
    }
}

/// The root's Quickwit build (`GET /api/v1/version`), for telling the
/// version comparison's runs apart (#251).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct QuickwitBuild {
    pub version: Option<String>,
    pub commit: Option<String>,
    pub num_cpus: Option<u64>,
}

impl QuickwitBuild {
    pub fn parse(v: &serde_json::Value) -> Self {
        QuickwitBuild {
            version: v["build"]["version"].as_str().map(str::to_owned),
            commit: v["build"]["commit_short_hash"].as_str().map(str::to_owned),
            num_cpus: v["runtime"]["num_cpus"].as_u64(),
        }
    }
}

async fn quickwit_build(http: &reqwest::Client, root: &str) -> Option<QuickwitBuild> {
    let v: serde_json::Value = http
        .get(format!("{root}/api/v1/version"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .ok()?
        .error_for_status()
        .ok()?
        .json()
        .await
        .ok()?;
    Some(QuickwitBuild::parse(&v))
}

/// `p` (0 to 1) of sorted `v`, nearest rank.
pub fn percentile(sorted: &[f64], p: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let rank = (p * sorted.len() as f64).ceil().max(1.0) as usize;
    sorted[rank.min(sorted.len()) - 1]
}

/// What to bench.
#[derive(Debug, Clone)]
pub struct Spec {
    pub root: String,
    pub indexes: Vec<String>,
    pub american_stories: bool,
    pub bounds: (NaiveDate, NaiveDate),
    /// Concurrency levels after `first` and `warm`.
    pub levels: Vec<usize>,
    /// Days every level's windows start shifted back (then one more per pass).
    pub offset: u32,
    pub pause: Duration,
    /// Skip the `first` and `warm` passes.
    pub levels_only: bool,
    /// Wait for this many ready searchers first.
    pub expect_searchers: usize,
    pub timeout: Duration,
}

/// A bench run.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub label: String,
    pub indexes: Vec<String>,
    pub american_stories: bool,
    /// The root's Quickwit build; `None` if it didn't say.
    pub quickwit: Option<QuickwitBuild>,
    pub members: Vec<Member>,
    pub searchers: usize,
    pub started_at: DateTime<Utc>,
    pub ended_at: DateTime<Utc>,
    pub passes: Vec<Pass>,
}

impl Report {
    /// The report without each search's calls, for the job's log line.
    pub fn summary(&self) -> serde_json::Value {
        let mut v = serde_json::to_value(self).unwrap_or_default();
        if let Some(passes) = v["passes"].as_array_mut() {
            for p in passes {
                if let Some(searches) = p["searches"].as_array_mut() {
                    for s in searches {
                        if let Some(o) = s.as_object_mut() {
                            o.remove("calls");
                        }
                    }
                }
            }
        }
        v
    }
}

async fn pass(
    http: &reqwest::Client,
    spec: &Spec,
    members_now: &[Member],
    name: &str,
    concurrency: usize,
    shift: u32,
) -> anyhow::Result<Pass> {
    let indexes = IndexSet::new(spec.indexes.clone())
        .with_common_grams(true)
        .with_american_stories(spec.american_stories);
    let mut jobs = Vec::new();
    for (n, q, w) in SEARCHES {
        jobs.push((n.to_string(), request(q, *w, spec.bounds, shift)?));
    }
    let before = members::counters(http, members_now, None).await;
    let (stop, stopped) = tokio::sync::oneshot::channel();
    let sampler = tokio::spawn(sample_pools(http.clone(), members_now.to_vec(), stopped));
    let started_at = Utc::now();
    let t = std::time::Instant::now();
    // One backend for the pass, as the API keeps one: connections are reused.
    let backend = QuickwitBackend::new(&spec.root, spec.timeout)?;
    let results: Vec<SearchResult> = stream::iter(jobs)
        .map(|(n, req)| {
            let (indexes, backend) = (&indexes, &backend);
            async move { search(backend, indexes, &n, &req).await }
        })
        .buffered(concurrency.max(1))
        .collect()
        .await;
    let wall_secs = t.elapsed().as_secs_f64();
    let _ = stop.send(());
    let sampled = sampler.await.unwrap_or_default();
    let after = members::counters(http, members_now, None).await;
    let nodes: BTreeMap<String, NodeCounters> = after
        .iter()
        .map(|(id, a)| {
            let b = before.get(id).cloned().unwrap_or_default();
            (id.clone(), a.since(&b))
        })
        .collect();
    let all_searchers_used = members_now
        .iter()
        .filter(|m| m.ready && m.runs("searcher"))
        .all(|m| nodes.get(&m.node_id).is_some_and(|c| c.leaf_splits > 0.0));
    let ok: Vec<&SearchResult> = results.iter().filter(|r| r.error.is_none()).collect();
    // Latency of the searches that answered; failures are counted apart.
    let mut times: Vec<f64> = ok.iter().map(|r| r.secs).collect();
    times.sort_by(f64::total_cmp);
    let p = Pass {
        name: name.to_owned(),
        concurrency,
        shift_days: shift,
        started_at,
        wall_secs,
        throughput: ok.len() as f64 / wall_secs.max(0.001),
        median_secs: percentile(&times, 0.5),
        p90_secs: percentile(&times, 0.9),
        max_secs: times.last().copied().unwrap_or(0.0),
        failed: results.len() - ok.len(),
        searches: results,
        nodes,
        sampled,
        all_searchers_used,
    };
    tracing::info!(
        pass = name,
        concurrency,
        wall_secs = p.wall_secs,
        median_secs = p.median_secs,
        p90_secs = p.p90_secs,
        per_sec = p.throughput,
        failed = p.failed,
        main_busy_secs = %p
            .nodes
            .iter()
            .map(|(n, c)| format!("{n}={:.1}", c.main_busy_ms / 1000.0))
            .collect::<Vec<_>>()
            .join(" "),
        leaf_splits = %p
            .nodes
            .iter()
            .map(|(n, c)| format!("{n}={}", c.leaf_splits))
            .collect::<Vec<_>>()
            .join(" "),
        "bench pass"
    );
    for r in p.searches.iter().filter(|r| r.error.is_some()) {
        tracing::warn!(search = %r.name, error = r.error.as_deref().unwrap_or(""), "search failed");
    }
    Ok(p)
}

/// Run the bench.
pub async fn run(label: &str, spec: &Spec) -> anyhow::Result<Report> {
    if spec.indexes.is_empty() {
        bail!("name at least one index");
    }
    let http = reqwest::Client::new();
    let (members_now, ready) = members::wait_ready(
        &http,
        &spec.root,
        spec.expect_searchers,
        Duration::from_secs(600),
    )
    .await
    .context("reading the cluster")?;
    let searchers = members_now
        .iter()
        .filter(|m| m.ready && m.runs("searcher"))
        .count();
    if !ready {
        bail!(
            "{searchers} ready searchers after 10 minutes, {} expected: {members_now:?}",
            spec.expect_searchers
        );
    }
    tracing::info!(
        label,
        searchers,
        members = %members_now
            .iter()
            .map(|m| format!("{}@{} gen {}", m.node_id, m.gossip, m.generation))
            .collect::<Vec<_>>()
            .join(", "),
        "bench start"
    );
    let quickwit = quickwit_build(&http, &spec.root).await;
    tracing::info!(label, quickwit = ?quickwit, "bench root");
    let started_at = Utc::now();
    let mut passes = Vec::new();
    if !spec.levels_only {
        passes.push(pass(&http, spec, &members_now, "first", 1, 0).await?);
        passes.push(pass(&http, spec, &members_now, "warm", 1, 0).await?);
    }
    for (i, &n) in spec.levels.iter().enumerate() {
        tokio::time::sleep(spec.pause).await;
        let shift = spec.offset + 1 + i as u32;
        passes.push(pass(&http, spec, &members_now, &format!("c{n}"), n, shift).await?);
    }
    Ok(Report {
        label: label.to_owned(),
        indexes: spec.indexes.clone(),
        american_stories: spec.american_stories,
        quickwit,
        members: members_now,
        searchers,
        started_at,
        ended_at: Utc::now(),
        passes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn every_search_parses_as_the_api_would() {
        let bounds = (d("1770-01-01"), d("1963-12-31"));
        for (name, q, w) in SEARCHES {
            let r = request(q, *w, bounds, 0).unwrap_or_else(|e| panic!("{name}: {e}"));
            if let Some((a, b)) = w {
                assert_eq!(r.filters.from, d(a), "{name}");
                assert_eq!(r.filters.to, d(b), "{name}");
            } else {
                assert_eq!((r.filters.from, r.filters.to), bounds, "{name}");
            }
        }
        let front = request(SEARCHES[11].1, None, bounds, 0).unwrap();
        assert!(front.filters.front_only && front.filters.states == ["GA"]);
        assert_eq!(
            request(SEARCHES[0].1, None, bounds, 0).unwrap().bucket,
            BucketUnit::Month
        );
    }

    #[test]
    fn windows_shift_back_and_stay_in_the_corpus() {
        let bounds = (d("1770-01-01"), d("1963-12-31"));
        let r = request("q=x", Some(("1896-01-01", "1896-12-31")), bounds, 3).unwrap();
        assert_eq!(
            (r.filters.from, r.filters.to),
            (d("1895-12-29"), d("1896-12-28"))
        );
        // The whole corpus keeps its start and loses its last days.
        let r = request("q=x", None, bounds, 3).unwrap();
        assert_eq!(
            (r.filters.from, r.filters.to),
            (d("1770-01-01"), d("1963-12-28"))
        );
        // A window outside a small corpus becomes the corpus.
        let small = (d("1895-01-01"), d("1897-12-31"));
        let r = request("q=x", Some(("1918-01-01", "1918-12-31")), small, 0).unwrap();
        assert_eq!((r.filters.from, r.filters.to), small);
    }

    #[test]
    fn samples_keep_the_pool_s_peaks() {
        let mut s = Sampled::default();
        for (ongoing, pending) in [(2.0, 0.0), (4.0, 3.0), (1.0, 1.0)] {
            s.add(&NodeCounters {
                search_ongoing: ongoing,
                search_pending: pending,
                ..NodeCounters::default()
            });
        }
        assert_eq!(s.samples, 3);
        assert_eq!((s.search_ongoing_max, s.search_pending_max), (4.0, 3.0));
        assert_eq!(s.samples_pending, 2);
    }

    #[test]
    fn reads_the_root_s_build() {
        let v = serde_json::json!({
            "build": {"version": "0.9.1", "commit_short_hash": "962685f", "cargo_pkg_version": "0.9.1"},
            "runtime": {"num_cpus": 4}
        });
        let b = QuickwitBuild::parse(&v);
        assert_eq!(b.version.as_deref(), Some("0.9.1"));
        assert_eq!(b.commit.as_deref(), Some("962685f"));
        assert_eq!(b.num_cpus, Some(4));
        assert_eq!(
            QuickwitBuild::parse(&serde_json::json!({})),
            QuickwitBuild::default()
        );
    }

    #[test]
    fn percentiles_by_nearest_rank() {
        let v: Vec<f64> = (1..=10).map(f64::from).collect();
        assert_eq!(percentile(&v, 0.5), 5.0);
        assert_eq!(percentile(&v, 0.9), 9.0);
        assert_eq!(percentile(&v, 1.0), 10.0);
        assert_eq!(percentile(&[3.0], 0.9), 3.0);
        assert_eq!(percentile(&[], 0.5), 0.0);
    }
}
