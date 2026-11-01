//! Jobs and per-file state, behind [`JobStore`].

use std::collections::VecDeque;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Mutex;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// What's wanted, not how to get it.
#[derive(Debug, Clone, PartialEq)]
pub struct Job {
    /// The file's name, which also names its `.part`.
    pub name: String,
    pub size: u64,
    /// Lower-case hex. It comes from the catalogue, never from a source.
    pub sha256: String,
    pub dest: PathBuf,
    /// URLs in preference order, all for the same bytes.
    pub sources: Vec<String>,
}

/// A file a preferred source can't be used for, and why.
#[derive(Debug, Clone, PartialEq)]
pub struct HeldBack {
    /// `absent-from-mirror` or `sha256-differs`.
    pub reason: &'static str,
    pub name: String,
    pub size: u64,
    pub sha256: String,
    pub page_count: u64,
}

/// A run's progress. This is `bulk-status.json`, which the dashboard reads.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub archives: usize,
    pub done: usize,
    pub fetched_this_run: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_this_run: u64,
    pub elapsed_secs: f64,
    /// Bytes a second this run, while downloading.
    pub bytes_per_sec: f64,
    pub failed: Vec<String>,
    pub updated_at: String,
    /// Files a preferred source can't be used for.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub held_back: u64,
    /// Verification failures on record, this run's included.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub checksum_failures: u64,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
fn is_zero(n: &u64) -> bool {
    *n == 0
}

/// How one try at a job ended.
#[derive(Debug, Clone, PartialEq)]
pub enum Attempt {
    /// Placed from this URL, or found there already.
    Fetched {
        source: String,
        bytes: u64,
    },
    Failed {
        message: String,
    },
}

/// Jobs and per-file state.
#[async_trait]
pub trait JobStore: Send + Sync {
    /// The next job no worker has.
    async fn claim(&self) -> anyhow::Result<Option<Job>>;
    /// A claimed job wasn't worked on (the run stopped, or no source can be used now).
    async fn release(&self, job: &Job) -> anyhow::Result<()>;
    async fn finished(&self, job: &Job, how: &Attempt) -> anyhow::Result<()>;
    /// Replace the list of files held back from a preferred source.
    async fn held_back(&self, rows: &[HeldBack]) -> anyhow::Result<()>;
    /// Verification failures on record, across runs.
    async fn checksum_failures(&self) -> anyhow::Result<u64>;
    /// Record one; returns the new total.
    async fn checksum_failed(&self, job: &Job, detail: &str) -> anyhow::Result<u64>;
    async fn status(&self, status: &Status) -> anyhow::Result<()>;
}

/// Jobs from a list in memory, with what outlives a run in files: the finished
/// file itself is the per-file record, so a rerun plans again from the disk.
#[derive(Debug)]
pub struct FileJobs {
    pending: Mutex<VecDeque<Job>>,
    status: PathBuf,
    /// The directory for `held-back.tsv` and `checksum-failures.tsv`. Without
    /// one, verification failures aren't kept across runs.
    state: Option<PathBuf>,
}

impl FileJobs {
    pub fn new(jobs: Vec<Job>, status: PathBuf, state: Option<PathBuf>) -> Self {
        Self {
            pending: Mutex::new(jobs.into()),
            status,
            state,
        }
    }

    fn failures_file(&self) -> Option<PathBuf> {
        Some(self.state.as_ref()?.join("checksum-failures.tsv"))
    }
}

fn write_atomic(path: &std::path::Path, body: &[u8]) -> std::io::Result<()> {
    let mut tmp = path.as_os_str().to_owned();
    tmp.push(".tmp");
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

#[async_trait]
impl JobStore for FileJobs {
    async fn claim(&self) -> anyhow::Result<Option<Job>> {
        Ok(self.pending.lock().expect("jobs lock").pop_front())
    }

    async fn release(&self, _job: &Job) -> anyhow::Result<()> {
        Ok(())
    }

    async fn finished(&self, _job: &Job, _how: &Attempt) -> anyhow::Result<()> {
        Ok(())
    }

    async fn held_back(&self, rows: &[HeldBack]) -> anyhow::Result<()> {
        let Some(dir) = &self.state else {
            return Ok(());
        };
        let body: String = rows
            .iter()
            .map(|r| {
                format!(
                    "{}\t{}\t{}\t{}\t{}\n",
                    r.reason, r.name, r.size, r.sha256, r.page_count
                )
            })
            .collect();
        write_atomic(&dir.join("held-back.tsv"), body.as_bytes())?;
        Ok(())
    }

    async fn checksum_failures(&self) -> anyhow::Result<u64> {
        let Some(path) = self.failures_file() else {
            return Ok(0);
        };
        match std::fs::read_to_string(path) {
            Ok(t) => Ok(t.lines().count() as u64),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(0),
            Err(e) => Err(e.into()),
        }
    }

    async fn checksum_failed(&self, job: &Job, detail: &str) -> anyhow::Result<u64> {
        let Some(path) = self.failures_file() else {
            return Ok(0);
        };
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        let at = chrono::Utc::now().format("%FT%TZ");
        writeln!(f, "{at}\t{}\t{detail}", job.name)?;
        self.checksum_failures().await
    }

    async fn status(&self, status: &Status) -> anyhow::Result<()> {
        write_atomic(&self.status, &serde_json::to_vec_pretty(status)?)?;
        Ok(())
    }
}
