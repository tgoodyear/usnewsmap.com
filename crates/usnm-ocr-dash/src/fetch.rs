//! The scan fetch (#368), from the files the fetcher writes into
//! `tank/ocr/scans` (read-only here): `status-full.json`, `index-full.jsonl`
//! and `failures-full.jsonl`; the plan it was given; and the fetch queue's
//! manifests (`manifests/queue-ocr-*` and `queue-quality-*`, #1082), which
//! `usnm-loc-fetch-queue.sh` runs one at a time.
//!
//! The status file describes the fetcher's current run only, and the queue
//! rewrites it for every manifest. Progress that outlasts a run comes from
//! the index (every page ever fetched) and from each queued manifest.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use anyhow::Context;
use bytes::Bytes;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use usnm_ocr_contract::prefetch::decode_where;
use usnm_ocr_worker::prefetch::{self, priority, set_of, Order, Set, Sets};

use crate::tail::{Got, Tail};

/// The fetch groups, by [`prefetch::priority`]'s number. An issue goes in
/// its best page's group, as the fetcher orders it.
pub const GROUPS: [&str; 6] = [
    "German before the 1920s",
    "Worst English batches",
    "Czech",
    "Serbian",
    "Russian",
    "The rest of the keep set",
];

/// The fetcher's `status-<tag>.json`, read leniently, so a fetcher a
/// version apart still shows. `loc_holds` is a counter a later fetcher may
/// add; without it, holds are counted from the status as it is seen.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FetchStatus {
    pub state: String,
    pub version: String,
    pub started_at: String,
    pub updated_at: String,
    pub manifest: String,
    pub manifest_sha256: String,
    pub resolution: String,
    pub pages: u64,
    pub finished: u64,
    pub already_there: u64,
    pub fetched: u64,
    pub failed_permanent: u64,
    pub failed_transient: u64,
    /// Pages skipped while their issue's item cools off (#414).
    pub failed_item_error: u64,
    pub issues_cooling: u64,
    pub issues_item_permanent: u64,
    pub bytes_fetched: u64,
    pub bytes_on_disk: u64,
    pub max_total_bytes: Option<u64>,
    pub buffer_bytes: u64,
    pub buffer_cap: u64,
    pub keep_pages: u64,
    pub buffer_pages: u64,
    pub elapsed_secs: u64,
    pub fetched_per_hour: f64,
    pub eta_secs: Option<u64>,
    pub loc_hold_secs: Option<u64>,
    pub loc_holds: Option<u64>,
    pub last_doc_id: String,
    pub error: Option<String>,
    pub pid: u64,
    pub out: String,
    pub buffer_resolution: String,
    /// Of `fetched`: JP2s, and pages through the item API and IIIF JPEG.
    pub fetched_jp2: u64,
    pub fetched_item_api: u64,
    /// Of `fetched_jp2`: those whose bytes came from a mirror (#1087), and
    /// from LoC's bucket (#1114).
    pub fetched_mirror: u64,
    pub fetched_bucket: u64,
    pub mirrors: BTreeMap<String, RouteCounts>,
    pub bucket: Option<RouteCounts>,
}

/// One fetch route's counts (a mirror's, or LoC's bucket's), read
/// leniently: each has a few fields the other lacks.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RouteCounts {
    pub requests: u64,
    /// A mirror's JP2s that matched the bag's md5.
    pub hits: u64,
    /// The bucket's JP2s.
    pub jp2s: u64,
    pub misses: u64,
    pub md5_mismatches: u64,
    pub skipped_held: u64,
    pub holds: u64,
    pub held_secs: Option<u64>,
}

impl RouteCounts {
    /// JP2s taken from this route.
    pub fn taken(&self) -> u64 {
        self.hits.max(self.jp2s)
    }
}

/// The bulk OCR archive download's `bulk-status.json` (usnm-loc-bulk,
/// #1075), read leniently.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct BulkStatus {
    pub state: String,
    pub archives: u64,
    pub done: u64,
    pub fetched_this_run: u64,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_this_run: u64,
    pub elapsed_secs: f64,
    pub bytes_per_sec: f64,
    pub failed: Vec<serde_json::Value>,
    pub updated_at: String,
}

/// The host's global LoC pacer (`pacer.json`, #1075), which every LoC
/// request from prefetch and the bulk download goes through.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PacerState {
    pub sent: u64,
    /// Set while LoC has pushed back and the budget is lowered.
    pub slowed: Option<serde_json::Value>,
    pub hold_until_ms: u64,
    pub next_ms: u64,
    pub generation: u64,
}

/// A small JSON file read at each poll, if one is configured.
#[derive(Debug, Default)]
pub struct Watched<T> {
    pub path: Option<PathBuf>,
    pub value: Option<T>,
    pub error: Option<String>,
    /// The file's modification time.
    pub modified: Option<DateTime<Utc>>,
}

impl<T: serde::de::DeserializeOwned> Watched<T> {
    fn poll(&mut self) {
        let Some(path) = &self.path else {
            return;
        };
        let read = std::fs::metadata(path)
            .and_then(|m| Ok((m.modified()?, std::fs::read(path)?)))
            .map_err(anyhow::Error::from)
            .and_then(|(m, b)| {
                anyhow::ensure!(b.len() <= 1 << 20, "over 1 MB");
                Ok((m, serde_json::from_slice::<T>(&b)?))
            });
        match read {
            Ok((m, v)) => {
                self.value = Some(v);
                self.modified = Some(m.into());
                self.error = None;
            }
            Err(e) => {
                self.value = None;
                self.error = Some(format!("{}: {e:#}", path.display()));
            }
        }
    }
}

/// A queued manifest's kind, from its directory's name.
pub fn queue_kind(name: &str) -> Option<&'static str> {
    if name.starts_with("queue-ocr-") {
        Some("ocr")
    } else if name.starts_with("queue-quality-") {
        Some("quality")
    } else {
        None
    }
}

/// Whether a manifest path is one of the queue's (`.../queue-*/manifest.parquet`).
pub fn is_queue_manifest(path: &Path) -> bool {
    path.parent()
        .and_then(|d| d.file_name())
        .and_then(|n| n.to_str())
        .is_some_and(|n| queue_kind(n).is_some())
}

/// A manifest in the fetch queue, and how far the fetcher has got with it.
#[derive(Debug, Clone, Serialize)]
pub struct QueuedManifest {
    pub name: String,
    /// `ocr` or `quality`.
    pub kind: &'static str,
    pub path: String,
    pub pages: u64,
    /// Its pages in the index, by any run.
    pub fetched: u64,
    /// `queued`, `fetching`, `cooling` (skipped for a day after its pages'
    /// issues cooled off) or `fetched` (its run ended done with no
    /// transient failure).
    pub state: &'static str,
    /// When the `fetched` or `cooling` file was written.
    pub marked_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
}

/// A queued manifest as read: its pages as hashes, and, while it is
/// unfinished, its first pages in file order for the "to fetch" list.
#[derive(Debug)]
struct QueueEntry {
    info: QueuedManifest,
    stamp: Option<(SystemTime, u64)>,
    hashes: Vec<u64>,
    rows: Vec<(Box<str>, Box<str>)>,
}

/// Rows kept per unfinished manifest for the "to fetch" list.
const QUEUE_ROWS: usize = 2000;
/// How long a `cooling` file holds a manifest back, as the queue script does.
const COOLING: Duration = Duration::hours(24);

/// The fields of an index line used here.
#[derive(Debug, Deserialize)]
struct IndexLine {
    doc_id: String,
    fetched_at: DateTime<Utc>,
    bytes: u64,
}

/// A failure line.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FailureLine {
    pub doc_id: String,
    pub kind: String,
    pub error: String,
    pub at: DateTime<Utc>,
}

/// A keep-set page, in fetch order.
#[derive(Debug, Clone)]
pub struct PlanRow {
    pub doc_id: Box<str>,
    /// ISO 639-3, or `und`.
    pub lang: Box<str>,
    pub group: u8,
}

/// The manifest's keep set in the fetcher's order, and the size of its
/// buffer set.
#[derive(Debug, Default)]
pub struct Plan {
    pub path: PathBuf,
    pub sha256: String,
    pub keep: Vec<PlanRow>,
    pub pos: HashMap<Box<str>, u32>,
    pub buffer_pages: u64,
    /// The buffer set's pages, as hashes of their doc_ids (a buffer set
    /// can run to millions of pages).
    pub buffer: HashSet<u64>,
    pub group_pages: [u64; 6],
}

/// A doc_id's hash, for [`Plan::buffer`].
pub fn doc_hash(doc_id: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    doc_id.hash(&mut h);
    h.finish()
}

fn iso(lang: &str) -> &str {
    usnm_core::lang::marc_to_iso3(lang).unwrap_or(lang)
}

impl Plan {
    /// Read a manifest and order its keep set as `prefetch --sets keep
    /// --order priority` does.
    pub fn load(path: &Path) -> anyhow::Result<Plan> {
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        let sha256 = prefetch::sha256(&bytes);
        let mut buffer = HashSet::new();
        let rows = decode_where(Bytes::from(bytes), |r| {
            let keep = set_of(r) == Set::Keep;
            if !keep {
                buffer.insert(doc_hash(&r.doc_id));
            }
            keep
        })?;
        let buffer_pages = buffer.len() as u64;
        let rows = prefetch::select(
            rows,
            &prefetch::Options {
                sets: Sets::Keep,
                order: Order::Priority,
                ..prefetch::Options::default()
            },
        );
        let mut best: HashMap<(&str, NaiveDate, u16), u8> = HashMap::new();
        for r in &rows {
            let e = best.entry((&r.lccn, r.date, r.edition)).or_insert(u8::MAX);
            *e = (*e).min(priority(r));
        }
        let mut plan = Plan {
            path: path.to_owned(),
            sha256,
            buffer_pages,
            buffer,
            ..Plan::default()
        };
        for r in &rows {
            let group = best[&(r.lccn.as_str(), r.date, r.edition)].min(5);
            plan.group_pages[usize::from(group)] += 1;
            plan.pos
                .insert(r.doc_id.clone().into(), plan.keep.len() as u32);
            plan.keep.push(PlanRow {
                doc_id: r.doc_id.clone().into(),
                lang: iso(r.lang_or_und()).into(),
                group,
            });
        }
        Ok(plan)
    }
}

/// Pages and bytes.
#[derive(Debug, Clone, Copy, Default, Serialize)]
pub struct Tally {
    pub pages: u64,
    pub bytes: u64,
}

/// What the fetcher has done, built up from its files.
#[derive(Debug)]
pub struct Fetch {
    pub scans: PathBuf,
    /// The plan shown by set and group. Given, it is pinned: the fetcher's
    /// status naming another manifest (each queued one does) doesn't
    /// replace it.
    pub manifest: Option<PathBuf>,
    /// Where the queue's manifests are (`queue-ocr-*/`, `queue-quality-*/`).
    pub manifests: Option<PathBuf>,
    queue: BTreeMap<String, QueueEntry>,
    pub queue_error: Option<String>,
    /// Every page in the index, by any run (hashes), when each was last
    /// fetched; and their pages and bytes (each page's first fetch).
    all: HashMap<u64, DateTime<Utc>>,
    pub total: Tally,
    pub index_error: Option<String>,
    pub bulk: Watched<BulkStatus>,
    pub pacer: Watched<PacerState>,
    pub plan: Option<Plan>,
    pub plan_error: Option<String>,
    pub status: Option<FetchStatus>,
    pub status_error: Option<String>,
    index: Tail,
    failures_tail: Tail,
    /// The pages fetched, and when each was last.
    pub fetched: HashMap<Box<str>, DateTime<Utc>>,
    pub groups: [Tally; 6],
    pub buffer: Tally,
    /// Fetches (time, bytes) over the last day.
    events: VecDeque<(DateTime<Utc>, u64)>,
    /// The latest failures, newest last.
    pub failures: VecDeque<FailureLine>,
    pub dead: HashSet<String>,
    /// Pages skipped because loc.gov couldn't serve their issue's item
    /// (`loc_item_error`), and when they last were; until permanent.
    pub cooling: HashMap<String, DateTime<Utc>>,
    failure_times: VecDeque<DateTime<Utc>>,
    /// LoC holds seen over the last day.
    holds: VecDeque<DateTime<Utc>>,
    last_hold_until: Option<DateTime<Utc>>,
    last_hold_count: Option<u64>,
    /// The manifest last read, or tried: (path, sha256), when.
    attempt: Option<((PathBuf, String), DateTime<Utc>)>,
}

const KEEP_FAILURES: usize = 50;

impl Fetch {
    pub fn new(scans: PathBuf, manifest: Option<PathBuf>) -> Self {
        Fetch {
            index: Tail::new(scans.join("index-full.jsonl")),
            failures_tail: Tail::new(scans.join("failures-full.jsonl")),
            scans,
            manifest,
            manifests: None,
            queue: BTreeMap::new(),
            queue_error: None,
            all: HashMap::new(),
            total: Tally::default(),
            index_error: None,
            bulk: Watched::default(),
            pacer: Watched::default(),
            plan: None,
            plan_error: None,
            status: None,
            status_error: None,
            fetched: HashMap::new(),
            groups: [Tally::default(); 6],
            buffer: Tally::default(),
            events: VecDeque::new(),
            failures: VecDeque::new(),
            dead: HashSet::new(),
            cooling: HashMap::new(),
            failure_times: VecDeque::new(),
            holds: VecDeque::new(),
            last_hold_until: None,
            last_hold_count: None,
            attempt: None,
        }
    }

    /// Read the fetch queue's manifests from `dir`.
    pub fn with_manifests(mut self, dir: Option<PathBuf>) -> Self {
        self.manifests = dir;
        self
    }

    /// Read the bulk download's status and the LoC pacer's state too.
    pub fn with_files(mut self, bulk: Option<PathBuf>, pacer: Option<PathBuf>) -> Self {
        self.bulk.path = bulk;
        self.pacer.path = pacer;
        self
    }

    /// Forget what the index said, to read it again from the start.
    fn reset_index(&mut self) {
        self.index.offset = 0;
        self.all.clear();
        self.total = Tally::default();
        self.fetched.clear();
        self.groups = [Tally::default(); 6];
        self.buffer = Tally::default();
        self.events.clear();
    }

    /// Read what changed. Blocking (file reads); run it off the async
    /// threads.
    pub fn poll(&mut self, now: DateTime<Utc>) {
        self.read_status(now);
        self.load_plan(now);
        self.index_error = match self.read_index() {
            Ok(()) => None,
            Err(e) => {
                tracing::warn!(error = %e, "can't read the fetch index");
                Some(format!("{e:#}"))
            }
        };
        self.read_queue(now);
        self.bulk.poll();
        self.pacer.poll();
        if let Err(e) = self.read_failures() {
            tracing::warn!(error = %e, "can't read the fetch failures");
        }
        let day = now - Duration::days(1);
        while self.events.front().is_some_and(|(t, _)| *t < day) {
            self.events.pop_front();
        }
        while self.failure_times.front().is_some_and(|t| *t < day) {
            self.failure_times.pop_front();
        }
        while self.holds.front().is_some_and(|t| *t < day) {
            self.holds.pop_front();
        }
    }

    fn read_status(&mut self, now: DateTime<Utc>) {
        let path = self.scans.join("status-full.json");
        match std::fs::read(&path)
            .map_err(anyhow::Error::from)
            .and_then(|b| Ok(serde_json::from_slice::<FetchStatus>(&b)?))
        {
            Ok(s) => {
                self.count_holds(&s, now);
                self.status = Some(s);
                self.status_error = None;
            }
            Err(e) => self.status_error = Some(format!("{}: {e:#}", path.display())),
        }
    }

    /// A hold is counted from the fetcher's counter when it has one, else
    /// each time the status shows a new end to a hold.
    fn count_holds(&mut self, s: &FetchStatus, now: DateTime<Utc>) {
        if let Some(n) = s.loc_holds {
            if let Some(prev) = self.last_hold_count {
                for _ in 0..n.saturating_sub(prev).min(1000) {
                    self.holds.push_back(now);
                }
            }
            self.last_hold_count = Some(n);
            return;
        }
        let Some(secs) = s.loc_hold_secs else {
            return;
        };
        let Ok(at) = DateTime::parse_from_rfc3339(&s.updated_at) else {
            return;
        };
        let until = at.with_timezone(&Utc) + Duration::seconds(secs as i64);
        let new = self
            .last_hold_until
            .is_none_or(|prev| until > prev + Duration::seconds(5));
        if new {
            self.holds.push_back(at.with_timezone(&Utc));
            self.last_hold_until = Some(until);
        }
    }

    fn load_plan(&mut self, now: DateTime<Utc>) {
        let named = self
            .status
            .as_ref()
            .map(|s| s.manifest.as_str())
            .filter(|m| !m.is_empty())
            .map(PathBuf::from);
        // A pinned plan stays whatever the fetcher runs. Without one, the
        // status's manifest is followed, but never a queued one: each is a
        // small batch, and following it would swap the plan every run.
        let want = match &self.manifest {
            Some(m) => Some(m.clone()),
            None => named.clone().filter(|m| !is_queue_manifest(m)),
        };
        let Some(want) = want else {
            return;
        };
        // The status's sha256 checks the file only when it names this one.
        let sha = match (&named, &self.status) {
            (Some(n), Some(s)) if *n == want => s.manifest_sha256.clone(),
            _ => String::new(),
        };
        if self
            .plan
            .as_ref()
            .is_some_and(|p| p.path == want && (sha.is_empty() || sha == p.sha256))
        {
            return;
        }
        // A manifest that can't be read is tried again when the status
        // names another, or after ten minutes.
        let key = (want.clone(), sha);
        if self
            .attempt
            .as_ref()
            .is_some_and(|(k, t)| *k == key && now - *t < Duration::minutes(10))
        {
            return;
        }
        self.attempt = Some((key.clone(), now));
        let loaded = Plan::load(&want).and_then(|p| {
            // The file must be the one the fetcher read; one swapped in
            // under the same path would mix two plans.
            anyhow::ensure!(
                key.1.is_empty() || p.sha256 == key.1,
                "{}'s sha256 is {}, not the {} the fetcher's status names",
                want.display(),
                p.sha256,
                key.1
            );
            Ok(p)
        });
        match loaded {
            Ok(p) => {
                tracing::info!(manifest = %want.display(), keep = p.keep.len(), buffer = p.buffer_pages, "manifest read");
                self.plan = Some(p);
                self.plan_error = None;
                self.reset_index();
            }
            Err(e) => {
                tracing::warn!(error = format!("{e:#}"), "can't read the manifest");
                self.plan_error = Some(format!("{e:#}"));
                // The plan held is no longer the fetcher's: drop it rather
                // than mix it with the new status and index.
                if self.plan.take().is_some() {
                    self.reset_index();
                }
            }
        }
    }

    fn read_index(&mut self) -> anyhow::Result<()> {
        loop {
            let lines = match self.index.read()? {
                Got::Reset => {
                    self.reset_index();
                    continue;
                }
                Got::Lines(l) => l,
            };
            if lines.is_empty() {
                return Ok(());
            }
            for l in lines {
                let Ok(line) = serde_json::from_str::<IndexLine>(&l) else {
                    continue;
                };
                self.add_fetch(line);
            }
        }
    }

    fn add_fetch(&mut self, line: IndexLine) {
        self.events.push_back((line.fetched_at, line.bytes));
        match self.all.entry(doc_hash(&line.doc_id)) {
            std::collections::hash_map::Entry::Occupied(mut e) => {
                let t = e.get_mut();
                *t = (*t).max(line.fetched_at);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(line.fetched_at);
                self.total.pages += 1;
                self.total.bytes += line.bytes;
            }
        }
        // A page of neither set (an older manifest's, or before the
        // manifest is read) counts in the rates only.
        let set = self
            .plan
            .as_ref()
            .and_then(|p| match p.pos.get(line.doc_id.as_str()) {
                Some(i) => Some(Some(p.keep[*i as usize].group)),
                None => p.buffer.contains(&doc_hash(&line.doc_id)).then_some(None),
            });
        let Some(group) = set else {
            return;
        };
        if let Some(old) = self.fetched.get_mut(line.doc_id.as_str()) {
            *old = (*old).max(line.fetched_at);
            return;
        }
        self.fetched.insert(line.doc_id.into(), line.fetched_at);
        let t = match group {
            Some(g) => &mut self.groups[usize::from(g)],
            None => &mut self.buffer,
        };
        t.pages += 1;
        t.bytes += line.bytes;
    }

    fn read_failures(&mut self) -> anyhow::Result<()> {
        loop {
            let lines = match self.failures_tail.read()? {
                Got::Reset => {
                    self.failures.clear();
                    self.dead.clear();
                    self.cooling.clear();
                    self.failure_times.clear();
                    continue;
                }
                Got::Lines(l) => l,
            };
            if lines.is_empty() {
                return Ok(());
            }
            for l in lines {
                let Ok(f) = serde_json::from_str::<FailureLine>(&l) else {
                    continue;
                };
                match f.kind.as_str() {
                    "permanent" => {
                        self.cooling.remove(&f.doc_id);
                        self.dead.insert(f.doc_id.clone());
                    }
                    "loc_item_error" => {
                        let t = self.cooling.entry(f.doc_id.clone()).or_insert(f.at);
                        *t = (*t).max(f.at);
                    }
                    _ => {}
                }
                self.failure_times.push_back(f.at);
                self.failures.push_back(f);
                if self.failures.len() > KEEP_FAILURES {
                    self.failures.pop_front();
                }
            }
        }
    }

    /// Fetches and bytes since `since`.
    pub fn since(&self, since: DateTime<Utc>) -> Tally {
        self.events
            .iter()
            .rev()
            .take_while(|(t, _)| *t >= since)
            .fold(Tally::default(), |a, (_, b)| Tally {
                pages: a.pages + 1,
                bytes: a.bytes + b,
            })
    }

    pub fn holds_since(&self, since: DateTime<Utc>) -> usize {
        self.holds.iter().rev().take_while(|t| **t >= since).count()
    }

    /// Pages skipped for an item error and not fetched since: those in a
    /// manifest the queue will run again (queued or cooling), and the rest,
    /// which are asked for again only if they are queued again.
    pub fn cooling_pages(&self) -> (usize, usize) {
        let pending: HashSet<u64> = self
            .queue
            .values()
            .filter(|e| e.info.state != "fetched")
            .flat_map(|e| e.hashes.iter().copied())
            .collect();
        let mut queued = 0;
        let mut other = 0;
        for (d, at) in &self.cooling {
            let h = doc_hash(d);
            if self.all.get(&h).is_some_and(|f| f >= at) {
                continue;
            }
            if pending.contains(&h) {
                queued += 1;
            } else {
                other += 1;
            }
        }
        (queued, other)
    }

    /// Read the queue's directories; a manifest is decoded again only when
    /// its file changes.
    fn read_queue(&mut self, now: DateTime<Utc>) {
        let Some(dir) = self.manifests.clone() else {
            return;
        };
        let names = match std::fs::read_dir(&dir) {
            Ok(rd) => rd
                .filter_map(|e| e.ok())
                .filter_map(|e| e.file_name().into_string().ok())
                .filter(|n| queue_kind(n).is_some())
                .collect::<HashSet<String>>(),
            Err(e) => {
                self.queue_error = Some(format!("{}: {e}", dir.display()));
                return;
            }
        };
        self.queue_error = None;
        self.queue.retain(|n, _| names.contains(n));
        for name in names {
            let d = dir.join(&name);
            let path = d.join("manifest.parquet");
            let stamp = std::fs::metadata(&path)
                .ok()
                .and_then(|m| Some((m.modified().ok()?, m.len())));
            let marker = |f: &str| {
                std::fs::metadata(d.join(f))
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .map(DateTime::<Utc>::from)
            };
            let fetched_at = marker("fetched");
            let cooling_at = marker("cooling").filter(|t| now - *t < COOLING);
            let (state, marked_at) = match (fetched_at, cooling_at) {
                (Some(t), _) => ("fetched", Some(t)),
                (None, Some(t)) => ("cooling", Some(t)),
                (None, None) => ("queued", None),
            };
            let entry = self
                .queue
                .entry(name.clone())
                .or_insert_with(|| QueueEntry {
                    info: QueuedManifest {
                        kind: queue_kind(&name).unwrap_or("ocr"),
                        name: name.clone(),
                        path: path.display().to_string(),
                        pages: 0,
                        fetched: 0,
                        state,
                        marked_at,
                        error: None,
                    },
                    stamp: None,
                    hashes: Vec::new(),
                    rows: Vec::new(),
                });
            entry.info.state = state;
            entry.info.marked_at = marked_at;
            if stamp.is_none() {
                // Not there yet (a PUT in progress), or gone.
                entry.stamp = None;
                entry.hashes.clear();
                entry.rows.clear();
                entry.info.pages = 0;
                entry.info.error = Some("no manifest.parquet".into());
                continue;
            }
            // Rows are cached a window at a time; once every cached row is
            // fetched and the manifest has more, it is read again for the next.
            let window_done = state != "fetched"
                && (entry.rows.len() as u64) < entry.info.pages
                && entry
                    .rows
                    .iter()
                    .all(|(d, _)| self.all.contains_key(&doc_hash(d)));
            if entry.stamp == stamp && entry.info.error.is_none() && !window_done {
                if state == "fetched" {
                    entry.rows = Vec::new();
                }
                continue;
            }
            entry.stamp = stamp;
            let mut hashes = Vec::new();
            let mut rows = Vec::new();
            let keep_rows = state != "fetched";
            let read = std::fs::read(&path)
                .map_err(anyhow::Error::from)
                .and_then(|b| {
                    decode_where(Bytes::from(b), |r| {
                        hashes.push(doc_hash(&r.doc_id));
                        if keep_rows
                            && rows.len() < QUEUE_ROWS
                            && !self.all.contains_key(&doc_hash(&r.doc_id))
                        {
                            rows.push((
                                Box::<str>::from(r.doc_id.as_str()),
                                Box::<str>::from(iso(r.lang_or_und())),
                            ));
                        }
                        false
                    })
                });
            match read {
                Ok(_) => {
                    entry.info.pages = hashes.len() as u64;
                    entry.hashes = hashes;
                    entry.rows = rows;
                    entry.info.error = None;
                }
                Err(e) => {
                    tracing::warn!(manifest = %path.display(), error = format!("{e:#}"), "can't read a queued manifest");
                    entry.hashes.clear();
                    entry.rows.clear();
                    entry.info.pages = 0;
                    entry.info.error = Some(format!("{e:#}"));
                }
            }
        }
        // The totals leave out a manifest that can't be read: say so.
        let unread: Vec<&str> = self
            .queue
            .values()
            .filter(|e| e.info.error.is_some())
            .map(|e| e.info.name.as_str())
            .collect();
        if !unread.is_empty() {
            self.queue_error = Some(format!(
                "{} queued manifests can't be read, so the queue totals leave them out: {}",
                unread.len(),
                unread.join(", ")
            ));
        }
    }

    /// The queue's manifests in the order the queue takes them (OCR's
    /// before quality's, each by name), with their progress. The one the
    /// fetcher's status names is `fetching` while its run is.
    pub fn queue(&self, now: DateTime<Utc>) -> Vec<QueuedManifest> {
        // Only a current status says which manifest is being fetched.
        let running = self
            .status
            .as_ref()
            .filter(|s| self.status_error.is_none() && s.state == "running")
            .filter(|s| {
                DateTime::parse_from_rfc3339(&s.updated_at)
                    .is_ok_and(|at| (now - at.with_timezone(&Utc)) <= Duration::minutes(15))
            })
            .map(|s| s.manifest.as_str());
        let mut v: Vec<QueuedManifest> = self
            .queue
            .values()
            .map(|e| {
                let mut m = e.info.clone();
                m.fetched = e.hashes.iter().filter(|h| self.all.contains_key(h)).count() as u64;
                if m.state == "queued" && running == Some(m.path.as_str()) {
                    m.state = "fetching";
                }
                m
            })
            .collect();
        v.sort_by(|a, b| (a.kind != "ocr", &a.name).cmp(&(b.kind != "ocr", &b.name)));
        v
    }

    /// The next `n` queued pages not yet fetched, in the queue's order:
    /// (manifest, doc_id, language).
    pub fn next_queued(&self, n: usize) -> Vec<(String, String, String)> {
        let mut entries: Vec<&QueueEntry> = self
            .queue
            .values()
            .filter(|e| e.info.state == "queued")
            .collect();
        entries.sort_by(|a, b| {
            (a.info.kind != "ocr", &a.info.name).cmp(&(b.info.kind != "ocr", &b.info.name))
        });
        entries
            .into_iter()
            .flat_map(|e| {
                e.rows
                    .iter()
                    .filter(|(d, _)| !self.all.contains_key(&doc_hash(d)))
                    .map(move |(d, l)| (e.info.name.clone(), d.to_string(), l.to_string()))
            })
            .take(n)
            .collect()
    }

    pub fn failures_since(&self, since: DateTime<Utc>) -> usize {
        self.failure_times.iter().filter(|t| **t >= since).count()
    }

    /// The group the fetcher is in: its last page's.
    pub fn current_group(&self) -> Option<u8> {
        let s = self.status.as_ref()?;
        let p = self.plan.as_ref()?;
        p.pos
            .get(s.last_doc_id.as_str())
            .map(|i| p.keep[*i as usize].group)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_core::ids::PageKey;
    use usnm_ocr_contract::prefetch::{encode, ManifestRow};

    fn row(doc: &str, batch: &str, lang: &str) -> ManifestRow {
        let key = PageKey::from_doc_id(doc).unwrap();
        ManifestRow::new(&key, batch, 1, Some(lang.into()), "detected", 0.5)
    }

    #[test]
    fn the_status_the_fetcher_writes_reads_here() {
        let s = prefetch::Status {
            state: "running".into(),
            pages: 10,
            loc_hold_secs: Some(30),
            ..prefetch::Status::default()
        };
        let back: FetchStatus = serde_json::from_slice(&serde_json::to_vec(&s).unwrap()).unwrap();
        assert_eq!(back.state, "running");
        assert_eq!(back.pages, 10);
        assert_eq!(back.loc_hold_secs, Some(30));
        assert_eq!(back.loc_holds, None);
    }

    #[test]
    fn follows_the_fetcher_through_its_files() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.parquet");
        let rows = vec![
            row("sn1_1890-01-01_ed-1_seq-1", "az_a", "eng"),
            row("sn2_1925-01-01_ed-1_seq-1", "az_a", "ger"),
            row("sn3_1890-01-01_ed-1_seq-1", "az_a", "cze"),
            row("sn4_1890-01-01_ed-1_seq-1", "az_a", "ger"),
            row("sn4_1890-01-01_ed-1_seq-2", "az_a", "ger"),
        ];
        std::fs::write(&manifest, encode(&rows).unwrap()).unwrap();
        let mut f = Fetch::new(dir.path().to_owned(), Some(manifest));
        let now = Utc::now();
        f.poll(now);
        let plan = f.plan.as_ref().unwrap();
        assert_eq!(plan.buffer_pages, 1);
        let order: Vec<&str> = plan.keep.iter().map(|r| &*r.doc_id).collect();
        assert_eq!(
            order,
            [
                "sn4_1890-01-01_ed-1_seq-1",
                "sn4_1890-01-01_ed-1_seq-2",
                "sn3_1890-01-01_ed-1_seq-1",
                "sn2_1925-01-01_ed-1_seq-1"
            ]
        );
        assert_eq!(plan.group_pages, [2, 0, 1, 0, 0, 1]);
        assert_eq!(&*plan.keep[0].lang, "deu");

        let at = (now - Duration::minutes(5)).to_rfc3339();
        let old = (now - Duration::hours(3)).to_rfc3339();
        let later = (now - Duration::minutes(1)).to_rfc3339();
        std::fs::write(
            dir.path().join("index-full.jsonl"),
            format!(
                "{{\"doc_id\":\"sn4_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{old}\",\"bytes\":100}}\n\
                 {{\"doc_id\":\"sn1_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{at}\",\"bytes\":7}}\n\
                 {{\"doc_id\":\"sn4_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{at}\",\"bytes\":100}}\n\
                 {{\"doc_id\":\"sn9_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{at}\",\"bytes\":5}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("failures-full.jsonl"),
            format!("{{\"doc_id\":\"sn3_1890-01-01_ed-1_seq-1\",\"kind\":\"permanent\",\"error\":\"HTTP 404\",\"at\":\"{at}\"}}\n\
                     {{\"doc_id\":\"sn4_1890-01-01_ed-1_seq-1\",\"kind\":\"loc_item_error\",\"error\":\"HTTP 503\",\"at\":\"{old}\"}}\n\
                     {{\"doc_id\":\"sn1_1890-01-01_ed-1_seq-1\",\"kind\":\"loc_item_error\",\"error\":\"HTTP 503\",\"at\":\"{later}\"}}\n\
                     {{\"doc_id\":\"sn5_1890-01-01_ed-1_seq-1\",\"kind\":\"loc_item_error\",\"error\":\"HTTP 503\",\"at\":\"{at}\"}}\n"),
        )
        .unwrap();
        std::fs::write(
            dir.path().join("status-full.json"),
            format!("{{\"state\":\"running\",\"updated_at\":\"{at}\",\"loc_hold_secs\":60,\"last_doc_id\":\"sn4_1890-01-01_ed-1_seq-1\"}}"),
        )
        .unwrap();
        f.poll(now);
        assert_eq!(f.groups[0].pages, 1);
        assert_eq!(f.groups[0].bytes, 100);
        assert_eq!(f.buffer.pages, 1);
        // An older manifest's page is in the rates, not in either set.
        assert_eq!(f.buffer.bytes, 7);
        assert_eq!(f.since(now - Duration::hours(1)).pages, 3);
        assert_eq!(f.since(now - Duration::days(1)).bytes, 212);
        assert_eq!(f.current_group(), Some(0));
        assert!(f.dead.contains("sn3_1890-01-01_ed-1_seq-1"));
        // sn4's page was skipped for an item error and fetched since; sn1's
        // was fetched before its item error, so it counts, as does sn5's.
        // No queue: neither is in a manifest that will run again.
        assert_eq!(f.cooling_pages(), (0, 2));
        // Every page fetched, by any run: sn4, sn1 and sn9 once each.
        assert_eq!(f.total.pages, 3);
        assert_eq!(f.total.bytes, 112);
        assert_eq!(f.holds_since(now - Duration::hours(1)), 1);
        // The same hold seen again isn't counted twice.
        f.poll(now);
        assert_eq!(f.holds_since(now - Duration::hours(1)), 1);
    }

    #[test]
    fn a_pinned_plan_stays_while_the_queue_runs_and_each_manifest_shows_its_progress() {
        let dir = tempfile::tempdir().unwrap();
        let scans = dir.path().join("scans");
        let manifests = dir.path().join("manifests");
        let plan_dir = manifests.join("pages-v1");
        std::fs::create_dir_all(&scans).unwrap();
        std::fs::create_dir_all(&plan_dir).unwrap();
        let plan = plan_dir.join("manifest.parquet");
        std::fs::write(
            &plan,
            encode(&[
                row("sn4_1890-01-01_ed-1_seq-1", "az_a", "ger"),
                row("sn4_1890-01-01_ed-1_seq-2", "az_a", "ger"),
            ])
            .unwrap(),
        )
        .unwrap();
        let queue = |name: &str, docs: &[&str]| {
            let d = manifests.join(name);
            std::fs::create_dir_all(&d).unwrap();
            let rows: Vec<ManifestRow> = docs.iter().map(|x| row(x, "az_a", "ger")).collect();
            std::fs::write(d.join("manifest.parquet"), encode(&rows).unwrap()).unwrap();
            d
        };
        let q1 = queue(
            "queue-ocr-20261024t181421",
            &["sn4_1890-01-01_ed-1_seq-1", "sn7_1890-01-01_ed-1_seq-1"],
        );
        std::fs::write(q1.join("fetched"), "2026-10-24T23:19:00Z\n").unwrap();
        let q2 = queue(
            "queue-ocr-20261025t015125",
            &["sn8_1890-01-01_ed-1_seq-1", "sn8_1890-01-01_ed-1_seq-2"],
        );
        queue(
            "queue-quality-20261025t000000",
            &["sn9_1890-01-01_ed-1_seq-1"],
        );
        std::fs::create_dir_all(manifests.join("batches")).unwrap();
        let now = Utc::now();
        let at = (now - Duration::minutes(5)).to_rfc3339();
        std::fs::write(
            scans.join("index-full.jsonl"),
            format!(
                "{{\"doc_id\":\"sn4_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{at}\",\"bytes\":10}}\n\
                 {{\"doc_id\":\"sn7_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{at}\",\"bytes\":10}}\n\
                 {{\"doc_id\":\"sn8_1890-01-01_ed-1_seq-1\",\"fetched_at\":\"{at}\",\"bytes\":10}}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            scans.join("failures-full.jsonl"),
            format!("{{\"doc_id\":\"sn8_1890-01-01_ed-1_seq-2\",\"kind\":\"loc_item_error\",\"error\":\"HTTP 503\",\"at\":\"{at}\"}}\n"),
        )
        .unwrap();
        std::fs::write(
            scans.join("status-full.json"),
            format!(
                "{{\"state\":\"running\",\"updated_at\":\"{at}\",\"manifest\":\"{}\",\"manifest_sha256\":\"{}\",\
                 \"fetched_jp2\":2,\"fetched_mirror\":1,\"mirrors\":{{\"wayback\":{{\"requests\":2,\"hits\":1,\"holds\":1,\"held_secs\":59}}}}}}",
                q2.join("manifest.parquet").display(),
                "0".repeat(64)
            ),
        )
        .unwrap();
        let mut f = Fetch::new(scans.clone(), Some(plan.clone())).with_manifests(Some(manifests));
        f.poll(now);
        // The pinned plan, not the queued manifest the status names.
        assert!(f.plan_error.is_none(), "{:?}", f.plan_error);
        assert_eq!(f.plan.as_ref().unwrap().path, plan);
        assert_eq!(f.groups[0].pages, 1);
        assert_eq!(f.total.pages, 3);
        let s = f.status.as_ref().unwrap();
        assert_eq!(s.mirrors["wayback"].taken(), 1);
        assert_eq!(s.fetched_mirror, 1);
        let q = f.queue(now);
        let names: Vec<(&str, &str, u64, u64)> = q
            .iter()
            .map(|m| (m.name.as_str(), m.state, m.pages, m.fetched))
            .collect();
        assert_eq!(
            names,
            [
                ("queue-ocr-20261024t181421", "fetched", 2, 2),
                ("queue-ocr-20261025t015125", "fetching", 2, 1),
                ("queue-quality-20261025t000000", "queued", 1, 0),
            ]
        );
        // sn8's second page cools off in a manifest the queue runs again.
        assert_eq!(f.cooling_pages(), (1, 0));
        let next = f.next_queued(5);
        let docs: Vec<&str> = next.iter().map(|(_, d, _)| d.as_str()).collect();
        // The one being fetched first, then the quality one.
        assert_eq!(
            docs,
            ["sn8_1890-01-01_ed-1_seq-2", "sn9_1890-01-01_ed-1_seq-1"]
        );
        // A cooling file holds a manifest back for a day.
        std::fs::write(q2.join("cooling"), "x").unwrap();
        f.poll(now);
        assert_eq!(f.queue(now)[1].state, "cooling");

        // Unpinned, a queued manifest named by the status is not followed.
        let mut f = Fetch::new(scans, None);
        f.poll(now);
        assert!(f.plan.is_none());
        assert!(f.plan_error.is_none());
        assert_eq!(f.total.pages, 3);
    }

    #[test]
    fn refuses_a_manifest_that_isnt_the_one_the_fetcher_read() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("manifest.parquet");
        let rows = vec![row("sn4_1890-01-01_ed-1_seq-1", "az_a", "ger")];
        std::fs::write(&manifest, encode(&rows).unwrap()).unwrap();
        std::fs::write(
            dir.path().join("status-full.json"),
            format!(
                "{{\"state\":\"running\",\"manifest\":\"{}\",\"manifest_sha256\":\"{}\"}}",
                manifest.display(),
                "0".repeat(64)
            ),
        )
        .unwrap();
        let mut f = Fetch::new(dir.path().to_owned(), None);
        f.poll(Utc::now());
        assert!(f.plan.is_none());
        assert!(f.plan_error.as_deref().unwrap().contains("sha256"));
    }
}
