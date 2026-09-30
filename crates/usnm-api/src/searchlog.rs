//! The anonymous search log (06 §6.8, ADR-0012).
//!
//! Every search a visitor runs on the site is kept, indefinitely, in the
//! `searches` container as one line of JSON ([`Record`]): the words, the
//! match mode and filters, the number of pages matched, the index version
//! and the UTC day. Nothing that identifies or links a person is stored
//! with it: no address, user agent, referrer, cookie, request or session id,
//! location, or time of day.
//!
//! A search counts once, on `/v1/aggregate`, which the site requests once
//! per search; the coverage and hits requests that follow aren't counted.
//! It counts when the response is a 200, from a cache or not. Not counted
//! ([`admit`]): the warm-up's queries (they don't pass through the handler),
//! requests with `DNT: 1` or `Sec-GPC: 1`, crawler, script and headless user
//! agents, and requests that don't come from the site's own pages.
//!
//! The handler hands a record to a bounded queue and never waits: when the
//! queue is full the record is dropped and counted. A background task
//! batches the records and, every `flush_interval` and at shutdown, appends
//! each day's batch in random order to `staging/{day}.jsonl` (an append
//! blob). Once a day has been over for [`CLOSE_AFTER`], a replica reads that
//! day's staging file, shuffles all of its lines and writes them once to
//! `days/{day}.jsonl`, which is never rewritten. A lifecycle rule deletes
//! staging files after [`STAGING_DAYS`] days. So the permanent file's line
//! order says nothing about when in the day a search ran, and the only time
//! a record carries is its day.
//!
//! Search text never goes to the console or to telemetry, write errors
//! included: errors name the blob path (`staging/2026-09-29.jsonl`) and the
//! number of records, never their content.

use std::collections::{BTreeMap, BTreeSet};
use std::hash::{BuildHasher, RandomState};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::HeaderMap;
use chrono::{DateTime, Days, NaiveDate, Utc};
use opentelemetry::metrics::{Counter, Meter};
use opentelemetry::KeyValue;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use usnm_core::params::{RawParams, SearchRequest};
use usnm_core::query::{Mode, MAX_QUERY_CHARS};
use usnm_store::{ObjectStore, MAX_APPEND_BYTES};

// The same crawler, script and headless classification as page views (06 §6.3.7).
use crate::routes::is_bot;

/// The record format; bump it when a field changes meaning.
pub const FORMAT: u32 = 1;

/// A day is written to `days/` this long after it ends (UTC), so that
/// batches still in memory or being retried when it ended are in it.
pub const CLOSE_AFTER: Duration = Duration::from_secs(3600);

/// Staging files are deleted this many days after their last write (the
/// `expire-search-staging` lifecycle rule in `infra/modules/storage.bicep`).
/// A day not written to `days/` within that time is lost.
pub const STAGING_DAYS: u64 = 7;

/// How often a replica looks for ended days to write to `days/`.
const CLOSE_CHECK: Duration = Duration::from_secs(3600);

const CONTENT_TYPE: &str = "application/x-ndjson";

/// One search, as stored.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    /// [`FORMAT`].
    pub v: u32,
    /// The UTC day of the search: the only time kept.
    pub day: NaiveDate,
    /// `api` (recorded by the API) or `import` (from the search engine's
    /// console log before the search log existed; see `scripts/import-search-log.py`).
    pub source: String,
    /// The words as submitted: trimmed, runs of spaces and control
    /// characters folded to one space, at most 256 characters. Imports have
    /// the canonical query here too.
    pub q: String,
    /// The query as the API ran it, in canonical form: the same rendering
    /// as `q=` in the response's canonical URL (06 §6.3.1).
    pub query: String,
    /// `phrase`, `all`, `any` or `near`; `null` when none was sent (query
    /// syntax), and for imports.
    pub mode: Option<String>,
    /// The `near` distance, with `mode` `near`.
    pub near: Option<u8>,
    pub fuzzy: u8,
    /// The date range searched, after clamping to the corpus.
    pub from: NaiveDate,
    pub to: NaiveDate,
    pub state: Vec<String>,
    pub lccn: Vec<String>,
    pub lang: Vec<String>,
    pub front: bool,
    /// The bucket asked for (`year`, `month`, `week` or `day`).
    pub bucket: String,
    /// Pages matched (`total.hits`); `null` when unknown (imports).
    pub pages: Option<u64>,
    /// The index version searched; `null` for imports.
    pub index_version: Option<String>,
}

/// Whether a search request goes in the log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Admission {
    Record,
    /// Not recorded, with the reason for `api.search_log`.
    Excluded(&'static str),
}

/// Decide from the request headers alone. `site_host` is the site's
/// canonical hostname (`usnewsmap.com`).
pub fn admit(headers: &HeaderMap, site_host: &str) -> Admission {
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::trim)
    };
    if header("dnt") == Some("1") || header("sec-gpc") == Some("1") {
        return Admission::Excluded("opted_out");
    }
    // The site calls the API on its own origin. Browsers send Sec-Fetch-Site
    // with every fetch, and Origin only on some; either one has to say the
    // request came from the site, and neither may say otherwise. Requests
    // typed into the address bar (`none`), from other sites and from
    // scripts without these headers don't count.
    let own = format!("https://{site_host}");
    let origin = header("origin");
    let fetch_site = header("sec-fetch-site");
    if origin.is_some_and(|o| o != own)
        || fetch_site.is_some_and(|s| s != "same-origin")
        || (origin.is_none() && fetch_site.is_none())
    {
        return Admission::Excluded("other_origin");
    }
    if is_bot(header("user-agent").unwrap_or_default()) {
        return Admission::Excluded("bot");
    }
    Admission::Record
}

/// What the handler knows about a search, before the response exists.
#[derive(Debug, Clone)]
pub(crate) struct Search {
    q: String,
    mode: Option<Mode>,
    near: Option<u8>,
    fuzzy: u8,
    req: SearchRequest,
    index_version: String,
}

impl Search {
    /// From a request that validated (`req` was built from `raw`).
    pub(crate) fn new(raw: &RawParams, req: &SearchRequest, index_version: &str) -> Self {
        let mode = raw.get("mode").and_then(Mode::parse);
        let near = match mode {
            Some(Mode::Near) => raw.get("near").and_then(|n| n.parse().ok()),
            _ => None,
        };
        Self {
            q: normalize(raw.get("q").unwrap_or_default()),
            mode,
            near,
            fuzzy: raw
                .get("fuzzy")
                .and_then(|f| f.parse().ok())
                .unwrap_or_default(),
            req: req.clone(),
            index_version: index_version.to_owned(),
        }
    }
}

/// The words as submitted, trimmed, with runs of whitespace and control
/// characters folded to one space and at most [`MAX_QUERY_CHARS`] characters
/// (the API refuses longer queries anyway).
pub fn normalize(q: &str) -> String {
    let spaced: String = q
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let folded = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    folded.chars().take(MAX_QUERY_CHARS).collect()
}

/// A search with its response body (for the page count), queued for the writer.
pub(crate) struct Entry {
    at: DateTime<Utc>,
    search: Search,
    body: Arc<Vec<u8>>,
}

impl Entry {
    pub(crate) fn new(at: DateTime<Utc>, search: Search, body: Arc<Vec<u8>>) -> Self {
        Self { at, search, body }
    }

    /// The stored record: the day of `at`, never the time.
    pub(crate) fn record(&self) -> Record {
        let s = &self.search;
        let f = &s.req.filters;
        Record {
            v: FORMAT,
            day: self.at.date_naive(),
            source: "api".to_owned(),
            q: s.q.clone(),
            query: s.req.query.to_string(),
            mode: s.mode.map(|m| m.as_str().to_owned()),
            near: s.near,
            fuzzy: s.fuzzy,
            from: f.from,
            to: f.to,
            state: f.states.clone(),
            lccn: f.lccns.clone(),
            lang: f.langs.clone(),
            front: f.front_only,
            bucket: s.req.bucket.as_str().to_owned(),
            pages: pages(&self.body),
            index_version: Some(s.index_version.clone()),
        }
    }
}

/// `total.hits` from an aggregate response body.
fn pages(body: &[u8]) -> Option<u64> {
    #[derive(Deserialize)]
    struct Body {
        total: Total,
    }
    #[derive(Deserialize)]
    struct Total {
        hits: u64,
    }
    serde_json::from_slice::<Body>(body)
        .ok()
        .map(|b| b.total.hits)
}

/// `staging/{day}.jsonl`: the day's batches as they were appended.
pub fn staging_path(day: NaiveDate) -> String {
    format!("staging/{day}.jsonl")
}

/// `days/{day}.jsonl`: the day's records in random order, written once.
pub fn day_path(day: NaiveDate) -> String {
    format!("days/{day}.jsonl")
}

/// Shuffle in place (Fisher-Yates), keyed by a fresh random SipHash key.
fn shuffle<T>(items: &mut [T]) {
    let keys = RandomState::new();
    for i in (1..items.len()).rev() {
        let j = (keys.hash_one(i) % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

/// Tuning for the writer.
#[derive(Debug, Clone)]
pub struct LogConfig {
    /// How often batches are appended.
    pub flush_interval: Duration,
    /// Records the handlers can queue before new ones are dropped.
    pub queue: usize,
    /// A batch this big is appended at once.
    pub max_batch: usize,
    /// Records kept for another try after failed appends; beyond this the
    /// oldest are dropped.
    pub max_pending: usize,
}

impl Default for LogConfig {
    fn default() -> Self {
        Self {
            flush_interval: Duration::from_secs(300),
            queue: 1024,
            max_batch: 2000,
            max_pending: 20_000,
        }
    }
}

#[derive(Clone)]
struct LogMetrics {
    /// Searches by `outcome`: queued, dropped (queue full), written, lost
    /// (a failed or late write), and the exclusions (opted_out, bot, other_origin).
    records: Counter<u64>,
    /// Blob operations by `op` (append, close) and `outcome` (ok, error).
    writes: Counter<u64>,
}

impl LogMetrics {
    fn new(meter: &Meter) -> Self {
        Self {
            records: meter
                .u64_counter("api.search_log")
                .with_description(
                    "Searches for the search log by outcome (queued, dropped, written, lost, \
                     opted_out, bot, other_origin)",
                )
                .build(),
            writes: meter
                .u64_counter("api.search_log_writes")
                .with_description("Search log blob writes by op (append, close) and outcome")
                .build(),
        }
    }

    fn records(&self, outcome: &'static str, n: usize) {
        if n > 0 {
            self.records
                .add(n as u64, &[KeyValue::new("outcome", outcome)]);
        }
    }

    fn write(&self, op: &'static str, ok: bool) {
        self.writes.add(
            1,
            &[
                KeyValue::new("op", op),
                KeyValue::new("outcome", if ok { "ok" } else { "error" }),
            ],
        );
    }
}

/// The handlers' side of the search log.
pub struct SearchLog {
    tx: mpsc::Sender<Entry>,
    stop: watch::Sender<bool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    metrics: LogMetrics,
}

impl std::fmt::Debug for SearchLog {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchLog").finish_non_exhaustive()
    }
}

impl SearchLog {
    /// Start the writer on the current runtime.
    pub fn start(store: Arc<dyn ObjectStore>, config: LogConfig, meter: &Meter) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(config.queue.max(1));
        let (stop, stopped) = watch::channel(false);
        let metrics = LogMetrics::new(meter);
        let worker = Writer {
            store,
            config,
            rx,
            stopped,
            pending: Vec::new(),
            closed: BTreeSet::new(),
            metrics: metrics.clone(),
        };
        Arc::new(Self {
            tx,
            stop,
            worker: Mutex::new(Some(tokio::spawn(worker.run()))),
            metrics,
        })
    }

    /// Queue a search without waiting; a full queue drops it.
    pub(crate) fn record(&self, entry: Entry) {
        match self.tx.try_send(entry) {
            Ok(()) => self.metrics.records("queued", 1),
            Err(_) => self.metrics.records("dropped", 1),
        }
    }

    /// Count a search that isn't recorded.
    pub(crate) fn excluded(&self, reason: &'static str) {
        self.metrics.records(reason, 1);
    }

    /// Append what's queued and stop the writer, waiting at most `limit`.
    pub async fn shutdown(&self, limit: Duration) {
        let _ = self.stop.send(true);
        let worker = self.worker.lock().expect("not poisoned").take();
        if let Some(worker) = worker {
            if tokio::time::timeout(limit, worker).await.is_err() {
                tracing::warn!("search log: the last append didn't finish in time");
            }
        }
    }
}

struct Writer {
    store: Arc<dyn ObjectStore>,
    config: LogConfig,
    rx: mpsc::Receiver<Entry>,
    stopped: watch::Receiver<bool>,
    /// Records waiting for the next append, in arrival order.
    pending: Vec<Record>,
    /// Days already written to `days/` (or found there).
    closed: BTreeSet<NaiveDate>,
    metrics: LogMetrics,
}

impl Writer {
    async fn run(mut self) {
        let mut flush = tokio::time::interval(self.config.flush_interval);
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        flush.tick().await;
        // The first tick is immediate: close days that ended while no
        // replica was running.
        let mut close = tokio::time::interval(CLOSE_CHECK);
        close.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = self.stopped.changed() => break,
                entry = self.rx.recv() => match entry {
                    Some(e) => {
                        self.pending.push(e.record());
                        if self.pending.len() >= self.config.max_batch {
                            self.flush().await;
                        }
                    }
                    None => break,
                },
                _ = flush.tick() => self.flush().await,
                _ = close.tick() => self.close_days(Utc::now()).await,
            }
        }
        while let Ok(e) = self.rx.try_recv() {
            self.pending.push(e.record());
        }
        self.flush().await;
    }

    /// Append the pending records, each day's to its staging file in random
    /// order. Records whose append failed stay pending.
    async fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        let mut by_day: BTreeMap<NaiveDate, Vec<Record>> = BTreeMap::new();
        for r in self.pending.drain(..) {
            by_day.entry(r.day).or_default().push(r);
        }
        let mut kept = Vec::new();
        for (day, mut records) in by_day {
            if self.closed.contains(&day) {
                // Its day file is already written; nothing reads staging now.
                self.metrics.records("lost", records.len());
                continue;
            }
            shuffle(&mut records);
            let lines: Vec<Vec<u8>> = records
                .iter()
                .map(|r| {
                    let mut line = serde_json::to_vec(r).expect("records serialize");
                    line.push(b'\n');
                    line
                })
                .collect();
            let path = staging_path(day);
            let mut start = 0;
            while start < lines.len() {
                // Whole lines, at most one Append Block each.
                let mut end = start;
                let mut size = 0;
                while end < lines.len()
                    && (end == start || size + lines[end].len() <= MAX_APPEND_BYTES)
                {
                    size += lines[end].len();
                    end += 1;
                }
                let body: Vec<u8> = lines[start..end].concat();
                match self.store.append(&path, body, CONTENT_TYPE).await {
                    Ok(()) => {
                        self.metrics.write("append", true);
                        self.metrics.records("written", end - start);
                        start = end;
                    }
                    Err(e) => {
                        self.metrics.write("append", false);
                        // The path and count only: never a record's content.
                        tracing::warn!(
                            error = %e,
                            records = lines.len() - start,
                            "search log append failed; keeping the records for the next try"
                        );
                        kept.extend(records.drain(start..));
                        break;
                    }
                }
            }
        }
        if kept.len() > self.config.max_pending {
            let lost = kept.len() - self.config.max_pending;
            kept.drain(..lost);
            self.metrics.records("lost", lost);
        }
        self.pending = kept;
    }

    /// Write `days/{day}.jsonl` for each day that ended at least
    /// [`CLOSE_AFTER`] before `now` and still has a staging file.
    async fn close_days(&mut self, now: DateTime<Utc>) {
        let today = now.date_naive();
        for back in 1..STAGING_DAYS {
            let Some(day) = today.checked_sub_days(Days::new(back)) else {
                continue;
            };
            if !closable(day, now) || self.closed.contains(&day) {
                continue;
            }
            match close_day(self.store.as_ref(), day).await {
                Ok(()) => {
                    self.closed.insert(day);
                }
                Err(e) => {
                    self.metrics.write("close", false);
                    tracing::warn!(error = %e, %day, "search log: writing the day file failed");
                }
            }
        }
        if let Some(oldest) = today.checked_sub_days(Days::new(STAGING_DAYS)) {
            self.closed.retain(|d| *d >= oldest);
        }
    }
}

/// Whether `day` ended at least [`CLOSE_AFTER`] before `now`.
pub fn closable(day: NaiveDate, now: DateTime<Utc>) -> bool {
    day.succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .is_some_and(|end| {
            now.signed_duration_since(end.and_utc())
                .to_std()
                .is_ok_and(|d| d >= CLOSE_AFTER)
        })
}

/// Shuffle a day's staged lines and write them to its day file, unless the
/// file exists (another replica wrote it) or nothing was staged.
pub async fn close_day(
    store: &dyn ObjectStore,
    day: NaiveDate,
) -> Result<(), usnm_store::StoreError> {
    if store.exists(&day_path(day)).await? {
        return Ok(());
    }
    let Some(staged) = store.get(&staging_path(day)).await? else {
        return Ok(());
    };
    let mut lines: Vec<&[u8]> = staged
        .split(|b| *b == b'\n')
        .filter(|l| !l.trim_ascii().is_empty())
        .collect();
    shuffle(&mut lines);
    let mut body = Vec::with_capacity(staged.len() + 1);
    for line in lines {
        body.extend_from_slice(line);
        body.push(b'\n');
    }
    store.put_new(&day_path(day), body, CONTENT_TYPE).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    const CHROME: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/140.0.0.0 Safari/537.36";

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for (k, v) in pairs {
            h.insert(*k, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    fn site() -> Vec<(&'static str, &'static str)> {
        vec![("user-agent", CHROME), ("sec-fetch-site", "same-origin")]
    }

    #[test]
    fn a_browser_on_the_site_is_recorded() {
        assert_eq!(admit(&headers(&site()), "usnewsmap.com"), Admission::Record);
        let mut h = site();
        h.push(("origin", "https://usnewsmap.com"));
        assert_eq!(admit(&headers(&h), "usnewsmap.com"), Admission::Record);
        // Origin alone (no Sec-Fetch-Site) is enough.
        let h = headers(&[("user-agent", CHROME), ("origin", "https://usnewsmap.com")]);
        assert_eq!(admit(&h, "usnewsmap.com"), Admission::Record);
        // DNT: 0 is not an opt-out.
        let mut h = site();
        h.push(("dnt", "0"));
        assert_eq!(admit(&headers(&h), "usnewsmap.com"), Admission::Record);
    }

    #[test]
    fn dnt_and_gpc_are_honored() {
        for opt in [("dnt", "1"), ("sec-gpc", "1"), ("dnt", " 1 ")] {
            let mut h = site();
            h.push(opt);
            assert_eq!(
                admit(&headers(&h), "usnewsmap.com"),
                Admission::Excluded("opted_out"),
                "{opt:?}"
            );
        }
    }

    #[test]
    fn other_origins_are_excluded() {
        let other = Admission::Excluded("other_origin");
        for extra in [
            vec![("origin", "https://example.com")],
            vec![("origin", "https://www.usnewsmap.com")],
            vec![("origin", "http://usnewsmap.com")],
            vec![("sec-fetch-site", "cross-site")],
            vec![("sec-fetch-site", "same-site")],
            // Typed into the address bar.
            vec![("sec-fetch-site", "none")],
        ] {
            let mut h = vec![("user-agent", CHROME)];
            h.extend(extra.iter().copied());
            if !extra.iter().any(|(k, _)| *k == "sec-fetch-site") {
                h.push(("sec-fetch-site", "same-origin"));
            }
            assert_eq!(admit(&headers(&h), "usnewsmap.com"), other, "{extra:?}");
        }
        // Neither header: not a page on the site.
        assert_eq!(
            admit(&headers(&[("user-agent", CHROME)]), "usnewsmap.com"),
            other
        );
    }

    #[test]
    fn bots_are_excluded() {
        for ua in [
            "",
            "Mozilla/5.0 (compatible; Googlebot/2.1; +http://www.google.com/bot.html)",
            "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) HeadlessChrome/140.0.0.0 Safari/537.36",
            "curl/8.7.1",
            "python-requests/2.32",
        ] {
            let h = headers(&[("user-agent", ua), ("sec-fetch-site", "same-origin")]);
            assert_eq!(
                admit(&h, "usnewsmap.com"),
                Admission::Excluded("bot"),
                "{ua}"
            );
        }
    }

    #[test]
    fn queries_are_trimmed_folded_and_capped() {
        assert_eq!(normalize("  Cross   of\tGold \n"), "Cross of Gold");
        assert_eq!(normalize("a\u{0}b\u{7}c"), "a b c");
        assert_eq!(normalize(&"é".repeat(300)).chars().count(), MAX_QUERY_CHARS);
    }

    #[test]
    fn shuffle_keeps_every_item() {
        let mut v: Vec<u32> = (0..1000).collect();
        shuffle(&mut v);
        assert_ne!(v, (0..1000).collect::<Vec<_>>());
        v.sort_unstable();
        assert_eq!(v, (0..1000).collect::<Vec<_>>());
        let mut one = [7];
        shuffle(&mut one);
        assert_eq!(one, [7]);
    }

    fn search(query: &str) -> Search {
        let raw = RawParams::parse(query).unwrap();
        let bounds = (
            NaiveDate::from_ymd_opt(1770, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(1963, 12, 31).unwrap(),
        );
        let req = SearchRequest::from_raw(&raw, bounds).unwrap();
        Search::new(&raw, &req, "pages-v1")
    }

    fn at(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    #[test]
    fn a_record_holds_the_search_and_its_day_only() {
        let body = Arc::new(br#"{"total":{"hits":1234}}"#.to_vec());
        let entry = Entry::new(
            at("2026-09-29T23:59:59.999Z"),
            search("q=++Cross+of+Gold+&mode=phrase&from=1896-06-01&to=1896-12-31&state=sc,GA&bucket=week&v=pages-v1"),
            body.clone(),
        );
        let json = serde_json::to_value(entry.record()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "v": 1,
                "day": "2026-09-29",
                "source": "api",
                "q": "Cross of Gold",
                "query": "\"cross of gold\"",
                "mode": "phrase",
                "near": null,
                "fuzzy": 0,
                "from": "1896-06-01",
                "to": "1896-12-31",
                "state": ["GA", "SC"],
                "lccn": [],
                "lang": [],
                "front": false,
                "bucket": "week",
                "pages": 1234,
                "index_version": "pages-v1"
            })
        );
        // Nothing finer than the day, and nothing about the client.
        let keys: Vec<&str> = json
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        for forbidden in [
            "ip",
            "user_agent",
            "referrer",
            "session",
            "request_id",
            "timestamp",
            "time",
            "hour",
            "city",
            "country",
        ] {
            assert!(!keys.contains(&forbidden), "{forbidden}");
        }
        let text = json.to_string();
        assert!(!text.contains("23:59"), "{text}");
        // The day is the UTC day of the search.
        let next = Entry::new(at("2026-09-30T00:00:00Z"), search("q=gold"), body.clone());
        assert_eq!(
            next.record().day,
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap()
        );
        let offset = Entry::new(at("2026-09-29T21:30:00-05:00"), search("q=gold"), body);
        assert_eq!(
            offset.record().day,
            NaiveDate::from_ymd_opt(2026, 9, 30).unwrap()
        );
    }

    #[test]
    fn modes_near_and_syntax() {
        let body = Arc::new(Vec::new());
        let r = Entry::new(
            at("2026-09-29T12:00:00Z"),
            search("q=gold+silver&mode=near&near=5"),
            body.clone(),
        )
        .record();
        assert_eq!((r.mode.as_deref(), r.near), (Some("near"), Some(5)));
        assert_eq!(r.query, "\"gold silver\"~5");
        let r = Entry::new(
            at("2026-09-29T12:00:00Z"),
            search("q=gold+OR+silver&front=true&lang=ENG"),
            body,
        )
        .record();
        assert_eq!((r.mode, r.near, r.pages), (None, None, None));
        assert_eq!(r.query, "gold OR silver");
        assert!(r.front);
        assert_eq!(r.lang, ["eng"]);
        assert_eq!(r.bucket, "year");
    }

    #[test]
    fn a_day_closes_an_hour_after_it_ends() {
        let day = NaiveDate::from_ymd_opt(2026, 9, 29).unwrap();
        assert!(!closable(day, at("2026-09-29T23:00:00Z")));
        assert!(!closable(day, at("2026-09-30T00:59:59Z")));
        assert!(closable(day, at("2026-09-30T01:00:00Z")));
        assert!(closable(day, at("2026-10-03T12:00:00Z")));
    }

    #[test]
    fn page_counts_come_from_the_response() {
        assert_eq!(
            pages(br#"{"total":{"hits":42,"places":3},"x":[1]}"#),
            Some(42)
        );
        assert_eq!(pages(b"not json"), None);
    }
}
