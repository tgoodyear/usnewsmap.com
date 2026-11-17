//! Per-word feedback from the page view (`POST /v1/word-feedback`, #2026).
//!
//! Stored as [`crate::ocr_requests`] stores page ids: a mark keeps only the
//! page id, the word id and the verdict. There is no query, request time
//! beyond its UTC day, address, user agent or session. Marks are shuffled
//! before they leave the process, written create-only, and a closed day is
//! shuffled again into one `days/<day>.jsonl` file (the same layout and the
//! same [`close_day`]). Queues are bounded: a full one refuses the mark.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Days, NaiveDate, Utc};
use opentelemetry::metrics::{Counter, Meter};
use opentelemetry::KeyValue;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use usnm_store::ObjectStore;

use crate::ocr_requests::{closable, close_day, shuffle, stageable, staging_path};

const FORMAT: u32 = 1;
const CONTENT_TYPE: &str = "application/x-ndjson";
const WRITE_LIMIT: Duration = Duration::from_secs(60);
const CLOSE_CHECK: Duration = Duration::from_secs(3600);
const STAGING_DAYS: u64 = 7;
const MAX_BATCH: usize = 2_000;
const MAX_PENDING: usize = 20_000;
const QUEUE: usize = 4096;

/// A reader's verdict on one word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Right,
    Wrong,
}

/// One stored mark: nothing but its version, the page, the word and the verdict.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Mark {
    pub v: u32,
    pub doc_id: String,
    pub word_id: String,
    pub verdict: Verdict,
}

impl Mark {
    /// `doc_id` is the bare LoC id, as `ocr_requests` stores it.
    pub fn new(doc_id: String, word_id: String, verdict: Verdict) -> Self {
        Self {
            v: FORMAT,
            doc_id,
            word_id,
            verdict,
        }
    }
}

#[derive(Clone)]
struct Metrics {
    marks: Counter<u64>,
    writes: Counter<u64>,
}

impl Metrics {
    fn new(meter: &Meter) -> Self {
        Self {
            marks: meter
                .u64_counter("api.word_feedback")
                .with_description("Word marks by queue/write outcome")
                .build(),
            writes: meter
                .u64_counter("api.word_feedback_writes")
                .with_description("Word feedback blob writes by operation and outcome")
                .build(),
        }
    }

    fn marks(&self, outcome: &'static str, count: usize) {
        if count > 0 {
            self.marks
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

/// A non-blocking writer for word marks.
pub struct WordFeedback {
    tx: mpsc::Sender<(NaiveDate, Mark)>,
    stop: watch::Sender<bool>,
    worker: std::sync::Mutex<Option<JoinHandle<()>>>,
    metrics: Metrics,
    /// Marks held anywhere: in the channel, pending, or in a retry batch. A
    /// mark is counted before `record` answers and released when it is
    /// written or dropped, so a full buffer is a refusal, not a silent loss.
    held: Arc<AtomicUsize>,
    capacity: usize,
}

impl std::fmt::Debug for WordFeedback {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WordFeedback").finish_non_exhaustive()
    }
}

impl WordFeedback {
    pub fn start(store: Arc<dyn ObjectStore>, meter: &Meter) -> Arc<Self> {
        let (this, worker) = Self::new(store, meter, MAX_PENDING);
        *this.worker.lock().expect("not poisoned") = Some(tokio::spawn(worker.run()));
        this
    }

    fn new(store: Arc<dyn ObjectStore>, meter: &Meter, capacity: usize) -> (Arc<Self>, Writer) {
        let (tx, rx) = mpsc::channel(QUEUE);
        let (stop, stopped) = watch::channel(false);
        let metrics = Metrics::new(meter);
        let held = Arc::new(AtomicUsize::new(0));
        let worker = Writer {
            store,
            rx,
            stopped,
            pending: Vec::new(),
            batches: Vec::new(),
            closed: BTreeSet::new(),
            metrics: metrics.clone(),
            held: held.clone(),
        };
        let this = Arc::new(Self {
            tx,
            stop,
            worker: std::sync::Mutex::new(None),
            metrics,
            held,
            capacity,
        });
        (this, worker)
    }

    /// Queue a mark without waiting for storage. `false` when the buffer
    /// (queue, pending marks and retry batches together) is full, so the
    /// caller can say the mark was not kept, as during a storage outage.
    pub fn record(&self, day: NaiveDate, mark: Mark) -> bool {
        let mut n = self.held.load(Ordering::Acquire);
        let reserved = loop {
            if n >= self.capacity {
                break false;
            }
            match self
                .held
                .compare_exchange_weak(n, n + 1, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => break true,
                Err(now) => n = now,
            }
        };
        if reserved && self.tx.try_send((day, mark)).is_ok() {
            self.metrics.marks("queued", 1);
            return true;
        }
        if reserved {
            self.held.fetch_sub(1, Ordering::AcqRel);
        }
        self.metrics.marks("dropped", 1);
        false
    }

    pub async fn shutdown(&self, limit: Duration) {
        let _ = self.stop.send(true);
        let worker = self.worker.lock().expect("not poisoned").take();
        if let Some(worker) = worker {
            if tokio::time::timeout(limit, worker).await.is_err() {
                tracing::warn!("word feedback writer: the last write didn't finish in time");
            }
        }
    }
}

struct Batch {
    day: NaiveDate,
    path: String,
    body: Vec<u8>,
    marks: usize,
}

struct Writer {
    store: Arc<dyn ObjectStore>,
    rx: mpsc::Receiver<(NaiveDate, Mark)>,
    stopped: watch::Receiver<bool>,
    pending: Vec<(NaiveDate, Mark)>,
    batches: Vec<Batch>,
    closed: BTreeSet<NaiveDate>,
    metrics: Metrics,
    held: Arc<AtomicUsize>,
}

impl Writer {
    fn release(&self, count: usize) {
        self.held.fetch_sub(count, Ordering::AcqRel);
    }

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
                item = self.rx.recv() => match item {
                    Some((day, mark)) => {
                        self.admit(day, mark);
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
        while let Ok((day, mark)) = self.rx.try_recv() {
            self.admit(day, mark);
        }
        self.flush().await;
    }

    fn admit(&mut self, day: NaiveDate, mark: Mark) {
        // `record` reserved this mark's place, so there is always room.
        self.pending.push((day, mark));
    }

    async fn flush(&mut self) {
        if self.pending.is_empty() && self.batches.is_empty() {
            return;
        }
        let mut by_day: BTreeMap<NaiveDate, Vec<Mark>> = BTreeMap::new();
        for (day, mark) in self.pending.drain(..) {
            by_day.entry(day).or_default().push(mark);
        }
        for (day, mut marks) in by_day {
            if !stageable(day, Utc::now()) || self.closed.contains(&day) {
                self.metrics.marks("lost", marks.len());
                self.release(marks.len());
                continue;
            }
            shuffle(&mut marks);
            let mut body = Vec::new();
            for mark in &marks {
                serde_json::to_writer(&mut body, mark).expect("a mark serializes");
                body.push(b'\n');
            }
            self.batches.push(Batch {
                day,
                path: staging_path(day),
                body,
                marks: marks.len(),
            });
        }

        let mut kept = Vec::new();
        for batch in std::mem::take(&mut self.batches) {
            if !stageable(batch.day, Utc::now()) || self.closed.contains(&batch.day) {
                self.metrics.marks("lost", batch.marks);
                self.release(batch.marks);
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
                    self.metrics.marks("written", batch.marks);
                    self.release(batch.marks);
                }
                Err(e) => {
                    self.metrics.write("stage", false);
                    tracing::warn!(error = %e, marks = batch.marks, "word feedback write failed; keeping the batch for retry");
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
                    tracing::warn!(error = %e, %day, "word feedback day close failed");
                }
            }
        }
        if let Some(oldest) = today.checked_sub_days(Days::new(STAGING_DAYS)) {
            self.closed.retain(|d| *d >= oldest);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;

    #[derive(Debug, Default)]
    struct SwitchStore {
        down: AtomicBool,
    }

    #[async_trait::async_trait]
    impl ObjectStore for SwitchStore {
        async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
            Ok(None)
        }
        async fn put_new(
            &self,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> Result<bool, usnm_store::StoreError> {
            if self.down.load(Ordering::Acquire) {
                Err(usnm_store::StoreError::Io("down".into()))
            } else {
                Ok(true)
            }
        }
        async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), usnm_store::StoreError> {
            Ok(())
        }
        async fn list(&self, _: &str) -> Result<Vec<String>, usnm_store::StoreError> {
            Ok(Vec::new())
        }
    }

    fn mark() -> Mark {
        Mark::new("a".into(), "w".into(), Verdict::Right)
    }

    #[tokio::test]
    async fn a_full_buffer_refuses_even_while_writes_fail_and_recovers_after() {
        let store = Arc::new(SwitchStore::default());
        store.down.store(true, Ordering::Release);
        let meter = opentelemetry::global::meter("test");
        let (feedback, mut writer) = WordFeedback::new(store.clone(), &meter, 3);
        let day = Utc::now().date_naive();
        for _ in 0..3 {
            assert!(feedback.record(day, mark()));
        }
        assert!(!feedback.record(day, mark()), "the buffer is full");
        while let Ok((d, m)) = writer.rx.try_recv() {
            writer.admit(d, m);
        }
        writer.flush().await;
        assert_eq!(
            writer.batches.len(),
            1,
            "the failed batch is kept for retry"
        );
        assert!(
            !feedback.record(day, mark()),
            "a retry batch still holds its place"
        );
        store.down.store(false, Ordering::Release);
        writer.flush().await;
        assert!(writer.batches.is_empty());
        assert!(
            feedback.record(day, mark()),
            "written marks free their places"
        );
    }

    #[test]
    fn a_mark_has_only_its_version_page_word_and_verdict() {
        let mark = Mark::new(
            "sn83045462_1890-05-03_ed-1_seq-7".into(),
            "20261102T000000Z:0:1:2".into(),
            Verdict::Wrong,
        );
        assert_eq!(
            serde_json::to_value(&mark).unwrap(),
            serde_json::json!({
                "v": 1,
                "doc_id": "sn83045462_1890-05-03_ed-1_seq-7",
                "word_id": "20261102T000000Z:0:1:2",
                "verdict": "wrong"
            })
        );
        assert!(serde_json::from_value::<Mark>(serde_json::json!({
            "v": 1, "doc_id": "a", "word_id": "b", "verdict": "right", "note": "x"
        }))
        .is_err());
    }
}
