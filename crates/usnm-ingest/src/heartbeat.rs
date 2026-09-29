//! What a curate worker is doing with the batch it holds (08 §8.1.2).
//!
//! A batch takes minutes, and a worker hung partway through one would log
//! nothing, the same as a busy one. So while a worker holds a batch, a
//! separate task logs a `curate progress` line every [`INTERVAL`]: the stage
//! it is in, how long it has been there, and the bytes, pages and parts so
//! far. The task reads shared atomics, so an await that never returns can't
//! stop it from reporting the stage the batch is stuck in, and it stops when
//! its [`Heartbeat`] guard is dropped at the end of the batch.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use tokio::time::Instant;

/// Time between `curate progress` lines. The per-replica silence alert
/// fires after 15 minutes without a line (`infra/modules/ingest-alerts.bicep`).
pub const INTERVAL: Duration = Duration::from_secs(60);

/// Where a batch is, in order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Stage {
    /// Claimed; waiting for a download slot from the shared pacer.
    WaitingForSlot = 0,
    /// Requesting the archive.
    Connecting = 1,
    /// Downloading, parsing and uploading parts, which overlap (the archive
    /// is streamed), then checking the sha256.
    Downloading = 2,
    /// Writing the page counts and issue items.
    Issues = 3,
    /// The conditional write that points the batch at this attempt.
    Committing = 4,
}

impl Stage {
    const ALL: [Stage; 5] = [
        Stage::WaitingForSlot,
        Stage::Connecting,
        Stage::Downloading,
        Stage::Issues,
        Stage::Committing,
    ];

    /// The `stage` field of the log lines.
    pub fn as_str(self) -> &'static str {
        match self {
            Stage::WaitingForSlot => "waiting_for_slot",
            Stage::Connecting => "connecting",
            Stage::Downloading => "downloading",
            Stage::Issues => "issues",
            Stage::Committing => "committing",
        }
    }
}

impl std::fmt::Display for Stage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// One batch's progress, shared by the worker and its heartbeat.
#[derive(Debug)]
pub struct BatchProgress {
    started: Instant,
    /// The stage in the low 8 bits, and the milliseconds after `started`
    /// when it began above them: one atomic, so a snapshot never pairs one
    /// stage with another's start.
    stage: AtomicU64,
    /// The download's own byte counter, once it has started.
    archive_bytes: OnceLock<Arc<AtomicU64>>,
    pages: AtomicU64,
    parts: AtomicU64,
}

impl BatchProgress {
    /// A batch just claimed, waiting for its download slot.
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            started: Instant::now(),
            stage: AtomicU64::new(Stage::WaitingForSlot as u64),
            archive_bytes: OnceLock::new(),
            pages: AtomicU64::new(0),
            parts: AtomicU64::new(0),
        })
    }

    pub fn set_stage(&self, stage: Stage) {
        // 56 bits of milliseconds is over two million years.
        let ms = u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX >> 8);
        self.stage.store(
            (ms.min(u64::MAX >> 8) << 8) | stage as u64,
            Ordering::Relaxed,
        );
    }

    pub fn stage(&self) -> Stage {
        self.stage_and_start().0
    }

    /// The current stage and when it began.
    fn stage_and_start(&self) -> (Stage, Duration) {
        let packed = self.stage.load(Ordering::Relaxed);
        let stage = Stage::ALL
            .get(usize::from(packed as u8))
            .copied()
            .unwrap_or(Stage::WaitingForSlot);
        (stage, Duration::from_millis(packed >> 8))
    }

    /// Report the bytes of this download as it runs.
    pub fn track_download(&self, bytes: Arc<AtomicU64>) {
        let _ = self.archive_bytes.set(bytes);
    }

    /// A page was parsed.
    pub fn page(&self) {
        self.pages.fetch_add(1, Ordering::Relaxed);
    }

    /// A Parquet part was uploaded.
    pub fn part(&self) {
        self.parts.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> Snapshot {
        let elapsed = self.started.elapsed();
        let (stage, stage_at) = self.stage_and_start();
        Snapshot {
            stage,
            elapsed,
            in_stage: elapsed.saturating_sub(stage_at),
            archive_bytes: self
                .archive_bytes
                .get()
                .map_or(0, |b| b.load(Ordering::Relaxed)),
            pages: self.pages.load(Ordering::Relaxed),
            parts: self.parts.load(Ordering::Relaxed),
        }
    }
}

/// What one `curate progress` line reports.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Snapshot {
    pub stage: Stage,
    /// Since the batch was claimed.
    pub elapsed: Duration,
    /// Since the current stage began.
    pub in_stage: Duration,
    pub archive_bytes: u64,
    pub pages: u64,
    pub parts: u64,
}

impl Snapshot {
    pub fn archive_mb(&self) -> f64 {
        mb(self.archive_bytes)
    }

    /// Emit the `curate progress` line (the text the silence alert and the
    /// `curate-replicas` query look for).
    pub fn log(&self, batch: &str) {
        tracing::info!(
            batch,
            stage = self.stage.as_str(),
            elapsed_secs = self.elapsed.as_secs(),
            stage_secs = self.in_stage.as_secs(),
            archive_mb = self.archive_mb(),
            pages = self.pages,
            parts = self.parts,
            "curate progress"
        );
    }
}

/// Bytes to MB, to one decimal.
pub fn mb(bytes: u64) -> f64 {
    (bytes as f64 / 104_857.6).round() / 10.0
}

/// Calls `emit` with a snapshot every `interval` (the first one `interval`
/// after the start) until dropped.
pub struct Heartbeat(tokio::task::JoinHandle<()>);

impl Heartbeat {
    pub fn start(
        progress: Arc<BatchProgress>,
        interval: Duration,
        emit: impl Fn(Snapshot) + Send + 'static,
    ) -> Self {
        Self(tokio::spawn(async move {
            let mut tick = tokio::time::interval_at(Instant::now() + interval, interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                emit(progress.snapshot());
            }
        }))
    }
}

impl Drop for Heartbeat {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    fn recorder() -> (
        Arc<Mutex<Vec<Snapshot>>>,
        impl Fn(Snapshot) + Send + 'static,
    ) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let sink = seen.clone();
        (seen, move |s| sink.lock().unwrap().push(s))
    }

    #[tokio::test(start_paused = true)]
    async fn beats_every_interval_with_the_current_stage_and_counts() {
        let p = BatchProgress::new();
        let (seen, emit) = recorder();
        let beat = Heartbeat::start(p.clone(), INTERVAL, emit);
        tokio::time::sleep(Duration::from_secs(59)).await;
        assert!(
            seen.lock().unwrap().is_empty(),
            "no line before the first interval"
        );
        tokio::time::sleep(Duration::from_secs(2)).await;
        p.set_stage(Stage::Downloading);
        p.track_download(Arc::new(AtomicU64::new(3 * 1024 * 1024)));
        for _ in 0..7 {
            p.page();
        }
        p.part();
        tokio::time::sleep(Duration::from_secs(60)).await;
        drop(beat);
        let seen = seen.lock().unwrap().clone();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].stage, Stage::WaitingForSlot);
        assert_eq!(seen[0].elapsed.as_secs(), 60);
        assert_eq!((seen[0].pages, seen[0].archive_bytes), (0, 0));
        let s = seen[1];
        assert_eq!(s.stage, Stage::Downloading);
        assert_eq!((s.elapsed.as_secs(), s.in_stage.as_secs()), (120, 59));
        assert_eq!((s.pages, s.parts, s.archive_mb()), (7, 1, 3.0));
    }

    #[tokio::test(start_paused = true)]
    async fn keeps_reporting_the_stage_an_await_is_stuck_in_and_stops_when_dropped() {
        let p = BatchProgress::new();
        let (seen, emit) = recorder();
        let beat = Heartbeat::start(p.clone(), INTERVAL, emit);
        p.set_stage(Stage::Issues);
        // An await that never returns, cut short the way the worker's
        // watchdog does it.
        let stuck = tokio::time::timeout(
            Duration::from_secs(5 * 60 + 1),
            std::future::pending::<()>(),
        )
        .await;
        assert!(stuck.is_err());
        drop(beat);
        let n = seen.lock().unwrap().len();
        assert_eq!(n, 5);
        assert!(seen
            .lock()
            .unwrap()
            .iter()
            .all(|s| s.stage == Stage::Issues));
        assert_eq!(seen.lock().unwrap()[4].in_stage.as_secs(), 300);
        // Nothing after the guard is dropped.
        tokio::time::sleep(Duration::from_secs(10 * 60)).await;
        assert_eq!(seen.lock().unwrap().len(), n);
    }

    #[test]
    fn stages_round_trip_through_the_atomic() {
        let p = BatchProgress::new();
        for s in Stage::ALL {
            p.set_stage(s);
            assert_eq!(p.stage(), s);
        }
        assert_eq!(Stage::WaitingForSlot.to_string(), "waiting_for_slot");
        assert_eq!(mb(1_048_576 + 104_858), 1.1);
    }

    #[tokio::test(start_paused = true)]
    async fn a_stage_and_its_start_are_stored_together() {
        let p = BatchProgress::new();
        tokio::time::sleep(Duration::from_millis(90_500)).await;
        p.set_stage(Stage::Committing);
        assert_eq!(
            p.stage_and_start(),
            (Stage::Committing, Duration::from_millis(90_500))
        );
        tokio::time::sleep(Duration::from_secs(10)).await;
        let s = p.snapshot();
        assert_eq!((s.stage, s.in_stage.as_secs()), (Stage::Committing, 10));
    }
}
