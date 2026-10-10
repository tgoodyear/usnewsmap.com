//! The `batch` stage (04 §4.4): claim a queued batch, curate it, commit.
//!
//! - **Claim:** a conditional replace of the batch item sets a lease and
//!   `status = downloading`. Losing the race just moves on to the next batch.
//!   An expired lease (a crashed or evicted worker) makes a batch claimable.
//! - **Curate:** the archive is streamed and parsed as it downloads, each
//!   page normalized into Parquet parts under a new attempt path. Its sha256
//!   is checked against the published one before anything is committed.
//!   Nothing is ever overwritten, and nothing touches local disk.
//! - **Commit:** issue items are upserted first (idempotent). The last write
//!   is one conditional replace that points the batch at this attempt, and it
//!   succeeds only while this worker still holds the lease. A crash anywhere
//!   before it leaves the batch retryable.
//! - **Watch:** each batch logs its stages, a heartbeat line every minute
//!   while it is held ([`crate::heartbeat`]), and one outcome line. A batch
//!   still unfinished [`BATCH_LIMIT`] after its download slot is abandoned
//!   like a failed one, and the worker moves on.
//! - **Stop:** a worker with a [`Worker::deadline`] claims nothing once it
//!   has passed, and a batch claimed but not yet downloading then (waiting
//!   for its download slot) is released without costing an attempt. A batch past its slot is finished
//!   (or abandoned by the watchdog), so a worker stops at most
//!   [`BATCH_LIMIT`] after the deadline, and exits cleanly well before the
//!   job's replica timeout would kill it.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{bail, Context};
use chrono::{Duration, NaiveDate, Utc};
use futures::stream::{self, StreamExt, TryStreamExt};
use opentelemetry::KeyValue;
use tracing::field::Empty;
use tracing::Instrument;
use usnm_core::text::TextStatus;
use usnm_core::time::day_number;
use usnm_store::ObjectStore;

use crate::archive;
use crate::curated::{CuratedRow, PartWriter};
use crate::heartbeat::{self, mb, BatchProgress, Heartbeat, Stage};
use crate::raw;
use crate::source;
use crate::state::{Batch, BatchStatus, Curated, Issue, Lease, State};
use crate::telemetry;

/// Attempts before a batch is marked failed (an operator re-queues it).
pub const MAX_ATTEMPTS: u32 = 5;

/// Seconds between bulk downloads across every worker. LoC allows 10 bulk
/// requests per 10 minutes per IP (04 §4.1); 75 s leaves room for the odd
/// retry after a server error.
pub const FETCH_INTERVAL_SECS: u32 = 75;

/// How long every worker holds its downloads after LoC answers 429.
const THROTTLE_BLOCK: Duration = Duration::hours(1);

/// How long a batch may take from its download slot to its commit before
/// the worker abandons it. In the September 2026 backfill (1,213 batches,
/// 4 workers), the time from one outcome line to the next on a worker (a
/// whole batch, claim and wait for a slot included) was 275 s at the median,
/// 745 s at p99 and 1,082 s at most; the largest archive (3.8 GB) took 974 s.
/// 45 minutes is 3.6 times that p99. The lease is renewed when the slot is
/// granted, so the limit ends well inside it however long the wait was, and
/// an abandoned batch is released before another worker could claim it.
pub const BATCH_LIMIT: std::time::Duration = std::time::Duration::from_secs(45 * 60);

/// A batch that ran past [`Worker::batch_limit`].
#[derive(Debug)]
pub struct TimedOut {
    pub stage: Stage,
    pub limit: std::time::Duration,
}

impl std::fmt::Display for TimedOut {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "curation passed the {} s limit in stage {}; the attempt is abandoned",
            self.limit.as_secs(),
            self.stage
        )
    }
}

impl std::error::Error for TimedOut {}

/// How one batch ended: the `outcome` of its span and metrics.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Ok,
    Failed,
    Throttled,
    TimedOut,
    /// The deadline passed while the batch waited for a download slot.
    Stopped,
}

impl Outcome {
    fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Throttled => "throttled",
            Outcome::TimedOut => "timed_out",
            Outcome::Stopped => "stopped",
        }
    }
}

pub struct Worker {
    pub state: State,
    pub curated: Arc<dyn ObjectStore>,
    /// Unique per process: lease owner and part of the attempt path.
    pub owner: String,
    pub lease: Duration,
    /// Spacing between downloads from remote sources, shared by every worker
    /// through the state store; `None` doesn't pace (local archives, tests).
    pub fetch_interval: Option<Duration>,
    /// The watchdog: how long a batch may take from its download slot to its
    /// commit ([`BATCH_LIMIT`]).
    pub batch_limit: std::time::Duration,
    /// When to stop claiming batches and waiting for download slots
    /// (`curate --max-runtime-secs`); `None` runs until the queue is empty.
    pub deadline: Option<tokio::time::Instant>,
    /// Where archives are retained (`USNM_RAW_URL`, [`crate::raw`]): each
    /// download is kept there, and a batch already there is read from it
    /// instead of LoC. `None` keeps nothing (production, ADR-0006).
    pub raw: Option<Arc<dyn ObjectStore>>,
}

/// Pages per (lccn → day → pages), stored beside the parts.
pub type Counts = BTreeMap<String, BTreeMap<u32, u32>>;

/// (pages, empty pages) per (lccn, date, edition).
type Issues = BTreeMap<(String, NaiveDate, u16), (u32, u32)>;

/// How long one curation took, and how much it read.
#[derive(Debug, Default, Clone, Copy)]
pub struct Timings {
    /// Archive bytes downloaded.
    pub archive_bytes: u64,
    /// Download, parse and part uploads (they overlap: the archive is streamed).
    pub stream_secs: f64,
    /// Counts and issue items.
    pub issues_secs: f64,
}

struct Written {
    parts: Vec<String>,
    counts: Counts,
    issues: Issues,
    pages: u64,
    ok_pages: u64,
    first: NaiveDate,
    last: NaiveDate,
}

impl Worker {
    /// Claim the next claimable batch, if any.
    pub async fn claim(&self) -> anyhow::Result<Option<(Batch, String)>> {
        self.claim_except(&HashSet::new()).await
    }

    async fn claim_except(
        &self,
        skip: &HashSet<String>,
    ) -> anyhow::Result<Option<(Batch, String)>> {
        let now = Utc::now();
        let candidates = self
            .state
            .batches(&[BatchStatus::Queued, BatchStatus::Downloading])
            .await?;
        for (mut b, etag) in candidates {
            if skip.contains(&b.batch) || b.lease.as_ref().is_some_and(|l| l.until > now) {
                continue;
            }
            // Workers that crash never reach release, so the attempt
            // cap is also enforced here.
            if b.attempts >= MAX_ATTEMPTS {
                b.status = BatchStatus::Failed;
                b.lease = None;
                b.last_error.get_or_insert_with(|| {
                    format!(
                        "abandoned after {} attempts (workers stopped mid-batch)",
                        b.attempts
                    )
                });
                b.updated_at = now;
                let _ = self.state.replace_batch(&b, &etag).await?;
                continue;
            }
            b.status = BatchStatus::Downloading;
            b.attempts += 1;
            b.lease = Some(Lease {
                owner: self.owner.clone(),
                until: now + self.lease,
            });
            b.updated_at = now;
            if let Some(etag) = self.state.replace_batch(&b, &etag).await? {
                return Ok(Some((b, etag)));
            }
        }
        Ok(None)
    }

    /// Claim and curate batches until none are left or the deadline has
    /// passed; returns how many were curated. A batch that fails is re-queued
    /// for a later run, not retried straight away (the failure is often the
    /// source, e.g. a throttled download).
    pub async fn run(&self, max_batches: Option<usize>) -> anyhow::Result<usize> {
        let mut done = 0;
        let mut failed = HashSet::new();
        while max_batches.is_none_or(|m| done < m) {
            if self.past_deadline() {
                self.log_stop(done).await?;
                break;
            }
            let Some((batch, _)) = self.claim_except(&failed).await? else {
                break;
            };
            match self.run_one(&batch).await? {
                Outcome::Ok => done += 1,
                Outcome::Throttled => {}
                Outcome::Stopped => {
                    self.log_stop(done).await?;
                    break;
                }
                Outcome::Failed | Outcome::TimedOut => {
                    failed.insert(batch.batch);
                }
            }
        }
        Ok(done)
    }

    fn past_deadline(&self) -> bool {
        self.deadline
            .is_some_and(|d| tokio::time::Instant::now() >= d)
    }

    /// The line that says why this worker stopped with work left, and how
    /// much is left for the next run.
    async fn log_stop(&self, curated: usize) -> anyhow::Result<()> {
        let queued = self.state.batches(&[BatchStatus::Queued]).await?.len();
        tracing::info!(
            curated,
            queued,
            "max runtime reached; claiming no more batches"
        );
        Ok(())
    }

    /// Curate one claimed batch: wait for a download slot, curate, commit,
    /// log the outcome, and release the batch unless it was committed.
    async fn run_one(&self, batch: &Batch) -> anyhow::Result<Outcome> {
        let name = format!("{}_ver{:02}", batch.batch, batch.version);
        // One span per batch (a request in Application Insights), from the
        // claim to the outcome line. `wait_secs` is the part spent waiting
        // for a download slot; the outcome line's `secs` leaves it out.
        let span = tracing::info_span!(
            "curate",
            otel.kind = "consumer",
            batch = %name,
            attempt = batch.attempts,
            wait_secs = Empty,
            pages = Empty,
            outcome = Empty,
            otel.status_code = Empty,
        );
        let progress = BatchProgress::new();
        // Stops when dropped, at the end of this function on every path.
        let _beat = {
            let (span, name) = (span.clone(), name.clone());
            Heartbeat::start(progress.clone(), heartbeat::INTERVAL, move |s| {
                span.in_scope(|| s.log(&name))
            })
        };
        let lease_until = batch.lease.as_ref().map(|l| l.until.to_rfc3339());
        span.in_scope(|| {
            tracing::info!(
                batch = %name,
                attempt = batch.attempts,
                owner = %self.owner,
                lease_until = lease_until.as_deref(),
                "claimed"
            )
        });
        // Past the deadline the batch goes back to the queue as it was,
        // attempt and all: when the claim itself ran past it, or while the
        // batch waits for a download slot.
        let mut wait_secs = 0.0;
        let mut granted = !self.past_deadline();
        // A retained copy is read from the raw store: no download slot.
        let retained = if granted {
            self.retained(batch, &name).await?
        } else {
            None
        };
        if granted {
            if let Some(interval) = self.fetch_interval {
                if source::local_path(&batch.source_url).is_none() && retained.is_none() {
                    let waiting = Instant::now();
                    let wait = self
                        .wait_for_fetch_slot(interval, &name)
                        .instrument(span.clone());
                    granted = match self.deadline {
                        Some(deadline) => tokio::select! {
                            biased;
                            () = tokio::time::sleep_until(deadline) => false,
                            r = wait => r.map(|()| true)?,
                        },
                        None => wait.await.map(|()| true)?,
                    };
                    wait_secs = round1(waiting.elapsed().as_secs_f64());
                    span.record("wait_secs", wait_secs);
                    if granted {
                        span.in_scope(
                            || tracing::info!(batch = %name, wait_secs, "download slot granted"),
                        );
                    }
                }
            }
        }
        if !granted {
            span.record("outcome", Outcome::Stopped.as_str());
            telemetry::metrics()
                .curate_batches
                .add(1, &[KeyValue::new("outcome", Outcome::Stopped.as_str())]);
            span.in_scope(|| {
                tracing::info!(
                    batch = %name,
                    wait_secs,
                    "max runtime reached before the download; batch released"
                )
            });
            self.release(
                &batch.batch,
                "released unstarted: the worker reached its max runtime before the download",
                false,
            )
            .await?;
            return Ok(Outcome::Stopped);
        }
        progress.set_stage(Stage::Connecting);
        let started = Instant::now();
        // A failed commit (e.g. a newer version was queued meanwhile) is
        // handled like a failed curation: clear the lease and re-queue.
        let work = async {
            // The wait for a slot can outlast the lease (a LoC block that
            // is extended, or many workers): renew it before downloading.
            if !self.renew_lease(batch).await? {
                bail!("lost the lease while waiting for a download slot; another worker has the batch");
            }
            let (c, t) = self
                .curate_timed(batch, &name, &progress, retained.clone())
                .await?;
            let (pages, parts) = (c.pages, c.parts.len());
            progress.set_stage(Stage::Committing);
            let commit_started = Instant::now();
            self.commit(&batch.batch, c).await?;
            Ok::<_, anyhow::Error>((pages, parts, t, commit_started.elapsed()))
        }
        .instrument(span.clone());
        // The watchdog. Dropping the attempt at any await is safe: its parts
        // are under a path of its own that nothing reads before the commit,
        // issue items are idempotent upserts, and the commit is a single
        // conditional write (ETag and lease) that either landed or didn't. If
        // it landed, the release below finds the lease gone and leaves the
        // batch alone; if not, the batch is re-queued like any failure.
        // Dropping the download stops it and the parser reading it.
        let result = match tokio::time::timeout(self.batch_limit, work).await {
            Ok(r) => r,
            Err(_) => Err(TimedOut {
                stage: progress.stage(),
                limit: self.batch_limit,
            }
            .into()),
        };
        let secs = started.elapsed().as_secs_f64();
        let m = telemetry::metrics();
        let outcome = match &result {
            Ok(_) => Outcome::Ok,
            Err(e) if e.downcast_ref::<source::Throttled>().is_some() => Outcome::Throttled,
            Err(e) if e.downcast_ref::<TimedOut>().is_some() => Outcome::TimedOut,
            Err(_) => Outcome::Failed,
        };
        span.record("outcome", outcome.as_str());
        m.curate_batches
            .add(1, &[KeyValue::new("outcome", outcome.as_str())]);
        m.curate_duration
            .record(secs, &[KeyValue::new("outcome", outcome.as_str())]);
        // The outcome lines belong to the batch's span (and trace).
        match result {
            Ok((pages, parts, t, commit)) => {
                span.record("pages", pages);
                m.curate_pages.add(pages, &[]);
                span.in_scope(|| {
                    tracing::info!(
                        batch = %name,
                        pages,
                        parts,
                        archive_mb = round1(t.archive_bytes as f64 / 1_048_576.0),
                        secs = round1(secs),
                        stream_secs = round1(t.stream_secs),
                        issues_secs = round1(t.issues_secs),
                        commit_secs = round1(commit.as_secs_f64()),
                        "curated"
                    )
                });
            }
            // LoC is refusing downloads from this IP. Every worker waits
            // out the block, and the batch doesn't lose an attempt.
            Err(e) if outcome == Outcome::Throttled => {
                span.in_scope(|| tracing::warn!(batch = %name, error = %format!("{e:#}"), "throttled; downloads pause for an hour"));
                self.state
                    .block_fetches(Utc::now() + THROTTLE_BLOCK)
                    .await?;
                self.release(&batch.batch, &format!("{e:#}"), false).await?;
            }
            // Where it was stuck, and how far it got.
            Err(e) if outcome == Outcome::TimedOut => {
                span.record("otel.status_code", "ERROR");
                let s = progress.snapshot();
                span.in_scope(|| {
                    tracing::error!(
                        batch = %name,
                        stage = s.stage.as_str(),
                        secs = round1(secs),
                        stage_secs = s.in_stage.as_secs(),
                        archive_mb = s.archive_mb(),
                        pages = s.pages,
                        parts = s.parts,
                        error = %format!("{e:#}"),
                        "curation timed out"
                    )
                });
                self.release(&batch.batch, &format!("{e:#}"), true).await?;
            }
            Err(e) => {
                span.record("otel.status_code", "ERROR");
                span.in_scope(|| tracing::error!(batch = %name, secs = round1(secs), error = %format!("{e:#}"), "curation failed"));
                self.release(&batch.batch, &format!("{e:#}"), true).await?;
            }
        }
        Ok(outcome)
    }

    /// Wait for a bulk-download slot. A slot reserved before another worker
    /// hit a 429 is void once that block is recorded, so check after waiting.
    async fn wait_for_fetch_slot(&self, interval: Duration, batch: &str) -> anyhow::Result<()> {
        loop {
            let slot = self.state.reserve_fetch_slot(interval).await?;
            let wait = (slot - Utc::now()).to_std().unwrap_or_default();
            tracing::info!(
                batch,
                slot_in_secs = wait.as_secs(),
                "waiting for a download slot"
            );
            tokio::time::sleep(wait).await;
            if self.state.fetch_allowed().await? {
                return Ok(());
            }
        }
    }

    /// Curate the claimed version of `b` into a new attempt path.
    pub async fn curate(&self, b: &Batch) -> anyhow::Result<Curated> {
        let name = format!("{}_ver{:02}", b.batch, b.version);
        let retained = self.retained(b, &name).await?;
        self.curate_timed(b, &name, &BatchProgress::new(), retained)
            .await
            .map(|(c, _)| c)
    }

    /// The batch's retained archive, when there is a raw store and it holds
    /// the listed one.
    async fn retained(&self, b: &Batch, name: &str) -> anyhow::Result<Option<raw::Manifest>> {
        match &self.raw {
            Some(r) => raw::find(r.as_ref(), name, b.source_sha256.as_deref()).await,
            None => Ok(None),
        }
    }

    /// Curate `b` (logged as `name`), reporting to `progress` as it goes,
    /// from its `retained` archive if there is one.
    async fn curate_timed(
        &self,
        b: &Batch,
        name: &str,
        progress: &Arc<BatchProgress>,
        retained: Option<raw::Manifest>,
    ) -> anyhow::Result<(Curated, Timings)> {
        let started = Instant::now();
        let mut upload = None;
        let download = match (&self.raw, &retained) {
            (Some(r), Some(m)) => raw::open(r.as_ref(), m).await?,
            _ => {
                // Retained as it downloads, when there is a raw store.
                let tee = self.raw.as_ref().map(|r| {
                    let path = format!("{name}/{}", raw::archive_name(name, &b.source_url));
                    let (tee, u) = raw::start(r.clone(), &path);
                    upload = Some(u);
                    tee
                });
                // Paced: one request per slot, and a failed attempt is retried
                // later through the pacer. Unpaced: retry server errors straight away.
                let tries = if self.fetch_interval.is_some() { 1 } else { 6 };
                source::open_with(&b.source_url, tries, tee).await?
            }
        };
        let headers = download.headers.clone();
        tracing::info!(
            batch = name,
            source = if retained.is_some() { "raw" } else { "loc" },
            retaining = upload.as_ref().map(raw::Upload::path),
            "archive source"
        );
        progress.track_download(download.bytes.clone());
        progress.set_stage(Stage::Downloading);
        tracing::info!(
            batch = name,
            archive_mb = download.size.map(mb),
            "download started"
        );
        // Unique per claim: the attempt count rises with every claim of this
        // version, and the sub-second time separates re-queued runs.
        let attempt = format!(
            "{}-{}-a{}",
            Utc::now()
                .format("%Y%m%dT%H%M%S%.6fZ")
                .to_string()
                .replace('.', ""),
            self.owner,
            b.attempts
        );
        let prefix = format!("pages/{}/v{:02}/{attempt}", b.batch, b.version);
        let w = self
            .write_parts(b, download.reader, &prefix, progress)
            .await?;
        // The parts are only an uncommitted attempt until this matches.
        let sha = download.digest.await.context("download task stopped")??;
        let mut timings = Timings {
            archive_bytes: download.bytes.load(Ordering::Relaxed),
            stream_secs: started.elapsed().as_secs_f64(),
            issues_secs: 0.0,
        };
        if let Some(want) = &b.source_sha256 {
            if !want.eq_ignore_ascii_case(&sha) {
                if let Some(u) = upload {
                    u.abandon();
                }
                bail!("archive sha256 {sha} does not match the published {want}");
            }
        }
        if let Some(m) = &retained {
            if !m.sha256.eq_ignore_ascii_case(&sha) {
                bail!(
                    "the retained archive `{}` reads as sha256 {sha}, not its recorded {}",
                    m.path,
                    m.sha256
                );
            }
        }
        if w.pages == 0 {
            if let Some(u) = upload {
                u.abandon();
            }
            bail!("the archive holds no pages");
        }
        // The archive checked out: commit its retained copy, then record it,
        // before the batch can be marked curated.
        if let (Some(u), Some(r)) = (upload, &self.raw) {
            let path = u.path().to_owned();
            if let Some(bytes) = u.commit().await? {
                raw::record_once(
                    r.as_ref(),
                    &raw::Manifest {
                        batch: name.to_owned(),
                        path: path.clone(),
                        source_url: b.source_url.clone(),
                        bytes,
                        sha256: sha.clone(),
                        fetched_at: Utc::now(),
                        headers,
                    },
                )
                .await?;
                tracing::info!(
                    batch = name,
                    path,
                    archive_mb = mb(bytes),
                    "archive retained"
                );
            } else {
                // Another curation's copy is already at the path (another
                // environment sharing the archival account, or an archive
                // LoC served before at this version). It stays as it is,
                // with whatever manifest names it; this batch curates from
                // what it downloaded.
                match raw::find(r.as_ref(), name, None).await? {
                    Some(t) if t.sha256.eq_ignore_ascii_case(&sha) => tracing::info!(
                        batch = name,
                        path,
                        "the archive was already retained, the same one"
                    ),
                    Some(t) => tracing::warn!(
                        batch = name,
                        path,
                        retained = %t.sha256,
                        downloaded = %sha,
                        "another archive is retained for this batch; this one isn't kept"
                    ),
                    None => tracing::warn!(
                        batch = name,
                        path,
                        "an archive with no manifest is at the path (a curation that stopped, or one recording it now); this one isn't kept"
                    ),
                }
            }
        }
        tracing::info!(
            batch = name,
            pages = w.pages,
            parts = w.parts.len(),
            archive_mb = mb(timings.archive_bytes),
            secs = round1(timings.stream_secs),
            sha256_checked = b.source_sha256.is_some(),
            "archive read"
        );
        progress.set_stage(Stage::Issues);
        let counts_path = format!("{prefix}/counts.json");
        let issues_started = Instant::now();
        self.put_new(
            &counts_path,
            serde_json::to_vec(&w.counts)?,
            "application/json",
        )
        .await?;
        let issues = w.issues.len();
        self.upsert_issues(b, w.issues).await?;
        timings.issues_secs = issues_started.elapsed().as_secs_f64();
        tracing::info!(
            batch = name,
            issues,
            issues_secs = round1(timings.issues_secs),
            "issue items written"
        );
        let curated = Curated {
            version: b.version,
            lccns: w.counts.keys().cloned().collect(),
            parts: w.parts,
            counts: counts_path,
            pages: w.pages,
            ok_pages: w.ok_pages,
            source_sha256: sha,
            first: w.first.to_string(),
            last: w.last.to_string(),
        };
        Ok((curated, timings))
    }

    async fn put_new(&self, path: &str, body: Vec<u8>, content_type: &str) -> anyhow::Result<()> {
        if !self.curated.put_new(path, body, content_type).await? {
            bail!("`{path}` already exists; attempt paths must be unique");
        }
        Ok(())
    }

    async fn write_parts(
        &self,
        b: &Batch,
        input: source::ChannelReader,
        prefix: &str,
        progress: &Arc<BatchProgress>,
    ) -> anyhow::Result<Written> {
        // Parsing and Parquet encoding are CPU-bound; finished parts come back
        // over a channel and are uploaded while the next one is built.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let (batch, version, ocr) = (b.batch.clone(), b.version, b.ocr_source.clone());
        let parsed = progress.clone();
        let producer = tokio::task::spawn_blocking(move || -> anyhow::Result<_> {
            let now = Utc::now();
            let mut writer = PartWriter::new();
            let mut counts = Counts::new();
            let mut issues = Issues::new();
            let (mut pages, mut ok_pages) = (0u64, 0u64);
            let (mut first, mut last) = (NaiveDate::MAX, NaiveDate::MIN);
            let stats = archive::read_pages_from(input, |p| {
                let row = CuratedRow::from_ocr(p.key, &p.text, &batch, version, &ocr, now);
                pages += 1;
                parsed.page();
                ok_pages += u64::from(row.status == TextStatus::Ok);
                first = first.min(row.key.date);
                last = last.max(row.key.date);
                *counts
                    .entry(row.key.lccn.clone())
                    .or_default()
                    .entry(day_number(row.key.date))
                    .or_default() += 1;
                let issue = issues
                    .entry((row.key.lccn.clone(), row.key.date, row.key.edition))
                    .or_default();
                issue.0 += 1;
                issue.1 += u32::from(row.status == TextStatus::Empty);
                if let Some(part) = writer.push(row)? {
                    tx.blocking_send(part).context("uploader stopped")?;
                }
                Ok(())
            })?;
            if stats.duplicates > 0 {
                tracing::warn!(
                    %batch,
                    duplicates = stats.duplicates,
                    "archive repeats pages with identical text; read each once"
                );
            }
            if stats.zero_numbered > 0 {
                tracing::warn!(
                    %batch,
                    pages = stats.zero_numbered,
                    "archive has pages numbered 0 (ed-0 or seq-0); skipped them"
                );
            }
            if let Some(part) = writer.finish_part()? {
                tx.blocking_send(part).context("uploader stopped")?;
            }
            Ok((counts, issues, pages, ok_pages, first, last))
        });
        let mut parts = Vec::new();
        while let Some(part) = rx.recv().await {
            let path = format!("{prefix}/part-{:04}.parquet", parts.len());
            self.put_new(&path, part, "application/vnd.apache.parquet")
                .await?;
            progress.part();
            parts.push(path);
        }
        let (counts, issues, pages, ok_pages, first, last) = producer.await??;
        Ok(Written {
            parts,
            counts,
            issues,
            pages,
            ok_pages,
            first,
            last,
        })
    }

    /// One issue item per (lccn, date, edition), with page and empty-page counts.
    async fn upsert_issues(&self, b: &Batch, issues: Issues) -> anyhow::Result<()> {
        stream::iter(issues)
            .map(|((lccn, date, edition), (pages, empty))| {
                let issue = Issue {
                    id: format!("{lccn}_{date}_ed-{edition}"),
                    lccn,
                    date: date.to_string(),
                    edition,
                    pages,
                    empty_pages: empty,
                    batch: b.batch.clone(),
                    batch_version: b.version,
                    ocr_source: b.ocr_source.clone(),
                };
                async move { self.state.upsert_issue(&issue).await }
            })
            .buffer_unordered(8)
            .try_collect::<()>()
            .await
    }

    /// Extend this worker's lease on `claimed` to a full lease from now.
    /// `false` if another worker has claimed the batch since, or a new
    /// version was queued. An expired lease that is still this worker's can
    /// be renewed: no one else has claimed the batch, and the ETag check
    /// fails if someone does meanwhile.
    async fn renew_lease(&self, claimed: &Batch) -> anyhow::Result<bool> {
        let Some((mut b, etag)) = self.state.batch(&claimed.batch).await? else {
            return Ok(false);
        };
        if b.version != claimed.version || b.lease.as_ref().is_none_or(|l| l.owner != self.owner) {
            return Ok(false);
        }
        let now = Utc::now();
        b.lease = Some(Lease {
            owner: self.owner.clone(),
            until: now + self.lease,
        });
        b.updated_at = now;
        Ok(self.state.replace_batch(&b, &etag).await?.is_some())
    }

    /// Point the batch at the curated attempt: the commit, and the last write.
    pub async fn commit(&self, batch: &str, c: Curated) -> anyhow::Result<()> {
        let (mut b, etag) = self.state.batch(batch).await?.context("batch vanished")?;
        let held = b
            .lease
            .as_ref()
            .is_some_and(|l| l.owner == self.owner && l.until > Utc::now());
        if !held || b.version != c.version {
            bail!("{batch}: lease lost or version changed before commit; the attempt is discarded");
        }
        b.status = BatchStatus::Curated;
        b.lease = None;
        b.last_error = None;
        b.curated = Some(c);
        b.updated_at = Utc::now();
        b.curated_at = Some(b.updated_at);
        if self.state.replace_batch(&b, &etag).await?.is_none() {
            bail!("{batch}: changed concurrently before commit; the attempt is discarded");
        }
        Ok(())
    }

    /// Clear this worker's lease and re-queue the batch (or mark it failed
    /// once it's out of attempts). An attempt that wasn't `counted` is given
    /// back, e.g. when the source refused to serve it at all.
    async fn release(&self, batch: &str, error: &str, counted: bool) -> anyhow::Result<()> {
        let Some((mut b, etag)) = self.state.batch(batch).await? else {
            return Ok(());
        };
        if b.lease.as_ref().is_none_or(|l| l.owner != self.owner) {
            return Ok(());
        }
        b.lease = None;
        b.last_error = Some(error.chars().take(2000).collect());
        if !counted {
            b.attempts = b.attempts.saturating_sub(1);
        }
        b.status = if b.attempts >= MAX_ATTEMPTS {
            BatchStatus::Failed
        } else {
            BatchStatus::Queued
        };
        b.updated_at = Utc::now();
        // Losing this race is harmless: the lease expires on its own.
        let _ = self.state.replace_batch(&b, &etag).await?;
        Ok(())
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::MemoryDocs;
    use crate::source::ListedBatch;
    use std::io::Write;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn listed(name: &str, url: String) -> ListedBatch {
        ListedBatch {
            name: name.into(),
            url,
            sha256: None,
            ocr_source: None,
            lccns: vec![],
        }
    }

    /// A one-page archive on disk.
    fn archive(dir: &std::path::Path) -> String {
        let text = b"a page with more than enough words in it to count as a real page of text";
        let mut t = tar::Builder::new(Vec::new());
        let mut h = tar::Header::new_gnu();
        h.set_size(text.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        t.append_data(
            &mut h,
            "sn84026749/1896/07/10/ed-1/seq-1/ocr.txt",
            &text[..],
        )
        .unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&t.into_inner().unwrap()).unwrap();
        let path = dir.join("batch_b_ver01.tar.gz");
        std::fs::write(&path, gz.finish().unwrap()).unwrap();
        path.to_str().unwrap().to_owned()
    }

    /// A server that answers 200 with a large Content-Length, sends no body,
    /// and reports when the client hangs up.
    async fn stalled_server() -> (String, tokio::sync::oneshot::Receiver<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/batch_a_ver01.tar.bz2",
            listener.local_addr().unwrap()
        );
        let (closed, hung_up) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req).await;
            let head = "HTTP/1.1 200 OK\r\ncontent-length: 104857600\r\n\r\n";
            sock.write_all(head.as_bytes()).await.unwrap();
            // Returns 0 (or fails) once the client drops the connection.
            let _ = sock.read(&mut req).await;
            let _ = closed.send(());
        });
        (url, hung_up)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_batch_past_the_limit_is_released_and_the_worker_moves_on() {
        let dir = tempfile::tempdir().unwrap();
        let (stalled, hung_up) = stalled_server().await;
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(
            &state,
            &[
                listed("batch_a_ver01", stalled),
                listed("batch_b_ver01", archive(dir.path())),
            ],
        )
        .await
        .unwrap();
        let w = Worker {
            state: state.clone(),
            curated: Arc::new(usnm_store::LocalStore::new(dir.path().join("curated"))),
            owner: "w1".into(),
            lease: Duration::hours(2),
            fetch_interval: None,
            batch_limit: std::time::Duration::from_millis(500),
            deadline: None,
            raw: None,
        };
        let run = tokio::time::timeout(std::time::Duration::from_secs(30), w.run(None));
        assert_eq!(
            run.await
                .expect("the watchdog ends the stalled batch")
                .unwrap(),
            1
        );

        // Released like a failed attempt: re-queued, attempt counted, no lease.
        let (a, _) = state.batch("batch_a").await.unwrap().unwrap();
        assert_eq!((a.status, a.attempts), (BatchStatus::Queued, 1));
        assert!(a.lease.is_none() && a.curated.is_none());
        let err = a.last_error.unwrap();
        assert!(err.contains("stage downloading"), "{err}");
        // The other batch was curated as usual.
        let (b, _) = state.batch("batch_b").await.unwrap().unwrap();
        assert_eq!(b.status, BatchStatus::Curated);
        assert_eq!(b.curated.unwrap().pages, 1);
        // The abandoned download was stopped, not left running.
        tokio::time::timeout(std::time::Duration::from_secs(10), hung_up)
            .await
            .expect("the download's connection was closed")
            .unwrap();
    }

    fn worker(state: &State, dir: &std::path::Path, owner: &str) -> Worker {
        Worker {
            state: state.clone(),
            curated: Arc::new(usnm_store::LocalStore::new(dir.join("curated"))),
            owner: owner.into(),
            lease: Duration::hours(2),
            fetch_interval: None,
            batch_limit: BATCH_LIMIT,
            deadline: None,
            raw: None,
        }
    }

    #[tokio::test]
    async fn a_lease_lost_during_the_slot_wait_is_not_downloaded() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/batch_a_ver01.tar.bz2",
            listener.local_addr().unwrap()
        );
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(&state, &[listed("batch_a_ver01", url)])
            .await
            .unwrap();
        let w1 = worker(&state, dir.path(), "w1");
        let (claimed, _) = w1.claim().await.unwrap().unwrap();
        // While w1 waits, its lease runs out and w2 claims the batch.
        let (mut b, etag) = state.batch("batch_a").await.unwrap().unwrap();
        b.lease.as_mut().unwrap().until = Utc::now() - Duration::seconds(1);
        state.replace_batch(&b, &etag).await.unwrap().unwrap();
        let w2 = worker(&state, dir.path(), "w2");
        w2.claim().await.unwrap().unwrap();

        assert_eq!(w1.run_one(&claimed).await.unwrap(), Outcome::Failed);
        let (b, _) = state.batch("batch_a").await.unwrap().unwrap();
        assert_eq!(b.lease.unwrap().owner, "w2");
        assert_eq!((b.status, b.attempts), (BatchStatus::Downloading, 2));
        // No request was made.
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(accepted.is_err(), "w1 downloaded a batch it no longer held");
    }

    #[tokio::test]
    async fn an_expired_lease_still_held_is_renewed_and_the_batch_curated() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(&state, &[listed("batch_b_ver01", archive(dir.path()))])
            .await
            .unwrap();
        let w = worker(&state, dir.path(), "w1");
        let (claimed, _) = w.claim().await.unwrap().unwrap();
        let (mut b, etag) = state.batch("batch_b").await.unwrap().unwrap();
        b.lease.as_mut().unwrap().until = Utc::now() - Duration::seconds(1);
        state.replace_batch(&b, &etag).await.unwrap().unwrap();

        assert_eq!(w.run_one(&claimed).await.unwrap(), Outcome::Ok);
        let (b, _) = state.batch("batch_b").await.unwrap().unwrap();
        assert_eq!(b.status, BatchStatus::Curated);
    }

    #[tokio::test]
    async fn a_commit_cut_short_after_it_landed_is_not_undone() {
        // The watchdog can fire while the commit's write is in flight. If it
        // landed, the release that follows must leave the batch curated.
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(&state, &[listed("batch_b_ver01", archive(dir.path()))])
            .await
            .unwrap();
        let w = Worker {
            state: state.clone(),
            curated: Arc::new(usnm_store::LocalStore::new(dir.path().join("curated"))),
            owner: "w1".into(),
            lease: Duration::hours(2),
            fetch_interval: None,
            batch_limit: BATCH_LIMIT,
            deadline: None,
            raw: None,
        };
        let (b, _) = w.claim().await.unwrap().unwrap();
        let c = w.curate(&b).await.unwrap();
        w.commit("batch_b", c).await.unwrap();
        w.release("batch_b", "timed out", true).await.unwrap();
        let (b, _) = state.batch("batch_b").await.unwrap().unwrap();
        assert_eq!((b.status, b.attempts), (BatchStatus::Curated, 1));
        assert!(b.last_error.is_none());
        // And nobody can claim it again.
        assert!(w.claim().await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_worker_past_its_deadline_claims_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(&state, &[listed("batch_b_ver01", archive(dir.path()))])
            .await
            .unwrap();
        let w = Worker {
            deadline: Some(tokio::time::Instant::now()),
            ..worker(&state, dir.path(), "w1")
        };
        assert_eq!(w.run(None).await.unwrap(), 0);
        let (b, _) = state.batch("batch_b").await.unwrap().unwrap();
        assert_eq!((b.status, b.attempts), (BatchStatus::Queued, 0));
        assert!(b.lease.is_none());
    }

    #[tokio::test]
    async fn a_batch_claimed_after_the_deadline_is_released_unstarted() {
        let dir = tempfile::tempdir().unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(&state, &[listed("batch_b_ver01", archive(dir.path()))])
            .await
            .unwrap();
        // The deadline passes while the claim is in flight.
        let w = Worker {
            deadline: Some(tokio::time::Instant::now()),
            ..worker(&state, dir.path(), "w1")
        };
        let (claimed, _) = w.claim().await.unwrap().unwrap();
        assert_eq!(w.run_one(&claimed).await.unwrap(), Outcome::Stopped);
        let (b, _) = state.batch("batch_b").await.unwrap().unwrap();
        assert_eq!((b.status, b.attempts), (BatchStatus::Queued, 0));
        assert!(b.lease.is_none() && b.curated.is_none());
    }

    #[tokio::test]
    async fn a_deadline_during_the_slot_wait_releases_the_batch_unstarted() {
        let dir = tempfile::tempdir().unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/batch_a_ver01.tar.bz2",
            listener.local_addr().unwrap()
        );
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(&state, &[listed("batch_a_ver01", url)])
            .await
            .unwrap();
        // LoC is blocking downloads for the next hour.
        state
            .block_fetches(Utc::now() + Duration::hours(1))
            .await
            .unwrap();
        let w = Worker {
            fetch_interval: Some(Duration::seconds(75)),
            deadline: Some(tokio::time::Instant::now() + std::time::Duration::from_millis(300)),
            ..worker(&state, dir.path(), "w1")
        };
        let run = tokio::time::timeout(std::time::Duration::from_secs(10), w.run(None));
        assert_eq!(run.await.expect("the worker stops waiting").unwrap(), 0);

        // Back in the queue as it was: no lease, and the attempt given back.
        let (a, _) = state.batch("batch_a").await.unwrap().unwrap();
        assert_eq!((a.status, a.attempts), (BatchStatus::Queued, 0));
        assert!(a.lease.is_none());
        // No request was made.
        let accepted =
            tokio::time::timeout(std::time::Duration::from_millis(200), listener.accept()).await;
        assert!(
            accepted.is_err(),
            "the worker downloaded after its deadline"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_batch_under_way_at_the_deadline_is_finished_and_no_other_claimed() {
        let dir = tempfile::tempdir().unwrap();
        // Serves the archive, but only after the deadline has passed.
        let body = std::fs::read(archive(dir.path())).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let slow = format!(
            "http://{}/batch_a_ver01.tar.gz",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req).await;
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            let head = format!("HTTP/1.1 200 OK\r\ncontent-length: {}\r\n\r\n", body.len());
            sock.write_all(head.as_bytes()).await.unwrap();
            sock.write_all(&body).await.unwrap();
            sock.shutdown().await.unwrap();
        });
        let state = State::new(Arc::new(MemoryDocs::default()));
        source::enqueue(
            &state,
            &[
                listed("batch_a_ver01", slow),
                listed("batch_b_ver01", archive(dir.path())),
            ],
        )
        .await
        .unwrap();
        let w = Worker {
            deadline: Some(tokio::time::Instant::now() + std::time::Duration::from_millis(100)),
            ..worker(&state, dir.path(), "w1")
        };
        let run = tokio::time::timeout(std::time::Duration::from_secs(30), w.run(None));
        assert_eq!(run.await.unwrap().unwrap(), 1);

        let (a, _) = state.batch("batch_a").await.unwrap().unwrap();
        assert_eq!(a.status, BatchStatus::Curated);
        let (b, _) = state.batch("batch_b").await.unwrap().unwrap();
        assert_eq!((b.status, b.attempts), (BatchStatus::Queued, 0));
    }
}
