//! Typed pipeline state on top of [`DocStore`] (05 §5.9.1): batches (the
//! work queue), issues, the single-writer lock and index runs.

use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{DateTime, Duration, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::docs::{DocStore, Versioned};

pub const BATCHES: &str = "batches";
pub const ISSUES: &str = "issues";
pub const INDEX_RUNS: &str = "index_runs";
pub const OPS: &str = "ops";
/// The `ops` item that paces bulk downloads from LoC across every worker.
pub const FETCH_PACER: &str = "loc-bulk-pacer";
/// The `ops` lock held by the one release (Quickwit writer) at a time (08 §8.4.1).
pub const WRITER_LOCK: &str = "quickwit-writer";
/// Deltas a version may carry before the next release compacts (08 §8.4.1).
pub const MAX_DELTAS: usize = 8;
/// The `ops` item a running release updates with how far it has got.
pub const RELEASE_PROGRESS: &str = "release-progress";
/// The `ops` item the ingest job (`run`, `titles-sync`, `release`) keeps up
/// to date with the step it is on, for the status page's "Right now" line.
pub const ACTIVITY: &str = "activity";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Queued,
    Downloading,
    Curated,
    Failed,
}

impl BatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Curated => "curated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: String,
    pub until: DateTime<Utc>,
}

/// The committed output of one curated batch version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Curated {
    pub version: u16,
    /// Object paths in the curated store.
    pub parts: Vec<String>,
    /// Pages per (lccn, day), for baselines without re-reading the parts.
    pub counts: String,
    pub pages: u64,
    /// Pages with `text_status = ok` (the ones that are indexed).
    pub ok_pages: u64,
    pub lccns: Vec<String>,
    pub source_sha256: String,
    pub first: String,
    pub last: String,
}

/// One LoC batch (partition key `/batch`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub batch: String,
    /// The newest version seen; the one to curate.
    pub version: u16,
    pub source_url: String,
    #[serde(default)]
    pub source_sha256: Option<String>,
    pub ocr_source: String,
    pub status: BatchStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub lease: Option<Lease>,
    /// The last committed curation (may be an older version while a newer one is queued).
    #[serde(default)]
    pub curated: Option<Curated>,
    #[serde(default)]
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
    /// When `curated` was committed. `updated_at` changes with every later
    /// write (a newer version queued, a claim), so throughput counts this.
    /// Absent on batches curated before it was recorded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub curated_at: Option<DateTime<Utc>>,
}

/// One issue (lccn + date + edition), partition key `/lccn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub lccn: String,
    pub date: String,
    pub edition: u16,
    pub pages: u32,
    pub empty_pages: u32,
    pub batch: String,
    pub batch_version: u16,
    pub ocr_source: String,
}

/// One batch a version was built from, at the curation it was built from.
/// A version's list lives in its reference snapshot (`{version}/batches.json`),
/// not in the [`IndexRun`] item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBatch {
    pub batch: String,
    pub curated: Curated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Building,
    Published,
    Failed,
}

/// The file in a version's reference snapshot that lists the batches it was
/// built from (a JSON array of [`RunBatch`]).
pub const RUN_BATCHES_FILE: &str = "batches.json";

/// The file in a version's reference snapshot with every title's published
/// pages, `{lccn: pages}` (the status page's pages by language). Snapshots
/// written before it existed don't have it.
pub const TITLE_PAGES_FILE: &str = "title_pages.json";

/// One index build (partition key `/index_version`).
///
/// The item holds only bounded fields. The version's batch list, which grows
/// with the corpus (500–650 bytes per batch), is in the reference store at
/// [`IndexRun::batch_list`], so the item stays far below Cosmos DB's 2 MB
/// item limit however many batches a version has.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexRun {
    pub id: String,
    pub index_version: String,
    pub full: bool,
    /// Every index the version serves (base + deltas).
    pub indexes: Vec<String>,
    /// The index this run wrote.
    pub new_index: String,
    /// Batches in the version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_count: Option<u64>,
    /// Path of the version's batch list in the reference store
    /// (`{version}/batches.json`), set once the snapshot is written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batch_list: Option<String>,
    /// The batch list inline, as runs written before it moved to the
    /// reference snapshot have it. New runs leave it out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub batches: Option<Vec<RunBatch>>,
    pub status: RunStatus,
    pub docs: u64,
    pub pages: u64,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// How far the running release has got: the `ops/release-progress` item,
/// rewritten every 30 s while an index is built. It is a separate item, not
/// part of the [`IndexRun`], so these writes never change the run's ETag,
/// which the release threads through its own conditional updates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReleaseProgress {
    pub index_version: String,
    pub docs_sent: u64,
    pub docs_expected: u64,
    pub mb_sent: f64,
    pub updated_at: DateTime<Utc>,
}

/// The step an ingest job execution is on (`ops/activity`), in the order a
/// `run` takes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    /// Reading LoC's batch listing and queueing new batches.
    Listing,
    /// Downloading and processing (curating) queued batches.
    Downloading,
    /// Fetching newspaper (title) records from loc.gov (titles-sync).
    Titles,
    /// Sending pages to the new search index.
    Indexing,
    /// Waiting for the new index's merges.
    Merging,
    /// Writing the reference snapshot and the version pointer.
    Publishing,
}

/// How an execution ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// A new version went live.
    Published,
    /// It finished with nothing new to publish.
    NothingNew,
    /// titles-sync didn't fetch every title in time (LoC rate limits it);
    /// a full rebuild then publishes nothing and the next execution goes on.
    TitlesLeft,
    /// It failed; `error` says why.
    Failed,
}

/// Where the merge wait is (`crate::merges` in the ingest crate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergeProgress {
    /// `settle` (the index is open and merging) or `finalize` (closed, last merges).
    pub step: String,
    pub splits: u64,
    pub merges_running: u64,
    pub merges_queued: u64,
}

/// What the ingest job execution is doing: the `ops/activity` item. One
/// execution of `caj-usnm-ingest` runs at a time (it has parallelism 1);
/// the backfill job's curate workers don't write it. The job rewrites it
/// on every step change and at least once a minute while it runs, so a
/// reader can tell a live execution from one that stopped without saying.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Activity {
    /// The `usnm-ingest` command: `run`, `titles-sync` or `release`.
    pub command: String,
    /// The writer's process id ([`crate`] users show its last characters only).
    pub owner: String,
    pub started_at: DateTime<Utc>,
    pub step: Step,
    pub step_started_at: DateTime<Utc>,
    /// The last write: a heartbeat while the execution runs.
    pub updated_at: DateTime<Utc>,
    /// The step's progress, where it counts something (titles fetched,
    /// batches curated); indexing reports in `ops/release-progress`.
    #[serde(default)]
    pub done: Option<u64>,
    #[serde(default)]
    pub total: Option<u64>,
    /// titles-sync sends nothing to loc.gov until then (LoC rate limited it).
    #[serde(default)]
    pub paused_until: Option<DateTime<Utc>>,
    /// The version being built, from the indexing step on.
    #[serde(default)]
    pub index_version: Option<String>,
    #[serde(default)]
    pub merge: Option<MergeProgress>,
    /// Set when the execution ends, with how.
    #[serde(default)]
    pub ended_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub outcome: Option<Outcome>,
    /// The error an execution failed with (the API sanitizes it).
    #[serde(default)]
    pub error: Option<String>,
}

/// Typed access to the pipeline's state.
#[derive(Clone)]
pub struct State {
    pub docs: Arc<dyn DocStore>,
}

fn to_value<T: Serialize>(item: &T) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(item)?)
}

fn from<T: DeserializeOwned>(v: Versioned) -> anyhow::Result<(T, String)> {
    let id = v.doc["id"].as_str().unwrap_or("?").to_owned();
    let item = serde_json::from_value(v.doc).with_context(|| format!("state item `{id}`"))?;
    Ok((item, v.etag))
}

impl State {
    pub fn new(docs: Arc<dyn DocStore>) -> Self {
        Self { docs }
    }

    // ---------------------------------------------------------------- batches

    pub async fn batch(&self, batch: &str) -> anyhow::Result<Option<(Batch, String)>> {
        self.docs
            .get(BATCHES, batch, batch)
            .await?
            .map(from)
            .transpose()
    }

    pub async fn batches(&self, statuses: &[BatchStatus]) -> anyhow::Result<Vec<(Batch, String)>> {
        let values: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
        self.docs
            .list(BATCHES, "status", &values)
            .await?
            .into_iter()
            .map(from)
            .collect()
    }

    pub async fn create_batch(&self, b: &Batch) -> anyhow::Result<bool> {
        Ok(self
            .docs
            .create(BATCHES, &b.batch, &to_value(b)?)
            .await?
            .is_some())
    }

    /// Replace if unchanged since read; returns the new ETag, or `None` if another writer won.
    pub async fn replace_batch(&self, b: &Batch, etag: &str) -> anyhow::Result<Option<String>> {
        self.docs
            .replace(BATCHES, &b.batch, &to_value(b)?, etag)
            .await
    }

    pub async fn upsert_issue(&self, i: &Issue) -> anyhow::Result<()> {
        self.docs.upsert(ISSUES, &i.lccn, &to_value(i)?).await
    }

    // ------------------------------------------------------------ index runs

    pub async fn run(&self, version: &str) -> anyhow::Result<Option<(IndexRun, String)>> {
        self.docs
            .get(INDEX_RUNS, version, version)
            .await?
            .map(from)
            .transpose()
    }

    pub async fn create_run(&self, r: &IndexRun) -> anyhow::Result<bool> {
        Ok(self
            .docs
            .create(INDEX_RUNS, &r.index_version, &to_value(r)?)
            .await?
            .is_some())
    }

    pub async fn update_run(&self, r: &IndexRun, etag: &str) -> anyhow::Result<String> {
        self.docs
            .replace(INDEX_RUNS, &r.index_version, &to_value(r)?, etag)
            .await?
            .with_context(|| format!("index run `{}` changed concurrently", r.id))
    }

    // ------------------------------------------------------------------- ops

    /// The published version, mirrored from `current.json` (05 §5.9.1).
    pub async fn current_version(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .docs
            .get(OPS, "current", "current")
            .await?
            .and_then(|v| v.doc["index_version"].as_str().map(str::to_owned)))
    }

    pub async fn set_current_version(&self, version: &str) -> anyhow::Result<()> {
        let doc = serde_json::json!({
            "id": "current", "kind": "current", "index_version": version,
            "updated_at": Utc::now(),
        });
        self.docs.upsert(OPS, "current", &doc).await
    }

    /// Take the single-writer lock (08 §8.4.1). Fails if another live holder has it.
    pub async fn lock(&self, name: &str, owner: &str, ttl: Duration) -> anyhow::Result<()> {
        // A lost ETag race just means re-reading: it may have been this
        // owner's own concurrent renewal, which is fine.
        for _ in 0..5 {
            let doc = serde_json::json!({
                "id": name, "kind": name, "owner": owner, "until": Utc::now() + ttl,
            });
            let Some(held) = self.docs.get(OPS, name, name).await? else {
                if self.docs.create(OPS, name, &doc).await?.is_some() {
                    return Ok(());
                }
                continue;
            };
            let until: Option<DateTime<Utc>> =
                serde_json::from_value(held.doc["until"].clone()).ok();
            let holder = held.doc["owner"].as_str().unwrap_or("?");
            if holder != owner && until.is_some_and(|u| u > Utc::now()) {
                bail!(
                    "lock `{name}` is held by `{holder}` until {}",
                    until.expect("checked")
                );
            }
            if self
                .docs
                .replace(OPS, name, &doc, &held.etag)
                .await?
                .is_some()
            {
                return Ok(());
            }
        }
        bail!("lock `{name}` kept changing concurrently")
    }

    /// Reserve the next bulk-download slot and return when it starts. Every
    /// worker and job execution shares one egress IP, and LoC allows 10 bulk
    /// requests per 10 minutes per IP (04 §4.1), so slots come from one item,
    /// `interval` apart, and none start before a recorded block lifts.
    pub async fn reserve_fetch_slot(&self, interval: Duration) -> anyhow::Result<DateTime<Utc>> {
        for _ in 0..20 {
            let now = Utc::now();
            let held = self.docs.get(OPS, FETCH_PACER, FETCH_PACER).await?;
            let (next, blocked) = pacer(held.as_ref());
            let slot = next.map_or(now, |n| n.max(now));
            if self
                .set_pacer(held.as_ref(), slot + interval, blocked)
                .await?
            {
                return Ok(slot);
            }
        }
        bail!("the download pacer kept changing concurrently")
    }

    /// Whether a download may start now: `false` while a recorded block is in
    /// force. A slot reserved before the block was recorded is void, so its
    /// holder asks this after waiting and reserves again if it must. A plain
    /// read isn't enough: Cosmos's Session consistency is per process, so
    /// another worker's block may not be visible yet. Rewriting the item with
    /// its ETag only succeeds against the latest version.
    pub async fn fetch_allowed(&self) -> anyhow::Result<bool> {
        for _ in 0..20 {
            let Some(held) = self.docs.get(OPS, FETCH_PACER, FETCH_PACER).await? else {
                return Ok(true);
            };
            if pacer(Some(&held)).1.is_some_and(|u| u > Utc::now()) {
                return Ok(false);
            }
            if self
                .docs
                .replace(OPS, FETCH_PACER, &held.doc, &held.etag)
                .await?
                .is_some()
            {
                return Ok(true);
            }
        }
        bail!("the download pacer kept changing concurrently")
    }

    /// Hold every worker's bulk downloads until `until`: LoC blocks an IP for
    /// about an hour once it answers 429, and each retry extends the block.
    pub async fn block_fetches(&self, until: DateTime<Utc>) -> anyhow::Result<()> {
        for _ in 0..20 {
            let held = self.docs.get(OPS, FETCH_PACER, FETCH_PACER).await?;
            let (next, blocked) = pacer(held.as_ref());
            if blocked.is_some_and(|b| b >= until) {
                return Ok(());
            }
            let next = next.map_or(until, |n| n.max(until));
            if self.set_pacer(held.as_ref(), next, Some(until)).await? {
                return Ok(());
            }
        }
        bail!("the download pacer kept changing concurrently")
    }

    /// Write the pacer, if it hasn't changed since `held` was read.
    async fn set_pacer(
        &self,
        held: Option<&Versioned>,
        next: DateTime<Utc>,
        blocked_until: Option<DateTime<Utc>>,
    ) -> anyhow::Result<bool> {
        let doc = serde_json::json!({
            "id": FETCH_PACER, "kind": FETCH_PACER, "next": next,
            "blocked_until": blocked_until,
        });
        Ok(match held {
            Some(h) => self.docs.replace(OPS, FETCH_PACER, &doc, &h.etag).await?,
            None => self.docs.create(OPS, FETCH_PACER, &doc).await?,
        }
        .is_some())
    }

    /// Record the running release's progress (unconditional: only the
    /// writer-lock holder writes it).
    pub async fn set_release_progress(&self, p: &ReleaseProgress) -> anyhow::Result<()> {
        let mut doc = to_value(p)?;
        doc["id"] = RELEASE_PROGRESS.into();
        doc["kind"] = RELEASE_PROGRESS.into();
        self.docs.upsert(OPS, RELEASE_PROGRESS, &doc).await
    }

    /// Record what the ingest job is doing (unconditional: one execution
    /// runs at a time).
    pub async fn set_activity(&self, a: &Activity) -> anyhow::Result<()> {
        let mut doc = to_value(a)?;
        doc["id"] = ACTIVITY.into();
        doc["kind"] = ACTIVITY.into();
        self.docs.upsert(OPS, ACTIVITY, &doc).await
    }

    pub async fn unlock(&self, name: &str, owner: &str) -> anyhow::Result<()> {
        if let Some(held) = self.docs.get(OPS, name, name).await? {
            if held.doc["owner"].as_str() == Some(owner) {
                let doc = serde_json::json!({
                    "id": name, "kind": name, "owner": "", "until": Utc::now(),
                });
                self.docs.replace(OPS, name, &doc, &held.etag).await?;
            }
        }
        Ok(())
    }
}

/// The pacer's next free slot and the end of its recorded block, if any.
pub fn pacer(held: Option<&Versioned>) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let at = |field: &str| -> Option<DateTime<Utc>> {
        held.and_then(|h| serde_json::from_value(h.doc[field].clone()).ok())
    };
    (at("next"), at("blocked_until"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::MemoryDocs;

    #[tokio::test]
    async fn download_slots_are_spaced_across_concurrent_workers() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        let gap = Duration::seconds(75);
        let mut slots = Vec::new();
        for _ in 0..3 {
            let (a, b) = tokio::join!(s.reserve_fetch_slot(gap), s.reserve_fetch_slot(gap));
            slots.push(a.unwrap());
            slots.push(b.unwrap());
        }
        slots.sort();
        for pair in slots.windows(2) {
            assert!(pair[1] - pair[0] >= gap, "{slots:?}");
        }
    }

    #[tokio::test]
    async fn a_block_holds_every_later_slot() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        let until = Utc::now() + Duration::hours(1);
        s.block_fetches(until).await.unwrap();
        // An earlier block never shortens a later one.
        s.block_fetches(Utc::now()).await.unwrap();
        assert_eq!(
            s.reserve_fetch_slot(Duration::seconds(75)).await.unwrap(),
            until
        );
    }

    #[tokio::test]
    async fn a_block_after_a_reservation_is_seen_by_its_holder() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        assert!(s.fetch_allowed().await.unwrap());
        s.reserve_fetch_slot(Duration::seconds(75)).await.unwrap();
        assert!(s.fetch_allowed().await.unwrap());
        let later = Utc::now() + Duration::hours(1);
        s.block_fetches(later).await.unwrap();
        assert!(!s.fetch_allowed().await.unwrap());
        // Reserving keeps the block on record.
        s.reserve_fetch_slot(Duration::seconds(75)).await.unwrap();
        assert!(!s.fetch_allowed().await.unwrap());
        // A block that has passed doesn't hold anything.
        let s2 = State::new(Arc::new(MemoryDocs::default()));
        let earlier = Utc::now() - Duration::seconds(1);
        s2.block_fetches(earlier).await.unwrap();
        assert!(s2.fetch_allowed().await.unwrap());
    }

    #[tokio::test]
    async fn concurrent_renewals_by_the_holder_both_succeed() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        s.lock("w", "a", Duration::hours(1)).await.unwrap();
        for _ in 0..20 {
            let (x, y) = tokio::join!(
                s.lock("w", "a", Duration::hours(1)),
                s.lock("w", "a", Duration::hours(1))
            );
            x.unwrap();
            y.unwrap();
        }
    }

    #[tokio::test]
    async fn writer_lock_is_exclusive_until_it_expires_or_is_released() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        s.lock("quickwit-writer", "a", Duration::hours(2))
            .await
            .unwrap();
        // Re-entrant for the same owner, exclusive for others.
        s.lock("quickwit-writer", "a", Duration::hours(2))
            .await
            .unwrap();
        assert!(s
            .lock("quickwit-writer", "b", Duration::hours(2))
            .await
            .is_err());
        s.unlock("quickwit-writer", "b").await.unwrap();
        assert!(s
            .lock("quickwit-writer", "b", Duration::hours(2))
            .await
            .is_err());
        s.unlock("quickwit-writer", "a").await.unwrap();
        s.lock("quickwit-writer", "b", Duration::seconds(-1))
            .await
            .unwrap();
        // An expired lease can be taken over.
        s.lock("quickwit-writer", "c", Duration::hours(1))
            .await
            .unwrap();
    }
}
