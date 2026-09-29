//! The status document's sections, built from the published reference data
//! and a [`Summary`] of the pipeline state. Pure functions of their inputs,
//! so they are tested without a database.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use chrono::{DateTime, Duration, DurationRound, Utc};
use serde::Serialize;
use usnm_state::state::{BatchStatus, RunStatus, MAX_DELTAS};
use usnm_state::summary::{BatchSummary, RunSummary, Summary};

use super::sanitize::{sanitize, short_id};
use crate::refdata::RefData;

/// Hours of curation history in the throughput chart.
pub const HISTORY_HOURS: i64 = 48;
/// Hours the curation rate (and so the ETA) is averaged over.
pub const RATE_WINDOW_HOURS: i64 = 12;
const RECENT: usize = 20;
const MAX_LISTED: usize = 100;
const MAX_RUNS: usize = 20;

/// A section that needs the pipeline state: its data, or why there is none.
#[derive(Debug, Clone, Serialize)]
pub struct Section<T> {
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub data: Option<T>,
}

impl<T> Section<T> {
    pub fn of(data: T) -> Self {
        Self {
            available: true,
            reason: None,
            data: Some(data),
        }
    }

    pub fn unavailable(reason: &str) -> Self {
        Self {
            available: false,
            reason: Some(reason.to_owned()),
            data: None,
        }
    }
}

/// The version the API is serving, from its reference data (always available).
#[derive(Debug, Clone, Serialize)]
pub struct Published {
    pub index_version: String,
    pub published_at: String,
    pub synthetic: bool,
    pub pages: u64,
    pub titles: usize,
    pub places: usize,
    pub bounds: crate::refdata::Bounds,
    /// Base index first, then its deltas.
    pub indexes: Vec<String>,
    pub deltas: usize,
    /// Deltas a version may carry; the release after that many is a full rebuild.
    pub max_deltas: usize,
    pub next_release_full: bool,
    /// Batches the version was built from, when its snapshot records them.
    pub batches: Option<usize>,
}

pub fn published(rd: &RefData) -> Published {
    let c = &rd.current;
    let deltas = c.indexes.len().saturating_sub(1);
    Published {
        index_version: c.index_version.clone(),
        published_at: c.published_at.clone(),
        synthetic: c.synthetic,
        pages: rd.pages,
        titles: rd.titles.len(),
        places: rd.places.len(),
        bounds: c.bounds.clone(),
        indexes: c.indexes.clone(),
        deltas,
        max_deltas: MAX_DELTAS,
        next_release_full: deltas >= MAX_DELTAS,
        batches: rd.published_batches.as_ref().map(HashMap::len),
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct ByStatus {
    pub queued: u64,
    pub downloading: u64,
    pub curated: u64,
    pub failed: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct HourBin {
    pub start: DateTime<Utc>,
    pub batches: u64,
    pub pages: u64,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct LocPacer {
    /// The next free bulk-download slot, shared by every worker.
    pub next_slot: Option<DateTime<Utc>>,
    /// Downloads are held until then (LoC answered 429).
    pub blocked_until: Option<DateTime<Utc>>,
    pub throttled: bool,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct Recent {
    pub batch: String,
    pub version: u16,
    pub pages: u64,
    pub curated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Failed {
    pub batch: String,
    pub version: u16,
    pub attempts: u32,
    /// Sanitized and shortened.
    pub error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
pub struct Writer {
    /// A release holds the single-writer lock.
    pub held: bool,
    pub holder: Option<String>,
    pub until: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReleaseProgress {
    pub index_version: String,
    pub docs_sent: u64,
    pub docs_expected: u64,
    pub percent: f64,
    pub mb_sent: f64,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Run {
    pub index_version: String,
    pub full: bool,
    pub status: RunStatus,
    pub new_index: String,
    pub indexes: usize,
    pub batches: Option<u64>,
    pub docs: u64,
    pub pages: u64,
    pub started_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub duration_secs: Option<i64>,
    pub previous_version: Option<String>,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Indexing {
    /// The published version as the pipeline recorded it (`ops/current`).
    pub current_version: Option<String>,
    pub writer: Writer,
    /// The release being built, with its progress, if one is running.
    pub release: Option<ReleaseProgress>,
    pub last_published_at: Option<DateTime<Utc>>,
    pub failed_runs: u64,
    /// Most recent first.
    pub runs: Vec<Run>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Catalog {
    pub titles: usize,
    pub places: usize,
}

#[derive(Debug, Clone, Serialize)]
pub struct TitlesPipeline {
    /// Titles with pages in curated batches.
    pub curated_titles: usize,
    /// Of those, titles the catalog doesn't have yet (they wait for titles-sync).
    pub awaiting_sync: Option<usize>,
    /// Curated batches with at least one such title: releases skip them.
    pub batches_waiting_for_titles: Option<usize>,
    /// Curated batches (at their curated version) not in the published version.
    pub unpublished_batches: Option<usize>,
    /// Batches the next release can take: unpublished ones whose titles are
    /// all catalogued, plus re-curated ones when it will be a full release.
    pub ready_for_release: Option<usize>,
    /// Published batches curated again at a newer version: only a full release takes them.
    pub recurated_awaiting_full: Option<usize>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Titles {
    /// `catalog/titles.json` and `places.json` (maintained by titles-sync and geocode).
    pub catalog: Section<Catalog>,
    /// Titles and places in the published version.
    pub published_titles: usize,
    pub published_places: usize,
    pub pipeline: Section<TitlesPipeline>,
}

/// Titles in the catalog (for the sync counts).
pub struct CatalogTitles {
    pub lccns: HashSet<String>,
    pub places: usize,
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

fn percent(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        round1(100.0 * part as f64 / whole as f64)
    }
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
    Indexing {
        current_version: s.ops.current_version.clone(),
        writer: Writer {
            held: writer.is_some(),
            holder: writer.map(|w| short_id(&w.owner)),
            until: writer.map(|w| w.until),
        },
        release,
        last_published_at: runs.iter().filter_map(|r| r.published_at).max(),
        failed_runs: runs
            .iter()
            .filter(|r| r.status == RunStatus::Failed)
            .count() as u64,
        runs: runs
            .iter()
            .take(MAX_RUNS)
            .map(|r| Run {
                index_version: r.index_version.clone(),
                full: r.full,
                status: r.status,
                new_index: r.new_index.clone(),
                indexes: r.indexes.len(),
                batches: r.batches_len,
                docs: r.docs,
                pages: r.pages,
                started_at: r.started_at,
                published_at: r.published_at,
                duration_secs: r.published_at.map(|p| (p - r.started_at).num_seconds()),
                previous_version: r.previous_version.clone(),
                error: r.last_error.as_deref().map(sanitize),
            })
            .collect(),
    }
}

pub fn titles(
    rd: &RefData,
    catalog: Option<&CatalogTitles>,
    catalog_reason: &str,
    pipeline: Option<&Summary>,
    pipeline_reason: &str,
) -> Titles {
    let pipeline = match pipeline {
        None => Section::unavailable(pipeline_reason),
        Some(s) => Section::of(titles_pipeline(rd, catalog, s)),
    };
    Titles {
        catalog: match catalog {
            Some(c) => Section::of(Catalog {
                titles: c.lccns.len(),
                places: c.places,
            }),
            None => Section::unavailable(catalog_reason),
        },
        published_titles: rd.titles.len(),
        published_places: rd.places.len(),
        pipeline,
    }
}

fn titles_pipeline(rd: &RefData, catalog: Option<&CatalogTitles>, s: &Summary) -> TitlesPipeline {
    let curated: Vec<&BatchSummary> = s
        .batches
        .iter()
        .filter(|b| b.curated_version.is_some())
        .collect();
    let curated_titles: BTreeSet<&str> = curated
        .iter()
        .flat_map(|b| b.curated_lccns.iter().map(String::as_str))
        .collect();
    let missing =
        |b: &BatchSummary, c: &CatalogTitles| b.curated_lccns.iter().any(|l| !c.lccns.contains(l));
    let awaiting_sync = catalog.map(|c| {
        curated_titles
            .iter()
            .filter(|l| !c.lccns.contains(**l))
            .count()
    });
    let waiting = catalog.map(|c| curated.iter().filter(|b| missing(b, c)).count());
    let published = rd.published_batches.as_ref();
    let next_full = rd.current.indexes.len() > MAX_DELTAS;
    let unpublished: Option<Vec<&&BatchSummary>> = published.map(|p| {
        curated
            .iter()
            .filter(|b| !p.contains_key(&b.batch))
            .collect()
    });
    TitlesPipeline {
        curated_titles: curated_titles.len(),
        awaiting_sync,
        batches_waiting_for_titles: waiting,
        // A full release (after MAX_DELTAS deltas) also takes re-curated batches.
        ready_for_release: match (published, catalog) {
            (Some(p), Some(c)) => Some(
                curated
                    .iter()
                    .filter(|b| match p.get(&b.batch) {
                        None => true,
                        Some(v) => next_full && Some(*v) != b.curated_version,
                    })
                    .filter(|b| !missing(b, c))
                    .count(),
            ),
            _ => None,
        },
        unpublished_batches: unpublished.as_ref().map(Vec::len),
        recurated_awaiting_full: published.map(|p| {
            curated
                .iter()
                .filter(|b| {
                    p.get(&b.batch)
                        .is_some_and(|v| Some(*v) != b.curated_version)
                })
                .count()
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use usnm_state::summary::{BatchSummary, LockSummary, RunSummary};

    fn at() -> DateTime<Utc> {
        "2026-09-29T12:30:00Z".parse().unwrap()
    }

    fn batch(v: Value) -> BatchSummary {
        serde_json::from_value(v).unwrap()
    }

    fn curated(name: &str, hours_ago: i64, pages: u64, lccns: &[&str]) -> BatchSummary {
        batch(json!({
            "batch": name, "version": 1, "status": "curated", "attempts": 1,
            "updated_at": at() - Duration::minutes(hours_ago * 60 + 1),
            "curated_version": 1, "curated_pages": pages, "curated_ok_pages": pages - 1,
            "curated_lccns": lccns,
        }))
    }

    fn summary() -> Summary {
        Summary {
            batches: vec![
                curated("batch_a", 0, 10, &["sn1"]),
                curated("batch_b", 1, 20, &["sn1", "sn2"]),
                curated("batch_c", 30, 5, &["sn3"]),
                curated("batch_old", 100, 7, &["sn1"]),
                batch(json!({"batch": "batch_q", "version": 1, "status": "queued",
                         "updated_at": at()})),
                batch(
                    json!({"batch": "batch_r", "version": 2, "status": "queued", "attempts": 2,
                         "last_error": "GET https://chroniclingamerica.loc.gov/x returned 503",
                         "updated_at": at(), "curated_version": 1, "curated_pages": 3,
                         "curated_at": at() - Duration::hours(30),
                         "curated_lccns": ["sn1"]}),
                ),
                batch(
                    json!({"batch": "batch_d", "version": 1, "status": "downloading",
                         "attempts": 1, "updated_at": at() - Duration::minutes(5),
                         "lease": {"owner": "caj-usnm-backfill-x-12-0a1b2c3d",
                                   "until": at() + Duration::hours(1)}}),
                ),
                batch(
                    json!({"batch": "batch_e", "version": 1, "status": "downloading",
                         "attempts": 3, "updated_at": at() - Duration::hours(5),
                         "lease": {"owner": "w-1-deadbeef", "until": at() - Duration::hours(1)}}),
                ),
                batch(
                    json!({"batch": "batch_f", "version": 1, "status": "failed", "attempts": 5,
                         "last_error": "put https://stusnmdata.blob.core.windows.net/curated/x: 403 from 10.0.0.4",
                         "updated_at": at() - Duration::hours(2)}),
                ),
            ],
            ..Summary::default()
        }
    }

    #[test]
    fn backfill_counts_rates_and_lists() {
        let mut s = summary();
        s.ops.pacer_blocked_until = Some(at() + Duration::minutes(40));
        s.ops.pacer_next = Some(at() - Duration::minutes(1));
        let b = backfill(at(), &s);
        assert_eq!(b.total, 9);
        assert_eq!(
            (
                b.by_status.queued,
                b.by_status.downloading,
                b.by_status.curated,
                b.by_status.failed
            ),
            (2, 2, 4, 1)
        );
        assert_eq!((b.in_progress, b.stale_leases, b.retrying), (1, 1, 1));
        assert_eq!(b.percent, 44.4);
        assert_eq!((b.pages, b.ok_pages), (42, 38));
        assert_eq!(b.versions.get("01"), Some(&8));
        assert_eq!(b.versions.get("02"), Some(&1));
        assert_eq!(b.newer_versions_pending, 1);
        // 48 hourly bins ending with this hour; batch_old is older than that.
        let t = &b.throughput;
        assert_eq!(t.hours.len(), 48);
        assert_eq!(
            t.hours[47].start,
            "2026-09-29T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        // batch_r's curation (queued again for a newer version) still counts.
        assert_eq!(t.hours.iter().map(|h| h.batches).sum::<u64>(), 4);
        assert_eq!(t.hours.iter().map(|h| h.pages).sum::<u64>(), 38);
        // Two batches in the last 12 hours: 2/12 per hour, 4 remaining → 24 h.
        assert_eq!(t.rate_per_hour, 0.2);
        assert_eq!(t.remaining, 4);
        assert_eq!(t.eta, Some(at() + Duration::hours(24)));
        assert!(b.loc.throttled);
        assert_eq!(b.loc.next_slot, None);
        // Workers appear only as short ids; the stopped one is flagged.
        let ids: Vec<_> = b
            .in_progress_batches
            .iter()
            .map(|a| (a.batch.as_str(), a.worker.as_deref(), a.lease_expired))
            .collect();
        assert_eq!(
            ids,
            [
                ("batch_e", Some("adbeef"), true),
                ("batch_d", Some("1b2c3d"), false)
            ]
        );
        assert_eq!(b.recent[0].batch, "batch_a");
        assert_eq!(b.recent.len(), 5);
        let err = b.failed_batches[0].error.as_deref().unwrap();
        assert_eq!(err, "put [url]: 403 from [ip]");
        let text = serde_json::to_string(&b).unwrap();
        assert!(!text.contains("caj-usnm"), "{text}");
        assert!(!text.contains("blob.core"), "{text}");
    }

    #[test]
    fn no_rate_means_no_eta() {
        let mut s = summary();
        s.batches.retain(|b| b.status != BatchStatus::Curated);
        let b = backfill(at(), &s);
        assert_eq!((b.throughput.rate_per_hour, b.throughput.eta), (0.0, None));
        assert_eq!(b.percent, 0.0);
        assert!(!b.loc.throttled);
    }

    fn run(v: Value) -> RunSummary {
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn indexing_runs_writer_and_live_progress() {
        let mut s = Summary {
            runs: vec![
                run(
                    json!({"index_version": "v1", "full": true, "indexes": ["b1"], "new_index": "b1",
                       "status": "published", "docs": 9, "pages": 10, "batches_len": 2,
                       "started_at": "2026-09-28T10:00:00Z", "published_at": "2026-09-28T11:00:00Z"}),
                ),
                run(
                    json!({"index_version": "v2", "full": false, "indexes": ["b1", "d1"], "new_index": "d1",
                       "status": "failed", "started_at": "2026-09-29T01:00:00Z",
                       "last_error": "lock `quickwit-writer` is held by `ca-x-1-0a1b2c3d` until later"}),
                ),
                run(
                    json!({"index_version": "v3", "full": false, "indexes": ["b1", "d2"], "new_index": "d2",
                       "status": "building", "started_at": "2026-09-29T12:00:00Z"}),
                ),
            ],
            ..Summary::default()
        };
        s.ops.current_version = Some("v1".into());
        s.ops.writer = Some(LockSummary {
            owner: "caj-usnm-ingest-9-0badf00d".into(),
            until: at() + Duration::minutes(20),
        });
        s.ops.release_progress = Some(usnm_state::state::ReleaseProgress {
            index_version: "v3".into(),
            docs_sent: 250,
            docs_expected: 1000,
            mb_sent: 12.5,
            updated_at: at(),
        });
        let i = indexing(at(), &s);
        assert_eq!(
            i.runs
                .iter()
                .map(|r| r.index_version.as_str())
                .collect::<Vec<_>>(),
            ["v3", "v2", "v1"]
        );
        assert_eq!(i.runs[2].duration_secs, Some(3600));
        assert_eq!(i.runs[2].batches, Some(2));
        assert_eq!(
            i.runs[1].error.as_deref(),
            Some("lock `quickwit-writer` is held by `[worker]` until later")
        );
        assert_eq!(i.failed_runs, 1);
        assert!(i.writer.held);
        assert_eq!(i.writer.holder.as_deref(), Some("adf00d"));
        assert_eq!(i.release.as_ref().unwrap().percent, 25.0);
        assert_eq!(
            i.last_published_at,
            Some("2026-09-28T11:00:00Z".parse().unwrap())
        );

        // An older run left `building` by a crash isn't the running one.
        let mut crashed = s.clone();
        crashed.runs.push(run(
            json!({"index_version": "v4", "full": false, "indexes": ["b1", "d3"],
            "new_index": "d3", "status": "failed", "started_at": "2026-09-29T12:20:00Z"}),
        ));
        assert!(indexing(at(), &crashed).release.is_none());

        // Progress from a release that is no longer running isn't shown.
        s.ops.writer.as_mut().unwrap().until = at() - Duration::minutes(1);
        let i = indexing(at(), &s);
        assert!(!i.writer.held && i.release.is_none() && i.writer.holder.is_none());
    }

    async fn fixture_refdata() -> RefData {
        let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data");
        RefData::load(&usnm_store::LocalStore::new(dir))
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn published_and_titles_sync() {
        let mut rd = fixture_refdata().await;
        let p = published(&rd);
        assert_eq!((p.deltas, p.max_deltas, p.next_release_full), (1, 8, false));
        assert_eq!(p.batches, None);

        let s = summary();
        let catalog = CatalogTitles {
            lccns: ["sn1", "sn2"].iter().map(|s| s.to_string()).collect(),
            places: 2,
        };
        // Without the published batch list, only the sync counts.
        let t = titles(&rd, Some(&catalog), "", Some(&s), "");
        let tp = t.pipeline.data.as_ref().unwrap();
        assert_eq!(tp.curated_titles, 3);
        assert_eq!(tp.awaiting_sync, Some(1));
        assert_eq!(tp.batches_waiting_for_titles, Some(1));
        assert_eq!(tp.unpublished_batches, None);

        rd.published_batches = Some(
            [("batch_a".to_owned(), 1), ("batch_r".to_owned(), 0)]
                .into_iter()
                .collect(),
        );
        let t = titles(&rd, Some(&catalog), "", Some(&s), "");
        let tp = t.pipeline.data.as_ref().unwrap();
        // b, c and old aren't published, and c waits for sn3; r was
        // published at another version than it is curated at now.
        assert_eq!(tp.unpublished_batches, Some(3));
        assert_eq!(tp.ready_for_release, Some(2));
        assert_eq!(tp.recurated_awaiting_full, Some(1));
        assert_eq!(t.catalog.data.as_ref().unwrap().titles, 2);
        // After 8 deltas the next release is full and takes batch_r too.
        rd.current.indexes = (0..9).map(|i| format!("i{i}")).collect();
        let t = titles(&rd, Some(&catalog), "", Some(&s), "");
        assert_eq!(t.pipeline.data.as_ref().unwrap().ready_for_release, Some(3));

        let t = titles(&rd, None, "no catalog", None, "no pipeline");
        assert!(!t.catalog.available && !t.pipeline.available);
        let v = serde_json::to_value(&t).unwrap();
        assert_eq!(
            v["pipeline"],
            json!({"available": false, "reason": "no pipeline"})
        );
    }
}
