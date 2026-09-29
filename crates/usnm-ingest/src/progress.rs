//! Release progress: a "release progress" log line every 30 s while an index
//! is built, so a long release shows how far it is, how fast it goes, and
//! whether the disk or memory is running out (Container Apps has no disk
//! metric for jobs).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use crate::sink::SinkStats;
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

    /// Log a line every [`INTERVAL`] until the returned guard is dropped.
    pub fn every(&self, interval: Duration) -> Ticker {
        let p = self.clone();
        Ticker(tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            tick.tick().await;
            loop {
                tick.tick().await;
                p.snapshot().log(None);
            }
        }))
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

    #[test]
    fn reads_vm_rss() {
        let status = "Name:\tusnm-ingest\nVmPeak:\t  900 kB\nVmRSS:\t  123456 kB\nThreads:\t9\n";
        assert_eq!(parse_vm_rss(status), Some(123_456 * 1024));
        assert_eq!(parse_vm_rss("Name:\tx\n"), None);
        assert_eq!(parse_vm_rss("VmRSS:\tlots kB\n"), None);
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
