//! The pipeline state as the status page reads it: one query per container
//! (`batches`, `index_runs`, `ops`), each reading only the fields it shows.
//! Large fields (a batch's part paths, an older run's inline batch list) are
//! never read.

use chrono::{DateTime, Utc};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use serde_json::Value;

use crate::docs::{DocStore, Field};
use crate::state::{
    self, Activity, BatchStatus, Lease, ReleaseProgress, RunStatus, ACTIVITY, BATCHES, FETCH_PACER,
    INDEX_RUNS, OPS, RELEASE_PROGRESS, WRITER_LOCK,
};

/// The fields of a batch item the status page uses.
pub const BATCH_FIELDS: [Field; 12] = [
    Field::Path("batch"),
    Field::Path("version"),
    Field::Path("status"),
    Field::Path("attempts"),
    Field::Path("lease"),
    Field::Path("last_error"),
    Field::Path("updated_at"),
    Field::Path("curated_at"),
    Field::Path("curated.version"),
    Field::Path("curated.pages"),
    Field::Path("curated.ok_pages"),
    Field::Path("curated.lccns"),
];

/// The fields of an index run the status page uses. The batch count is
/// `batch_count`, or for runs written before that, the inline list's length.
pub const RUN_FIELDS: [Field; 14] = [
    Field::Path("index_version"),
    Field::Path("full"),
    Field::Path("indexes"),
    Field::Path("new_index"),
    Field::Path("status"),
    Field::Path("docs"),
    Field::Path("pages"),
    Field::Path("started_at"),
    Field::Path("published_at"),
    Field::Path("previous_version"),
    Field::Path("last_error"),
    Field::Path("failed_at"),
    Field::Path("batch_count"),
    Field::Len("batches"),
];

#[derive(Debug, Clone, Deserialize)]
pub struct BatchSummary {
    pub batch: String,
    pub version: u16,
    pub status: BatchStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub lease: Option<Lease>,
    #[serde(default)]
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub curated_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub curated_version: Option<u16>,
    #[serde(default)]
    pub curated_pages: Option<u64>,
    #[serde(default)]
    pub curated_ok_pages: Option<u64>,
    #[serde(default)]
    pub curated_lccns: Vec<String>,
}

impl BatchSummary {
    /// When the batch's last curation was committed: its recorded time, or
    /// for batches curated before that was recorded, `updated_at` while the
    /// batch is still `curated` (nothing else writes a curated batch).
    pub fn curated_time(&self) -> Option<DateTime<Utc>> {
        self.curated_version?;
        self.curated_at
            .or((self.status == BatchStatus::Curated).then_some(self.updated_at))
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct RunSummary {
    pub index_version: String,
    pub full: bool,
    #[serde(default)]
    pub indexes: Vec<String>,
    #[serde(default)]
    pub new_index: String,
    pub status: RunStatus,
    #[serde(default)]
    pub docs: u64,
    #[serde(default)]
    pub pages: u64,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
    /// When the run was marked failed; absent on runs that failed before it was recorded.
    #[serde(default)]
    pub failed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub batch_count: Option<u64>,
    /// The length of an older run's inline batch list.
    #[serde(default)]
    pub batches_len: Option<u64>,
}

impl RunSummary {
    /// Batches in the version, in either item format.
    pub fn batches(&self) -> Option<u64> {
        self.batch_count.or(self.batches_len)
    }
}

/// A lock item: who holds it and until when (an empty owner is released).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LockSummary {
    pub owner: String,
    pub until: DateTime<Utc>,
}

#[derive(Debug, Clone, Default)]
pub struct OpsSummary {
    /// `ops/current`: the published version as the pipeline recorded it.
    pub current_version: Option<String>,
    pub writer: Option<LockSummary>,
    /// The LoC download pacer's next free slot.
    pub pacer_next: Option<DateTime<Utc>>,
    /// Downloads are held until then after LoC answered 429.
    pub pacer_blocked_until: Option<DateTime<Utc>>,
    pub release_progress: Option<ReleaseProgress>,
    /// `ops/activity`: the ingest job execution's step.
    pub activity: Option<Activity>,
}

#[derive(Debug, Clone, Default)]
pub struct Summary {
    pub batches: Vec<BatchSummary>,
    pub runs: Vec<RunSummary>,
    pub ops: OpsSummary,
    /// Items that didn't parse (skipped, not fatal).
    pub unreadable: usize,
    /// Request units the reads cost, where the store measures them.
    pub request_charge: Option<f64>,
}

fn parse_all<T: DeserializeOwned>(items: Vec<Value>, unreadable: &mut usize) -> Vec<T> {
    items
        .into_iter()
        .filter_map(|v| match serde_json::from_value(v) {
            Ok(t) => Some(t),
            Err(_) => {
                *unreadable += 1;
                None
            }
        })
        .collect()
}

/// Read the summary: three queries, whatever the number of items.
pub async fn read(docs: &dyn DocStore) -> anyhow::Result<Summary> {
    let charge_before = docs.request_charge();
    let (batches, runs, ops) = tokio::join!(
        docs.select(BATCHES, &BATCH_FIELDS),
        docs.select(INDEX_RUNS, &RUN_FIELDS),
        docs.list(OPS, "id", &[]),
    );
    let (batches, runs, ops) = (batches?, runs?, ops?);
    let mut unreadable = 0;
    let batches = parse_all(batches, &mut unreadable);
    let runs = parse_all(runs, &mut unreadable);
    let mut summary = OpsSummary::default();
    for item in ops {
        let doc = &item.doc;
        match doc["id"].as_str() {
            Some("current") => {
                summary.current_version = doc["index_version"].as_str().map(str::to_owned);
            }
            Some(WRITER_LOCK) => {
                let until = serde_json::from_value(doc["until"].clone()).ok();
                if let (Some(owner), Some(until)) = (doc["owner"].as_str(), until) {
                    summary.writer = Some(LockSummary {
                        owner: owner.to_owned(),
                        until,
                    });
                }
            }
            Some(FETCH_PACER) => {
                let (next, blocked) = state::pacer(Some(&item));
                summary.pacer_next = next;
                summary.pacer_blocked_until = blocked;
            }
            Some(RELEASE_PROGRESS) => match serde_json::from_value(doc.clone()) {
                Ok(p) => summary.release_progress = Some(p),
                Err(_) => unreadable += 1,
            },
            Some(ACTIVITY) => match serde_json::from_value(doc.clone()) {
                Ok(a) => summary.activity = Some(a),
                Err(_) => unreadable += 1,
            },
            _ => {}
        }
    }
    let request_charge = match (charge_before, docs.request_charge()) {
        (Some(a), Some(b)) => Some(b - a),
        _ => None,
    };
    Ok(Summary {
        batches,
        runs,
        ops: summary,
        unreadable,
        request_charge,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::MemoryDocs;
    use crate::state::State;
    use serde_json::json;
    use std::sync::Arc;

    #[tokio::test]
    async fn reads_batches_runs_and_ops() {
        let docs = Arc::new(MemoryDocs::default());
        let s = State::new(docs.clone());
        docs.upsert(
            BATCHES,
            "b1",
            &json!({
                "id": "b1", "batch": "b1", "version": 1, "source_url": "https://x",
                "ocr_source": "o", "status": "curated", "attempts": 1,
                "updated_at": "2026-09-29T10:00:00Z",
                "curated": {"version": 1, "parts": ["p1", "p2"], "counts": "c", "pages": 10,
                            "ok_pages": 9, "lccns": ["sn1"], "source_sha256": "s",
                            "first": "1890-01-01", "last": "1890-12-31"},
            }),
        )
        .await
        .unwrap();
        docs.upsert(BATCHES, "junk", &json!({"id": "junk", "status": 3}))
            .await
            .unwrap();
        docs.upsert(
            INDEX_RUNS,
            "v1",
            &json!({
                "id": "v1", "index_version": "v1", "full": true, "indexes": ["i1"],
                "new_index": "i1", "batches": [{"batch": "b1"}], "status": "published",
                "docs": 9, "pages": 10, "started_at": "2026-09-29T11:00:00Z",
                "published_at": "2026-09-29T11:30:00Z",
            }),
        )
        .await
        .unwrap();
        // A run written since the batch list moved to the reference snapshot.
        docs.upsert(
            INDEX_RUNS,
            "v2",
            &json!({
                "id": "v2", "index_version": "v2", "full": false, "indexes": ["i1", "i2"],
                "new_index": "i2", "batch_count": 3, "batch_list": "v2/batches.json",
                "status": "published", "docs": 12, "pages": 14,
                "started_at": "2026-09-30T11:00:00Z", "published_at": "2026-09-30T11:30:00Z",
            }),
        )
        .await
        .unwrap();
        s.set_current_version("v1").await.unwrap();
        s.lock(WRITER_LOCK, "host-1-0000abcd", chrono::Duration::hours(1))
            .await
            .unwrap();
        s.block_fetches(Utc::now() + chrono::Duration::hours(1))
            .await
            .unwrap();
        s.set_release_progress(&ReleaseProgress {
            index_version: "v2".into(),
            docs_sent: 5,
            docs_expected: 10,
            mb_sent: 1.5,
            updated_at: Utc::now(),
        })
        .await
        .unwrap();

        let now = Utc::now();
        s.set_activity(&Activity {
            command: "run".into(),
            owner: "host-1-0000abcd".into(),
            started_at: now,
            step: crate::state::Step::Titles,
            step_started_at: now,
            updated_at: now,
            done: Some(342),
            total: Some(3464),
            paused_until: Some(now + chrono::Duration::minutes(65)),
            index_version: None,
            merge: None,
            ended_at: None,
            outcome: None,
            error: None,
        })
        .await
        .unwrap();

        let got = read(docs.as_ref()).await.unwrap();
        assert_eq!(got.batches.len(), 1);
        assert_eq!(got.unreadable, 1);
        let b = &got.batches[0];
        assert_eq!(
            (b.curated_pages, b.curated_ok_pages, &b.curated_lccns[..]),
            (Some(10), Some(9), &["sn1".to_owned()][..])
        );
        // Both item formats give the batch count.
        let count = |v: &str| {
            got.runs
                .iter()
                .find(|r| r.index_version == v)
                .unwrap()
                .batches()
        };
        assert_eq!((count("v1"), count("v2")), (Some(1), Some(3)));
        assert_eq!(got.ops.current_version.as_deref(), Some("v1"));
        assert_eq!(got.ops.writer.as_ref().unwrap().owner, "host-1-0000abcd");
        assert!(got.ops.pacer_blocked_until.is_some());
        assert_eq!(got.ops.release_progress.as_ref().unwrap().docs_sent, 5);
        let a = got.ops.activity.as_ref().unwrap();
        assert_eq!(
            (a.step, a.done, a.total),
            (crate::state::Step::Titles, Some(342), Some(3464))
        );
        assert!(a.paused_until.is_some());
        assert_eq!(got.request_charge, None);
    }
}
