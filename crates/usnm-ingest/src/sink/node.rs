//! The Quickwit writer node as a child process: started, supervised, restarted for
//! ingest v1's final merges, and stopped (08 §8.4.1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use super::WriterTuning;
use crate::merges::NodeEvents;
use crate::progress;
use crate::telemetry;

/// Per-shard ingest rate on the writer node: Quickwit's maximum (its default
/// is 5 MB/s). A single-node writer can't spread load over more shards, so
/// past the ~50 MiB burst allowance ingest runs at this rate.
const SHARD_THROUGHPUT_LIMIT: &str = "20MB";

/// The writer's local cache of uploaded splits (Quickwit's default is 100 GiB).
/// A job replica has ~19.5 GiB of disk: the first prod release filled it,
/// the ingester closed its shards, and every request got 503 "no shards
/// available" until the release gave up.
const SPLIT_STORE_BYTES: &str = "4GiB";
/// The ingest write-ahead log's disk cap (Quickwit's default, set here so
/// the scratch disk budget in 08 §8.4 adds up): past it, ingest gets 429s.
const WAL_DISK_BYTES: &str = "4GiB";
/// One merge at a time (the default is two thirds of the CPUs): the largest
/// merge needs its inputs and its output on disk at once, about twice
/// `split_num_docs_target` pages (08 §8.4).
const MERGE_CONCURRENCY: u32 = 1;

/// Lines of the writer's output kept for error messages.
const TAIL_LINES: usize = 50;

/// The last lines of the writer node's output, ANSI colors removed.
#[derive(Clone, Default)]
pub struct Tail(Arc<Mutex<VecDeque<String>>>);

impl Tail {
    fn push(&self, line: String) {
        let mut t = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if t.len() == TAIL_LINES {
            t.pop_front();
        }
        t.push_back(line);
    }

    pub fn lines(&self) -> Vec<String> {
        let t = self.0.lock().unwrap_or_else(|e| e.into_inner());
        t.iter().cloned().collect()
    }
}

/// Remove ANSI escape sequences (Quickwit colors its log even when piped).
fn strip_ansi(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.next() {
            // CSI: parameters and intermediates, then one final byte @–~.
            Some('[') => {
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            // OSC: up to BEL or ESC \.
            Some(']') => {
                while let Some(c) = chars.next() {
                    if c == '\u{7}' || (c == '\u{1b}' && chars.next_if_eq(&'\\').is_some()) {
                        break;
                    }
                }
            }
            // Any other escape is two characters.
            _ => {}
        }
    }
    out
}

/// Quickwit's own line for an ingest request it answered 429 or 503:
/// `ERROR http_request{… url.path=/api/v1/<index>/ingest}:
/// tower_http::trace::on_failure: response failed classification=Status
/// code: 503 Service Unavailable …`. The pipeline logs each of those as a
/// `Quickwit pushed back; retrying` warning, with the status (or fails with
/// it), so forwarding Quickwit's copy only doubled the lines (#231). It
/// stays in the tail and is still observed.
fn is_pushback_echo(line: &str) -> bool {
    line.contains("tower_http::trace::on_failure")
        && line.contains("/ingest}")
        && (line.contains("Status code: 429") || line.contains("Status code: 503"))
}

/// The level a writer line is forwarded at: warnings and errors, and the
/// lines that say the node is up. `stderr` lines without a level (a panic,
/// the CLI's final `Error: …`) are warnings at least. Quickwit's echo of a
/// pushback isn't forwarded ([`is_pushback_echo`]).
fn forward_level(line: &str, stderr: bool) -> Option<tracing::Level> {
    if is_pushback_echo(line) {
        return None;
    }
    // `2026-09-29T00:20:17.017Z  WARN quickwit_config::…: peer seeds are empty`
    let level = line.split_whitespace().take(3).find_map(|w| match w {
        "ERROR" => Some(tracing::Level::ERROR),
        "WARN" => Some(tracing::Level::WARN),
        "INFO" => Some(tracing::Level::INFO),
        "DEBUG" => Some(tracing::Level::DEBUG),
        "TRACE" => Some(tracing::Level::TRACE),
        _ => None,
    });
    match level {
        Some(l) if l <= tracing::Level::WARN => Some(l),
        Some(tracing::Level::INFO)
            if line.contains("REST server is ready")
                || line.contains("starting REST server listening on")
                || line.contains("has transitioned to ready state") =>
        {
            Some(tracing::Level::INFO)
        }
        Some(_) => None,
        None if line.starts_with("Error") => Some(tracing::Level::ERROR),
        None if stderr && !line.trim().is_empty() => Some(tracing::Level::WARN),
        None => None,
    }
}

/// Read the writer's `stream` line by line into `tail` and `events`,
/// forwarding lines through `tracing` (target `quickwit`) at their level.
fn forward(
    stream: impl AsyncRead + Unpin + Send + 'static,
    stderr: bool,
    tail: Tail,
    events: Arc<NodeEvents>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        loop {
            let line = match lines.next_line().await {
                Ok(Some(l)) => strip_ansi(&l),
                // Invalid UTF-8 ends `lines`; nothing more can be read anyway.
                Ok(None) | Err(_) => break,
            };
            match forward_level(&line, stderr) {
                Some(tracing::Level::ERROR) => tracing::error!(target: "quickwit", "{line}"),
                Some(tracing::Level::WARN) => tracing::warn!(target: "quickwit", "{line}"),
                Some(_) => tracing::info!(target: "quickwit", "{line}"),
                None => {}
            }
            events.observe(&line);
            tail.push(line);
        }
    })
}

/// The writer's `RUST_LOG`: the job's filter (Quickwit's default `info` when
/// there is none), with the merge pipelines and the indexing service always
/// at `info`. The release follows each merge pipeline through their lines
/// (`merges::NodeEvents`), so a quieter filter, or one naming only other
/// crates, must not hide them.
fn writer_log_filter(inherited: Option<&str>) -> String {
    let base = inherited
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or("info");
    format!(
        "{base},quickwit_indexing::actors::merge_pipeline=info,\
         quickwit_indexing::actors::indexing_service=info"
    )
}

/// A Quickwit indexer node run as a child process for the length of a
/// release: the one writer of the file-backed metastore. Its output goes to
/// the job's console: warnings, errors and readiness through `tracing`, and
/// the last lines into error messages.
pub struct QuickwitNode {
    pub url: String,
    /// When it was started, in seconds since the epoch (taken before the
    /// process starts).
    pub(super) started: i64,
    pub(super) work_dir: PathBuf,
    pub(super) events: Arc<NodeEvents>,
    /// Shared with the sink, which restarts it for ingest v1's final merges.
    pub(super) process: Arc<WriterProcess>,
}

impl Drop for QuickwitNode {
    /// A node that wasn't stopped (a release that failed) is killed: the
    /// supervisor owns the child, which is killed when it is dropped.
    fn drop(&mut self) {
        self.process.abort();
    }
}

/// Why the writer exited, for the release's error: a SIGKILL nobody sent is
/// almost always the kernel's out-of-memory killer, which the container's
/// cgroup counts when it can be read.
pub fn describe_exit(
    status: std::process::ExitStatus,
    memory: Option<progress::CgroupMemory>,
) -> String {
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status);
    #[cfg(not(unix))]
    let signal: Option<i32> = None;
    let mut why = match (signal, status.code()) {
        (Some(9), _) => "the Quickwit writer was killed by signal 9 (SIGKILL)".to_owned(),
        (Some(sig), _) => format!("the Quickwit writer was killed by signal {sig}"),
        (None, Some(code)) => format!("the Quickwit writer exited with status {code}"),
        (None, None) => format!("the Quickwit writer exited ({status})"),
    };
    let limit = memory
        .and_then(|m| m.limit)
        .map(|b| {
            format!(
                "; the container's memory limit is {:.1} GiB",
                b as f64 / f64::from(1u32 << 30)
            )
        })
        .unwrap_or_default();
    match (signal, memory.and_then(|m| m.oom_kills)) {
        (Some(9), Some(n)) if n > 0 => why.push_str(&format!(
            ": it ran out of memory (the container's cgroup counts {n} out-of-memory kill{}{limit})",
            if n == 1 { "" } else { "s" }
        )),
        // The counter can't be read: say what is most likely. A counter
        // of 0 means someone else sent the signal.
        (Some(9), None) => why.push_str(&format!(
            ", most likely by the kernel's out-of-memory killer{}",
            if limit.is_empty() { String::new() } else { format!(" ({})", &limit[2..]) }
        )),
        _ => {}
    }
    why
}

// Quickwit 0.9.1 puts the v2 WAL in data_dir/wal and the v1 queues in
// data_dir/queues. A fresh local directory for both avoids NFS persist-lock
// stalls without moving the large merge/split files.
fn prepare_local_wal(
    data: &Path,
    local_root: Option<&Path>,
    min_free_bytes: u64,
) -> anyhow::Result<Option<tempfile::TempDir>> {
    let Some(root) = local_root else {
        return Ok(None);
    };
    anyhow::ensure!(root.is_absolute(), "USNM_WRITER_WAL_DIR must be absolute");
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let data = data.canonicalize()?;
    anyhow::ensure!(
        !root.starts_with(&data) && !data.starts_with(&root),
        "the local WAL root must not overlap the writer data directory"
    );
    let free = progress::disk_free(&root).context("reading local WAL free disk space")?;
    anyhow::ensure!(
        free >= min_free_bytes,
        "insufficient local WAL disk space: {free} bytes free, need {min_free_bytes}"
    );
    let local = tempfile::Builder::new()
        .prefix("usnm-wal-")
        .tempdir_in(&root)
        .context("creating a fresh local writer WAL")?;
    for dir in LOCAL_DIRS {
        std::fs::create_dir(local.path().join(dir))?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(local.path().join(dir), data.join(dir))
            .with_context(|| format!("linking the writer's {dir} to local disk"))?;
        #[cfg(not(unix))]
        anyhow::bail!("USNM_WRITER_WAL_DIR requires Unix symbolic links");
    }
    tracing::info!(
        dir = %local.path().display(),
        disk_free_mb = free / 1_048_576,
        max_queue_disk_bytes = WAL_DISK_BYTES,
        "writer WAL and queues on local disk"
    );
    Ok(Some(local))
}

/// The writer's directories `prepare_local_wal` keeps on local disk: the
/// ingest v2 WAL and the ingest v1 queues.
const LOCAL_DIRS: [&str; 2] = ["wal", "queues"];

// Construct this owner before spawning: even a never-polled supervisor must
// signal the child before its local WAL is removed. Fields drop in this
// order; a restarted writer's supervisors share the WAL, which goes with
// the last of them.
struct SupervisedWriter {
    child: tokio::process::Child,
    _local_wal: Option<Arc<tempfile::TempDir>>,
}

impl SupervisedWriter {
    async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        self.child.wait().await
    }
}

impl Drop for SupervisedWriter {
    fn drop(&mut self) {
        let _ = self.child.start_kill();
    }
}

/// Wait for the writer to exit, let the readers take in its last output,
/// then record why it exited in `events` and on the returned channel. A
/// notification on `kill` sends the child SIGKILL: the supervisor owns it,
/// so the signal can't reach a reused pid.
fn supervise(
    child: tokio::process::Child,
    mut readers: Vec<tokio::task::JoinHandle<()>>,
    tail: Tail,
    events: Arc<NodeEvents>,
    local_wal: Option<Arc<tempfile::TempDir>>,
    kill: Arc<tokio::sync::Notify>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::watch::Receiver<Option<String>>,
) {
    let (tx, rx) = tokio::sync::watch::channel(None);
    let mut writer = SupervisedWriter {
        child,
        _local_wal: local_wal,
    };
    let task = tokio::spawn(async move {
        let status = loop {
            tokio::select! {
                status = writer.wait() => break status,
                () = kill.notified() => {
                    let _ = writer.child.start_kill();
                }
            }
        };
        for r in readers.iter_mut() {
            let _ = tokio::time::timeout(Duration::from_secs(2), r).await;
        }
        let mut why = match status {
            Ok(s) => describe_exit(s, progress::cgroup_memory()),
            Err(e) => format!("the Quickwit writer could not be waited for: {e}"),
        };
        let last: Vec<String> = tail.lines().into_iter().rev().take(5).rev().collect();
        if !last.is_empty() {
            why.push_str(&format!(". Its last output: {}", last.join(" | ")));
        }
        events.writer_exited(&why);
        let _ = tx.send(Some(why));
    });
    (task, rx)
}

/// The writer node's process: started once by `QuickwitNode::start`, and
/// again on the same data and config by `restart`.
pub struct WriterProcess {
    bin: PathBuf,
    config_path: PathBuf,
    url: String,
    tail: Tail,
    events: Arc<NodeEvents>,
    local_wal: Option<Arc<tempfile::TempDir>>,
    run: Mutex<Run>,
    /// How long `terminate` waits after SIGTERM, then after SIGKILL.
    grace: Duration,
    kill_wait: Duration,
}

/// One run of the writer process.
#[derive(Default)]
struct Run {
    pid: Option<u32>,
    /// Owns the process: waits for it to exit, then records why (`supervise`).
    supervisor: Option<tokio::task::JoinHandle<()>>,
    /// Why the process exited, once it has.
    exit: Option<tokio::sync::watch::Receiver<Option<String>>>,
    /// Asks the supervisor to SIGKILL the process.
    kill: Arc<tokio::sync::Notify>,
}

impl WriterProcess {
    fn run(&self) -> std::sync::MutexGuard<'_, Run> {
        self.run.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start the process, without waiting for it to be ready.
    fn spawn(&self) -> anyhow::Result<()> {
        let mut cmd = tokio::process::Command::new(&self.bin);
        // Quickwit's environment overrides its config file, and the official
        // image sets QW_LISTEN_ADDRESS=0.0.0.0, QW_DATA_DIR and QW_CONFIG: the
        // writer refused to start on it ("listen address `0.0.0.0` is
        // unspecified"). None is passed through: the storage account is in
        // the config above, and a QW_AZURE_STORAGE_ACCESS_KEY must never
        // reach it (ADR-0009, Entra identities only).
        for (key, _) in std::env::vars_os() {
            if key.to_string_lossy().starts_with("QW_") {
                cmd.env_remove(&key);
            }
        }
        let mut child = cmd
            .args(["run", "--config"])
            .arg(&self.config_path)
            .env("QW_DISABLE_TELEMETRY", "1")
            .env(
                "RUST_LOG",
                writer_log_filter(std::env::var("RUST_LOG").ok().as_deref()),
            )
            // AZURE_CLIENT_ID selects the pipeline's user-assigned identity;
            // Quickwit's credential chain uses the system-assigned one (08 §8.2).
            .env_remove("AZURE_CLIENT_ID")
            // The node's own telemetry setting isn't the pipeline's.
            .env_remove(telemetry::CONNECTION_STRING_VAR)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", self.bin.display()))?;
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(forward(out, false, self.tail.clone(), self.events.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(forward(err, true, self.tail.clone(), self.events.clone()));
        }
        let pid = child.id();
        let kill = Arc::new(tokio::sync::Notify::new());
        let (supervisor, exit) = supervise(
            child,
            readers,
            self.tail.clone(),
            self.events.clone(),
            self.local_wal.clone(),
            kill.clone(),
        );
        *self.run() = Run {
            pid,
            supervisor: Some(supervisor),
            exit: Some(exit),
            kill,
        };
        Ok(())
    }

    /// Wait for the process to answer as ready, or fail with its output.
    async fn ready(&self) -> anyhow::Result<()> {
        let mut exit = self
            .run()
            .exit
            .clone()
            .context("the writer was not started")?;
        let http = reqwest::Client::new();
        for _ in 0..120 {
            // A node that has already exited won't become ready.
            if exit.borrow().is_some() {
                break;
            }
            if http
                .get(format!("{}/health/readyz", self.url))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(());
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // An exited node: the supervisor records its last output first.
        let _ = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some)).await;
        let lines = self.tail.lines().join("\n");
        match self.exited() {
            Some(why) => bail!("Quickwit writer did not become ready: {why}; its output:\n{lines}"),
            None => bail!("Quickwit writer did not become ready; its last output:\n{lines}"),
        }
    }

    /// The process id of its current run.
    pub fn pid(&self) -> Option<u32> {
        self.run().pid
    }

    /// Why its current run exited, if it has.
    pub fn exited(&self) -> Option<String> {
        self.run().exit.as_ref().and_then(|e| e.borrow().clone())
    }

    /// Kill the current run (by dropping its child), without waiting.
    fn abort(&self) {
        if let Some(s) = &self.run().supervisor {
            s.abort();
        }
    }

    /// Stop the current run: SIGTERM, then SIGKILL after `grace` (60 s).
    /// Fails unless the process is confirmed to have exited and been reaped
    /// (its supervisor's `wait` returned): a restart must not start a second
    /// process on the same port and data directory.
    async fn terminate(&self) -> anyhow::Result<()> {
        // The supervisor stays in `Run` until the run has stopped or been
        // aborted: a release cancelled during the wait (a merge timeout, a
        // lost lease) still leaves `stop`/`Drop` a handle to kill it with.
        let (pid, exit, kill) = {
            let r = self.run();
            (r.pid, r.exit.clone(), r.kill.clone())
        };
        let Some(mut exit) = exit else { return Ok(()) };
        let running = exit.borrow().is_none();
        if let (Some(pid), true) = (pid, running) {
            // SIGTERM lets Quickwit shut down cleanly; `kill` is a shell builtin.
            let _ = tokio::process::Command::new("sh")
                .args(["-c", "kill -TERM \"$0\"", &pid.to_string()])
                .status()
                .await;
        }
        // Only the supervisor's own notice confirms the exit: a channel it
        // closed without one (it was aborted) confirms nothing.
        let mut stopped = matches!(
            tokio::time::timeout(self.grace, exit.wait_for(Option::is_some)).await,
            Ok(Ok(_))
        );
        if !stopped {
            // Expected after a release: the closed index's ingest shards
            // have no pipeline left to drain them, and the ingester waits
            // for that. Everything is published by now, and the next
            // writer starts from an empty data directory (a restart
            // reads what is published from the metastore, and resumes
            // ingest v1 queues from their checkpoints).
            tracing::info!(
                pid,
                "the Quickwit writer did not stop within {} s; killing it",
                self.grace.as_secs()
            );
            kill.notify_one();
            stopped = matches!(
                tokio::time::timeout(self.kill_wait, exit.wait_for(Option::is_some)).await,
                Ok(Ok(_))
            );
        }
        if !stopped {
            // Dropping the supervisor's child kills it (`kill_on_drop`),
            // but nothing then confirms it has gone.
            self.abort();
            bail!(
                "the Quickwit writer (pid {pid:?}) was not confirmed to have exited {} s after SIGKILL",
                self.kill_wait.as_secs()
            );
        }
        let supervisor = self.run().supervisor.take();
        if let Some(mut supervisor) = supervisor {
            let _ = tokio::time::timeout(Duration::from_secs(5), &mut supervisor).await;
        }
        Ok(())
    }

    /// Stop the writer and start it again on the same data and config, for
    /// ingest v1's final merges (`merges::seal_restarting`). A writer that
    /// has exited on its own is not restarted: that fails the release.
    pub async fn restart(&self) -> anyhow::Result<()> {
        if let Some(why) = self.exited() {
            bail!("{why}; not publishing");
        }
        tracing::info!(pid = self.pid(), "restarting the Quickwit writer");
        self.terminate()
            .await
            .context("stopping the Quickwit writer to restart it; not restarting")?;
        // The exit was asked for.
        self.events.clear_exit();
        self.spawn()?;
        self.ready().await?;
        tracing::info!(pid = self.pid(), "the Quickwit writer restarted");
        Ok(())
    }
}

impl Drop for WriterProcess {
    fn drop(&mut self) {
        self.abort();
    }
}

impl QuickwitNode {
    /// `metastore` and `index_root` are `azure://qw-index` in Azure and
    /// `file:///…` locally. On Azure, Quickwit authenticates with the job's
    /// system-assigned identity (08 §8.2).
    pub async fn start(
        bin: &Path,
        work_dir: &Path,
        port: u16,
        metastore: &str,
        index_root: &str,
    ) -> anyhow::Result<Self> {
        let started = chrono::Utc::now().timestamp();
        let data = work_dir.join("qwdata");
        // Only this release writes here (it holds the writer lock), and a
        // previous run's data must not come back: on a persistent scratch
        // volume, its write-ahead log would replay into an index that a
        // published version may already list.
        if data.exists() {
            tracing::info!(dir = %data.display(), "removing a previous writer's data");
            std::fs::remove_dir_all(&data)
                .with_context(|| format!("removing {}", data.display()))?;
        }
        std::fs::create_dir_all(&data)?;
        let local_root = std::env::var_os("USNM_WRITER_WAL_DIR").map(PathBuf::from);
        let local_wal = prepare_local_wal(&data, local_root.as_deref(), 8 * 1024 * 1024 * 1024)?;
        let queue = WriterTuning::from_env()?.queue;
        // A Kafka source's pipelines each take 4 cores of the indexer's
        // capacity; without room, Quickwit schedules fewer (`crate::kafka`).
        let cpu = match crate::kafka::Kafka::from_env()? {
            Some(k) => format!("cpu_capacity: {}\n  ", k.cpu_capacity()),
            None => String::new(),
        };
        let mut config = format!(
            "version: 0.8\ncluster_id: usnm-writer\nnode_id: writer\nlisten_address: 127.0.0.1\n\
             rest:\n  listen_port: {port}\ngrpc_listen_port: {}\ndata_dir: {}\n\
             metastore_uri: {metastore}\ndefault_index_root_uri: {index_root}\n\
             ingest_api:\n  shard_throughput_limit: {SHARD_THROUGHPUT_LIMIT}\n  \
             max_queue_memory_usage: {queue}\n  \
             max_queue_disk_usage: {WAL_DISK_BYTES}\n\
             indexer:\n  split_store_max_num_bytes: {SPLIT_STORE_BYTES}\n  {cpu}\
             merge_concurrency: {MERGE_CONCURRENCY}\n",
            port.checked_add(1)
                .context("--quickwit-port must be below 65535")?,
            data.display()
        );
        if let Ok(account) = std::env::var("QW_AZURE_STORAGE_ACCOUNT") {
            config.push_str(&format!("storage:\n  azure:\n    account: {account}\n"));
        }
        let config_path = work_dir.join("writer.yaml");
        std::fs::write(&config_path, config)?;
        let tail = Tail::default();
        let events = Arc::new(NodeEvents::default());
        let url = format!("http://127.0.0.1:{port}");
        let process = Arc::new(WriterProcess {
            bin: bin.to_owned(),
            config_path,
            url: url.clone(),
            tail: tail.clone(),
            events: events.clone(),
            local_wal: local_wal.map(Arc::new),
            run: Mutex::default(),
            grace: Duration::from_secs(60),
            kill_wait: Duration::from_secs(30),
        });
        process.spawn()?;
        let node = Self {
            url,
            started,
            work_dir: work_dir.to_owned(),
            events,
            process,
        };
        node.process.ready().await?;
        Ok(node)
    }

    /// What the node's output has reported so far.
    pub fn events(&self) -> Arc<NodeEvents> {
        self.events.clone()
    }

    /// The node's process id.
    pub fn pid(&self) -> Option<u32> {
        self.process.pid()
    }

    /// Why the node exited, if it has.
    pub fn exited(&self) -> Option<String> {
        self.process.exited()
    }

    /// Stop the node and wait for it to exit.
    pub async fn stop(self) -> anyhow::Result<()> {
        // At the end of a release, which has published or failed by now;
        // the job's exit takes the process with it.
        if let Err(e) = self.process.terminate().await {
            tracing::warn!("{e:#}");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn an_unconfigured_wal_keeps_the_existing_data_layout() {
        let root = tempfile::tempdir().unwrap();
        assert!(prepare_local_wal(root.path(), None, u64::MAX)
            .unwrap()
            .is_none());
        assert!(!root.path().join("wal").exists());
    }

    #[cfg(unix)]
    #[test]
    fn local_wal_is_fresh_and_scratch_cleanup_does_not_follow_its_link() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("scratch/qwdata");
        let local = root.path().join("local");
        std::fs::create_dir_all(&data).unwrap();
        let wal = prepare_local_wal(&data, Some(&local), 0).unwrap().unwrap();
        let previous = wal.path().to_owned();
        // The v2 WAL and the v1 queues.
        for dir in ["wal", "queues"] {
            assert!(std::fs::symlink_metadata(data.join(dir))
                .unwrap()
                .file_type()
                .is_symlink());
            assert_eq!(
                data.join(dir).canonicalize().unwrap(),
                previous.join(dir).canonicalize().unwrap()
            );
        }
        std::fs::write(data.join("wal/accepted"), b"page").unwrap();
        std::fs::write(data.join("queues/queued"), b"page").unwrap();
        std::fs::write(local.join("unrelated"), b"keep").unwrap();
        std::fs::remove_dir_all(&data).unwrap();
        assert_eq!(
            std::fs::read(previous.join("wal/accepted")).unwrap(),
            b"page"
        );
        assert_eq!(
            std::fs::read(previous.join("queues/queued")).unwrap(),
            b"page"
        );
        drop(wal);
        assert!(!previous.exists());
        assert!(local.join("unrelated").exists());
        std::fs::create_dir_all(&data).unwrap();
        let next = prepare_local_wal(&data, Some(&local), 0).unwrap().unwrap();
        assert_ne!(next.path(), previous);
        assert!(!next.path().join("wal/accepted").exists());
        assert!(!next.path().join("queues/queued").exists());
    }

    #[tokio::test]
    async fn the_supervisor_keeps_the_local_wal_until_the_child_stops() {
        let root = tempfile::tempdir().unwrap();
        let wal = tempfile::Builder::new().tempdir_in(root.path()).unwrap();
        let path = wal.path().to_owned();
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let input = child.stdin.take().unwrap();
        let (task, _) = supervise(
            child,
            vec![],
            Tail::default(),
            Arc::new(NodeEvents::default()),
            Some(Arc::new(wal)),
            Default::default(),
        );
        assert!(path.exists());
        drop(input);
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .unwrap()
            .unwrap();
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test(flavor = "current_thread")]
    async fn cancelling_an_unpolled_supervisor_kills_the_child_and_cleans_its_wal() {
        let root = tempfile::tempdir().unwrap();
        let wal = tempfile::Builder::new().tempdir_in(root.path()).unwrap();
        let path = wal.path().to_owned();
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "read line"])
            .stdin(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let pid = child.id().unwrap().to_string();
        let input = child.stdin.take().unwrap();
        let (task, _) = supervise(
            child,
            vec![],
            Tail::default(),
            Arc::new(NodeEvents::default()),
            Some(Arc::new(wal)),
            Default::default(),
        );
        // There has been no yield: the supervisor has not run even once.
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        assert!(!path.exists());
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let alive = tokio::process::Command::new("kill")
                    .args(["-0", &pid])
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await
                    .unwrap()
                    .success();
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        // Keep stdin open until after the kill proof, so EOF cannot mask a leak.
        drop(input);
    }

    #[test]
    fn local_wal_rejects_relative_overlapping_and_full_roots() {
        let root = tempfile::tempdir().unwrap();
        let data = root.path().join("qwdata");
        let local = root.path().join("local");
        std::fs::create_dir_all(&data).unwrap();
        assert!(prepare_local_wal(&data, Some(Path::new("relative")), 0).is_err());
        assert!(prepare_local_wal(&data, Some(&data), 0).is_err());
        assert!(prepare_local_wal(&data, Some(root.path()), 0).is_err());
        let error = prepare_local_wal(&data, Some(&local), u64::MAX).unwrap_err();
        assert!(error
            .to_string()
            .contains("insufficient local WAL disk space"));
        assert_eq!(std::fs::read_dir(local).unwrap().count(), 0);
    }

    #[test]
    fn the_writer_always_logs_its_merge_pipelines() {
        let pipeline = "quickwit_indexing::actors::merge_pipeline=info,\
                        quickwit_indexing::actors::indexing_service=info";
        assert_eq!(writer_log_filter(None), format!("info,{pipeline}"));
        assert_eq!(writer_log_filter(Some(" ")), format!("info,{pipeline}"));
        assert_eq!(writer_log_filter(Some("warn")), format!("warn,{pipeline}"));
        assert_eq!(
            writer_log_filter(Some("usnm_ingest=info")),
            format!("usnm_ingest=info,{pipeline}")
        );
    }

    #[test]
    fn tail_keeps_the_last_lines() {
        let t = Tail::default();
        for i in 0..TAIL_LINES + 3 {
            t.push(i.to_string());
        }
        let lines = t.lines();
        assert_eq!(lines.len(), TAIL_LINES);
        assert_eq!(lines[0], "3");
        assert_eq!(lines[TAIL_LINES - 1], (TAIL_LINES + 2).to_string());
    }

    #[test]
    fn strips_ansi_colors() {
        // As Quickwit 0.9 writes it to a pipe.
        let raw = "\u{1b}[2m2026-09-29T00:20:17.017Z\u{1b}[0m \u{1b}[33m WARN\u{1b}[0m \
                   \u{1b}[2mquickwit_config\u{1b}[0m\u{1b}[2m:\u{1b}[0m peer seeds are empty \
                   \u{1b}[3mkey\u{1b}[0m\u{1b}[2m=\u{1b}[0mvalue";
        assert_eq!(
            strip_ansi(raw),
            "2026-09-29T00:20:17.017Z  WARN quickwit_config: peer seeds are empty key=value"
        );
        assert_eq!(strip_ansi("\u{1b}]0;title\u{7}plain"), "plain");
        assert_eq!(strip_ansi("no escapes"), "no escapes");
        // A trailing lone escape doesn't panic.
        assert_eq!(strip_ansi("x\u{1b}"), "x");
    }

    #[test]
    fn forwards_warnings_errors_and_readiness() {
        use tracing::Level;
        let line = |level: &str, msg: &str| {
            format!("2026-09-29T00:20:17.017Z {level} quickwit_serve: {msg}")
        };
        assert_eq!(
            forward_level(&line(" WARN", "no shards available"), false),
            Some(Level::WARN)
        );
        assert_eq!(
            forward_level(&line("ERROR", "data dir volume too small"), false),
            Some(Level::ERROR)
        );
        assert_eq!(
            forward_level(&line(" INFO", "REST server is ready"), false),
            Some(Level::INFO)
        );
        assert_eq!(
            forward_level(
                &line(" INFO", "starting REST server listening on 127.0.0.1:7380"),
                false
            ),
            Some(Level::INFO)
        );
        assert_eq!(
            forward_level(&line(" INFO", "starting janitor service"), false),
            None
        );
        assert_eq!(forward_level(&line("DEBUG", "x"), true), None);
        assert_eq!(
            forward_level("Error: failed to bind", false),
            Some(Level::ERROR)
        );
        assert_eq!(
            forward_level("thread 'main' panicked at src/main.rs", true),
            Some(Level::WARN)
        );
        assert_eq!(forward_level("some unlevelled stdout line", false), None);
        assert_eq!(forward_level("", true), None);
    }

    #[test]
    fn pushback_echoes_are_not_forwarded() {
        use tracing::Level;
        // As the writer logged them in prod (October 2026).
        let echo = |path: &str, status: &str| {
            format!(
                "2026-10-08T10:00:00.123Z ERROR http_request{{otel.kind=\"Server\" \
                 http.request.method=POST url.path={path}}}: tower_http::trace::on_failure: \
                 response failed classification=Status code: {status} latency=12 ms"
            )
        };
        let ingest = "/api/v1/pages-base-20261008-1/ingest";
        assert_eq!(
            forward_level(&echo(ingest, "503 Service Unavailable"), false),
            None
        );
        assert_eq!(
            forward_level(&echo(ingest, "429 Too Many Requests"), false),
            None
        );
        // Other failures are still forwarded: another status, another path.
        assert_eq!(
            forward_level(&echo(ingest, "500 Internal Server Error"), false),
            Some(Level::ERROR)
        );
        assert_eq!(
            forward_level(&echo("/health/readyz", "503 Service Unavailable"), false),
            Some(Level::ERROR)
        );
    }

    /// A writer that exits at startup: the error carries its last output.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_writer_that_exits_explains_why() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quickwit");
        std::fs::write(
            &bin,
            "#!/bin/sh\n\
             printf '\\033[2m2026-09-29T00:20:17Z\\033[0m \\033[33m WARN\\033[0m quickwit_config: peer seeds are empty\\n'\n\
             echo 'Error: data dir volume too small' >&2\n\
             exit 1\n",
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let err = QuickwitNode::start(&bin, dir.path(), 7399, "file:///m", "file:///i")
            .await
            .err()
            .expect("the writer exited")
            .to_string();
        assert!(err.contains("did not become ready"), "{err}");
        assert!(err.contains("exited with status 1"), "{err}");
        assert!(
            err.contains("2026-09-29T00:20:17Z  WARN quickwit_config: peer seeds are empty"),
            "{err}"
        );
        assert!(err.contains("Error: data dir volume too small"), "{err}");
    }

    /// A release cancelled while it waits for the writer to stop (a merge
    /// timeout, a lost lease) can still kill a writer that ignores SIGTERM.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancelled_stop_still_leaves_the_writer_to_kill() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let bin = dir.path().join("quickwit");
        let trapped = dir.path().join("trapped");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ntrap '' TERM\ntouch '{}'\nwhile :; do sleep 1; done\n",
                trapped.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let writer = WriterProcess {
            bin,
            config_path: dir.path().join("node.yaml"),
            url: "http://127.0.0.1:1".into(),
            tail: Tail::default(),
            events: Arc::new(NodeEvents::default()),
            local_wal: None,
            run: Mutex::new(Run::default()),
            grace: Duration::from_secs(60),
            kill_wait: Duration::from_secs(30),
        };
        writer.spawn().unwrap();
        while !trapped.exists() {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let stopping = tokio::time::timeout(Duration::from_millis(500), writer.terminate()).await;
        assert!(stopping.is_err(), "the writer ignores SIGTERM");
        assert!(writer.exited().is_none());
        assert!(writer.run().supervisor.is_some());
        let pid = writer.pid().unwrap().to_string();
        writer.abort();
        // The aborted supervisor drops the child, which kills it.
        let gone = || {
            !std::process::Command::new("kill")
                .args(["-0", &pid])
                .stderr(Stdio::null())
                .status()
                .unwrap()
                .success()
        };
        for _ in 0..100 {
            if gone() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        panic!("the writer outlived its aborted supervisor");
    }

    /// A writer that ignores SIGTERM, and a `/health/readyz` that answers
    /// for it, for the restart tests.
    #[cfg(unix)]
    async fn stubborn_writer(
        dir: &Path,
        kill_wait: Duration,
    ) -> (WriterProcess, PathBuf, tokio::task::JoinHandle<()>) {
        use std::os::unix::fs::PermissionsExt;
        let bin = dir.join("quickwit");
        let trapped = dir.join("trapped");
        std::fs::write(
            &bin,
            format!(
                "#!/bin/sh\ntrap '' TERM\ntouch '{}'\nwhile :; do sleep 1; done\n",
                trapped.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let health = tokio::spawn(async move {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            loop {
                let Ok((mut conn, _)) = listener.accept().await else {
                    return;
                };
                let mut buf = [0u8; 1024];
                let _ = conn.read(&mut buf).await;
                let _ = conn
                    .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                    .await;
            }
        });
        let writer = WriterProcess {
            bin,
            config_path: dir.join("node.yaml"),
            url,
            tail: Tail::default(),
            events: Arc::new(NodeEvents::default()),
            local_wal: None,
            run: Mutex::new(Run::default()),
            grace: Duration::from_millis(300),
            kill_wait,
        };
        (writer, trapped, health)
    }

    #[cfg(unix)]
    async fn wait_for_file(path: &Path) {
        tokio::time::timeout(Duration::from_secs(10), async {
            while !path.exists() {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the writer started");
    }

    #[cfg(unix)]
    fn pid_alive(pid: u32) -> bool {
        std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success()
    }

    /// The restart for ingest v1's final merges kills a writer that ignores
    /// SIGTERM, and starts the next only once the old one has exited and
    /// been reaped: two never share the port and data directory.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_restart_kills_a_stubborn_writer_before_starting_another() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, trapped, health) = stubborn_writer(dir.path(), Duration::from_secs(10)).await;
        writer.spawn().unwrap();
        wait_for_file(&trapped).await;
        let old = writer.pid().unwrap();
        std::fs::remove_file(&trapped).unwrap();
        writer.restart().await.unwrap();
        // `kill -0` fails for a reaped process (a zombie would still answer).
        assert!(!pid_alive(old), "the old writer is gone");
        let new = writer.pid().unwrap();
        assert_ne!(new, old);
        assert!(writer.exited().is_none(), "the new run is running");
        wait_for_file(&trapped).await;
        assert!(pid_alive(new));
        writer.abort();
        health.abort();
    }

    /// A run whose supervisor was aborted (a stop cancelled earlier) has
    /// no one left to confirm its exit: the restart fails.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_restart_fails_once_the_supervisor_is_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, trapped, health) = stubborn_writer(dir.path(), Duration::from_secs(2)).await;
        writer.spawn().unwrap();
        wait_for_file(&trapped).await;
        let old = writer.pid().unwrap();
        writer.abort();
        let err = format!("{:#}", writer.restart().await.unwrap_err());
        assert!(err.contains("not confirmed to have exited"), "{err}");
        assert_eq!(writer.pid(), Some(old), "no second writer was started");
        health.abort();
    }

    /// A writer that isn't confirmed gone after SIGKILL fails the restart,
    /// and no second writer is started.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_restart_fails_unless_the_old_writer_is_confirmed_gone() {
        let dir = tempfile::tempdir().unwrap();
        let (writer, trapped, health) = stubborn_writer(dir.path(), Duration::ZERO).await;
        writer.spawn().unwrap();
        wait_for_file(&trapped).await;
        let old = writer.pid().unwrap();
        let err = format!("{:#}", writer.restart().await.unwrap_err());
        assert!(err.contains("not restarting"), "{err}");
        assert!(err.contains("not confirmed to have exited"), "{err}");
        assert_eq!(writer.pid(), Some(old), "no second writer was started");
        // The aborted supervisor still kills it.
        tokio::time::timeout(Duration::from_secs(10), async {
            while pid_alive(old) {
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the old writer was killed");
        health.abort();
    }

    #[cfg(unix)]
    #[test]
    fn explains_how_the_writer_exited() {
        use std::os::unix::process::ExitStatusExt;
        let killed = std::process::ExitStatus::from_raw(9);
        let oom = progress::CgroupMemory {
            limit: Some(7680 << 20),
            oom_kills: Some(1),
        };
        assert_eq!(
            describe_exit(killed, Some(oom)),
            "the Quickwit writer was killed by signal 9 (SIGKILL): it ran out of memory (the \
             container's cgroup counts 1 out-of-memory kill; the container's memory limit is 7.5 GiB)"
        );
        let unknown = progress::CgroupMemory {
            limit: Some(7680 << 20),
            oom_kills: None,
        };
        assert_eq!(
            describe_exit(killed, Some(unknown)),
            "the Quickwit writer was killed by signal 9 (SIGKILL), most likely by the kernel's \
             out-of-memory killer (the container's memory limit is 7.5 GiB)"
        );
        assert_eq!(
            describe_exit(killed, None),
            "the Quickwit writer was killed by signal 9 (SIGKILL), most likely by the kernel's \
             out-of-memory killer"
        );
        // No out-of-memory kill counted: someone else sent the signal.
        let none = progress::CgroupMemory {
            limit: Some(7680 << 20),
            oom_kills: Some(0),
        };
        assert_eq!(
            describe_exit(killed, Some(none)),
            "the Quickwit writer was killed by signal 9 (SIGKILL)"
        );
        assert_eq!(
            describe_exit(std::process::ExitStatus::from_raw(15), Some(oom)),
            "the Quickwit writer was killed by signal 15"
        );
        assert_eq!(
            describe_exit(std::process::ExitStatus::from_raw(1 << 8), None),
            "the Quickwit writer exited with status 1"
        );
    }

    /// A writer the kernel kills mid-release: the exit is recorded with
    /// its signal and last output, for the release's error.
    #[cfg(unix)]
    #[tokio::test]
    async fn a_killed_writer_is_reported() {
        let mut child = tokio::process::Command::new("sh")
            .args(["-c", "echo 'INFO merging splits'; kill -KILL $$"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let tail = Tail::default();
        let events = Arc::new(NodeEvents::default());
        let readers = vec![
            forward(
                child.stdout.take().unwrap(),
                false,
                tail.clone(),
                events.clone(),
            ),
            forward(
                child.stderr.take().unwrap(),
                true,
                tail.clone(),
                events.clone(),
            ),
        ];
        let (task, mut exit) = supervise(
            child,
            readers,
            tail,
            events.clone(),
            None,
            Default::default(),
        );
        tokio::time::timeout(Duration::from_secs(10), exit.wait_for(Option::is_some))
            .await
            .unwrap()
            .unwrap();
        task.await.unwrap();
        let why = events.exit().unwrap();
        assert!(
            why.starts_with("the Quickwit writer was killed by signal 9 (SIGKILL)"),
            "{why}"
        );
        assert!(
            why.ends_with("Its last output: INFO merging splits"),
            "{why}"
        );
        let err = events.check().unwrap_err().to_string();
        assert!(err.contains("not publishing"), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_restarted_writer_keeps_its_local_wal_until_its_last_run_stops() {
        let root = tempfile::tempdir().unwrap();
        let wal = Arc::new(tempfile::Builder::new().tempdir_in(root.path()).unwrap());
        let path = wal.path().to_owned();
        let run = || {
            let mut child = tokio::process::Command::new("sh")
                .args(["-c", "read line"])
                .stdin(Stdio::piped())
                .kill_on_drop(true)
                .spawn()
                .unwrap();
            let input = child.stdin.take().unwrap();
            let (task, _) = supervise(
                child,
                vec![],
                Tail::default(),
                Arc::new(NodeEvents::default()),
                Some(wal.clone()),
                Default::default(),
            );
            (task, input)
        };
        let (first, input) = run();
        let (second, input2) = run();
        drop(wal);
        drop(input);
        tokio::time::timeout(Duration::from_secs(5), first)
            .await
            .unwrap()
            .unwrap();
        assert!(path.exists(), "the restarted writer still uses it");
        drop(input2);
        tokio::time::timeout(Duration::from_secs(5), second)
            .await
            .unwrap()
            .unwrap();
        assert!(!path.exists());
    }
}
