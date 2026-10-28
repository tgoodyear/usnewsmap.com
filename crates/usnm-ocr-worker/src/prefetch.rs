//! `usnm-ocr-worker prefetch` (#368): fetch the scans a manifest lists
//! (`usnm_ocr_contract::prefetch`) onto a node's disk ahead of the workers,
//! so every engine reads the same local scan and LoC serves each image
//! once.
//!
//! **Fetching** goes through [`LocClient`], the worker's own client, batch
//! route first: the page's JP2 from its batch directory on LoC's storage
//! service, found through the batch's bag manifest and the issue's METS
//! (both kept on disk), and only for a page that route can't place, the
//! item API and the IIIF JPEG. The same User-Agent, 1 s between storage
//! requests (sharing the image server's budget unless told otherwise), 1 s
//! between image requests and 5 s between loc.gov API requests (shared by
//! every page in flight in this process),
//! `Retry-After` honoured on 429 and 503 and a 403 held the same way, at
//! most an hour. Nothing works around a block: after
//! [`Options::max_failures_in_a_row`] pages from at least
//! [`Options::stop_min_titles`] titles fail in a row the run stops.
//! A lock file in the output directory keeps it to one fetching process.
//!
//! **Mirrors** ([`Options::mirrors`], off unless named): a full-size JP2
//! the batch route has placed (LoC's METS gave its frame and the bag its
//! md5) is asked of a copy of LoC's files first: the state partner that
//! serves its awardee's batches, then the Wayback Machine
//! ([`crate::loc::mirror`]). Bytes that match the bag's md5 are taken with
//! LoC's URL and IIIF base, as LoC's own would be; anything else (a miss,
//! another file, a hold) goes on to LoC. Each mirror is paced and held on
//! its own, never in LoC's budget. A page a mirror served says nothing
//! about LoC, so it neither ends a run of failures nor clears a cool-off.
//! Where each mirrored page's bytes came from is in
//! `<out>/sources-<tag>.jsonl` ([`SourceLine`]); the sidecar is the same as
//! for LoC's.
//!
//! **LoC's bucket** ([`crate::loc::bucket`], off unless the client is given
//! it): LoC's own S3 copy of the batches it holds, asked before
//! `tile.loc.gov` for the bag manifest and the METS (inside the client) and
//! for a full-size JP2, before the mirrors. It is checked, recorded and
//! counted as a mirror is (`sources-<tag>.jsonl` with source
//! `ndnp-batches`, `fetched_bucket` and the status's `bucket`), and like a
//! mirror's, a page it served neither ends a run of failures nor clears a
//! cool-off: it says nothing about `tile.loc.gov`. Its sidecar has the
//! URL and IIIF base the client placed the page under: for a batch whose
//! manifest came from the bucket, the guess its name gives until LoC
//! confirms or moves it ([`crate::loc::Bag::guessed`]).
//!
//! **Item errors** (#414): when loc.gov answers 503 for one issue's item
//! while serving others ([`FetchError::Item`], the client's notes say when;
//! on the storage service, the issue's METS or a JP2), the issue's pages
//! are skipped on that route for [`Options::item_cooloff`] and asked
//! again after it, in the same run (it waits for them at the end) or the
//! next. After [`Options::item_max_cooloffs`] cool-offs the issue is
//! permanent. Each skipped page is a `loc_item_error` failure line; the
//! issues are kept in `<out>/item-errors.json` ([`ItemError`]) so a restart
//! keeps their cool-offs.
//!
//! **Two sets** (the maintainer's split, #368): the *keep* set, every
//! non-English page and the worst English batches' pages, is kept for good;
//! the *buffer* set, the rest of English, is a rolling buffer that the
//! workers empty as they read it (ADR-0015 decision 9). Keep pages are
//! fetched first, in [`priority`] order, then buffer pages in issue order,
//! and the fetcher waits whenever the buffer's scans on disk reach
//! [`Options::buffer_cap`], looking again every minute.
//!
//! **Layout**: `<out>/<set>/<batch>/<lccn>/<date>/<edition>/<seq>.<ext>`
//! (`<set>` is `keep` or `buffer`, so nothing that empties the buffer can
//! reach a keep scan), where
//! `<ext>` is `jp2` for a full scan fetched through its batch (the original
//! JP2, checked against the bag's md5), `jpg` for a full scan fetched as
//! the IIIF JPEG (before #368's batch route, or through the item API when
//! the batch route can't place the page) and `pct<n>.jpg` for one scaled
//! to n%, with a sidecar `<file>.json` ([`Sidecar`]) holding the LoC URL,
//! the IIIF base, the fetch time, the size, the sha256 and, for a JP2, the
//! bag's md5. The scan is written to a `.part`
//! file, synced and renamed, then the sidecar the same way, so a scan
//! counts only once its sidecar is there. Each fetch is also appended to
//! `<out>/index-<tag>.jsonl`, and each failure to `<out>/failures-<tag>.jsonl`
//! (`<tag>` is `full` or `pct<n>`).
//!
//! **Resuming**: a page whose scan and sidecar are there, in either format,
//! with the size, sha256 and (if recorded) md5 the sidecar records, is
//! skipped; anything else is fetched again. The JPEGs fetched before the
//! batch route stay valid and aren't fetched again as JP2s.
//!
//! **Status**: `<out>/status-<tag>.json` ([`Status`]), rewritten every
//! [`Options::progress_every`] pages and every 30 s, with a log line
//! (JSON on stdout, so journald under systemd) at each one. Logs name
//! pages by doc_id.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use bytes::Bytes;
use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use tokio::sync::{watch, Semaphore};
use tokio::time::Instant;
use usnm_core::ids::PageKey;
use usnm_ocr_contract::prefetch::{self, ManifestRow};
// The sets and the fetch order are the contract's, so the seeder (12 Step
// 11) can queue pages in the order they are fetched.
pub use usnm_ocr_contract::prefetch::{priority, set_of, Set, WORST_ENGLISH};

use crate::loc::bucket::{self, BucketStatus};
use crate::loc::mirror::{self, Mirrors};
use crate::loc::{issue_id, FetchError, ImageSize, LocClient, Scan, ScanFormat, USER_AGENT};

mod cooling;
mod estimate;
mod layout;
mod select;

use cooling::{Check, Cooling};
use layout::{last_fetched, lock, there, write_atomic, Lines};

pub use cooling::ItemError;
pub use estimate::{estimate, Estimate};
pub use layout::{
    consumed_path, dir_scan_bytes, formats, matches_sidecar, page_path, read_index, scan_path,
    sidecar_path, tag, Sidecar,
};
pub use select::{sample, select};

/// How often a fetcher waiting at the buffer cap counts the buffer again.
pub const BUFFER_RECHECK: Duration = Duration::from_secs(60);

/// What to fetch.
#[derive(Debug, Clone)]
pub struct Options {
    /// The manifest (`manifest.parquet`).
    pub manifest: PathBuf,
    /// The root the scans go under.
    pub out: PathBuf,
    /// The keep set's resolution.
    pub size: ImageSize,
    /// The buffer set's resolution.
    pub buffer_size: ImageSize,
    /// Which sets to fetch.
    pub sets: Sets,
    /// Wait while the buffer set's scans on disk reach this many bytes.
    pub buffer_cap: u64,
    /// Only the first this many pages (after `sample`).
    pub limit: Option<usize>,
    /// A sample of about this many pages, stratified by language and decade.
    pub sample: Option<usize>,
    /// Pages in flight at once (they share the pacers).
    pub concurrency: usize,
    /// A status write and log line every this many pages.
    pub progress_every: u64,
    /// Stop after this many pages fail in a row...
    pub max_failures_in_a_row: u32,
    /// ...from at least this many titles, so one title's broken items
    /// can't stop the run (1: any pages).
    pub stop_min_titles: usize,
    /// How long an issue whose item was an item error is skipped.
    pub item_cooloff: Duration,
    /// Cool-offs before such an issue is taken as permanent.
    pub item_max_cooloffs: u32,
    /// The order pages are fetched in.
    pub order: Order,
    /// Stop starting pages once the scans of this run's pages that are
    /// on disk (already there or fetched) add up to this many bytes.
    pub max_total_bytes: Option<u64>,
    /// The lock that keeps it to one fetching process; default
    /// `<out>/.prefetch.lock`. Runs into different directories that must
    /// not overlap (a sample beside the bulk fetch) share one.
    pub lock_file: Option<PathBuf>,
    /// Copies of LoC's JP2s to ask before LoC (the module notes); none by
    /// default.
    pub mirrors: Option<Arc<Mirrors>>,
    /// Only pages of these awardees' batches (the batch name's first part,
    /// e.g. `nbu`), such as those a mirror serves; all when empty.
    pub awardees: Vec<String>,
    /// Manifest directory-name prefixes (e.g. `queue-quality-`) whose
    /// manifests take pages' JP2s only from LoC's bucket or a mirror: a page
    /// neither has is deferred (`deferred_needs_loc`), never asked of LoC's
    /// storage service, item API or IIIF, and left for a later run. Batch
    /// manifests and METS still come from LoC when the bucket can't serve
    /// them. None by default.
    pub no_loc_prefixes: Vec<String>,
}

/// Whether `manifest` lives in a directory whose name starts with one of
/// `prefixes` (empty ones ignored).
pub fn no_loc_for(manifest: &Path, prefixes: &[String]) -> bool {
    // A relative path (`manifest.parquet`) is taken from where it is run.
    let manifest = std::path::absolute(manifest).unwrap_or_else(|_| manifest.to_path_buf());
    let Some(dir) = manifest
        .parent()
        .and_then(Path::file_name)
        .and_then(|d| d.to_str())
    else {
        return false;
    };
    prefixes
        .iter()
        .map(|p| p.trim())
        .any(|p| !p.is_empty() && dir.starts_with(p))
}

/// Which sets a run fetches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sets {
    Keep,
    Buffer,
    Both,
}

impl Sets {
    fn has(self, s: Set) -> bool {
        matches!(
            (self, s),
            (Self::Both, _) | (Self::Keep, Set::Keep) | (Self::Buffer, Set::Buffer)
        )
    }
}

/// The order pages are fetched in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Order {
    /// The manifest's (issue) order.
    Issue,
    /// By [`priority`] group, then issue order within each.
    Priority,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            manifest: PathBuf::new(),
            out: PathBuf::new(),
            size: ImageSize::Full,
            buffer_size: ImageSize::Full,
            sets: Sets::Both,
            buffer_cap: 1_000_000_000_000,
            limit: None,
            sample: None,
            concurrency: 4,
            progress_every: 100,
            max_failures_in_a_row: 20,
            stop_min_titles: 3,
            item_cooloff: Duration::from_secs(24 * 3600),
            item_max_cooloffs: 3,
            order: Order::Priority,
            max_total_bytes: None,
            lock_file: None,
            mirrors: None,
            awardees: Vec::new(),
            no_loc_prefixes: Vec::new(),
        }
    }
}

/// `status-<tag>.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Status {
    /// `running`, `waiting` (the buffer is at its cap), `cooling` (every
    /// page left is in an issue cooling off after an item error), `done`,
    /// `budget` (`--max-total-bytes` reached), `stopped` (asked to) or
    /// `failed`.
    pub state: String,
    pub version: String,
    pub pid: u32,
    pub started_at: String,
    pub updated_at: String,
    pub manifest: String,
    pub manifest_sha256: String,
    pub resolution: String,
    pub out: String,
    /// Pages this run works through (after `--sample` and `--limit`).
    pub pages: u64,
    /// Pages finished: already there, fetched or failed.
    pub finished: u64,
    pub already_there: u64,
    pub fetched: u64,
    pub failed_permanent: u64,
    pub failed_transient: u64,
    /// Pages skipped because their issue's item was an item error (#414),
    /// each time one is skipped.
    #[serde(default)]
    pub failed_item_error: u64,
    /// Issues cooling off after an item error, and issues taken as
    /// permanent after the last cool-off.
    #[serde(default)]
    pub issues_cooling: u64,
    #[serde(default)]
    pub issues_item_permanent: u64,
    /// Of `fetched`: JP2s through the batch route, and pages that route
    /// couldn't place, fetched through the item API.
    #[serde(default)]
    pub fetched_jp2: u64,
    #[serde(default)]
    pub fetched_item_api: u64,
    /// Of `fetched_jp2`: JP2s whose bytes came from a mirror of LoC's
    /// files, matching the bag's md5.
    #[serde(default)]
    pub fetched_mirror: u64,
    /// Each mirror's counts and hold, by name.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mirrors: BTreeMap<String, mirror::MirrorStatus>,
    /// Of `fetched_jp2`: JP2s whose bytes came from LoC's bucket, matching
    /// the bag's md5.
    #[serde(default)]
    pub fetched_bucket: u64,
    /// LoC's bucket's counts and hold, when it is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket: Option<BucketStatus>,
    /// Whether this run's manifest takes JP2s only from LoC's bucket and
    /// the mirrors ([`Options::no_loc_prefixes`]).
    #[serde(default)]
    pub no_loc: bool,
    /// Pages neither LoC's bucket nor a mirror had, left for a run that
    /// may ask LoC. Counted in `finished`, not as failures.
    #[serde(default)]
    pub deferred_needs_loc: u64,
    pub bytes_fetched: u64,
    /// This run's pages on disk: already there plus fetched.
    pub bytes_on_disk: u64,
    pub max_total_bytes: Option<u64>,
    /// The buffer set's scans on disk, as last counted, and its cap.
    pub buffer_bytes: u64,
    pub buffer_cap: u64,
    pub buffer_resolution: String,
    /// Pages of each set in this run.
    pub keep_pages: u64,
    pub buffer_pages: u64,
    pub elapsed_secs: u64,
    pub fetched_per_hour: f64,
    pub eta_secs: Option<u64>,
    /// Seconds loc.gov still holds our requests for, if it asked us to wait.
    pub loc_hold_secs: Option<u64>,
    pub last_doc_id: String,
    pub error: Option<String>,
}

pub use usnm_core::digest::sha256_hex as sha256;

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A failure line.
#[derive(Debug, Serialize, Deserialize)]
pub struct Failure {
    pub doc_id: String,
    /// `permanent` (LoC said no, or lists no image, or an item error
    /// outlasted its cool-offs), `transient`, or `loc_item_error` (skipped
    /// while its issue cools off after an item error, #414).
    pub kind: String,
    pub error: String,
    pub at: String,
}

#[derive(Default)]
struct Counts {
    finished: AtomicU64,
    already_there: AtomicU64,
    fetched: AtomicU64,
    failed_permanent: AtomicU64,
    failed_transient: AtomicU64,
    bytes_fetched: AtomicU64,
    fetched_jp2: AtomicU64,
    fetched_item_api: AtomicU64,
    fetched_mirror: AtomicU64,
    fetched_bucket: AtomicU64,
    /// The keep set's scans of this run on disk: found there or fetched.
    keep_bytes: AtomicU64,
    /// The buffer set's scans on disk, all of them: the last count plus
    /// fetches since. Workers delete buffer scans, so it is counted again
    /// while waiting. It is what the cap is checked against.
    buffer_bytes: AtomicU64,
    /// This run's buffer pages found on disk or fetched (for the status and
    /// `--max-total-bytes`; consumed scans aren't taken off).
    run_buffer_bytes: AtomicU64,
    failed_item_error: AtomicU64,
    deferred_needs_loc: AtomicU64,
}

/// The failures since the last page that was done, and their titles.
#[derive(Default)]
struct Streak {
    pages: u32,
    titles: std::collections::BTreeSet<String>,
}

/// How a page ended.
#[derive(Debug, PartialEq)]
enum Outcome {
    /// Fetched, there already, or failed.
    Done,
    /// Left for a later run (stopped while waiting for the buffer).
    Left,
    /// Its issue is cooling off: asked again after this.
    Deferred(DateTime<Utc>),
}

struct Run {
    opts: Options,
    loc: Arc<LocClient>,
    counts: Counts,
    status: Mutex<Status>,
    status_path: PathBuf,
    status_write: Mutex<()>,
    /// The current state (`running` or `waiting`), which every status write
    /// reports until the run ends.
    state: Mutex<String>,
    /// Index and failure lines by resolution tag, so each file holds one
    /// resolution.
    index: BTreeMap<String, Lines>,
    failures: BTreeMap<String, Lines>,
    /// Where mirrored pages' bytes came from, by resolution tag (only
    /// with mirrors or LoC's bucket).
    sources: BTreeMap<String, Lines>,
    stop_rx: watch::Receiver<bool>,
    started: Instant,
    /// Set when the run must stop taking pages: (state, error).
    stop: Mutex<Option<(String, Option<String>)>>,
    streak: Mutex<Streak>,
    cooling: Cooling,
    /// Pages whose issue is cooling off, and when it can be asked again.
    deferred: Mutex<Vec<(DateTime<Utc>, ManifestRow)>>,
    /// JP2s only from LoC's bucket or a mirror ([`Options::no_loc_prefixes`]).
    no_loc: bool,
}

impl Run {
    fn snapshot(&self, state: &str) -> Status {
        let c = &self.counts;
        let mut s = self.status.lock().expect("status lock").clone();
        s.state = state.to_owned();
        s.updated_at = now_rfc3339();
        s.finished = c.finished.load(Ordering::SeqCst);
        s.already_there = c.already_there.load(Ordering::SeqCst);
        s.fetched = c.fetched.load(Ordering::SeqCst);
        s.failed_permanent = c.failed_permanent.load(Ordering::SeqCst);
        s.failed_transient = c.failed_transient.load(Ordering::SeqCst);
        s.failed_item_error = c.failed_item_error.load(Ordering::SeqCst);
        s.deferred_needs_loc = c.deferred_needs_loc.load(Ordering::SeqCst);
        (s.issues_cooling, s.issues_item_permanent) = self.cooling.counts();
        s.bytes_fetched = c.bytes_fetched.load(Ordering::SeqCst);
        s.fetched_jp2 = c.fetched_jp2.load(Ordering::SeqCst);
        s.fetched_item_api = c.fetched_item_api.load(Ordering::SeqCst);
        s.fetched_mirror = c.fetched_mirror.load(Ordering::SeqCst);
        if let Some(m) = &self.opts.mirrors {
            s.mirrors = m.status();
        }
        s.fetched_bucket = c.fetched_bucket.load(Ordering::SeqCst);
        s.bucket = self.loc.bucket_status();
        s.buffer_bytes = c.buffer_bytes.load(Ordering::SeqCst);
        s.bytes_on_disk = self.on_disk();
        let secs = self.started.elapsed().as_secs_f64();
        s.elapsed_secs = secs as u64;
        let worked = s.finished.saturating_sub(s.already_there);
        s.fetched_per_hour = if secs >= 1.0 {
            (s.fetched as f64 * 3600.0 / secs * 10.0).round() / 10.0
        } else {
            0.0
        };
        s.eta_secs = (worked > 0 && secs >= 1.0)
            .then(|| (s.pages.saturating_sub(s.finished) as f64 * secs / worked as f64) as u64);
        s.loc_hold_secs = self
            .loc
            .held_until()
            .map(|t| t.saturating_duration_since(Instant::now()).as_secs());
        s
    }

    /// This run's scans: its keep and buffer pages found on disk or fetched.
    fn on_disk(&self) -> u64 {
        let c = &self.counts;
        c.keep_bytes.load(Ordering::SeqCst) + c.run_buffer_bytes.load(Ordering::SeqCst)
    }

    fn set_state(&self, state: &str) {
        *self.state.lock().expect("state lock") = state.to_owned();
    }

    /// Write the status in the current state.
    fn write_status(&self) {
        let state = self.state.lock().expect("state lock").clone();
        self.write_status_as(&state);
    }

    fn write_status_as(&self, state: &str) {
        // One write at a time: the ticker and the page tasks share the
        // `.tmp` file.
        let _one = self.status_write.lock().expect("status write lock");
        let s = self.snapshot(state);
        let write = || -> anyhow::Result<()> {
            let mut tmp = self.status_path.as_os_str().to_owned();
            tmp.push(".tmp");
            let tmp = PathBuf::from(tmp);
            std::fs::write(&tmp, serde_json::to_vec_pretty(&s)?)?;
            std::fs::rename(&tmp, &self.status_path)?;
            Ok(())
        };
        if let Err(e) = write() {
            tracing::warn!(error = %e, "can't write the status file");
        }
        tracing::info!(
            state,
            finished = s.finished,
            pages = s.pages,
            already_there = s.already_there,
            fetched = s.fetched,
            fetched_jp2 = s.fetched_jp2,
            fetched_item_api = s.fetched_item_api,
            failed_permanent = s.failed_permanent,
            failed_transient = s.failed_transient,
            failed_item_error = s.failed_item_error,
            deferred_needs_loc = s.deferred_needs_loc,
            issues_cooling = s.issues_cooling,
            bytes_fetched = s.bytes_fetched,
            bytes_on_disk = s.bytes_on_disk,
            buffer_bytes = s.buffer_bytes,
            fetched_per_hour = s.fetched_per_hour,
            eta_secs = s.eta_secs,
            loc_hold_secs = s.loc_hold_secs,
            "prefetch progress"
        );
    }

    fn stop_with(&self, state: &str, error: Option<String>) {
        let mut stop = self.stop.lock().expect("stop lock");
        if stop.is_none() {
            *stop = Some((state.to_owned(), error));
        }
    }

    fn stopping(&self) -> bool {
        self.stop.lock().expect("stop lock").is_some()
    }

    /// A page is done: the run of failures is over.
    fn streak_ok(&self) {
        *self.streak.lock().expect("streak lock") = Streak::default();
    }

    /// A page failed: stop once the run of failures is long enough and
    /// spans enough titles.
    fn streak_fail(&self, lccn: &str, msg: &str) {
        let mut s = self.streak.lock().expect("streak lock");
        s.pages += 1;
        s.titles.insert(lccn.to_owned());
        if s.pages >= self.opts.max_failures_in_a_row
            && s.titles.len() >= self.opts.stop_min_titles.max(1)
        {
            self.stop_with(
                "failed",
                Some(format!(
                    "{} pages from {} titles failed in a row (the last: {msg}); stopping rather than pressing on",
                    s.pages,
                    s.titles.len()
                )),
            );
        }
    }

    /// A page skipped for its issue's item error: a `loc_item_error` line
    /// and a later try, or a `permanent` line after the last cool-off.
    fn skip(&self, row: ManifestRow, t: &str, check: Check, why: &str) -> anyhow::Result<Outcome> {
        let c = &self.counts;
        let (kind, error, outcome) = match check {
            Check::Cooling(until) => {
                c.failed_item_error.fetch_add(1, Ordering::SeqCst);
                (
                    "loc_item_error",
                    format!(
                        "{why}; asked again after {}",
                        until.to_rfc3339_opts(SecondsFormat::Secs, true)
                    ),
                    Outcome::Deferred(until),
                )
            }
            Check::Permanent | Check::Go => {
                c.failed_permanent.fetch_add(1, Ordering::SeqCst);
                (
                    "permanent",
                    format!(
                        "{why}; loc.gov still answered 503 for the issue's item after {} cool-offs",
                        self.opts.item_max_cooloffs
                    ),
                    Outcome::Done,
                )
            }
        };
        self.failures[t].push(&Failure {
            doc_id: row.doc_id.clone(),
            kind: kind.into(),
            error: error.chars().take(300).collect(),
            at: now_rfc3339(),
        })?;
        if let Outcome::Deferred(until) = outcome {
            self.deferred
                .lock()
                .expect("deferred lock")
                .push((until, row));
        }
        Ok(outcome)
    }

    /// A deferred page whose issue can be asked again, if any.
    fn take_due(&self) -> Option<ManifestRow> {
        let now = Utc::now();
        let mut d = self.deferred.lock().expect("deferred lock");
        let i = d.iter().position(|(due, _)| *due <= now)?;
        Some(d.remove(i).1)
    }

    /// When the first deferred page can be asked again.
    fn next_due(&self) -> Option<DateTime<Utc>> {
        let d = self.deferred.lock().expect("deferred lock");
        d.iter().map(|(due, _)| *due).min()
    }

    /// Count the buffer again. If it can't be counted, it is taken as full,
    /// so the fetcher waits rather than overfill the pool.
    async fn count_buffer(&self) {
        let dir = self.opts.out.join(Set::Buffer.dir());
        let n = match tokio::task::spawn_blocking(move || dir_scan_bytes(&dir)).await {
            Ok(Ok(n)) => n,
            Ok(Err(e)) => {
                tracing::warn!(error = %e, "can't count the buffer; treating it as full");
                u64::MAX
            }
            Err(e) => {
                tracing::warn!(error = %e, "can't count the buffer; treating it as full");
                u64::MAX
            }
        };
        self.counts.buffer_bytes.store(n, Ordering::SeqCst);
    }

    /// Wait while the buffer is at its cap, counting it again every
    /// minute. False if the run was stopped meanwhile.
    async fn wait_for_buffer(&self) -> bool {
        let mut stop = self.stop_rx.clone();
        while self.counts.buffer_bytes.load(Ordering::SeqCst) >= self.opts.buffer_cap {
            self.set_state("waiting");
            self.write_status();
            tokio::select! {
                _ = tokio::time::sleep(BUFFER_RECHECK) => {}
                _ = stop.wait_for(|s| *s) => {
                    self.stop_with("stopped", None);
                }
            }
            if self.stopping() {
                return false;
            }
            self.count_buffer().await;
        }
        self.set_state("running");
        true
    }

    /// The page's scan: through its batch, or through the item API when
    /// the batch route can't place it. An issue cooling off on the route
    /// the page would take is skipped without asking LoC.
    async fn fetch(&self, row: &ManifestRow, key: &PageKey, size: ImageSize) -> Fetched {
        let issue = issue_id(key);
        let storage_issue = format!("storage:{issue}");
        if let c @ (Check::Cooling(_) | Check::Permanent) = self.cooling.check(&storage_issue) {
            return Fetched::Skip(
                c,
                "its issue answered 503 on LoC's storage service while it served others",
            );
        }
        let resolved = match self.loc.resolve(key, &row.batch, row.batch_version).await {
            Ok(Some(p)) => {
                // LoC's bag and METS placed the page; a copy of the JP2 that
                // is the bag's, from LoC's bucket and then a mirror, saves
                // tile.loc.gov the image.
                if size == ImageSize::Full {
                    if let Some((scan, hit)) = self.loc.bucket_jp2(&p).await {
                        return Fetched::Scan(scan, storage_issue, Via::Mirror(hit));
                    }
                    if let Some(m) = &self.opts.mirrors {
                        if let Some((scan, hit)) = m.jp2(&row.batch, key, &p).await {
                            return Fetched::Scan(scan, storage_issue, Via::Mirror(hit));
                        }
                    }
                }
                if self.no_loc {
                    return Fetched::NeedsLoc("neither LoC's bucket nor a mirror has its JP2");
                }
                self.loc.scan_resolved(key, &p, size).await
            }
            Ok(None) if self.no_loc => {
                return Fetched::NeedsLoc("its batch can't place it, and the item API is LoC's");
            }
            Ok(None) => Ok(None),
            Err(e) => Err(e),
        };
        match resolved {
            Ok(Some(scan)) => return Fetched::Scan(scan, storage_issue, Via::Storage),
            Ok(None) => {}
            Err(e) => return Fetched::Failed(e, storage_issue),
        }
        if let c @ (Check::Cooling(_) | Check::Permanent) = self.cooling.check(&issue) {
            return Fetched::Skip(
                c,
                "its issue's item answered 503 while loc.gov served others",
            );
        }
        match self.loc.scan_at(key, size).await {
            Ok(scan) => Fetched::Scan(scan, issue, Via::ItemApi),
            Err(e) => Fetched::Failed(e, issue),
        }
    }

    /// One page.
    async fn page(&self, row: ManifestRow) -> anyhow::Result<Outcome> {
        let set = set_of(&row);
        let size = match set {
            Set::Keep => self.opts.size,
            Set::Buffer => self.opts.buffer_size,
        };
        let c = &self.counts;
        if let Some(n) = there(&self.opts.out, &row, size).await {
            c.already_there.fetch_add(1, Ordering::SeqCst);
            match set {
                Set::Keep => c.keep_bytes.fetch_add(n, Ordering::SeqCst),
                Set::Buffer => c.run_buffer_bytes.fetch_add(n, Ordering::SeqCst),
            };
            // A page that is done breaks a run of failures too.
            self.streak_ok();
            return Ok(Outcome::Done);
        }
        // A buffer page a worker has read and the scan server deleted.
        if set == Set::Buffer {
            for f in formats(size) {
                let done = consumed_path(&scan_path(&self.opts.out, &row, size, *f));
                if tokio::fs::try_exists(done).await.unwrap_or(false) {
                    c.already_there.fetch_add(1, Ordering::SeqCst);
                    self.streak_ok();
                    return Ok(Outcome::Done);
                }
            }
        }
        let t = tag(size);
        let key = row.key()?;
        // A scaled page under no-LoC is deferred before it waits for room
        // in the buffer: nothing but LoC serves it.
        if self.no_loc && size != ImageSize::Full {
            c.deferred_needs_loc.fetch_add(1, Ordering::SeqCst);
            tracing::info!(
                doc_id = row.doc_id,
                why = "only a full-size JP2 comes from LoC's bucket or a mirror",
                "deferred: needs LoC"
            );
            return Ok(Outcome::Done);
        }
        // Only a page that needs fetching waits for room in the buffer.
        if set == Set::Buffer && !self.wait_for_buffer().await {
            return Ok(Outcome::Left);
        }
        match self.fetch(&row, &key, size).await {
            // An issue cooling off says nothing new about loc.gov, so the
            // run of failures is left as is.
            Fetched::Skip(check, why) => self.skip(row, &t, check, why),
            // Not a failure and nothing about LoC: no failure line, no
            // cool-off, and the run of failures is left as is. With no scan
            // on disk, a later run takes it again.
            Fetched::NeedsLoc(why) => {
                c.deferred_needs_loc.fetch_add(1, Ordering::SeqCst);
                tracing::info!(doc_id = row.doc_id, why, "deferred: needs LoC");
                Ok(Outcome::Done)
            }
            Fetched::Scan(scan, route, via) => {
                let path = scan_path(&self.opts.out, &row, size, scan.format);
                let side = Sidecar {
                    doc_id: row.doc_id.clone(),
                    batch: row.batch.clone(),
                    batch_version: row.batch_version,
                    loc_url: scan.url.clone(),
                    iiif_base: scan.iiif_base.clone(),
                    resolution: size.to_string(),
                    fetched_at: Utc::now(),
                    bytes: scan.bytes.len() as u64,
                    sha256: sha256(&scan.bytes),
                    download_ms: u64::try_from(scan.download.as_millis()).unwrap_or(u64::MAX),
                    user_agent: USER_AGENT.to_owned(),
                    format: scan.format.name().into(),
                    md5: scan.md5.clone(),
                };
                write_atomic(&path, scan.bytes.to_vec()).await?;
                write_atomic(&sidecar_path(&path), serde_json::to_vec_pretty(&side)?).await?;
                self.index[&t].push(&side)?;
                c.fetched.fetch_add(1, Ordering::SeqCst);
                c.bytes_fetched.fetch_add(side.bytes, Ordering::SeqCst);
                if scan.format == ScanFormat::Jp2 {
                    c.fetched_jp2.fetch_add(1, Ordering::SeqCst);
                }
                match &via {
                    Via::Storage => {}
                    Via::ItemApi => {
                        c.fetched_item_api.fetch_add(1, Ordering::SeqCst);
                    }
                    Via::Mirror(hit) => {
                        if hit.source == bucket::NAME {
                            &c.fetched_bucket
                        } else {
                            &c.fetched_mirror
                        }
                        .fetch_add(1, Ordering::SeqCst);
                        if let Some(lines) = self.sources.get(&t) {
                            lines.push(&SourceLine {
                                doc_id: side.doc_id.clone(),
                                source: hit.source.into(),
                                url: hit.url.clone(),
                                md5: side.md5.clone().unwrap_or_default(),
                                bytes: side.bytes,
                                download_ms: side.download_ms,
                                at: now_rfc3339(),
                            })?;
                        }
                    }
                }
                match set {
                    Set::Keep => c.keep_bytes.fetch_add(side.bytes, Ordering::SeqCst),
                    Set::Buffer => {
                        c.run_buffer_bytes.fetch_add(side.bytes, Ordering::SeqCst);
                        c.buffer_bytes.fetch_add(side.bytes, Ordering::SeqCst)
                    }
                };
                // A page from LoC's bucket or a mirror says nothing about
                // tile.loc.gov: the run of failures and the route's cool-off
                // are tile.loc.gov's to end.
                if !matches!(via, Via::Mirror(_)) {
                    self.streak_ok();
                    self.cooling.clear(&route)?;
                }
                Ok(Outcome::Done)
            }
            Fetched::Failed(FetchError::Item(msg), route) => {
                tracing::warn!(
                    doc_id = row.doc_id,
                    error = msg,
                    route,
                    "loc.gov item error"
                );
                let check = self.cooling.mark(&route, &msg)?;
                let lccn = row.lccn.clone();
                let outcome = self.skip(row, &t, check, &msg)?;
                self.streak_fail(&lccn, &msg);
                Ok(outcome)
            }
            Fetched::Failed(e, _) => {
                let (kind, msg) = match &e {
                    FetchError::Permanent(m) => ("permanent", m.clone()),
                    FetchError::Transient(m) => ("transient", m.clone()),
                    FetchError::Item(_) => unreachable!("handled above"),
                };
                tracing::warn!(doc_id = row.doc_id, kind, error = msg, "prefetch failed");
                self.failures[&t].push(&Failure {
                    doc_id: row.doc_id.clone(),
                    kind: kind.into(),
                    error: msg.chars().take(300).collect(),
                    at: now_rfc3339(),
                })?;
                if kind == "permanent" {
                    c.failed_permanent.fetch_add(1, Ordering::SeqCst);
                } else {
                    c.failed_transient.fetch_add(1, Ordering::SeqCst);
                }
                self.streak_fail(&row.lccn, &msg);
                Ok(Outcome::Done)
            }
        }
    }
}

/// What [`Run::fetch`] got for a page.
enum Fetched {
    /// The scan, the cooling key of the route it came by, and how it came.
    Scan(Scan, String, Via),
    /// Failed on the route with this cooling key.
    Failed(FetchError, String),
    /// Its issue is cooling off on the route the page would take.
    Skip(Check, &'static str),
    /// Only LoC has it, and this run doesn't ask LoC for JP2s.
    NeedsLoc(&'static str),
}

/// How a scan was fetched.
enum Via {
    /// The JP2 from its batch on LoC's storage service.
    Storage,
    /// The IIIF JPEG, through the item API.
    ItemApi,
    /// The JP2 from LoC's bucket or a mirror, matching the bag's md5.
    Mirror(mirror::Hit),
}

/// A line of `sources-<tag>.jsonl`: where the bytes of a page from LoC's
/// bucket or a mirror came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceLine {
    pub doc_id: String,
    /// [`bucket::NAME`], or the mirror's name ([`mirror::Partner::name`] or
    /// [`mirror::WAYBACK`]).
    pub source: String,
    /// The URL the bytes were read from.
    pub url: String,
    /// The bag's md5, which they matched.
    pub md5: String,
    pub bytes: u64,
    pub download_ms: u64,
    pub at: String,
}

/// Fetch the manifest's scans until done, stopped by `stop`, or too many
/// pages fail in a row. Returns the final status.
pub async fn run(
    opts: Options,
    loc: Arc<LocClient>,
    stop: watch::Receiver<bool>,
) -> anyhow::Result<Status> {
    let mut stop_main = stop.clone();
    std::fs::create_dir_all(&opts.out)
        .with_context(|| format!("creating {}", opts.out.display()))?;
    let lock_path = opts
        .lock_file
        .clone()
        .unwrap_or_else(|| opts.out.join(".prefetch.lock"));
    let _lock = lock(&lock_path)?;
    let bytes = tokio::fs::read(&opts.manifest)
        .await
        .with_context(|| format!("reading {}", opts.manifest.display()))?;
    let manifest_sha256 = sha256(&bytes);
    // Only the rows of the sets asked for are held (about 250 bytes a row).
    let mut manifest_pages = 0usize;
    let sets = opts.sets;
    let rows = prefetch::decode_where(Bytes::from(bytes), |r| {
        manifest_pages += 1;
        sets.has(set_of(r))
    })
    .context("reading the manifest")?;
    let rows = select(rows, &opts);
    // The status is named for the resolution of the set the run starts with.
    let status_tag = tag(if opts.sets == Sets::Buffer {
        opts.buffer_size
    } else {
        opts.size
    });
    let mut index = BTreeMap::new();
    let mut failures = BTreeMap::new();
    let mut sources = BTreeMap::new();
    for size in [opts.size, opts.buffer_size] {
        let t = tag(size);
        if !index.contains_key(&t) {
            index.insert(
                t.clone(),
                Lines::open(&opts.out.join(format!("index-{t}.jsonl")))?,
            );
            failures.insert(
                t.clone(),
                Lines::open(&opts.out.join(format!("failures-{t}.jsonl")))?,
            );
            // Only full-size pages are asked of LoC's bucket or a mirror.
            if (opts.mirrors.is_some() || loc.has_bucket()) && size == ImageSize::Full {
                sources.insert(
                    t.clone(),
                    Lines::open(&opts.out.join(format!("sources-{t}.jsonl")))?,
                );
            }
        }
    }
    let cooling = Cooling::load(
        opts.out.join("item-errors.json"),
        opts.item_cooloff,
        opts.item_max_cooloffs,
    )?;
    // The last page fetched, in the index of any set this run fetches,
    // names an item that answered, to check an item error against before
    // any item answers in this run.
    let mut tags = Vec::new();
    if opts.sets.has(Set::Keep) {
        tags.push(tag(opts.size));
    }
    if opts.sets.has(Set::Buffer) {
        tags.push(tag(opts.buffer_size));
    }
    tags.dedup();
    let last: Vec<Sidecar> = tags
        .iter()
        .flat_map(|t| last_fetched(&opts.out.join(format!("index-{t}.jsonl"))))
        .collect();
    // Only a full-size JPEG came through the item API (or from before the
    // batch route); a JP2 or a scaled JPEG proves nothing about its item.
    if let Some(key) = last
        .iter()
        .filter(|s| s.format == ScanFormat::Jpeg.name() && s.resolution == "full")
        .max_by_key(|s| s.fetched_at)
        .and_then(|s| PageKey::from_doc_id(&s.doc_id).ok())
    {
        loc.set_known_good(loc.item_url(&key));
    }
    // On the storage service, the METS beside the last JP2 fetched.
    if let Some(jp2) = last
        .iter()
        .filter(|s| s.format == ScanFormat::Jp2.name())
        .max_by_key(|s| s.fetched_at)
    {
        loc.seed_storage_known_good(&jp2.loc_url);
    }
    let no_loc = no_loc_for(&opts.manifest, &opts.no_loc_prefixes);
    let run = Arc::new(Run {
        loc,
        counts: Counts::default(),
        status: Mutex::new(Status {
            version: env!("CARGO_PKG_VERSION").into(),
            pid: std::process::id(),
            started_at: now_rfc3339(),
            manifest: opts.manifest.display().to_string(),
            manifest_sha256,
            resolution: opts.size.to_string(),
            out: opts.out.display().to_string(),
            pages: rows.len() as u64,
            max_total_bytes: opts.max_total_bytes,
            buffer_cap: opts.buffer_cap,
            buffer_resolution: opts.buffer_size.to_string(),
            keep_pages: rows.iter().filter(|r| set_of(r) == Set::Keep).count() as u64,
            buffer_pages: rows.iter().filter(|r| set_of(r) == Set::Buffer).count() as u64,
            no_loc,
            ..Status::default()
        }),
        status_path: opts.out.join(format!("status-{status_tag}.json")),
        status_write: Mutex::new(()),
        state: Mutex::new("running".into()),
        index,
        failures,
        sources,
        stop_rx: stop,
        started: Instant::now(),
        stop: Mutex::new(None),
        streak: Mutex::new(Streak::default()),
        cooling,
        deferred: Mutex::new(Vec::new()),
        no_loc,
        opts,
    });
    tracing::info!(
        manifest_pages,
        pages = rows.len(),
        resolution = %run.opts.size,
        buffer_resolution = %run.opts.buffer_size,
        buffer_cap = run.opts.buffer_cap,
        concurrency = run.opts.concurrency,
        out = %run.opts.out.display(),
        no_loc = run.no_loc,
        "prefetch starting"
    );
    run.count_buffer().await;
    run.write_status();
    // A status write every 30 s, so a long hold still shows.
    let ticker = {
        let run = Arc::clone(&run);
        tokio::spawn(async move {
            let mut every = tokio::time::interval(Duration::from_secs(30));
            every.tick().await;
            loop {
                every.tick().await;
                run.write_status();
            }
        })
    };
    let slots = Arc::new(Semaphore::new(run.opts.concurrency.max(1)));
    let mut tasks = tokio::task::JoinSet::new();
    let every = run.opts.progress_every.max(1);
    let mut rows = rows.into_iter();
    loop {
        if run.stopping() {
            break;
        }
        // Deferred pages whose cool-off is over go first, then the walk.
        // Once the walk is done, wait for the pages in flight, then for
        // the deferred ones' cool-offs.
        let row = match run.take_due().or_else(|| rows.next()) {
            Some(r) => r,
            None if !tasks.is_empty() => {
                if let Some(r) = tasks.join_next().await {
                    r?;
                }
                continue;
            }
            None => {
                let Some(due) = run.next_due() else { break };
                run.set_state("cooling");
                run.write_status();
                let wait = (due - Utc::now())
                    .to_std()
                    .unwrap_or_default()
                    .min(BUFFER_RECHECK);
                tokio::select! {
                    _ = tokio::time::sleep(wait) => {}
                    _ = stop_main.wait_for(|s| *s) => {
                        run.stop_with("stopped", None);
                        break;
                    }
                }
                run.set_state("running");
                continue;
            }
        };
        let permit = tokio::select! {
            p = Arc::clone(&slots).acquire_owned() => p.expect("semaphore open"),
            _ = stop_main.wait_for(|s| *s) => {
                run.stop_with("stopped", None);
                break;
            }
        };
        if run.stopping() {
            break;
        }
        if let Some(max) = run.opts.max_total_bytes {
            if run.on_disk() >= max {
                run.stop_with("budget", None);
                break;
            }
        }
        let run = Arc::clone(&run);
        tasks.spawn(async move {
            let doc_id = row.doc_id.clone();
            match run.page(row).await {
                Ok(Outcome::Done) => {}
                Ok(Outcome::Left | Outcome::Deferred(_)) => return,
                Err(e) => run.stop_with("failed", Some(format!("{doc_id}: {e:#}"))),
            }
            let n = run.counts.finished.fetch_add(1, Ordering::SeqCst) + 1;
            run.status.lock().expect("status lock").last_doc_id = doc_id;
            if n.is_multiple_of(every) {
                run.write_status();
            }
            drop(permit);
        });
        // Collect what's finished as we go, so the set stays small.
        while let Some(r) = tasks.try_join_next() {
            r?;
        }
    }
    while let Some(r) = tasks.join_next().await {
        r?;
    }
    // Stopped before the last write, so it can't put `running` back.
    ticker.abort();
    let _ = ticker.await;
    let (state, error) = run
        .stop
        .lock()
        .expect("stop lock")
        .clone()
        .unwrap_or_else(|| ("done".into(), None));
    run.status.lock().expect("status lock").error = error.clone();
    run.set_state(&state);
    run.write_status();
    let status = run.snapshot(&state);
    if state == "failed" {
        bail!(error.unwrap_or_else(|| "prefetch failed".into()));
    }
    Ok(status)
}

#[cfg(test)]
mod tests;
