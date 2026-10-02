//! Where index documents go: JSONL files (the API's memory backend, local
//! development and tests) or a Quickwit writer node (08 §8.4.1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use opentelemetry::KeyValue;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use crate::merges::{self, IndexLayout, MergeWait, NodeEvents};
use crate::progress;
use crate::telemetry;

/// The index config every base and delta shares (05 §5.5.1).
pub const INDEX_TEMPLATE: &str = include_str!("../../../infra/quickwit/pages-index.yaml");

/// Documents per ingest request, bounded well under Quickwit's 10 MiB body limit.
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Per-shard ingest rate on the writer node: Quickwit's maximum (its default
/// is 5 MB/s). A single-node writer can't spread load over more shards, so
/// past the ~50 MiB burst allowance ingest runs at this rate.
const SHARD_THROUGHPUT_LIMIT: &str = "20MB";
/// The node's in-memory ingest queue (Quickwit's default is 2 GiB). The first
/// prod release ran out of memory at 4 GiB with the defaults.
const INGEST_QUEUE_MEMORY: &str = "1GiB";
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

/// How long one ingest request keeps retrying while the node pushes back
/// (503 "no shards available" once the shard's rate limit is spent, or 429).
/// No attempt starts after this window, but one already in flight runs to the
/// client's 300 s timeout: cutting it short could abandon documents the node
/// has already taken, and resending them would duplicate them.
const INGEST_RETRY_FOR: Duration = Duration::from_secs(600);

#[async_trait]
pub trait IndexSink: Send {
    /// Start a new, empty index. Fails if it already exists.
    async fn create(&mut self, index_id: &str) -> anyhow::Result<()>;
    async fn add(&mut self, doc: &Value) -> anyhow::Result<()>;
    /// Flush and confirm the index holds exactly `expected` documents.
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()>;
    /// `memory` or `quickwit`, recorded in `current.json`.
    fn backend(&self) -> &'static str;
    /// The published splits of `index_ids`, for the release log and
    /// manifest. Sinks without splits report none.
    async fn layout(&mut self, _index_ids: &[String]) -> anyhow::Result<Vec<IndexLayout>> {
        Ok(Vec::new())
    }
    /// Counters for release progress, shared with whoever reports it. A
    /// sink that doesn't count reports zeros.
    fn stats(&self) -> Arc<SinkStats> {
        Arc::default()
    }
}

/// What a sink has sent so far, and where to look for its resources.
#[derive(Debug, Default)]
pub struct SinkStats {
    docs_sent: AtomicU64,
    bytes_sent: AtomicU64,
    retries_429: AtomicU64,
    retries_503: AtomicU64,
    /// The file system whose free space the progress line reports.
    pub work_dir: Option<PathBuf>,
    /// The Quickwit writer node's process, for its memory use.
    pub node_pid: Option<u32>,
}

impl SinkStats {
    pub fn docs_sent(&self) -> u64 {
        self.docs_sent.load(Ordering::Relaxed)
    }
    pub fn bytes_sent(&self) -> u64 {
        self.bytes_sent.load(Ordering::Relaxed)
    }
    pub fn retries_429(&self) -> u64 {
        self.retries_429.load(Ordering::Relaxed)
    }
    pub fn retries_503(&self) -> u64 {
        self.retries_503.load(Ordering::Relaxed)
    }

    fn sent(&self, docs: u64, bytes: u64) {
        self.docs_sent.fetch_add(docs, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        let m = telemetry::metrics();
        m.docs_sent.add(docs, &[]);
        m.bytes_sent.add(bytes, &[]);
    }

    fn retried(&self, status: u16) {
        match status {
            429 => self.retries_429.fetch_add(1, Ordering::Relaxed),
            _ => self.retries_503.fetch_add(1, Ordering::Relaxed),
        };
        telemetry::metrics()
            .retries
            .add(1, &[KeyValue::new("status", i64::from(status))]);
    }
}

/// `{dir}/{index_id}.jsonl`, the layout the memory backend loads.
pub struct JsonlSink {
    dir: PathBuf,
    current: Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
    stats: Arc<SinkStats>,
}

impl JsonlSink {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        let dir = dir.into();
        Self {
            stats: Arc::new(SinkStats {
                work_dir: Some(dir.clone()),
                ..SinkStats::default()
            }),
            dir,
            current: None,
        }
    }
}

#[async_trait]
impl IndexSink for JsonlSink {
    async fn create(&mut self, index_id: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{index_id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("{} (indexes are never rewritten)", path.display()))?;
        self.current = Some((path, std::io::BufWriter::new(file)));
        Ok(())
    }

    async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
        use std::io::Write;
        let (_, w) = self.current.as_mut().context("no index created")?;
        let mut line = serde_json::to_vec(doc)?;
        line.push(b'\n');
        w.write_all(&line)?;
        self.stats.sent(1, line.len() as u64);
        Ok(())
    }

    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        use std::io::{BufRead, Write};
        let (path, mut w) = self.current.take().context("no index created")?;
        w.flush()?;
        w.get_ref().sync_all()?;
        let lines = std::io::BufReader::new(std::fs::File::open(&path)?)
            .lines()
            .count() as u64;
        if lines != expected {
            bail!(
                "{} holds {lines} documents, expected {expected}",
                path.display()
            );
        }
        Ok(())
    }

    fn backend(&self) -> &'static str {
        "memory"
    }

    fn stats(&self) -> Arc<SinkStats> {
        self.stats.clone()
    }
}

/// Ingests into a Quickwit node that runs the indexer (the sole metastore writer).
pub struct QuickwitSink {
    base: String,
    index_root: String,
    http: reqwest::Client,
    index: Option<String>,
    buf: Vec<u8>,
    /// First pause after pushback; doubles up to 8 s.
    retry_initial: Duration,
    retry_for: Duration,
    stats: Arc<SinkStats>,
    /// How `finish` waits for the new index's merges.
    merge_wait: MergeWait,
    /// The writer node's output, when this release runs the node.
    events: Option<Arc<NodeEvents>>,
}

impl QuickwitSink {
    /// `index_root` is where new indexes live (`azure://qw-index` or `file:///…`).
    pub fn new(base_url: &str, index_root: &str) -> anyhow::Result<Self> {
        Ok(Self {
            base: base_url.trim_end_matches('/').to_owned(),
            index_root: index_root.trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()?,
            index: None,
            buf: Vec::new(),
            retry_initial: Duration::from_millis(500),
            retry_for: INGEST_RETRY_FOR,
            stats: Arc::default(),
            merge_wait: MergeWait::default(),
            events: None,
        })
    }

    /// Report the free disk of `node`'s work directory and its memory in
    /// release progress, and stop the release if its output reports a full
    /// disk or a failed merge. Call before sending anything.
    pub fn watching(mut self, node: &QuickwitNode) -> Self {
        self.stats = Arc::new(SinkStats {
            work_dir: Some(node.work_dir.clone()),
            node_pid: node.pid(),
            ..SinkStats::default()
        });
        self.events = Some(node.events.clone());
        self
    }

    /// How long `finish` may wait for merges, and how it polls.
    pub fn merges(mut self, wait: MergeWait) -> Self {
        self.merge_wait = wait;
        self
    }

    fn node(&self) -> merges::Node<'_> {
        merges::Node {
            http: &self.http,
            base: &self.base,
        }
    }

    async fn send(&mut self, commit: &str) -> anyhow::Result<()> {
        if self.buf.is_empty() && commit != "force" {
            return Ok(());
        }
        if let Some(e) = &self.events {
            e.check()?;
        }
        let id = self.index.as_deref().context("no index created")?;
        let body = bytes::Bytes::from(std::mem::take(&mut self.buf));
        let url = format!("{}/api/v1/{id}/ingest?commit={commit}", self.base);
        let deadline = tokio::time::Instant::now() + self.retry_for;
        let mut pause = self.retry_initial;
        let mut attempt = 1u32;
        let (status, text) = loop {
            let sent = self
                .http
                .post(&url)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await
                .map_err(anyhow::Error::from);
            let resp = merges::explained(self.events.as_deref(), sent).await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // The whole request is resent; `finish` checks the final count, so
            // a duplicate would fail the release rather than go unnoticed.
            let pushback = status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
            if !pushback || tokio::time::Instant::now() + pause > deadline {
                break (status, text);
            }
            tracing::warn!(
                index = id,
                status = status.as_u16(),
                attempt,
                pause_ms = pause.as_millis() as u64,
                error = %text.chars().take(200).collect::<String>(),
                "Quickwit pushed back; retrying"
            );
            self.stats.retried(status.as_u16());
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(8));
            attempt += 1;
        };
        if !status.is_success() {
            bail!(
                "ingest into `{id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        let v: Value = serde_json::from_str(&text).context("ingest response")?;
        if v["num_rejected_docs"].as_u64() != Some(0) {
            bail!(
                "ingest into `{id}` rejected documents: {}",
                text.chars().take(500).collect::<String>()
            );
        }
        let docs = body.iter().filter(|&&b| b == b'\n').count() as u64;
        self.stats.sent(docs, body.len() as u64);
        Ok(())
    }

    async fn count(&self, id: &str) -> anyhow::Result<u64> {
        let counted = self.count_once(id).await;
        merges::explained(self.events.as_deref(), counted).await
    }

    async fn count_once(&self, id: &str) -> anyhow::Result<u64> {
        let v: Value = self
            .http
            .get(format!(
                "{}/api/v1/{id}/search?query=*&max_hits=0",
                self.base
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        v["num_hits"]
            .as_u64()
            .context("search response has no num_hits")
    }
}

#[async_trait]
impl IndexSink for QuickwitSink {
    async fn create(&mut self, index_id: &str) -> anyhow::Result<()> {
        let config = INDEX_TEMPLATE
            .replace("${INDEX_ID}", index_id)
            .replace("${INDEX_URI}", &format!("{}/{index_id}", self.index_root));
        let resp = self
            .http
            .post(format!("{}/api/v1/indexes", self.base))
            .header("content-type", "application/yaml")
            .body(config)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "creating index `{index_id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        self.index = Some(index_id.to_owned());
        Ok(())
    }

    async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(doc)?;
        line.push(b'\n');
        if line.len() > CHUNK_BYTES {
            bail!(
                "document `{}` is {} bytes, over the {CHUNK_BYTES}-byte ingest request limit",
                doc["doc_id"].as_str().unwrap_or("?"),
                line.len()
            );
        }
        // Send first if this document would push the request over the limit.
        if self.buf.len() + line.len() > CHUNK_BYTES {
            self.send("auto").await?;
        }
        self.buf.extend_from_slice(&line);
        Ok(())
    }

    /// Commit, confirm the count, then wait for the index's merges and
    /// close it (`merges::seal`): nothing is published half merged.
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        // A forced commit publishes everything still buffered in the node.
        // `add` always leaves its document in the buffer, so an empty one
        // means none was added (a batch with no text): there is nothing to
        // commit, and the node rejects an empty request (411).
        if !self.buf.is_empty() {
            self.send("force").await?;
        }
        let id = self.index.clone().context("no index created")?;
        let mut last = 0;
        for _ in 0..120 {
            last = self.count(&id).await?;
            if last >= expected {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        if last != expected {
            bail!("index `{id}` holds {last} documents, expected {expected}");
        }
        let layout = merges::seal(
            &self.node(),
            &id,
            expected,
            self.events.as_deref(),
            &self.merge_wait,
            self.stats.work_dir.as_deref(),
        )
        .await?;
        tracing::info!(
            index = %id,
            splits = layout.splits,
            "merged; the index is closed to further writes"
        );
        Ok(())
    }

    fn backend(&self) -> &'static str {
        "quickwit"
    }

    async fn layout(&mut self, index_ids: &[String]) -> anyhow::Result<Vec<IndexLayout>> {
        let node = self.node();
        let mut out = Vec::new();
        for id in index_ids {
            out.push(IndexLayout::of(id, &node.splits(id).await?));
        }
        Ok(out)
    }

    fn stats(&self) -> Arc<SinkStats> {
        self.stats.clone()
    }
}

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

/// The level a writer line is forwarded at: warnings and errors, and the
/// lines that say the node is up. `stderr` lines without a level (a panic,
/// the CLI's final `Error: …`) are warnings at least.
fn forward_level(line: &str, stderr: bool) -> Option<tracing::Level> {
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
/// there is none), with the merge pipeline always at `info`. The release
/// waits for that pipeline's "completed" line (`merges::NodeEvents`), so a
/// quieter filter, or one naming only the pipeline's crates, must not hide it.
fn writer_log_filter(inherited: Option<&str>) -> String {
    let base = inherited
        .map(str::trim)
        .filter(|f| !f.is_empty())
        .unwrap_or("info");
    format!("{base},quickwit_indexing::actors::merge_pipeline=info")
}

/// A Quickwit indexer node run as a child process for the length of a
/// release: the one writer of the file-backed metastore. Its output goes to
/// the job's console: warnings, errors and readiness through `tracing`, and
/// the last lines into error messages.
pub struct QuickwitNode {
    pid: Option<u32>,
    pub url: String,
    work_dir: PathBuf,
    tail: Tail,
    events: Arc<NodeEvents>,
    /// Owns the process: waits for it to exit, then records why (`supervise`).
    supervisor: tokio::task::JoinHandle<()>,
    /// Why the process exited, once it has.
    exit: tokio::sync::watch::Receiver<Option<String>>,
}

impl Drop for QuickwitNode {
    /// A node that wasn't stopped (a release that failed) is killed: the
    /// supervisor owns the child, which is killed when it is dropped.
    fn drop(&mut self) {
        self.supervisor.abort();
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
        (Some(9), _) => why.push_str(&format!(
            ", most likely by the kernel's out-of-memory killer{}",
            if limit.is_empty() { String::new() } else { format!(" ({})", &limit[2..]) }
        )),
        _ => {}
    }
    why
}

/// Wait for the writer to exit, let the readers take in its last output,
/// then record why it exited in `events` and on the returned channel.
fn supervise(
    mut child: tokio::process::Child,
    mut readers: Vec<tokio::task::JoinHandle<()>>,
    tail: Tail,
    events: Arc<NodeEvents>,
) -> (
    tokio::task::JoinHandle<()>,
    tokio::sync::watch::Receiver<Option<String>>,
) {
    let (tx, rx) = tokio::sync::watch::channel(None);
    let task = tokio::spawn(async move {
        let status = child.wait().await;
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
        let mut config = format!(
            "version: 0.8\ncluster_id: usnm-writer\nnode_id: writer\nlisten_address: 127.0.0.1\n\
             rest:\n  listen_port: {port}\ngrpc_listen_port: {}\ndata_dir: {}\n\
             metastore_uri: {metastore}\ndefault_index_root_uri: {index_root}\n\
             ingest_api:\n  shard_throughput_limit: {SHARD_THROUGHPUT_LIMIT}\n  \
             max_queue_memory_usage: {INGEST_QUEUE_MEMORY}\n  \
             max_queue_disk_usage: {WAL_DISK_BYTES}\n\
             indexer:\n  split_store_max_num_bytes: {SPLIT_STORE_BYTES}\n  \
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
        let mut cmd = tokio::process::Command::new(bin);
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
            .arg(&config_path)
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
            .with_context(|| format!("starting {}", bin.display()))?;
        let tail = Tail::default();
        let events = Arc::new(NodeEvents::default());
        let mut readers = Vec::new();
        if let Some(out) = child.stdout.take() {
            readers.push(forward(out, false, tail.clone(), events.clone()));
        }
        if let Some(err) = child.stderr.take() {
            readers.push(forward(err, true, tail.clone(), events.clone()));
        }
        let pid = child.id();
        let (supervisor, mut exit) = supervise(child, readers, tail.clone(), events.clone());
        let node = Self {
            pid,
            url: format!("http://127.0.0.1:{port}"),
            work_dir: work_dir.to_owned(),
            tail,
            events,
            supervisor,
            exit: exit.clone(),
        };
        let http = reqwest::Client::new();
        for _ in 0..120 {
            // A node that has already exited won't become ready.
            if exit.borrow().is_some() {
                break;
            }
            if http
                .get(format!("{}/health/readyz", node.url))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(node);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        // An exited node: the supervisor records its last output first.
        let _ = tokio::time::timeout(Duration::from_secs(5), exit.wait_for(Option::is_some)).await;
        bail!(
            "Quickwit writer did not become ready; its last output:\n{}",
            node.tail.lines().join("\n")
        )
    }

    /// What the node's output has reported so far.
    pub fn events(&self) -> Arc<NodeEvents> {
        self.events.clone()
    }

    /// The node's process id.
    pub fn pid(&self) -> Option<u32> {
        self.pid
    }

    /// Why the node exited, if it has.
    pub fn exited(&self) -> Option<String> {
        self.exit.borrow().clone()
    }

    /// Stop the node and wait for it to exit.
    pub async fn stop(mut self) -> anyhow::Result<()> {
        if let (Some(pid), None) = (self.pid, self.exited()) {
            // SIGTERM lets Quickwit shut down cleanly; `kill` is a shell builtin.
            let _ = tokio::process::Command::new("sh")
                .args(["-c", "kill -TERM \"$0\"", &pid.to_string()])
                .status()
                .await;
        }
        let stopped =
            tokio::time::timeout(Duration::from_secs(60), self.exit.wait_for(Option::is_some))
                .await;
        if stopped.is_err() {
            // Expected after a release: the closed index's ingest shards
            // have no pipeline left to drain them, and the ingester waits
            // for that. Everything is published by now, and the next
            // writer starts from an empty data directory.
            tracing::info!("the Quickwit writer did not stop within 60 s; killing it");
            // Dropping the supervisor's child kills it (`kill_on_drop`).
            self.supervisor.abort();
        }
        let _ = tokio::time::timeout(Duration::from_secs(5), &mut self.supervisor).await;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn the_writer_always_logs_merge_pipeline_completion() {
        let pipeline = "quickwit_indexing::actors::merge_pipeline=info";
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
        assert!(
            err.contains("2026-09-29T00:20:17Z  WARN quickwit_config: peer seeds are empty"),
            "{err}"
        );
        assert!(err.contains("Error: data dir volume too small"), "{err}");
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
            limit: None,
            oom_kills: Some(0),
        };
        assert!(!describe_exit(killed, Some(none)).contains("ran out of memory"));
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
        let (task, mut exit) = supervise(child, readers, tail, events.clone());
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

    #[tokio::test]
    async fn requests_stay_under_the_ingest_limit() {
        let mut s = QuickwitSink::new("http://127.0.0.1:9", "file:///tmp/x").unwrap();
        s.index = Some("i".into());
        // Documents that fit are buffered without sending.
        let doc = serde_json::json!({"doc_id": "a", "text": "x".repeat(1000)});
        s.add(&doc).await.unwrap();
        assert!(s.buf.len() < CHUNK_BYTES);
        // One document over the limit is refused, not sent.
        let big = serde_json::json!({"doc_id": "big", "text": "x".repeat(CHUNK_BYTES)});
        let err = s.add(&big).await.unwrap_err().to_string();
        assert!(err.contains("`big`"), "{err}");
    }

    /// A fake ingest endpoint that answers with `statuses` in turn (then 200)
    /// and records every body it receives.
    async fn fake_node(statuses: Vec<u16>) -> (QuickwitSink, Arc<Mutex<Vec<Vec<u8>>>>) {
        use axum::http::StatusCode;
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(statuses)));
        let app = axum::Router::new().route(
            "/api/v1/{id}/ingest",
            axum::routing::post({
                let seen = seen.clone();
                move |body: axum::body::Bytes| async move {
                    seen.lock().unwrap().push(body.to_vec());
                    match queue.lock().unwrap().pop_front() {
                        Some(code) => (
                            StatusCode::from_u16(code).unwrap(),
                            r#"{"message":"ingest service is unavailable (no shards available)"}"#,
                        ),
                        None => (StatusCode::OK, r#"{"num_rejected_docs":0}"#),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut s = QuickwitSink::new(&format!("http://{addr}"), "file:///tmp/x").unwrap();
        s.index = Some("i".into());
        s.retry_initial = Duration::from_millis(1);
        s.retry_for = Duration::from_secs(5);
        (s, seen)
    }

    #[tokio::test]
    async fn ingest_retries_while_the_node_pushes_back() {
        let (mut s, seen) = fake_node(vec![503, 429, 503]).await;
        s.buf = b"{\"doc_id\":\"a\"}\n".to_vec();
        s.send("auto").await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().all(|b| b == b"{\"doc_id\":\"a\"}\n"));
        // Progress counts what was accepted, and the retries by status.
        let stats = s.stats();
        assert_eq!((stats.docs_sent(), stats.bytes_sent()), (1, 15));
        assert_eq!((stats.retries_429(), stats.retries_503()), (1, 2));
    }

    #[tokio::test]
    async fn ingest_gives_up_after_the_retry_window() {
        let (mut s, _) = fake_node(vec![503; 1000]).await;
        s.retry_for = Duration::from_millis(50);
        s.buf = b"{}\n".to_vec();
        let err = s.send("auto").await.unwrap_err().to_string();
        assert!(err.contains("503"), "{err}");
    }

    #[tokio::test]
    async fn other_ingest_errors_are_not_retried() {
        let (mut s, seen) = fake_node(vec![400]).await;
        s.buf = b"{}\n".to_vec();
        let err = s.send("auto").await.unwrap_err().to_string();
        assert!(err.contains("400"), "{err}");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
