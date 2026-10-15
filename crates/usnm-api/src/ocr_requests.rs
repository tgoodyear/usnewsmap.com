//! Search-result page ids queued for on-prem OCR (12 §12.5 Step 12).
//!
//! This is deliberately separate from the search log: it keeps no query,
//! filters, request time, or request identity. Each id is stored under only
//! its UTC day, shuffled before it leaves the process, and written create-only.

use std::collections::BTreeSet;
use std::hash::{BuildHasher, RandomState};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, Utc};
use opentelemetry::metrics::{Counter, Meter};
use opentelemetry::KeyValue;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use usnm_core::ids::PageKey;
use usnm_store::ObjectStore;

const FORMAT: u32 = 1;
const STAGE_UNTIL: Duration = Duration::from_secs(45 * 60);
const CLOSE_AFTER: Duration = Duration::from_secs(3600);
const WRITE_LIMIT: Duration = Duration::from_secs(60);
const STAGING_DAYS: u64 = 7;
const CLOSE_CHECK: Duration = Duration::from_secs(3600);
const CONTENT_TYPE: &str = "application/x-ndjson";
const MAX_BATCH: usize = 2_000;
const MAX_PENDING: usize = 20_000;
const MAX_DAY_REQUESTS: usize = 10_000;

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Request {
    v: u32,
    doc_id: String,
}

impl Request {
    fn new(doc_id: String) -> Self {
        Self { v: FORMAT, doc_id }
    }
}

#[derive(Clone)]
struct Metrics {
    requests: Counter<u64>,
    writes: Counter<u64>,
}

impl Metrics {
    fn new(meter: &Meter) -> Self {
        Self {
            requests: meter
                .u64_counter("api.ocr_requests")
                .with_description("Page ids for OCR requests by queue/write outcome")
                .build(),
            writes: meter
                .u64_counter("api.ocr_request_writes")
                .with_description("OCR request blob writes by operation and outcome")
                .build(),
        }
    }

    fn requests(&self, outcome: &'static str, count: usize) {
        if count > 0 {
            self.requests
                .add(count as u64, &[KeyValue::new("outcome", outcome)]);
        }
    }

    fn write(&self, op: &'static str, ok: bool) {
        self.writes.add(
            1,
            &[
                KeyValue::new("op", op),
                KeyValue::new("outcome", if ok { "ok" } else { "error" }),
            ],
        );
    }
}

/// A non-blocking writer for qualifying hit page ids.
pub struct OcrRequests {
    tx: mpsc::Sender<(NaiveDate, Vec<String>)>,
    stop: watch::Sender<bool>,
    worker: std::sync::Mutex<Option<JoinHandle<()>>>,
    metrics: Metrics,
}

impl std::fmt::Debug for OcrRequests {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OcrRequests").finish_non_exhaustive()
    }
}

impl OcrRequests {
    pub fn start(store: Arc<dyn ObjectStore>, meter: &Meter) -> Arc<Self> {
        let (tx, rx) = mpsc::channel(1024);
        let (stop, stopped) = watch::channel(false);
        let metrics = Metrics::new(meter);
        let worker = Writer {
            store,
            rx,
            stopped,
            pending: Vec::new(),
            batches: Vec::new(),
            closed: BTreeSet::new(),
            metrics: metrics.clone(),
        };
        Arc::new(Self {
            tx,
            stop,
            worker: std::sync::Mutex::new(Some(tokio::spawn(worker.run()))),
            metrics,
        })
    }

    /// Queue ids without waiting for storage. Invalid ids are ignored; a full
    /// queue drops this response's ids, never slowing the search response.
    pub fn record(&self, day: NaiveDate, doc_ids: impl IntoIterator<Item = String>) {
        let ids: Vec<String> = doc_ids
            .into_iter()
            .filter(|id| PageKey::from_doc_id(id).is_ok())
            .collect();
        if ids.is_empty() {
            return;
        }
        let count = ids.len();
        match self.tx.try_send((day, ids)) {
            Ok(()) => self.metrics.requests("queued", count),
            Err(_) => self.metrics.requests("dropped", count),
        }
    }

    pub async fn shutdown(&self, limit: Duration) {
        let _ = self.stop.send(true);
        let worker = self.worker.lock().expect("not poisoned").take();
        if let Some(worker) = worker {
            if tokio::time::timeout(limit, worker).await.is_err() {
                tracing::warn!("OCR request writer: the last write didn't finish in time");
            }
        }
    }
}

struct Batch {
    day: NaiveDate,
    path: String,
    body: Vec<u8>,
    requests: usize,
}

struct Writer {
    store: Arc<dyn ObjectStore>,
    rx: mpsc::Receiver<(NaiveDate, Vec<String>)>,
    stopped: watch::Receiver<bool>,
    pending: Vec<(NaiveDate, String)>,
    batches: Vec<Batch>,
    closed: BTreeSet<NaiveDate>,
    metrics: Metrics,
}

impl Writer {
    async fn run(mut self) {
        let mut flush = tokio::time::interval(Duration::from_secs(300));
        flush.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        flush.tick().await;
        let mut close = tokio::time::interval(CLOSE_CHECK);
        close.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                _ = self.stopped.changed() => break,
                batch = self.rx.recv() => match batch {
                    Some((day, ids)) => {
                        self.admit(day, ids);
                        if self.pending.len() >= MAX_BATCH {
                            self.flush().await;
                        }
                    }
                    None => break,
                },
                _ = flush.tick() => self.flush().await,
                _ = close.tick() => self.close_days(Utc::now()).await,
            }
        }
        while let Ok((day, ids)) = self.rx.try_recv() {
            self.admit(day, ids);
        }
        self.flush().await;
    }

    fn admit(&mut self, day: NaiveDate, ids: Vec<String>) {
        let retries: usize = self.batches.iter().map(|batch| batch.requests).sum();
        let capacity = MAX_PENDING.saturating_sub(retries);
        for id in ids {
            if self.pending.len() < capacity {
                self.pending.push((day, id));
            } else {
                self.metrics.requests("dropped", 1);
            }
        }
    }

    async fn flush(&mut self) {
        if self.pending.is_empty() && self.batches.is_empty() {
            return;
        }
        let mut by_day: std::collections::BTreeMap<NaiveDate, Vec<String>> =
            std::collections::BTreeMap::new();
        for (day, id) in self.pending.drain(..) {
            by_day.entry(day).or_default().push(id);
        }
        for (day, mut ids) in by_day {
            if !stageable(day, Utc::now()) || self.closed.contains(&day) {
                self.metrics.requests("lost", ids.len());
                continue;
            }
            shuffle(&mut ids);
            let mut body = Vec::new();
            for id in &ids {
                serde_json::to_writer(&mut body, &Request::new(id.clone()))
                    .expect("OCR request serializes");
                body.push(b'\n');
            }
            let path = staging_path(day);
            self.batches.push(Batch {
                day,
                path,
                body,
                requests: ids.len(),
            });
        }

        let mut kept = Vec::new();
        for batch in std::mem::take(&mut self.batches) {
            if !stageable(batch.day, Utc::now()) || self.closed.contains(&batch.day) {
                self.metrics.requests("lost", batch.requests);
                continue;
            }
            let write = self
                .store
                .put_new(&batch.path, batch.body.clone(), CONTENT_TYPE);
            let written = match tokio::time::timeout(WRITE_LIMIT, write).await {
                Ok(r) => r,
                Err(_) => Err(usnm_store::StoreError::Io(format!(
                    "put `{}`: no answer in {} s",
                    batch.path,
                    WRITE_LIMIT.as_secs()
                ))),
            };
            match written {
                Ok(_) => {
                    self.metrics.write("stage", true);
                    self.metrics.requests("written", batch.requests);
                }
                Err(e) => {
                    self.metrics.write("stage", false);
                    tracing::warn!(error = %e, requests = batch.requests, "OCR request write failed; keeping the batch for retry");
                    kept.push(batch);
                }
            }
        }
        self.batches = kept;
    }

    async fn close_days(&mut self, now: DateTime<Utc>) {
        let today = now.date_naive();
        for back in 1..STAGING_DAYS {
            let Some(day) = today.checked_sub_days(Days::new(back)) else {
                continue;
            };
            if !closable(day, now) || self.closed.contains(&day) {
                continue;
            }
            match close_day(self.store.as_ref(), day).await {
                Ok(()) => {
                    self.metrics.write("close", true);
                    self.closed.insert(day);
                }
                Err(e) => {
                    self.metrics.write("close", false);
                    tracing::warn!(error = %e, %day, "OCR request day close failed");
                }
            }
        }
        if let Some(oldest) = today.checked_sub_days(Days::new(STAGING_DAYS)) {
            self.closed.retain(|d| *d >= oldest);
        }
    }
}

pub fn staging_dir(day: NaiveDate) -> String {
    format!("staging/{day}")
}

fn staging_path(day: NaiveDate) -> String {
    let keys = RandomState::new();
    format!(
        "{}/{:016x}{:016x}.jsonl",
        staging_dir(day),
        keys.hash_one(0u8),
        keys.hash_one(1u8)
    )
}

pub fn day_path(day: NaiveDate) -> String {
    format!("days/{day}.jsonl")
}

pub fn completed_path(day: NaiveDate) -> String {
    format!("completed/{day}.json")
}

fn shuffle<T>(items: &mut [T]) {
    let keys = RandomState::new();
    for i in (1..items.len()).rev() {
        let j = (keys.hash_one(i) % (i as u64 + 1)) as usize;
        items.swap(i, j);
    }
}

fn ended(day: NaiveDate, now: DateTime<Utc>, after: Duration) -> bool {
    day.succ_opt()
        .and_then(|d| d.and_hms_opt(0, 0, 0))
        .is_some_and(|end| {
            now.signed_duration_since(end.and_utc())
                .to_std()
                .is_ok_and(|elapsed| elapsed >= after)
        })
}

pub fn stageable(day: NaiveDate, now: DateTime<Utc>) -> bool {
    !ended(day, now, STAGE_UNTIL)
}

pub fn closable(day: NaiveDate, now: DateTime<Utc>) -> bool {
    ended(day, now, CLOSE_AFTER)
}

/// Close a UTC day once, randomly ordering its staged page ids together.
pub async fn close_day(
    store: &dyn ObjectStore,
    day: NaiveDate,
) -> Result<(), usnm_store::StoreError> {
    if store.exists(&completed_path(day)).await? || store.exists(&day_path(day)).await? {
        return Ok(());
    }
    let paths = store.list(&staging_dir(day)).await?;
    let keys = RandomState::new();
    let mut lines = Vec::with_capacity(MAX_DAY_REQUESTS);
    let mut seen = 0u64;
    for path in paths {
        let Some(batch) = store.get(&path).await? else {
            continue;
        };
        for line in batch
            .split(|c| *c == b'\n')
            .filter(|line| !line.trim_ascii().is_empty())
        {
            seen = seen.saturating_add(1);
            if lines.len() < MAX_DAY_REQUESTS {
                lines.push(line.to_vec());
            } else {
                let replace = keys.hash_one(seen) % seen;
                if replace < MAX_DAY_REQUESTS as u64 {
                    lines[replace as usize] = line.to_vec();
                }
            }
        }
    }
    if lines.is_empty() {
        return Ok(());
    }
    shuffle(&mut lines);
    let mut body = Vec::new();
    for line in lines {
        body.extend_from_slice(&line);
        body.push(b'\n');
    }
    store.put_new(&day_path(day), body, CONTENT_TYPE).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use usnm_store::LocalStore;
    use usnm_store::StoreError;

    #[derive(Debug)]
    struct FailOnceStore {
        inner: LocalStore,
        fail: std::sync::atomic::AtomicBool,
        staging_read: Option<(Arc<tokio::sync::Notify>, Arc<tokio::sync::Notify>)>,
    }

    #[async_trait]
    impl ObjectStore for FailOnceStore {
        async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
            if path.starts_with("staging/") {
                if let Some((started, resume)) = &self.staging_read {
                    started.notify_one();
                    resume.notified().await;
                }
            }
            self.inner.get(path).await
        }

        async fn put_new(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<bool, StoreError> {
            if self.fail.swap(false, std::sync::atomic::Ordering::SeqCst) {
                return Err(StoreError::Io("injected write failure".into()));
            }
            self.inner.put_new(path, body, content_type).await
        }

        async fn put(
            &self,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<(), StoreError> {
            self.inner.put(path, body, content_type).await
        }

        async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
            self.inner.list(prefix).await
        }
    }

    #[test]
    fn request_record_has_only_its_version_and_page_id() {
        let record = Request::new("sn83045462_1890-05-03_ed-1_seq-7".into());
        let json = serde_json::to_value(record).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "v": FORMAT,
                "doc_id": "sn83045462_1890-05-03_ed-1_seq-7"
            })
        );
        assert!(serde_json::from_value::<Request>(serde_json::json!({
            "v": FORMAT,
            "doc_id": "sn83045462_1890-05-03_ed-1_seq-7",
            "query": "private words"
        }))
        .is_err());
    }

    #[tokio::test]
    async fn close_day_shuffles_staged_ids_without_adding_metadata() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("test-ocr-requests-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = LocalStore::new(&root);
        let day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        let a =
            serde_json::to_vec(&Request::new("sn83045462_1890-05-03_ed-1_seq-7".into())).unwrap();
        let b =
            serde_json::to_vec(&Request::new("sn83045462_1890-05-04_ed-1_seq-8".into())).unwrap();
        let mut body = a;
        body.push(b'\n');
        body.extend_from_slice(&b);
        body.push(b'\n');
        store
            .put_new(&format!("{}/a.jsonl", staging_dir(day)), body, CONTENT_TYPE)
            .await
            .unwrap();

        close_day(&store, day).await.unwrap();
        let closed = store.get(&day_path(day)).await.unwrap().unwrap();
        let rows: Vec<Request> = closed
            .split(|c| *c == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| {
            r.v == FORMAT
                && (r.doc_id == "sn83045462_1890-05-03_ed-1_seq-7"
                    || r.doc_id == "sn83045462_1890-05-04_ed-1_seq-8")
        }));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn close_day_publishes_no_more_than_the_consumer_limit() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("test-ocr-requests-limit-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = LocalStore::new(&root);
        let day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        let request =
            serde_json::to_vec(&Request::new("sn83045462_1890-05-03_ed-1_seq-7".into())).unwrap();
        let mut body = Vec::new();
        for _ in 0..=MAX_DAY_REQUESTS {
            body.extend_from_slice(&request);
            body.push(b'\n');
        }
        store
            .put_new(&format!("{}/a.jsonl", staging_dir(day)), body, CONTENT_TYPE)
            .await
            .unwrap();

        close_day(&store, day).await.unwrap();
        let closed = store.get(&day_path(day)).await.unwrap().unwrap();
        assert_eq!(
            closed.iter().filter(|byte| **byte == b'\n').count(),
            MAX_DAY_REQUESTS
        );
        assert!(closed.len() < MAX_DAY_REQUESTS * 100);
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_completed_day_is_not_republished_from_staging() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!(
                "test-ocr-requests-completed-{}",
                std::process::id()
            ));
        let _ = std::fs::remove_dir_all(&root);
        let store = LocalStore::new(&root);
        let day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        store
            .put_new(
                &completed_path(day),
                b"{\"v\":1}".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        store
            .put_new(
                &format!("{}/a.jsonl", staging_dir(day)),
                br#"{"v":1,"doc_id":"sn83045462_1890-05-03_ed-1_seq-7"}"#.to_vec(),
                CONTENT_TYPE,
            )
            .await
            .unwrap();

        close_day(&store, day).await.unwrap();
        assert!(!store.exists(&day_path(day)).await.unwrap());
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn failed_staging_batches_are_retried_without_new_ids() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("test-ocr-requests-retry-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Arc::new(FailOnceStore {
            inner: LocalStore::new(&root),
            fail: std::sync::atomic::AtomicBool::new(true),
            staging_read: None,
        });
        let day = Utc::now().date_naive();
        let (tx, rx) = mpsc::channel(1);
        drop(tx);
        let (_, stopped) = watch::channel(false);
        let mut writer = Writer {
            store: store.clone(),
            rx,
            stopped,
            pending: vec![(day, "sn83045462_1890-05-03_ed-1_seq-7".into())],
            batches: Vec::new(),
            closed: BTreeSet::new(),
            metrics: Metrics::new(&opentelemetry::global::meter("test")),
        };

        writer.flush().await;
        assert!(writer.pending.is_empty());
        assert_eq!(writer.batches.len(), 1);
        let retry_path = writer.batches[0].path.clone();
        writer.batches[0].requests = 18_000;
        writer.admit(day, vec!["sn83045462_1890-05-03_ed-1_seq-7".into(); 1_000]);
        writer.admit(day, vec!["sn83045462_1890-05-03_ed-1_seq-7".into(); 1_001]);
        assert_eq!(
            writer.pending.len(),
            2_000,
            "all free slots are used, without exceeding the cap"
        );
        writer.pending.clear();
        writer.batches[0].requests = 1;
        writer.flush().await;
        assert!(writer.batches.is_empty());
        let staged = store.list(&staging_dir(day)).await.unwrap();
        assert_eq!(staged.len(), 1);
        assert_eq!(staged[0], retry_path);
        assert_eq!(
            store
                .get(&staged[0])
                .await
                .unwrap()
                .unwrap()
                .split(|b| *b == b'\n')
                .count(),
            2
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn a_close_already_in_flight_cannot_replace_a_consumed_day_tombstone() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("test-ocr-requests-race-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let started = Arc::new(tokio::sync::Notify::new());
        let resume = Arc::new(tokio::sync::Notify::new());
        let store = Arc::new(FailOnceStore {
            inner: LocalStore::new(&root),
            fail: std::sync::atomic::AtomicBool::new(false),
            staging_read: Some((started.clone(), resume.clone())),
        });
        let day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
        store
            .put_new(
                &format!("{}/a.jsonl", staging_dir(day)),
                br#"{"v":1,"doc_id":"sn83045462_1890-05-03_ed-1_seq-7"}"#.to_vec(),
                CONTENT_TYPE,
            )
            .await
            .unwrap();
        let closer = Arc::clone(&store);
        let closing = tokio::spawn(async move { close_day(closer.as_ref(), day).await });
        started.notified().await;
        store
            .put_new(
                &completed_path(day),
                br#"{"v":1}"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        store
            .put(&day_path(day), Vec::new(), CONTENT_TYPE)
            .await
            .unwrap();
        resume.notify_one();
        closing.await.unwrap().unwrap();
        assert_eq!(store.get(&day_path(day)).await.unwrap(), Some(Vec::new()));
        let _ = std::fs::remove_dir_all(root);
    }

    #[tokio::test]
    async fn shutdown_uses_every_free_slot_without_exceeding_the_combined_buffer_bound() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("test-ocr-requests-drain-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let store = Arc::new(LocalStore::new(&root));
        let day = Utc::now().date_naive();
        let id = "sn83045462_1890-05-03_ed-1_seq-7".to_owned();
        let mut line = serde_json::to_vec(&Request::new(id.clone())).unwrap();
        line.push(b'\n');
        let (tx, rx) = mpsc::channel(1);
        tx.send((day, vec![id.clone(); 1_001])).await.unwrap();
        let (stop, stopped) = watch::channel(false);
        stop.send(true).unwrap();
        let writer = Writer {
            store: store.clone(),
            rx,
            stopped,
            pending: vec![(day, id); 1_000],
            batches: vec![Batch {
                day,
                path: staging_path(day),
                body: line.repeat(18_000),
                requests: 18_000,
            }],
            closed: BTreeSet::new(),
            metrics: Metrics::new(&opentelemetry::global::meter("test")),
        };
        writer.run().await;
        let mut count = 0;
        for path in store.list(&staging_dir(day)).await.unwrap() {
            count += store
                .get(&path)
                .await
                .unwrap()
                .unwrap()
                .iter()
                .filter(|b| **b == b'\n')
                .count();
        }
        assert_eq!(count, MAX_PENDING);
        let _ = std::fs::remove_dir_all(root);
    }
}
