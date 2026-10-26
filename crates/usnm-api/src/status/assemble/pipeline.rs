//! Backfill and index-release pipeline sections.

use super::super::sanitize::{sanitize, short_id};
use super::{percent, round1};
use chrono::{DateTime, Duration, DurationRound, Utc};
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::BTreeMap;
use usnm_state::state::{BatchStatus, RunStatus};
use usnm_state::summary::{BatchSummary, RunSummary, Summary};

/// Hours of curation history in the throughput chart.
pub const HISTORY_HOURS: i64 = 48;
/// Hours the curation rate (and so the ETA) is averaged over.
pub const RATE_WINDOW_HOURS: i64 = 12;
const RECENT: usize = 20;
const MAX_LISTED: usize = 100;
const MAX_RUNS: usize = 20;

#[derive(Debug, Clone, Default, Serialize, JsonSchema)]
pub struct ByStatus {
    pub queued: u64,
    pub downloading: u64,
    pub curated: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct HourBin {
    pub start: DateTime<Utc>,
    pub batches: u64,
    pub pages: u64,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Throughput {
    /// Batches curated per hour, oldest first, ending with the current hour.
    pub hours: Vec<HourBin>,
    pub rate_window_hours: i64,
    /// Batches curated per hour over the last `rate_window_hours`.
    pub rate_per_hour: f64,
    /// Batches still to curate (queued or downloading).
    pub remaining: u64,
    /// When the remaining batches finish at the current rate.
    pub eta: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct LocPacer {
    /// The next free bulk-download slot, shared by every worker.
    pub next_slot: Option<DateTime<Utc>>,
    /// Downloads are held until then (LoC answered 429).
    pub blocked_until: Option<DateTime<Utc>>,
    pub throttled: bool,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct InProgress {
    pub batch: String,
    pub version: u16,
    /// A short, opaque id for the worker holding the lease.
    pub worker: Option<String>,
    /// When the worker claimed it.
    pub since: DateTime<Utc>,
    pub lease_until: Option<DateTime<Utc>>,
    /// The worker stopped renewing it; another worker may claim it.
    pub lease_expired: bool,
    pub attempts: u32,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Recent {
    pub batch: String,
    pub version: u16,
    pub pages: u64,
    pub curated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Failed {
    pub batch: String,
    pub version: u16,
    pub attempts: u32,
    /// Sanitized and shortened.
    pub error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Backfill {
    /// Batches listed by LoC and recorded by the pipeline.
    pub total: u64,
    pub by_status: ByStatus,
    /// Downloading with a live lease.
    pub in_progress: u64,
    /// Downloading with an expired lease (the worker stopped).
    pub stale_leases: u64,
    /// Queued again after a failed attempt.
    pub retrying: u64,
    pub percent: f64,
    /// Pages in curated batches, and those with usable text (the indexed ones).
    pub pages: u64,
    pub ok_pages: u64,
    /// Batches by the LoC version being curated (`_ver01`, `_ver02`, …).
    pub versions: BTreeMap<String, u64>,
    /// A newer version is waiting to replace a curated one.
    pub newer_versions_pending: u64,
    pub throughput: Throughput,
    pub loc: LocPacer,
    pub in_progress_batches: Vec<InProgress>,
    pub recent: Vec<Recent>,
    pub failed_batches: Vec<Failed>,
    /// The lists above are cut to their first entries.
    pub listed_limit: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Writer {
    /// A release holds the single-writer lock.
    pub held: bool,
    pub holder: Option<String>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct ReleaseProgress {
    pub index_version: String,
    pub docs_sent: u64,
    pub docs_expected: u64,
    pub percent: f64,
    pub mb_sent: f64,
    pub updated_at: DateTime<Utc>,
}

/// A run's state as the status page shows it. The pipeline records only
/// building, published and failed; a release that was stopped (a job stopped
/// by hand, a replica killed) never records an end, so a run left `building`
/// that isn't the one holding the writer is shown as stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    Building,
    Published,
    Failed,
    Stopped,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Run {
    pub index_version: String,
    pub full: bool,
    pub status: RunState,
    pub new_index: String,
    pub indexes: usize,
    pub batches: Option<u64>,
    pub docs: u64,
    pub pages: u64,
    pub duplicate_pages: u64,
    pub started_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<i64>,
    pub previous_version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Indexing {
    /// The published version as the pipeline recorded it (`ops/current`).
    pub current_version: Option<String>,
    pub writer: Writer,
    /// The release being built, with its progress, if one is running.
    pub release: Option<ReleaseProgress>,
    pub last_published_at: Option<DateTime<Utc>>,
    /// Failed runs among the recent ones, whenever they happened.
    pub failed_runs: u64,
    /// Failed runs that started after the last successful publish: the
    /// ones not yet superseded by a version that did publish.
    pub failed_since_last_publish: u64,
    /// Most recent first.
    pub runs: Vec<Run>,
}

pub fn backfill(at: DateTime<Utc>, s: &Summary) -> Backfill {
    let mut by = ByStatus::default();
    let mut versions = BTreeMap::new();
    let (mut in_progress, mut stale, mut retrying, mut newer) = (0, 0, 0, 0);
    let (mut pages, mut ok_pages) = (0, 0);
    let mut active = Vec::new();
    let mut failed = Vec::new();
    for b in &s.batches {
        *versions.entry(format!("{:02}", b.version)).or_insert(0) += 1;
        if b.curated_version.is_some_and(|v| v < b.version) {
            newer += 1;
        }
        match b.status {
            BatchStatus::Queued => {
                by.queued += 1;
                if b.attempts > 0 {
                    retrying += 1;
                }
            }
            BatchStatus::Downloading => {
                by.downloading += 1;
                let expired = b.lease.as_ref().is_none_or(|l| l.until <= at);
                if expired {
                    stale += 1;
                } else {
                    in_progress += 1;
                }
                active.push(InProgress {
                    batch: b.batch.clone(),
                    version: b.version,
                    worker: b.lease.as_ref().map(|l| short_id(&l.owner)),
                    since: b.updated_at,
                    lease_until: b.lease.as_ref().map(|l| l.until),
                    lease_expired: expired,
                    attempts: b.attempts,
                });
            }
            BatchStatus::Curated => {
                by.curated += 1;
                pages += b.curated_pages.unwrap_or(0);
                ok_pages += b.curated_ok_pages.unwrap_or(0);
            }
            BatchStatus::Failed => {
                by.failed += 1;
                failed.push(Failed {
                    batch: b.batch.clone(),
                    version: b.version,
                    attempts: b.attempts,
                    error: b.last_error.as_deref().map(sanitize),
                    updated_at: b.updated_at,
                });
            }
        }
    }
    // Every batch's last committed curation, whatever its status now (a
    // curated batch may be queued again for a newer version).
    let mut curated: Vec<(&BatchSummary, DateTime<Utc>)> = s
        .batches
        .iter()
        .filter_map(|b| b.curated_time().map(|t| (b, t)))
        .collect();
    let this_hour = at.duration_trunc(Duration::hours(1)).unwrap_or(at);
    let first_hour = this_hour - Duration::hours(HISTORY_HOURS - 1);
    let mut hours: Vec<HourBin> = (0..HISTORY_HOURS)
        .map(|i| HourBin {
            start: first_hour + Duration::hours(i),
            batches: 0,
            pages: 0,
        })
        .collect();
    let window_start = at - Duration::hours(RATE_WINDOW_HOURS);
    let mut in_window = 0u64;
    for &(b, t) in &curated {
        if t >= first_hour && t <= at {
            let i = ((t - first_hour).num_hours()).clamp(0, HISTORY_HOURS - 1) as usize;
            hours[i].batches += 1;
            hours[i].pages += b.curated_pages.unwrap_or(0);
        }
        if t > window_start && t <= at {
            in_window += 1;
        }
    }
    let rate = in_window as f64 / RATE_WINDOW_HOURS as f64;
    let remaining = by.queued + by.downloading;
    let eta = (rate > 0.0 && remaining > 0)
        .then(|| at + Duration::seconds((remaining as f64 / rate * 3600.0) as i64));

    curated.sort_by(|(a, x), (b, y)| y.cmp(x).then(a.batch.cmp(&b.batch)));
    let recent = curated
        .iter()
        .take(RECENT)
        .map(|&(b, t)| Recent {
            batch: b.batch.clone(),
            version: b.curated_version.unwrap_or(b.version),
            pages: b.curated_pages.unwrap_or(0),
            curated_at: t,
        })
        .collect();
    active.sort_by(|a, b| a.since.cmp(&b.since).then(a.batch.cmp(&b.batch)));
    active.truncate(MAX_LISTED);
    failed.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(a.batch.cmp(&b.batch)));
    failed.truncate(MAX_LISTED);

    let blocked = s.ops.pacer_blocked_until.filter(|u| *u > at);
    Backfill {
        total: s.batches.len() as u64,
        percent: percent(by.curated, s.batches.len() as u64),
        by_status: by,
        in_progress,
        stale_leases: stale,
        retrying,
        pages,
        ok_pages,
        versions,
        newer_versions_pending: newer,
        throughput: Throughput {
            hours,
            rate_window_hours: RATE_WINDOW_HOURS,
            rate_per_hour: round1(rate),
            remaining,
            eta,
        },
        loc: LocPacer {
            next_slot: s.ops.pacer_next.filter(|n| *n > at),
            blocked_until: blocked,
            throttled: blocked.is_some(),
        },
        in_progress_batches: active,
        recent,
        failed_batches: failed,
        listed_limit: MAX_LISTED,
    }
}

pub fn indexing(at: DateTime<Utc>, s: &Summary) -> Indexing {
    let writer = s
        .ops
        .writer
        .as_ref()
        .filter(|w| !w.owner.is_empty() && w.until > at);
    let mut runs: Vec<&RunSummary> = s.runs.iter().collect();
    runs.sort_by_key(|r| std::cmp::Reverse(r.started_at));
    // Only the newest run can be the one running: an older run left
    // `building` by a crashed release stays that way.
    let running = runs
        .first()
        .filter(|r| r.status == RunStatus::Building)
        .map(|r| r.index_version.as_str());
    let release = s
        .ops
        .release_progress
        .as_ref()
        .filter(|p| writer.is_some() && running == Some(p.index_version.as_str()))
        .map(|p| ReleaseProgress {
            index_version: p.index_version.clone(),
            docs_sent: p.docs_sent,
            docs_expected: p.docs_expected,
            percent: percent(p.docs_sent, p.docs_expected),
            mb_sent: p.mb_sent,
            updated_at: p.updated_at,
        });
    let last_published_at = runs.iter().filter_map(|r| r.published_at).max();
    Indexing {
        current_version: s.ops.current_version.clone(),
        writer: Writer {
            held: writer.is_some(),
            holder: writer.map(|w| short_id(&w.owner)),
            until: writer.map(|w| w.until),
        },
        release,
        last_published_at,
        failed_runs: runs
            .iter()
            .filter(|r| r.status == RunStatus::Failed)
            .count() as u64,
        failed_since_last_publish: runs
            .iter()
            .filter(|r| r.status == RunStatus::Failed)
            .filter(|r| last_published_at.is_none_or(|p| r.started_at > p))
            .count() as u64,
        runs: runs
            .iter()
            .take(MAX_RUNS)
            .map(|r| Run {
                index_version: r.index_version.clone(),
                full: r.full,
                status: match r.status {
                    RunStatus::Published => RunState::Published,
                    RunStatus::Failed => RunState::Failed,
                    // Building only while it is the newest run and a writer
                    // holds the lock; otherwise nothing is building it.
                    RunStatus::Building
                        if writer.is_some() && running == Some(r.index_version.as_str()) =>
                    {
                        RunState::Building
                    }
                    RunStatus::Building => RunState::Stopped,
                },
                new_index: r.new_index.clone(),
                indexes: r.indexes.len(),
                batches: r.batches(),
                docs: r.docs,
                pages: r.pages,
                duplicate_pages: r.duplicate_pages,
                started_at: r.started_at,
                published_at: r.published_at,
                duration_secs: r.published_at.map(|p| (p - r.started_at).num_seconds()),
                previous_version: r.previous_version.clone(),
                error: r.last_error.as_deref().map(sanitize),
            })
            .collect(),
    }
}
