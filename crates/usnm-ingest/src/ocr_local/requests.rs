//! Consume daily page-id requests written by the API (12 §12.5 Step 12).
//!
//! Request files contain no query or request metadata. This worker validates
//! and deduplicates ids, applies the daily queue cap and damage threshold,
//! then reuses the ledger-first priority queue path used by `ocr-enqueue`.

use std::collections::HashSet;
use std::sync::Arc;

use anyhow::Context;
use chrono::NaiveDate;
use serde::Deserialize;
use usnm_core::ids::PageKey;
use usnm_state::state::State;
use usnm_store::queue::Queue;
use usnm_store::{ObjectInfo, ObjectStore};

use super::enqueue::{self, QueueReport};
use super::ledger;
use super::sync::Lease;

const FORMAT: u32 = 1;
const MAX_REQUEST_FILE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_REQUEST_IDS: usize = enqueue::MAX_PAGES;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    v: u32,
    doc_id: String,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RequestReport {
    pub day: Option<NaiveDate>,
    pub queued: usize,
    pub skipped: usize,
    pub deleted: bool,
}

fn request_day(info: &ObjectInfo) -> Option<NaiveDate> {
    let name = info.path.strip_prefix("days/")?.strip_suffix(".jsonl")?;
    NaiveDate::parse_from_str(name, "%Y-%m-%d").ok()
}

fn decode(body: &[u8]) -> anyhow::Result<(Vec<String>, usize)> {
    let mut ids = Vec::new();
    let mut seen = HashSet::new();
    let mut rejected = 0;
    for (i, line) in body
        .split(|c| *c == b'\n')
        .filter(|line| !line.is_empty())
        .enumerate()
    {
        if i >= MAX_REQUEST_IDS {
            rejected += 1;
            continue;
        }
        let request = match serde_json::from_slice::<Request>(line) {
            Ok(request) if request.v == FORMAT => request,
            _ => {
                rejected += 1;
                continue;
            }
        };
        if PageKey::from_doc_id(&request.doc_id).is_err() {
            rejected += 1;
            continue;
        }
        if seen.insert(request.doc_id.clone()) {
            ids.push(request.doc_id);
        }
    }
    Ok((ids, rejected))
}

/// Process at most one UTC day per run so a backlog cannot bypass the
/// per-day cap and flood the priority queue after an outage.
pub(super) async fn process(
    state: &State,
    requests: &dyn ObjectStore,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    lease: &Lease,
    daily_max: usize,
) -> anyhow::Result<RequestReport> {
    let mut days: Vec<(NaiveDate, ObjectInfo)> = requests
        .list_info("days")
        .await?
        .into_iter()
        .filter_map(|info| request_day(&info).map(|day| (day, info)))
        .collect();
    days.sort_by_key(|(day, info)| (*day, info.path.clone()));
    let Some((day, info)) = days.into_iter().next() else {
        return Ok(RequestReport::default());
    };

    lease.check()?;
    let mut report = RequestReport {
        day: Some(day),
        ..RequestReport::default()
    };
    let completed = format!("completed/{day}.json");
    if requests.exists(&completed).await? {
        lease.check()?;
        report.deleted = requests.delete_if_match(&info.path, &info.etag).await?;
        return Ok(report);
    }
    let ledger = ledger::load(curated).await?;
    let already_queued = ledger.user_requests_for_day(day);
    let remaining = daily_max.saturating_sub(already_queued);
    if remaining > 0 {
        let body = requests
            .get_if_match(&info.path, &info.etag, MAX_REQUEST_FILE_BYTES)
            .await?
            .with_context(|| format!("request file {} changed or vanished", info.path))?;
        let (ids, rejected) =
            decode(&body).with_context(|| format!("reading OCR request day {}", info.path))?;
        report.skipped += rejected;
        let queued: QueueReport = enqueue::enqueue_hit_requests(
            state,
            curated,
            reference,
            queue,
            lease,
            enqueue::HitRequestBatch {
                pages: ids,
                day,
                max_queued: remaining,
            },
        )
        .await
        .with_context(|| format!("queueing OCR requests for {day}"))?;
        report.queued = queued.queued;
        report.skipped += queued.skipped.values().sum::<usize>();
    }

    lease.check()?;
    requests
        .put_new(&completed, br#"{"v":1}"#.to_vec(), "application/json")
        .await?;
    report.deleted = requests.delete_if_match(&info.path, &info.etag).await?;
    tracing::info!(
        day = %day,
        queued = report.queued,
        skipped = report.skipped,
        deleted = report.deleted,
        "OCR hit requests processed"
    );
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_ids_and_deduplicates_without_accepting_other_fields() {
        let good = "sn83045462_1890-05-03_ed-1_seq-7";
        let mut body = Vec::new();
        for value in [
            json!({"v": FORMAT, "doc_id": good}),
            json!({"v": FORMAT, "doc_id": good}),
            json!({"v": FORMAT, "doc_id": "not-a-page"}),
            json!({"v": FORMAT, "doc_id": good, "query": "private words"}),
            json!({"v": 2, "doc_id": good}),
        ] {
            serde_json::to_writer(&mut body, &value).unwrap();
            body.push(b'\n');
        }
        let (ids, rejected) = decode(&body).unwrap();
        assert_eq!(ids, [good]);
        assert_eq!(rejected, 3);
    }

    #[test]
    fn only_closed_day_object_paths_are_accepted() {
        assert_eq!(
            request_day(&ObjectInfo {
                path: "days/2026-10-14.jsonl".into(),
                size: 0,
                etag: String::new(),
                created: None,
            }),
            Some(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap())
        );
        for path in [
            "staging/2026-10-14/id.jsonl",
            "days/nope.jsonl",
            "days/2026-10-14.txt",
        ] {
            assert!(request_day(&ObjectInfo {
                path: path.into(),
                size: 0,
                etag: String::new(),
                created: None,
            })
            .is_none());
        }
    }

    #[test]
    fn request_day_rows_are_bounded_before_enqueue() {
        let line = serde_json::to_vec(&json!({
            "v": FORMAT,
            "doc_id": "sn83045462_1890-05-03_ed-1_seq-7"
        }))
        .unwrap();
        let mut body = Vec::new();
        for _ in 0..MAX_REQUEST_IDS + 1 {
            body.extend_from_slice(&line);
            body.push(b'\n');
        }
        let (ids, rejected) = decode(&body).unwrap();
        assert_eq!(ids.len(), 1);
        assert_eq!(rejected, 1);
    }
}
