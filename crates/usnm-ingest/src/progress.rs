//! Release progress: a "release progress" log line every 30 s while an index
//! is built, so a long release shows how far it is, how fast it goes, and
//! whether the disk or memory is running out (Container Apps has no disk
//! metric for jobs). The same counts go to the pipeline state
//! (`ops/release-progress`) for the public status page.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use chrono::Utc;

use crate::sink::SinkStats;
use crate::state::{ReleaseProgress, State};
use crate::telemetry;

pub const INTERVAL: Duration = Duration::from_secs(30);

/// Free bytes for unprivileged users on the file system holding `path`.
// statvfs field types differ by platform (u64 on Linux, u32 on macOS).
#[allow(clippy::useless_conversion)]
pub fn disk_free(path: &Path) -> Option<u64> {
    let s = rustix::fs::statvfs(path).ok()?;
    Some(u64::from(s.f_bavail).saturating_mul(u64::from(s.f_frsize)))
}

/// Resident memory of this process (`None`) or of `pid`, from
/// `/proc/<pid>/status` (Linux only; `None` elsewhere).
pub fn rss_bytes(pid: Option<u32>) -> Option<u64> {
    if !cfg!(target_os = "linux") {
        return None;
    }
    let path = match pid {
        Some(pid) => format!("/proc/{pid}/status"),
        None => "/proc/self/status".into(),
    };
    parse_vm_rss(&std::fs::read_to_string(path).ok()?)
}

/// `VmRSS:     12345 kB` → bytes.
fn parse_vm_rss(status: &str) -> Option<u64> {
    let line = status.lines().find(|l| l.starts_with("VmRSS:"))?;
    let mut parts = line["VmRSS:".len()..].split_whitespace();
    let n: u64 = parts.next()?.parse().ok()?;
    match parts.next() {
        Some("kB") | None => n.checked_mul(1024),
        _ => None,
    }
}

/// The container's memory limit and out-of-memory kills so far, from its
/// cgroup (v2, then v1). `None` where there is no cgroup to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CgroupMemory {
    pub limit: Option<u64>,
    pub oom_kills: Option<u64>,
}

pub fn cgroup_memory() -> Option<CgroupMemory> {
    let read = |p: &str| std::fs::read_to_string(p).ok();
    let (limit, events) = match read("/sys/fs/cgroup/memory.events") {
        Some(events) => (read("/sys/fs/cgroup/memory.max"), events),
        None => (
            read("/sys/fs/cgroup/memory/memory.limit_in_bytes"),
            read("/sys/fs/cgroup/memory/memory.oom_control")?,
        ),
    };
    Some(CgroupMemory {
        limit: limit.as_deref().and_then(parse_memory_limit),
        oom_kills: parse_oom_kills(&events),
    })
}

/// `memory.max` or `memory.limit_in_bytes`: bytes, or none for "max" or
/// v1's "no limit" (a number near `i64::MAX`).
fn parse_memory_limit(text: &str) -> Option<u64> {
    let n: u64 = text.trim().parse().ok()?;
    (n < 1 << 60).then_some(n)
}

/// The `oom_kill N` line of `memory.events` (v2) or `memory.oom_control` (v1).
fn parse_oom_kills(text: &str) -> Option<u64> {
    text.lines()
        .find_map(|l| l.strip_prefix("oom_kill ")?.trim().parse().ok())
}

/// What one progress line reports.
#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub docs_sent: u64,
    pub docs_expected: u64,
    pub bytes_sent: u64,
    pub retries_429: u64,
    pub retries_503: u64,
    pub elapsed: Duration,
    pub disk_free: Option<u64>,
    pub rss: Option<u64>,
    pub quickwit_rss: Option<u64>,
}

const MB: f64 = 1024.0 * 1024.0;

impl Snapshot {
    pub fn percent(&self) -> f64 {
        if self.docs_expected == 0 {
            return 0.0;
        }
        round1(100.0 * self.docs_sent as f64 / self.docs_expected as f64)
    }

    pub fn docs_per_sec(&self) -> f64 {
        let secs = self.elapsed.as_secs_f64();
        if secs <= 0.0 {
            return 0.0;
        }
        round1(self.docs_sent as f64 / secs)
    }

    pub fn mb_sent(&self) -> f64 {
        round1(self.bytes_sent as f64 / MB)
    }

    /// Emit the "release progress" line (the text the saved queries and the
    /// stall alert look for) and update the gauges.
    pub fn log(&self, batch: Option<&str>) {
        let mb = |b: Option<u64>| b.map(|b| round1(b as f64 / MB));
        tracing::info!(
            batch,
            docs_sent = self.docs_sent,
            docs_expected = self.docs_expected,
            percent = self.percent(),
            mb_sent = self.mb_sent(),
            docs_per_sec = self.docs_per_sec(),
            elapsed_secs = self.elapsed.as_secs(),
            retries_429 = self.retries_429,
            retries_503 = self.retries_503,
            disk_free_mb = mb(self.disk_free),
            rss_mb = mb(self.rss),
            quickwit_rss_mb = mb(self.quickwit_rss),
            "release progress"
        );
        let m = telemetry::metrics();
        m.docs_expected.record(self.docs_expected, &[]);
        if let Some(free) = self.disk_free {
            m.disk_free.record(free, &[]);
        }
    }
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// Tracks one index build.
#[derive(Clone)]
pub struct Progress {
    stats: Arc<SinkStats>,
    docs_expected: u64,
    started: Instant,
    /// The sink's counts when this build started (a sink may be reused).
    base: (u64, u64),
}

impl Progress {
    pub fn new(stats: Arc<SinkStats>, docs_expected: u64) -> Self {
        Self {
            base: (stats.docs_sent(), stats.bytes_sent()),
            stats,
            docs_expected,
            started: Instant::now(),
        }
    }

    pub fn snapshot(&self) -> Snapshot {
        let s = &self.stats;
        let work_dir: Option<&PathBuf> = s.work_dir.as_ref();
        Snapshot {
            docs_sent: s.docs_sent().saturating_sub(self.base.0),
            docs_expected: self.docs_expected,
            bytes_sent: s.bytes_sent().saturating_sub(self.base.1),
            retries_429: s.retries_429(),
            retries_503: s.retries_503(),
            elapsed: self.started.elapsed(),
            disk_free: work_dir.and_then(|d| disk_free(d)),
            rss: rss_bytes(None),
            quickwit_rss: s.node_pid.and_then(|p| rss_bytes(Some(p))),
        }
    }

    /// Log a line every `interval` until the returned guard is dropped, in
    /// the caller's span (a spawned task doesn't inherit it).
    pub fn every(&self, interval: Duration) -> Ticker {
        use tracing::Instrument;
        let p = self.clone();
        Ticker(tokio::spawn(
            async move {
                let mut tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await;
                loop {
                    tick.tick().await;
                    p.snapshot().log(None);
                }
            }
            .instrument(tracing::Span::current()),
        ))
    }
}

impl Progress {
    /// Record progress in the pipeline state every `interval` until the
    /// returned guard is dropped (the caller records the first and last). A failed write is logged and skipped:
    /// the status page may lag, but the release never stops for it.
    pub fn report_every(&self, interval: Duration, state: State, version: String) -> Ticker {
        use tracing::Instrument;
        let p = self.clone();
        Ticker(tokio::spawn(
            async move {
                let mut tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await;
                loop {
                    tick.tick().await;
                    report(&state, &version, &p.snapshot()).await;
                }
            }
            .instrument(tracing::Span::current()),
        ))
    }
}

/// Longest a progress write may take: the release awaits the first and last
/// writes, and Cosmos retries 429s for minutes.
const REPORT_TIMEOUT: Duration = Duration::from_secs(10);

/// Write one snapshot to `ops/release-progress`; errors and timeouts are
/// logged only. (A write cut short is harmless: it is an idempotent upsert
/// of an item nothing else depends on.)
pub async fn report(state: &State, version: &str, s: &Snapshot) {
    report_within(state, version, s, REPORT_TIMEOUT).await
}

async fn report_within(state: &State, version: &str, s: &Snapshot, limit: Duration) {
    let p = ReleaseProgress {
        index_version: version.to_owned(),
        docs_sent: s.docs_sent,
        docs_expected: s.docs_expected,
        mb_sent: s.mb_sent(),
        updated_at: Utc::now(),
    };
    match tokio::time::timeout(limit, state.set_release_progress(&p)).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => {
            tracing::warn!(error = %format!("{e:#}"), "could not record release progress; continuing")
        }
        Err(_) => tracing::warn!("recording release progress timed out; continuing"),
    }
}

/// Stops the periodic progress lines when dropped.
pub struct Ticker(tokio::task::JoinHandle<()>);

impl Drop for Ticker {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A store whose every operation fails, like Cosmos while it's unreachable.
    struct Broken;

    #[async_trait::async_trait]
    impl crate::docs::DocStore for Broken {
        async fn get(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<Option<crate::docs::Versioned>> {
            anyhow::bail!("unreachable")
        }
        async fn create(
            &self,
            _: &str,
            _: &str,
            _: &serde_json::Value,
        ) -> anyhow::Result<Option<String>> {
            anyhow::bail!("unreachable")
        }
        async fn replace(
            &self,
            _: &str,
            _: &str,
            _: &serde_json::Value,
            _: &str,
        ) -> anyhow::Result<Option<String>> {
            anyhow::bail!("unreachable")
        }
        async fn upsert(&self, _: &str, _: &str, _: &serde_json::Value) -> anyhow::Result<()> {
            anyhow::bail!("unreachable")
        }
        async fn list(
            &self,
            _: &str,
            _: &str,
            _: &[&str],
        ) -> anyhow::Result<Vec<crate::docs::Versioned>> {
            anyhow::bail!("unreachable")
        }
    }

    #[tokio::test]
    async fn progress_reaches_the_pipeline_state() {
        let docs = Arc::new(crate::docs::MemoryDocs::default());
        let stats = Arc::new(SinkStats::default());
        let p = Progress::new(stats, 100);
        let ticker = p.report_every(
            Duration::from_millis(10),
            State::new(docs.clone()),
            "v1".into(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        drop(ticker);
        let item = crate::docs::DocStore::get(
            docs.as_ref(),
            "ops",
            "release-progress",
            "release-progress",
        )
        .await
        .unwrap()
        .unwrap();
        assert_eq!(item.doc["index_version"], "v1");
        assert_eq!(item.doc["docs_expected"], 100);
        assert_eq!(item.doc["kind"], "release-progress");
    }

    /// A store whose writes never finish, like Cosmos retrying 429s.
    struct Stuck;

    #[async_trait::async_trait]
    impl crate::docs::DocStore for Stuck {
        async fn get(
            &self,
            _: &str,
            _: &str,
            _: &str,
        ) -> anyhow::Result<Option<crate::docs::Versioned>> {
            Ok(None)
        }
        async fn create(
            &self,
            _: &str,
            _: &str,
            _: &serde_json::Value,
        ) -> anyhow::Result<Option<String>> {
            std::future::pending().await
        }
        async fn replace(
            &self,
            _: &str,
            _: &str,
            _: &serde_json::Value,
            _: &str,
        ) -> anyhow::Result<Option<String>> {
            std::future::pending().await
        }
        async fn upsert(&self, _: &str, _: &str, _: &serde_json::Value) -> anyhow::Result<()> {
            std::future::pending().await
        }
        async fn list(
            &self,
            _: &str,
            _: &str,
            _: &[&str],
        ) -> anyhow::Result<Vec<crate::docs::Versioned>> {
            Ok(vec![])
        }
    }

    #[tokio::test]
    async fn a_stuck_store_delays_a_report_by_the_timeout_at_most() {
        let p = Progress::new(Arc::new(SinkStats::default()), 10);
        let started = Instant::now();
        let limit = Duration::from_millis(50);
        report_within(&State::new(Arc::new(Stuck)), "v1", &p.snapshot(), limit).await;
        assert!(started.elapsed() >= limit && started.elapsed() < limit * 20);
    }

    #[tokio::test]
    async fn a_failing_store_only_logs() {
        let p = Progress::new(Arc::new(SinkStats::default()), 10);
        // Returns normally, and the ticker keeps running through failures.
        report(&State::new(Arc::new(Broken)), "v1", &p.snapshot()).await;
        let ticker = p.report_every(
            Duration::from_millis(5),
            State::new(Arc::new(Broken)),
            "v1".into(),
        );
        tokio::time::sleep(Duration::from_millis(30)).await;
        assert!(!ticker.0.is_finished());
    }

    #[test]
    fn reads_vm_rss() {
        let status = "Name:\tusnm-ingest\nVmPeak:\t  900 kB\nVmRSS:\t  123456 kB\nThreads:\t9\n";
        assert_eq!(parse_vm_rss(status), Some(123_456 * 1024));
        assert_eq!(parse_vm_rss("Name:\tx\n"), None);
        assert_eq!(parse_vm_rss("VmRSS:\tlots kB\n"), None);
    }

    #[test]
    fn reads_the_cgroup_memory_files() {
        let v2 = "low 0\nhigh 0\nmax 12\noom 1\noom_kill 1\noom_group_kill 0\n";
        assert_eq!(parse_oom_kills(v2), Some(1));
        let v1 = "oom_kill_disable 0\nunder_oom 0\noom_kill 0\n";
        assert_eq!(parse_oom_kills(v1), Some(0));
        assert_eq!(parse_oom_kills("low 0\n"), None);
        assert_eq!(parse_memory_limit("8053063680\n"), Some(8_053_063_680));
        assert_eq!(parse_memory_limit("max\n"), None);
        assert_eq!(parse_memory_limit("9223372036854771712\n"), None);
        assert_eq!(cgroup_memory().is_some(), {
            Path::new("/sys/fs/cgroup/memory.events").exists()
                || Path::new("/sys/fs/cgroup/memory/memory.oom_control").exists()
        });
    }

    #[test]
    fn rates_and_percentages() {
        let s = Snapshot {
            docs_sent: 1500,
            docs_expected: 6000,
            bytes_sent: 3 * 1024 * 1024 + 512 * 1024,
            retries_429: 1,
            retries_503: 2,
            elapsed: Duration::from_secs(60),
            disk_free: None,
            rss: None,
            quickwit_rss: None,
        };
        assert_eq!(s.percent(), 25.0);
        assert_eq!(s.docs_per_sec(), 25.0);
        assert_eq!(s.mb_sent(), 3.5);
        let empty = Snapshot {
            docs_expected: 0,
            elapsed: Duration::ZERO,
            ..s
        };
        assert_eq!(empty.percent(), 0.0);
        assert_eq!(empty.docs_per_sec(), 0.0);
    }

    #[test]
    fn free_space_of_an_existing_directory() {
        let dir = tempfile::tempdir().unwrap();
        assert!(disk_free(dir.path()).is_some_and(|b| b > 0));
        assert_eq!(disk_free(Path::new("/nonexistent/usnm")), None);
    }

    #[test]
    fn own_memory_on_linux() {
        assert_eq!(rss_bytes(None).is_some(), cfg!(target_os = "linux"));
    }
}
