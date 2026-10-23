//! The worker loop: take a page, fetch its scan, read it, write ALTO and an
//! overlay part, delete the message. Messages are hidden for a fixed time
//! when taken and never renewed (ADR-0015).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;
use opentelemetry::KeyValue;
use tokio::sync::{watch, Semaphore};
use tokio::task::JoinSet;
use tracing::field::Empty;
use tracing::Instrument;
use usnm_core::ids::PageKey;
use usnm_overlay::OverlayRow;
use usnm_store::{ObjectStore, StoreError};

use crate::engine::{exited_136, Engine, EngineError, EngineOutput, ReadInfo};
use crate::loc::{FetchError, Scan, ScanFormat};
use crate::message::{self, Cause, DeadLetter, OcrMessage};
use crate::queue::{random_token, LeaseLost, Queue, Received, BULK, DEADLETTER, PRIORITY};
use crate::status::{Completion, Tracker};
use crate::telemetry::{page_attrs, Metrics};

mod scan_source;
pub use scan_source::ScanSource;

#[derive(Debug, Clone)]
pub struct Config {
    /// Names this node in output names and logs: `[A-Za-z0-9-]`, 1 to 32.
    pub node_id: String,
    pub concurrency: usize,
    /// How long a received message stays hidden. It isn't renewed (the
    /// nodes can't update messages, ADR-0015), so it must cover the slowest page.
    pub visibility: Duration,
    /// A page not done by this share of `visibility` after it was received
    /// is abandoned without output, so it reappears cleanly for another node.
    pub abandon_at: f64,
    /// A message received more often than this goes to `ocr-deadletter`.
    pub max_dequeue: u32,
    /// The longest wait between polls when both queues are empty.
    pub idle_max: Duration,
    /// Stop once both queues are empty and nothing is in flight.
    pub once: bool,
    /// After SIGTERM, how long in-flight pages may take before they are
    /// abandoned (they reappear when their visibility runs out).
    pub shutdown_grace: Duration,
    /// Let in-flight pages run to their configured page timeout on stop.
    pub drain_on_stop: bool,
    /// The engine's page timeout, used as the planned-stop drain window.
    pub page_timeout: Duration,
    /// Optional durable local execution observations; unset leaves execution unchanged.
    pub run_metadata_dir: Option<PathBuf>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            node_id: "local".into(),
            concurrency: 1,
            visibility: Duration::from_secs(1800),
            abandon_at: 0.8,
            max_dequeue: 5,
            idle_max: Duration::from_secs(60),
            once: false,
            shutdown_grace: Duration::from_secs(60),
            drain_on_stop: false,
            page_timeout: Duration::from_secs(600),
            run_metadata_dir: None,
        }
    }
}

impl Config {
    /// How long after receiving a message its page may still write output.
    pub fn budget(&self) -> Duration {
        self.visibility.mul_f64(self.abandon_at.clamp(0.0, 1.0))
    }
}

pub fn check_node_id(id: &str) -> anyhow::Result<()> {
    if !id.is_empty()
        && id.len() <= 32
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
    {
        Ok(())
    } else {
        anyhow::bail!("--node-id must be 1 to 32 letters, digits or `-`")
    }
}

#[derive(Debug)]
enum Failure {
    /// Retrying won't help: dead-letter it.
    Permanent(String),
    /// Leave the message; it reappears when its visibility runs out. The
    /// first field is the kind, for metrics: fetch, engine, no_model,
    /// convert, output or scratch.
    Transient(&'static str, String),
    /// Out of time: no output written; the message reappears.
    Abandoned,
}

impl From<FetchError> for Failure {
    fn from(e: FetchError) -> Self {
        match e {
            FetchError::Permanent(m) => Failure::Permanent(m),
            // An item loc.gov can't serve just now: the message comes
            // back later.
            FetchError::Transient(m) | FetchError::Item(m) => Failure::Transient("fetch", m),
        }
    }
}

impl From<StoreError> for Failure {
    fn from(e: StoreError) -> Self {
        Failure::Transient("output", format!("output store: {e}"))
    }
}

/// What was written for a page.
#[derive(Debug)]
pub struct Done {
    pub run_id: String,
    pub alto: String,
    pub runs: String,
    pub part: String,
    pub chars: usize,
    pub status: &'static str,
    pub engine: String,
    /// The engine crashed on the JP2, and the IIIF JPEG was read (#454).
    pub jpeg_fallback: bool,
}

/// A scratch-file failure inside [`Worker::read_scan`], told apart from
/// the engine's own.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(crate) struct ScratchError(pub(crate) String);

/// What a page's engine read got to, for the `ocr_engine_run` event of a
/// read that writes nothing (13 §13.8.3).
#[derive(Debug, Default)]
struct ReadTrace {
    state: std::sync::Mutex<TraceState>,
    metadata: Option<crate::run_metadata::Capture>,
}

#[derive(Debug, Default)]
struct TraceState {
    /// When the engine was called; none if it wasn't, or the failure
    /// wasn't the read's (no model, scratch files).
    started: Option<Instant>,
    /// The `ocr_engine` tag and engine fields the read would have.
    planned: Option<(String, ReadInfo)>,
    /// The ones it had, once it answered.
    read: Option<(String, ReadInfo)>,
    /// The engine's status and error kind, if it failed.
    error: Option<(&'static str, &'static str)>,
}

impl ReadTrace {
    fn start(&self, planned: Option<(String, ReadInfo)>) {
        if let Some(c) = &self.metadata {
            c.start(planned.clone());
        }
        if let Ok(mut t) = self.state.lock() {
            *t = TraceState {
                started: Some(Instant::now()),
                planned,
                ..TraceState::default()
            };
        }
    }

    /// The engine fields of the instance the read got, replacing the
    /// planned ones (`describe` guessed before the read had a slot).
    fn instance(&self, info: ReadInfo) {
        if let Some(c) = &self.metadata {
            c.instance(info.clone());
        }
        if let Ok(mut t) = self.state.lock() {
            let tag = t.planned.take().map(|(tag, _)| tag).unwrap_or_default();
            t.planned = Some((tag, info));
        }
    }

    fn ended(&self, out: &Result<EngineOutput, EngineError>) {
        if let Some(c) = &self.metadata {
            c.ended(out);
        }
        let Ok(mut t) = self.state.lock() else { return };
        match out {
            Ok(o) => t.read = Some((o.engine.clone(), o.read.clone())),
            Err(EngineError::Failed(e)) if !e.is::<ScratchError>() => {
                t.error = Some(crate::engine::read_failure(e));
            }
            // Not a read: no model here, or the worker's own scratch files.
            Err(_) => t.started = None,
        }
    }

    /// The read's runs record is uploaded: it is the read's record now, so
    /// a later failure (the part's upload, or a stop while the scan is
    /// evicted) sends no event of its own.
    fn recorded(&self) {
        if let Ok(mut t) = self.state.lock() {
            t.started = None;
        }
    }
}

/// Reports a started read whose page future was dropped before it
/// finished: disarmed when the page ends normally.
struct CancelledRead<'a> {
    worker: &'a Worker,
    key: &'a PageKey,
    run_id: &'a str,
    trace: &'a ReadTrace,
    armed: bool,
}

impl Drop for CancelledRead<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.worker
                .report_failed_read(self.key, self.run_id, self.trace, None);
            if let Some(c) = &self.trace.metadata {
                c.cancelled();
            }
        }
    }
}

/// The page's failure for an engine error.
fn engine_failure(e: EngineError, node: &str) -> Failure {
    match e {
        // Another node may have the model: leave it, and let the dequeue
        // count dead-letter it if no node does.
        EngineError::Unsupported(l) => {
            Failure::Transient("no_model", format!("no model for `{l}` on node {node}"))
        }
        EngineError::Failed(e) if e.is::<ScratchError>() => {
            Failure::Transient("scratch", e.to_string())
        }
        EngineError::Failed(e) => Failure::Transient("engine", format!("{e:#}")),
    }
}

/// A page's output names in `ocr-out`, all from its `run_id`
/// (`<node>.<stamp>`, 13 §13.8.4), which is unique per node and moment
/// (the nodes can create blobs in `ocr-out` but not replace or delete
/// them, ADR-0015).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPaths {
    /// `alto/<doc_id>.<run_id>.xml`.
    pub alto: String,
    /// `runs/<issue>.<run_id>.parquet` (13 §13.8.3).
    pub runs: String,
    /// `pages/<issue>.<run_id>.parquet`.
    pub part: String,
}

pub fn output_paths(key: &PageKey, run_id: &str) -> OutputPaths {
    let issue = format!(
        "{}_{}_ed-{}",
        key.lccn,
        key.date.format("%Y-%m-%d"),
        key.edition
    );
    OutputPaths {
        alto: format!("alto/{}.{run_id}.xml", key.doc_id()),
        runs: usnm_ocr_contract::runs::path(&issue, run_id),
        part: format!("pages/{issue}.{run_id}.parquet"),
    }
}

fn fetch_dimensions(
    result: &Result<Scan, FetchError>,
    fallback: bool,
) -> (&'static str, &'static str, bool) {
    let outcome = if result.is_ok() { "ok" } else { "error" };
    let source = match result {
        Ok(scan) if scan.from_cache => "cache",
        _ => "loc",
    };
    (outcome, source, fallback)
}

fn record_image_fetch(
    metrics: &Metrics,
    started: Instant,
    result: &Result<Scan, FetchError>,
    fallback: bool,
) {
    let (outcome, source, fallback) = fetch_dimensions(result, fallback);
    metrics.image_fetch_seconds.record(
        started.elapsed().as_secs_f64(),
        &[
            KeyValue::new("outcome", outcome),
            KeyValue::new("source", source),
            KeyValue::new("jpeg_fallback", fallback),
        ],
    );
}

#[derive(Debug)]
pub struct Worker {
    pub queue: Arc<dyn Queue>,
    pub out: Arc<dyn ObjectStore>,
    pub scans: Arc<dyn ScanSource>,
    pub engine: Arc<dyn Engine>,
    pub cfg: Config,
    pub metrics: Metrics,
    /// What this worker is on and has finished, for the LAN dashboard.
    pub tracker: Arc<Tracker>,
}

/// A page in the tracker, ended as abandoned if dropped before its
/// outcome is known (the task was cancelled at shutdown).
struct Tracked {
    tracker: Arc<Tracker>,
    token: String,
    page: Option<Completion>,
}

impl Drop for Tracked {
    fn drop(&mut self) {
        if let Some(c) = self.page.take() {
            self.tracker.end(&self.token, c);
        }
    }
}

/// How a message ended, for the tracker.
struct Ended {
    outcome: &'static str,
    engine: String,
    uploaded: bool,
    error_kind: Option<&'static str>,
    jpeg_fallback: bool,
}

impl Ended {
    /// A dead letter: `dead_lettered` once it is on `ocr-deadletter` and
    /// off its work queue, else `failed` (the message comes back).
    fn dead_lettered(moved: bool, cause: &'static str) -> Self {
        if moved {
            Ended::not_done("dead_lettered", cause)
        } else {
            Ended::not_done("failed", "dead_letter")
        }
    }

    fn not_done(outcome: &'static str, error_kind: &'static str) -> Self {
        Ended {
            outcome,
            engine: String::new(),
            uploaded: false,
            error_kind: Some(error_kind),
            jpeg_fallback: false,
        }
    }
}

impl Worker {
    /// The next message, `ocr-priority` first.
    async fn next_message(&self) -> anyhow::Result<Option<Received>> {
        for q in [PRIORITY, BULK] {
            if let Some(m) = self.queue.receive(q, self.cfg.visibility).await? {
                return Ok(Some(m));
            }
        }
        Ok(None)
    }

    /// Run until `stop` turns true (or, with `once`, the queues are empty).
    /// In-flight pages get `shutdown_grace` to finish, or their page timeout
    /// when `drain_on_stop` is set; then they're abandoned and reappear when
    /// their visibility runs out.
    pub async fn run(self: Arc<Self>, mut stop: watch::Receiver<bool>) -> anyhow::Result<()> {
        if let Some(dir) = &self.cfg.run_metadata_dir {
            crate::run_metadata::prepare(dir)?;
        }
        let slots = Arc::new(Semaphore::new(self.cfg.concurrency.max(1)));
        let mut tasks = JoinSet::new();
        let mut idle = Duration::from_secs(1);
        tracing::info!(
            node = %self.cfg.node_id,
            concurrency = self.cfg.concurrency,
            visibility_secs = self.cfg.visibility.as_secs(),
            "worker started"
        );
        loop {
            while tasks.try_join_next().is_some() {}
            let permit = tokio::select! {
                p = slots.clone().acquire_owned() => p?,
                _ = stop.changed() => break,
            };
            if *stop.borrow() {
                break;
            }
            // While loc.gov holds this node's requests for more than half a
            // new page's budget, a page taken now would mostly wait, and
            // could be abandoned and spend one of its receives; take none
            // until at least half the budget is left after the hold, for
            // the fetch and the OCR.
            if let Some(until) = self.scans.held_until() {
                let resume = until.checked_sub(self.cfg.budget() / 2);
                if let Some(resume) = resume.filter(|r| *r > tokio::time::Instant::now()) {
                    drop(permit);
                    tracing::info_span!("ocr_hold").in_scope(|| {
                        tracing::warn!(
                            held_secs = until.duration_since(tokio::time::Instant::now()).as_secs(),
                            "loc.gov holds requests past a page's budget; taking no pages until it eases"
                        )
                    });
                    tokio::select! {
                        _ = tokio::time::sleep_until(resume) => {}
                        _ = stop.changed() => break,
                    }
                    continue;
                }
            }
            let next = tokio::select! {
                biased;
                // A message received as this is dropped reappears when its visibility runs out.
                _ = stop.changed() => break,
                r = self.next_message() => r,
            };
            match next {
                Ok(Some(msg)) => {
                    if *stop.borrow() {
                        drop(permit);
                        break;
                    }
                    idle = Duration::from_secs(1);
                    let me = self.clone();
                    let received = Instant::now();
                    tasks.spawn(async move {
                        me.handle(msg, received).await;
                        drop(permit);
                    });
                    continue;
                }
                Ok(None) => {
                    drop(permit);
                    self.metrics.empty_polls.add(1, &[]);
                    if self.cfg.once && tasks.is_empty() {
                        tracing::info!("queues empty; stopping (--once)");
                        break;
                    }
                }
                Err(e) => {
                    drop(permit);
                    // In a span of its own, so the warning reaches
                    // Application Insights too.
                    tracing::info_span!("ocr_receive")
                        .in_scope(|| tracing::warn!(error = format!("{e:#}"), "receive failed"));
                }
            }
            tokio::select! {
                _ = tokio::time::sleep(idle) => {}
                _ = stop.changed() => break,
            }
            idle = (idle * 2).min(self.cfg.idle_max);
        }
        if !tasks.is_empty() {
            let shutdown_grace = if self.cfg.drain_on_stop {
                self.cfg.page_timeout
            } else {
                self.cfg.shutdown_grace
            };
            tracing::info!(
                in_flight = tasks.len(),
                grace_secs = shutdown_grace.as_secs(),
                "stopping; finishing in-flight pages"
            );
            let drained = tokio::time::timeout(shutdown_grace, async {
                while tasks.join_next().await.is_some() {}
            })
            .await;
            if drained.is_err() {
                let n = tasks.len();
                self.metrics
                    .pages_abandoned
                    .add(n as u64, &[KeyValue::new("cause", "shutdown")]);
                tracing::info_span!("ocr_shutdown").in_scope(|| {
                    tracing::warn!(
                        in_flight = n,
                        "grace over; abandoning in-flight pages, which reappear when their visibility runs out"
                    )
                });
                tasks.shutdown().await;
            }
        }
        tracing::info!("worker stopped");
        Ok(())
    }

    /// Move `msg` to `ocr-deadletter`. The dead letter keeps the message
    /// only if it was valid (`valid`, re-encoded from its checked fields):
    /// a message that failed its checks might carry anything, search text
    /// included, so only its id and what was wrong with it (`kind`) are
    /// kept.
    async fn dead_letter(
        &self,
        msg: &Received,
        cause: Cause,
        kind: Option<&'static str>,
        error: &str,
        valid: Option<&OcrMessage>,
    ) -> bool {
        let body = DeadLetter::new(
            &msg.queue,
            &msg.id,
            valid,
            cause,
            kind,
            None,
            error,
            msg.dequeue_count,
            &self.cfg.node_id,
        );
        let text = body.encode();
        if let Err(e) = self.queue.send(DEADLETTER, &text).await {
            // Leave the message: it comes back and is dead-lettered again.
            tracing::error!(error = format!("{e:#}"), "dead-letter send failed");
            return false;
        }
        match self.queue.delete(msg).await {
            Ok(()) => {
                self.metrics.dead_lettered.add(
                    1,
                    &[
                        KeyValue::new("queue", msg.queue.clone()),
                        KeyValue::new("cause", cause.as_str()),
                    ],
                );
                tracing::warn!(queue = %msg.queue, message_id = %msg.id, cause = cause.as_str(), kind, error, "dead-lettered");
                true
            }
            Err(e) => {
                tracing::error!(error = format!("{e:#}"), "delete after dead-letter failed");
                false
            }
        }
    }

    /// Do one message, received at `received`, start to finish; never
    /// fails (the outcome is in the queue). One span per message (a request
    /// in Application Insights), named by its page id only.
    pub async fn handle(&self, msg: Received, received: Instant) {
        let span = tracing::info_span!(
            "ocr_page",
            otel.kind = "consumer",
            queue = %msg.queue,
            message_id = %msg.id,
            dequeue_count = msg.dequeue_count,
            page = Empty,
            language = Empty,
            reason = Empty,
            outcome = Empty,
            otel.status_code = Empty,
        );
        let page = message::decode(&msg.text)
            .ok()
            .map(|(m, k)| (k.doc_id(), m.language, m.reason.as_str().to_owned()));
        // Ends the page in the tracker however the task ends: with its
        // outcome, or as abandoned at shutdown if the task is dropped.
        // One attempt: the message as received this time (its id and pop
        // receipt), so a redelivered message running beside an earlier
        // attempt is tracked apart.
        let attempt = format!("{}/{}", msg.id, msg.pop_receipt);
        let mut guard = page.map(|(doc_id, language, reason)| {
            self.tracker.begin(&attempt, &doc_id, &language, &msg.queue);
            Tracked {
                tracker: self.tracker.clone(),
                token: attempt.clone(),
                page: Some(Completion {
                    doc_id,
                    language,
                    queue: msg.queue.clone(),
                    reason,
                    outcome: "abandoned".into(),
                    error_kind: Some("shutdown".into()),
                    ..Completion::default()
                }),
            }
        });
        let ended = self
            .handle_in(msg, &attempt, received, &span)
            .instrument(span.clone())
            .await;
        if let Some((g, mut c)) = guard
            .as_mut()
            .and_then(|g| g.page.take().map(|c| (g.token.clone(), c)))
        {
            c.engine = ended.engine;
            c.outcome = ended.outcome.into();
            c.uploaded = ended.uploaded;
            c.error_kind = ended.error_kind.map(Into::into);
            c.jpeg_fallback = ended.jpeg_fallback;
            self.tracker.end(&g, c);
        }
    }

    async fn handle_in(
        &self,
        msg: Received,
        attempt: &str,
        received: Instant,
        span: &tracing::Span,
    ) -> Ended {
        let decoded = message::decode(&msg.text);
        if let Ok((m, key)) = &decoded {
            span.record("page", tracing::field::display(key));
            span.record("language", m.language.as_str());
            span.record("reason", m.reason.as_str());
        }
        let outcome = |o: &'static str| {
            span.record("outcome", o);
            span.record("otel.status_code", if o == "done" { "ok" } else { "error" });
        };
        if msg.dequeue_count > self.cfg.max_dequeue {
            let why = format!("received {} times", msg.dequeue_count);
            let valid = decoded.as_ref().ok().map(|(m, _)| m);
            let moved = self
                .dead_letter(&msg, Cause::DequeueCount, None, &why, valid)
                .await;
            outcome("dead_lettered");
            return Ended::dead_lettered(moved, "dequeue_count");
        }
        let (m, key) = match decoded {
            Ok(v) => v,
            Err(e) => {
                let why = format!("invalid message: {}", e.kind());
                let moved = self
                    .dead_letter(&msg, Cause::Invalid, Some(e.kind()), &why, None)
                    .await;
                outcome("dead_lettered");
                return Ended::dead_lettered(moved, "invalid");
            }
        };
        let deadline = received + self.cfg.budget();
        let page = key.to_string();
        let attrs = page_attrs(
            self.engine.source(),
            &m.language,
            m.reason.as_str(),
            &msg.queue,
        );
        match self.process(&m, &key, attempt, deadline).await {
            Ok(done) => {
                let ended = Ended {
                    outcome: "done",
                    engine: done.engine.clone(),
                    uploaded: true,
                    error_kind: None,
                    jpeg_fallback: done.jpeg_fallback,
                };
                self.metrics.pages_done.add(1, &attrs);
                outcome("done");
                tracing::info!(
                    page, queue = %msg.queue, reason = m.reason.as_str(), language = %m.language,
                    engine = %done.engine, status = done.status, chars = done.chars,
                    jpeg_fallback = done.jpeg_fallback,
                    run_id = %done.run_id, alto = %done.alto, runs = %done.runs, part = %done.part,
                    secs = received.elapsed().as_secs_f64(), "page done"
                );
                if let Err(e) = self.queue.delete(&msg).await {
                    if e.downcast_ref::<LeaseLost>().is_some() {
                        tracing::warn!(
                            page,
                            "message was gone at delete; another node may redo it"
                        );
                    } else {
                        tracing::error!(
                            page,
                            error = format!("{e:#}"),
                            "delete failed; the page will be redone"
                        );
                    }
                }
                ended
            }
            Err(Failure::Permanent(e)) => {
                let moved = self
                    .dead_letter(&msg, Cause::Permanent, None, &e, Some(&m))
                    .await;
                outcome("dead_lettered");
                Ended::dead_lettered(moved, "permanent")
            }
            Err(Failure::Transient(kind, e)) => {
                let mut attrs = attrs;
                attrs.push(KeyValue::new("error", kind));
                self.metrics.pages_failed.add(1, &attrs);
                outcome("failed");
                tracing::warn!(
                    page, error = %e, kind, dequeue_count = msg.dequeue_count,
                    "page failed; it reappears when its visibility runs out"
                );
                Ended::not_done("failed", kind)
            }
            Err(Failure::Abandoned) => {
                let mut attrs = attrs;
                attrs.push(KeyValue::new("cause", "budget"));
                self.metrics.pages_abandoned.add(1, &attrs);
                outcome("abandoned");
                tracing::warn!(
                    page,
                    budget_secs = self.cfg.budget().as_secs(),
                    "page abandoned: out of time, nothing written; it reappears for another node"
                );
                Ended::not_done("abandoned", "budget")
            }
        }
    }

    async fn process(
        &self,
        m: &OcrMessage,
        key: &PageKey,
        token: &str,
        deadline: Instant,
    ) -> Result<Done, Failure> {
        // Minted before the engine call: it names the page's request to
        // an engine behind the contract, its outputs and its runs record
        // (13 §13.4.3).
        let run_id = crate::provenance::mint_run_id(&self.cfg.node_id);
        let trace = ReadTrace {
            metadata: self.cfg.run_metadata_dir.as_ref().map(|dir| {
                crate::run_metadata::Capture::new(
                    dir,
                    &run_id,
                    &key.doc_id(),
                    &self.cfg,
                    self.engine.as_ref(),
                    m,
                )
            }),
            ..ReadTrace::default()
        };
        // Reports the read if this future is dropped mid-page (the
        // shutdown grace ran out), inside the page's span: `Instrumented`
        // enters it while dropping.
        let mut cancelled = CancelledRead {
            worker: self,
            key,
            run_id: &run_id,
            trace: &trace,
            armed: true,
        };
        let result = self
            .process_page(m, key, token, deadline, &run_id, &trace)
            .await;
        cancelled.armed = false;
        if let Err(f) = &result {
            self.report_failed_read(key, &run_id, &trace, Some(f));
        }
        if let Some(c) = &trace.metadata {
            let (status, kind) = match &result {
                Ok(_) => ("ok", None),
                Err(Failure::Permanent(_)) => ("failed", Some("permanent")),
                Err(Failure::Transient(kind, _)) => ("failed", Some(*kind)),
                Err(Failure::Abandoned) => ("abandoned", Some("budget")),
            };
            c.finish(status, kind, deadline).await;
        }
        result
    }

    /// The `ocr_engine_run` event for a page that failed after its engine
    /// was called, so wrote nothing (13 §13.8.3); `None` for a page
    /// dropped at shutdown. At most one per read: the first report takes
    /// the read's start.
    fn report_failed_read(
        &self,
        key: &PageKey,
        run_id: &str,
        trace: &ReadTrace,
        f: Option<&Failure>,
    ) {
        let Ok(mut t) = trace.state.lock() else {
            return;
        };
        let Some(started) = t.started.take() else {
            return;
        };
        let (status, error_kind) = match (f, t.error) {
            (None, _) => ("abandoned", "shutdown"),
            (Some(Failure::Abandoned), _) => ("abandoned", "budget"),
            (_, Some(e)) => e,
            (Some(Failure::Transient(kind, _)), None) => ("failed", *kind),
            (Some(Failure::Permanent(_)), None) => ("failed", "permanent"),
        };
        let fallback;
        let (ocr_engine, info) = match t.read.as_ref().or(t.planned.as_ref()) {
            Some((e, i)) => (e.as_str(), i),
            None => {
                fallback = ReadInfo::local(self.engine.source());
                ("", &fallback)
            }
        };
        crate::provenance::emit_failed(&crate::provenance::FailedRead {
            run_id,
            doc_id: &key.doc_id(),
            node: &self.cfg.node_id,
            worker_version: &crate::stored::worker_version(),
            ocr_source: self.engine.source(),
            ocr_engine,
            info,
            status,
            error_kind,
            seconds_wall: started.elapsed().as_secs_f64(),
        });
    }

    async fn process_page(
        &self,
        m: &OcrMessage,
        key: &PageKey,
        token: &str,
        deadline: Instant,
        run_id: &str,
        trace: &ReadTrace,
    ) -> Result<Done, Failure> {
        let run_id = run_id.to_owned();
        let work = tempfile::tempdir()
            .map_err(|e| Failure::Transient("scratch", format!("scratch dir: {e}")))?;
        tracing::debug!(run_id, doc_id = %key.doc_id(), "page started");
        // Another node may have the model: leave the message, and let the
        // dequeue count dead-letter it if no node does. Asked before the
        // scan is fetched, so it costs loc.gov nothing.
        if !self.engine.can_read(&m.language) {
            return Err(Failure::Transient(
                "no_model",
                format!("no model for `{}` on node {}", m.language, self.cfg.node_id),
            ));
        }
        let read = async {
            let t = Instant::now();
            let scan = self.scans.scan(key, &m.batch).await;
            record_image_fetch(&self.metrics, t, &scan, false);
            let scan = scan?;
            self.tracker.scanned(token, scan.from_cache);
            self.tracker.stage(token, "ocr");
            if scan.from_cache {
                tracing::debug!(doc_id = %key.doc_id(), format = scan.format.name(), "scan read from the cache");
            }
            let mut read_secs = Instant::now();
            let first_read = Instant::now();
            let mut ocr_read_time = Duration::ZERO;
            let mut jpeg_fallback = false;
            let mut evict_jp2 = None;
            let first = self.read_scan(&scan, m, work.path(), &run_id, trace).await;
            ocr_read_time += first_read.elapsed();
            let (scan, out) = match first {
                Ok(out) => (scan, out),
                // Some intact JP2s crash the engine with status 136 (#454);
                // their IIIF JPEG reads. One more run on it; no second
                // fallback, and its failure is the page's, retried as any.
                Err(EngineError::Failed(e)) if scan.format == ScanFormat::Jp2 && exited_136(&e) => {
                    tracing::warn!(
                        doc_id = %key.doc_id(), error = format!("{e:#}"),
                        "the engine crashed on the JP2 (status 136); reading the IIIF JPEG instead"
                    );
                    let fetch_started = Instant::now();
                    let fetched = self.scans.jpeg(key, &m.batch, &scan.iiif_base).await;
                    record_image_fetch(&self.metrics, fetch_started, &fetched, true);
                    let jpeg = fetched?;
                    // The scan read is the JPEG now, wherever it came from.
                    self.tracker.scanned(token, jpeg.from_cache);
                    // A directory of its own, so nothing the crashed run
                    // left behind is read as this run's output.
                    let again = work.path().join("jpeg");
                    tokio::fs::create_dir(&again)
                        .await
                        .map_err(|e| Failure::Transient("scratch", format!("scratch dir: {e}")))?;
                    read_secs = Instant::now();
                    let retry_started = Instant::now();
                    let retry = self.read_scan(&jpeg, m, &again, &run_id, trace).await;
                    ocr_read_time += retry_started.elapsed();
                    let out = retry.map_err(|e| engine_failure(e, &self.cfg.node_id))?;
                    // The JP2's buffer copy goes with the JPEG's, after the upload.
                    evict_jp2 = scan.evict.clone();
                    jpeg_fallback = true;
                    (jpeg, out)
                }
                Err(e) => return Err(engine_failure(e, &self.cfg.node_id)),
            };
            let seconds_wall = read_secs.elapsed().as_secs_f64();
            let scan_sha256 = crate::prefetch::sha256(&scan.bytes);
            self.metrics.ocr_seconds.record(
                ocr_read_time.as_secs_f64(),
                &[
                    KeyValue::new("engine", self.engine.source().to_owned()),
                    // The short key (`tesseract/5.5.0 frak2021`), so the
                    // attribute doesn't grow with image and model digests.
                    KeyValue::new(
                        "models",
                        usnm_core::ocr_engine::EngineTag::parse(&out.engine)
                            .map_or_else(|_| out.engine.clone(), |t| t.short_key()),
                    ),
                    KeyValue::new("language", m.language.clone()),
                    KeyValue::new("jpeg_fallback", jpeg_fallback),
                ],
            );
            Ok((
                scan.iiif_base,
                scan.url,
                scan_sha256,
                scan.md5,
                scan.evict,
                out,
                jpeg_fallback,
                evict_jp2,
                seconds_wall,
            ))
        };
        let (
            iiif_base,
            scan_url,
            scan_sha256,
            scan_md5,
            evict,
            out,
            jpeg_fallback,
            evict_jp2,
            seconds_wall,
        ) = tokio::time::timeout_at(deadline.into(), read)
            .await
            .map_err(|_| Failure::Abandoned)??;
        // The stored ALTO is written from the engine's parsed ALTO, and the
        // text is derived from it in reading order (13 §13.4.7), so the two
        // hold the same words. The ALTO names the file read (the IIIF JPEG
        // or the batch's JP2) and its sha256, and a JP2's bag md5; the
        // overlay row names the page by its IIIF base either way.
        let source = crate::stored::Source {
            url: &scan_url,
            sha256: &scan_sha256,
            md5: scan_md5.as_deref(),
            run_id: Some(&run_id),
            damage_before: m.score,
        };
        let mut out = out;
        let stored = crate::stored::from_output(
            &mut out,
            &m.language,
            self.engine.dpi(),
            &source,
            &crate::stored::worker_version(),
            Utc::now(),
        )
        .map_err(|e| Failure::Transient("convert", e.to_string()))?;
        let worker_version = crate::stored::worker_version();
        let runs = crate::provenance::record(&crate::provenance::Read {
            run_id: &run_id,
            doc_id: &key.doc_id(),
            node: &self.cfg.node_id,
            worker_version: &worker_version,
            message: m,
            ocr_source: self.engine.source(),
            ocr_engine: &out.engine,
            info: &out.read,
            page: &stored.page,
            text: &stored.text,
            image_sha256: &scan_sha256,
            dpi: self.engine.dpi(),
            seconds_wall,
            finished_at: Utc::now(),
        });
        if let Some(c) = &trace.metadata {
            c.record(&runs);
        }
        let runs = usnm_ocr_contract::runs::write(std::slice::from_ref(&runs))
            .map_err(|e| Failure::Transient("output", format!("runs record: {e:#}")))?;
        let (text, alto) = (stored.text, stored.alto);
        let row = OverlayRow {
            key: key.clone(),
            batch: m.batch.clone(),
            ocr_source: self.engine.source().to_owned(),
            ocr_engine: out.engine.clone(),
            // The overlay's `loc_text` says why the page was OCRed: here,
            // the queue's reason (`bulk` or `user`).
            loc_text: m.reason.as_str().to_owned(),
            text,
            image_url: iiif_base,
            ocred_at: Utc::now(),
        };
        let status = usnm_overlay::status_str(row.status());
        let chars = row.text.chars().count();
        let part = usnm_overlay::write_part(std::slice::from_ref(&row))
            .map_err(|e| Failure::Transient("output", format!("overlay part: {e:#}")))?;
        // The uploads are cut off at the deadline too: past it the message
        // may be with another node already. An ALTO without its part is
        // harmless; parts are only read with their ALTO.
        self.tracker.stage(token, "upload");
        let paths = output_paths(key, &run_id);
        tokio::time::timeout_at(
            deadline.into(),
            self.write_output(key, &paths, alto, runs, part, trace),
        )
        .await
        .map_err(|_| Failure::Abandoned)??;
        // The output is uploaded: a buffer-set scan isn't needed any more.
        // Best effort; a failure is logged, never the page's.
        for url in evict.into_iter().chain(evict_jp2) {
            // Within the page's deadline, so a slow server can't push the
            // message past its hide.
            let _ = tokio::time::timeout_at(deadline.into(), self.scans.evict(&url)).await;
        }
        Ok(Done {
            run_id,
            alto: paths.alto,
            runs: paths.runs,
            part: paths.part,
            chars,
            status,
            engine: out.engine,
            jpeg_fallback,
        })
    }

    /// Write `scan` to the scratch dir and read it.
    async fn read_scan(
        &self,
        scan: &Scan,
        m: &OcrMessage,
        work: &Path,
        run_id: &str,
        trace: &ReadTrace,
    ) -> Result<EngineOutput, EngineError> {
        if let Some(c) = &trace.metadata {
            c.input(scan);
        }
        // The extension names the format: Tesseract (Leptonica) reads
        // JPEG and, through OpenJPEG, JP2.
        let image = work.join(format!("page.{}", scan.format.ext()));
        tokio::fs::write(&image, &scan.bytes)
            .await
            .map_err(|e| anyhow::anyhow!(ScratchError(format!("scratch image: {e}"))))?;
        trace.start(self.engine.describe(&m.language));
        let out = self
            .engine
            .read_traced(&image, &m.language, work, run_id, &|info| {
                trace.instance(info)
            })
            .await;
        trace.ended(&out);
        out
    }

    /// Write a page's ALTO, then its part, create-only under fresh names.
    async fn write_output(
        &self,
        key: &PageKey,
        paths: &OutputPaths,
        alto: String,
        runs: Vec<u8>,
        part: Vec<u8>,
        trace: &ReadTrace,
    ) -> Result<(), Failure> {
        // ALTO first, then the runs record, then the part (13 §13.8.3): a
        // part is only written once its page's ALTO and record are there.
        // All are create-only, and their names come from the page's
        // run_id, so a name already taken isn't retried under another.
        let outputs = [
            (&paths.alto, alto.into_bytes(), "application/xml"),
            (&paths.runs, runs, "application/vnd.apache.parquet"),
            (&paths.part, part, "application/vnd.apache.parquet"),
        ];
        for (path, bytes, content_type) in outputs {
            if !self.out.put_new(path, bytes, content_type).await? {
                return Err(Failure::Transient(
                    "output",
                    format!("{path} for {key} already exists ({})", random_token()),
                ));
            }
            if *path == paths.runs {
                trace.recorded();
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
