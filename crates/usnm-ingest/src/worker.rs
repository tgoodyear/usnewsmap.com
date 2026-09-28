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

use std::collections::{BTreeMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{Duration, NaiveDate, Utc};
use futures::stream::{self, StreamExt, TryStreamExt};
use usnm_core::text::TextStatus;
use usnm_core::time::day_number;
use usnm_store::ObjectStore;

use crate::archive;
use crate::curated::{CuratedRow, PartWriter};
use crate::source;
use crate::state::{Batch, BatchStatus, Curated, Issue, Lease, State};

/// Attempts before a batch is marked failed (an operator re-queues it).
pub const MAX_ATTEMPTS: u32 = 5;

/// Seconds between bulk downloads across every worker. LoC allows 10 bulk
/// requests per 10 minutes per IP (04 §4.1); 75 s leaves room for the odd
/// retry after a server error.
pub const FETCH_INTERVAL_SECS: u32 = 75;

/// How long every worker holds its downloads after LoC answers 429.
const THROTTLE_BLOCK: Duration = Duration::hours(1);

pub struct Worker {
    pub state: State,
    pub curated: Arc<dyn ObjectStore>,
    /// Unique per process: lease owner and part of the attempt path.
    pub owner: String,
    pub lease: Duration,
    /// Spacing between downloads from remote sources, shared by every worker
    /// through the state store; `None` doesn't pace (local archives, tests).
    pub fetch_interval: Option<Duration>,
}

/// Pages per (lccn → day → pages), stored beside the parts.
pub type Counts = BTreeMap<String, BTreeMap<u32, u32>>;

/// (pages, empty pages) per (lccn, date, edition).
type Issues = BTreeMap<(String, NaiveDate, u16), (u32, u32)>;

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

    /// Claim and curate batches until none are left; returns how many were
    /// curated. A batch that fails is re-queued for a later run, not retried
    /// straight away (the failure is often the source, e.g. a throttled download).
    pub async fn run(&self, max_batches: Option<usize>) -> anyhow::Result<usize> {
        let mut done = 0;
        let mut failed = HashSet::new();
        while max_batches.is_none_or(|m| done < m) {
            let Some((batch, _)) = self.claim_except(&failed).await? else {
                break;
            };
            let name = format!("{}_ver{:02}", batch.batch, batch.version);
            if let Some(interval) = self.fetch_interval {
                if source::local_path(&batch.source_url).is_none() {
                    self.wait_for_fetch_slot(interval).await?;
                }
            }
            // A failed commit (e.g. a newer version was queued meanwhile) is
            // handled like a failed curation: clear the lease and re-queue.
            let result = match self.curate(&batch).await {
                Ok(c) => self.commit(&batch.batch, c).await,
                Err(e) => Err(e),
            };
            match result {
                Ok(()) => {
                    tracing::info!(batch = %name, "curated");
                    done += 1;
                }
                // LoC is refusing downloads from this IP. Every worker waits
                // out the block, and the batch doesn't lose an attempt.
                Err(e) if e.downcast_ref::<source::Throttled>().is_some() => {
                    tracing::warn!(batch = %name, error = %format!("{e:#}"), "throttled; downloads pause for an hour");
                    self.state
                        .block_fetches(Utc::now() + THROTTLE_BLOCK)
                        .await?;
                    self.release(&batch.batch, &format!("{e:#}"), false).await?;
                }
                Err(e) => {
                    tracing::error!(batch = %name, error = %format!("{e:#}"), "curation failed");
                    self.release(&batch.batch, &format!("{e:#}"), true).await?;
                    failed.insert(batch.batch);
                }
            }
        }
        Ok(done)
    }

    /// Wait for a bulk-download slot. A slot reserved before another worker
    /// hit a 429 is void once that block is recorded, so check after waiting.
    async fn wait_for_fetch_slot(&self, interval: Duration) -> anyhow::Result<()> {
        loop {
            let slot = self.state.reserve_fetch_slot(interval).await?;
            if let Ok(wait) = (slot - Utc::now()).to_std() {
                tokio::time::sleep(wait).await;
            }
            if self.state.fetch_allowed().await? {
                return Ok(());
            }
        }
    }

    /// Curate the claimed version of `b` into a new attempt path.
    pub async fn curate(&self, b: &Batch) -> anyhow::Result<Curated> {
        // Paced: one request per slot, and a failed attempt is retried later
        // through the pacer. Unpaced: retry server errors straight away.
        let download = if self.fetch_interval.is_some() {
            source::open_once(&b.source_url).await?
        } else {
            source::open(&b.source_url).await?
        };
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
        let w = self.write_parts(b, download.reader, &prefix).await?;
        // The parts are only an uncommitted attempt until this matches.
        let sha = download.digest.await.context("download task stopped")??;
        if let Some(want) = &b.source_sha256 {
            if !want.eq_ignore_ascii_case(&sha) {
                bail!("archive sha256 {sha} does not match the published {want}");
            }
        }
        if w.pages == 0 {
            bail!("the archive holds no pages");
        }
        let counts_path = format!("{prefix}/counts.json");
        self.put_new(
            &counts_path,
            serde_json::to_vec(&w.counts)?,
            "application/json",
        )
        .await?;
        self.upsert_issues(b, w.issues).await?;
        Ok(Curated {
            version: b.version,
            lccns: w.counts.keys().cloned().collect(),
            parts: w.parts,
            counts: counts_path,
            pages: w.pages,
            ok_pages: w.ok_pages,
            source_sha256: sha,
            first: w.first.to_string(),
            last: w.last.to_string(),
        })
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
    ) -> anyhow::Result<Written> {
        // Parsing and Parquet encoding are CPU-bound; finished parts come back
        // over a channel and are uploaded while the next one is built.
        let (tx, mut rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let (batch, version, ocr) = (b.batch.clone(), b.version, b.ocr_source.clone());
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
