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
use tokio::time::Instant;
use usnm_core::ids::PageKey;
use usnm_state::state::State;
use usnm_store::queue::Queue;
use usnm_store::{ObjectInfo, ObjectStore};

use super::enqueue;
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
    pub completed: bool,
}

pub(super) struct Options {
    pub daily_max: usize,
    pub deadline: Option<Instant>,
}

pub(super) struct Stores<'a> {
    pub requests: &'a dyn ObjectStore,
    pub curated: &'a dyn ObjectStore,
    pub reference: &'a dyn ObjectStore,
    pub queue: &'a Arc<dyn Queue>,
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
    stores: Stores<'_>,
    lease: &Lease,
    options: Options,
    report: &mut RequestReport,
) -> anyhow::Result<()> {
    let Stores {
        requests,
        curated,
        reference,
        queue,
    } = stores;
    let completed: HashSet<String> = requests.list("completed").await?.into_iter().collect();
    let listed = requests.list_info("days").await?;
    // Complete an interrupted cleanup without spending this run's processing
    // slot. The empty object also stops an in-flight API close from recreating it.
    for info in &listed {
        if let Some(day) =
            request_day(info).filter(|day| completed.contains(&format!("completed/{day}.json")))
        {
            lease.check()?;
            clear_lookup(curated, day).await?;
            if info.size == 0 {
                continue;
            }
            requests
                .put(&info.path, Vec::new(), "application/x-ndjson")
                .await?;
        }
    }
    let mut days: Vec<(NaiveDate, ObjectInfo)> = listed
        .into_iter()
        .filter_map(|info| request_day(&info).map(|day| (day, info)))
        .filter(|(day, _)| !completed.contains(&format!("completed/{day}.json")))
        .collect();
    days.sort_by_key(|(day, info)| (*day, info.path.clone()));
    let Some((day, info)) = days.into_iter().next() else {
        return Ok(());
    };

    lease.check()?;
    report.day = Some(day);
    let completed = format!("completed/{day}.json");
    if requests.exists(&completed).await? {
        lease.check()?;
        clear_lookup(curated, day).await?;
        requests
            .put(&info.path, Vec::new(), "application/x-ndjson")
            .await?;
        report.completed = true;
        return Ok(());
    }
    let ledger = if let Some(deadline) = options.deadline {
        match tokio::time::timeout_at(deadline, ledger::load(curated)).await {
            Ok(ledger) => ledger?,
            Err(_) => return Ok(()),
        }
    } else {
        ledger::load(curated).await?
    };
    let already_queued = ledger.user_requests_for_day(day);
    let remaining = options.daily_max.saturating_sub(already_queued);
    if remaining > 0 {
        let read = requests.get_if_match(&info.path, &info.etag, MAX_REQUEST_FILE_BYTES);
        let body = if let Some(deadline) = options.deadline {
            match tokio::time::timeout_at(deadline, read).await {
                Ok(body) => body?,
                Err(_) => return Ok(()),
            }
        } else {
            read.await?
        }
        .with_context(|| format!("request file {} changed or vanished", info.path))?;
        let (ids, rejected) =
            decode(&body).with_context(|| format!("reading OCR request day {}", info.path))?;
        report.skipped += rejected;
        let queued = enqueue::enqueue_hit_requests(
            state,
            curated,
            reference,
            queue,
            lease,
            enqueue::HitRequestBatch {
                pages: ids,
                day,
                max_queued: remaining,
                deadline: options.deadline,
            },
        )
        .await;
        let queued = match queued {
            Ok(queued) => queued,
            Err(error) => {
                report.queued += error
                    .downcast_ref::<enqueue::SuccessfulSends>()
                    .map_or(0, |sent| sent.0);
                return Err(error.context(format!("queueing OCR requests for {day}")));
            }
        };
        report.queued = queued.queued;
        report.skipped += queued.skipped.values().sum::<usize>();
        if queued.paused {
            return Ok(());
        }
    }

    lease.check()?;
    if options.deadline.is_some_and(|d| Instant::now() >= d) {
        return Ok(());
    }
    requests
        .put_new(&completed, br#"{"v":1}"#.to_vec(), "application/json")
        .await?;
    clear_lookup(curated, day).await?;
    requests
        .put(&info.path, Vec::new(), "application/x-ndjson")
        .await?;
    report.completed = true;
    tracing::info!(
        day = %day,
        queued = report.queued,
        skipped = report.skipped,
        completed = report.completed,
        "OCR hit requests processed"
    );
    Ok(())
}

async fn clear_lookup(curated: &dyn ObjectStore, day: NaiveDate) -> anyhow::Result<()> {
    let path = format!("{}/requests/{day}/lookup.json", super::ROOT);
    if curated
        .get(&path)
        .await?
        .is_some_and(|body| !body.is_empty())
    {
        curated.put(&path, Vec::new(), "application/json").await?;
    }
    Ok(())
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
