//! Where index documents go: JSONL files (the API's memory backend, local
//! development and tests) or a Quickwit writer node (08 §8.4.1).

use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{bail, Context};
use async_trait::async_trait;
use opentelemetry::KeyValue;
use serde_json::Value;

use crate::catalog::PeriodCuts;
use crate::merges::IndexLayout;
use crate::telemetry;

mod node;
mod quickwit;
mod template;

pub use node::{describe_exit, QuickwitNode, Tail, WriterProcess};
pub use quickwit::{IngestApi, LineSender, Poster, QuickwitSink};
pub use template::{
    main_template, page_language_fields, split_target, text_fields, Decades, MainLayout,
    WriterTuning, MAX_NUM_PARTITIONS, MAX_SPLIT_DOCS,
};

/// The index config every base and delta shares (05 §5.5.1).
pub const INDEX_TEMPLATE: &str = include_str!("../../../infra/quickwit/pages-index.yaml");

/// The node's in-memory ingest queue (Quickwit's default is 2 GiB). The first
/// prod release ran out of memory at 4 GiB with the defaults.
const INGEST_QUEUE_MEMORY: &str = "1GiB";

#[async_trait]
pub trait IndexSink: Send {
    /// Start a new, empty main index, laid out as `layout` says: by decade
    /// (05 §5.5.5) and in its text fields (05 §5.5.7). Fails if it already
    /// exists.
    async fn create(&mut self, index_id: &str, layout: MainLayout) -> anyhow::Result<()>;
    /// Create an index with another mapping (the Japanese pages',
    /// `ocr_ja::JA_TEMPLATE`). Sinks without mappings just create it.
    async fn create_with(&mut self, index_id: &str, _template: &str) -> anyhow::Result<()> {
        self.create(index_id, MainLayout::default()).await
    }
    async fn add(&mut self, doc: &Value) -> anyhow::Result<()>;
    /// One document as the JSON line `add` would send, without its newline:
    /// for documents already written out (`crate::decade_order`).
    async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        self.add(&serde_json::from_slice(line)?).await
    }
    /// Flush and confirm the index holds exactly `expected` documents.
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()>;
    /// `memory` or `quickwit`, recorded in `current.json`.
    fn backend(&self) -> &'static str;
    /// The search engine's name and version ("Quickwit 0.9.1"), for the
    /// version's build record (#161): the engine this sink writes to, however
    /// it was started. `None` when the sink has no engine or can't tell.
    async fn engine(&mut self) -> Option<String> {
        None
    }
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
    /// An owned sender to the current index, which `crate::decade_order`
    /// runs beside the release so reading goes on while a decade is sent.
    /// `None`: the sink is fed in line, as before.
    async fn line_sender(&mut self) -> anyhow::Result<Option<LineSender>> {
        Ok(None)
    }
    /// Take up the main index `index_id` a release that stopped wrote, laid
    /// out as `layout` says, in place of [`IndexSink::create`]: nothing is
    /// added to it, and [`IndexSink::finish`] then counts and seals it as it
    /// would a new one (`crate::finish`).
    async fn attach(&mut self, index_id: &str, _layout: MainLayout) -> anyhow::Result<()> {
        bail!("this sink can't take up `{index_id}`, an index a stopped release wrote")
    }
    /// Whether `index_id` exists.
    async fn has_index(&mut self, index_id: &str) -> anyhow::Result<bool> {
        bail!("this sink can't tell whether `{index_id}` exists")
    }
    /// The documents `index_id` holds where searches see them (Quickwit's
    /// published splits).
    async fn published_docs(&mut self, index_id: &str) -> anyhow::Result<u64> {
        bail!("this sink can't count `{index_id}`")
    }
    /// The documents of `index_id` by decade, when it is partitioned by
    /// decade (05 §5.5.5) and the sink can tell; `None` otherwise.
    async fn decade_docs(
        &mut self,
        _index_id: &str,
    ) -> anyhow::Result<Option<std::collections::BTreeMap<u16, u64>>> {
        Ok(None)
    }
    /// What `index_id`'s documents of each title carry from the catalog
    /// (`crate::finish::TitleFields`, by LCCN), when the sink can tell. For
    /// the titles in `cuts` (`crate::catalog::Catalog::period_cuts`) it
    /// counts the documents in each place by period too, every one of them.
    async fn title_fields(
        &mut self,
        _index_id: &str,
        _cuts: &PeriodCuts,
    ) -> anyhow::Result<Option<TitleFieldsByLccn>> {
        Ok(None)
    }
}

/// [`IndexSink::title_fields`]' answer.
pub type TitleFieldsByLccn = std::collections::BTreeMap<String, crate::finish::TitleFields>;

/// Fold one document's catalog fields into `by` ([`IndexSink::title_fields`]).
pub(crate) fn add_title_fields(
    by: &mut TitleFieldsByLccn,
    doc: &Value,
    cuts: &PeriodCuts,
) -> anyhow::Result<()> {
    let text = |k: &str| {
        doc[k]
            .as_str()
            .map(str::to_owned)
            .with_context(|| format!("a document without `{k}`: {}", doc["doc_id"]))
    };
    let ordinal = doc["sort_key"]
        .as_u64()
        .with_context(|| format!("a document without `sort_key`: {}", doc["doc_id"]))?
        >> 32;
    let f = by
        .entry(text("lccn")?)
        .or_insert_with(|| crate::finish::TitleFields {
            ordinals: (ordinal, ordinal),
            ..Default::default()
        });
    let place_id = text("place_id")?;
    // A title with places by date: in the period its `day` falls in.
    if let Some(cuts) = cuts.get(&text("lccn")?) {
        let day = u32::try_from(
            doc["day"]
                .as_u64()
                .with_context(|| format!("a document without `day`: {}", doc["doc_id"]))?,
        )?;
        f.by_period
            .entry(place_id.clone())
            .or_insert_with(|| vec![0; cuts.len() + 1])[cuts.partition_point(|&c| c <= day)] += 1;
    }
    f.places.insert(place_id);
    f.states.insert(text("state")?);
    f.shards.insert(
        doc["place_shard"]
            .as_u64()
            .with_context(|| format!("a document without `place_shard`: {}", doc["doc_id"]))?,
    );
    for l in doc["language"].as_array().into_iter().flatten() {
        if let Some(l) = l.as_str().filter(|l| !l.is_empty()) {
            f.languages.insert(l.to_owned());
        }
    }
    f.ordinals = (f.ordinals.0.min(ordinal), f.ordinals.1.max(ordinal));
    Ok(())
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
    /// The Quickwit writer node's process, for its memory use: 0 for none.
    /// A restart (ingest v1's final merges) changes it.
    node_pid: AtomicU32,
}

impl SinkStats {
    /// Counters of their own, reporting the free space of `work_dir`: a
    /// finish's replay counts the documents it makes again here, so the
    /// release's progress lines follow it (`crate::finish`).
    pub fn replaying(work_dir: Option<PathBuf>) -> Self {
        Self {
            work_dir,
            ..Self::default()
        }
    }

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
    pub fn node_pid(&self) -> Option<u32> {
        Some(self.node_pid.load(Ordering::Relaxed)).filter(|&p| p != 0)
    }
    fn set_node_pid(&self, pid: Option<u32>) {
        self.node_pid.store(pid.unwrap_or(0), Ordering::Relaxed);
    }

    pub(crate) fn sent(&self, docs: u64, bytes: u64) {
        self.docs_sent.fetch_add(docs, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
        let m = telemetry::metrics();
        m.docs_sent.add(docs, &[]);
        m.bytes_sent.add(bytes, &[]);
    }

    /// Count documents a finish's replay made again (`SinkStats::replaying`):
    /// the progress lines follow them, the sent-documents metric doesn't.
    pub(crate) fn replayed(&self, docs: u64, bytes: u64) {
        self.docs_sent.fetch_add(docs, Ordering::Relaxed);
        self.bytes_sent.fetch_add(bytes, Ordering::Relaxed);
    }

    fn retried(&self, status: u16) {
        match status {
            429 => self.retries_429.fetch_add(1, Ordering::Relaxed),
            503 => self.retries_503.fetch_add(1, Ordering::Relaxed),
            // v1's 404 before the index's queue exists: only the metric.
            _ => 0,
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
    /// The memory backend has no splits: the layout only changes the
    /// documents, which the release writes.
    async fn create(&mut self, index_id: &str, _layout: MainLayout) -> anyhow::Result<()> {
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
        self.add_line(&serde_json::to_vec(doc)?).await
    }

    async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        use std::io::Write;
        let (_, w) = self.current.as_mut().context("no index created")?;
        w.write_all(line)?;
        w.write_all(b"\n")?;
        self.stats.sent(1, line.len() as u64 + 1);
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

    async fn attach(&mut self, index_id: &str, _layout: MainLayout) -> anyhow::Result<()> {
        let path = self.dir.join(format!("{index_id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .with_context(|| format!("{} (the stopped release's index)", path.display()))?;
        self.current = Some((path, std::io::BufWriter::new(file)));
        Ok(())
    }

    async fn has_index(&mut self, index_id: &str) -> anyhow::Result<bool> {
        Ok(self.dir.join(format!("{index_id}.jsonl")).exists())
    }

    async fn published_docs(&mut self, index_id: &str) -> anyhow::Result<u64> {
        use std::io::BufRead;
        let path = self.dir.join(format!("{index_id}.jsonl"));
        let file = std::fs::File::open(&path).with_context(|| path.display().to_string())?;
        Ok(std::io::BufReader::new(file).lines().count() as u64)
    }

    /// Read from the documents' own `decade` field.
    async fn decade_docs(
        &mut self,
        index_id: &str,
    ) -> anyhow::Result<Option<std::collections::BTreeMap<u16, u64>>> {
        use std::io::BufRead;
        let path = self.dir.join(format!("{index_id}.jsonl"));
        let file = std::fs::File::open(&path).with_context(|| path.display().to_string())?;
        let mut by = std::collections::BTreeMap::new();
        for line in std::io::BufReader::new(file).lines() {
            let doc: Value = serde_json::from_str(&line?)?;
            let Some(decade) = doc["decade"].as_u64().and_then(|d| u16::try_from(d).ok()) else {
                return Ok(None);
            };
            *by.entry(decade).or_default() += 1;
        }
        Ok(Some(by))
    }

    async fn title_fields(
        &mut self,
        index_id: &str,
        cuts: &PeriodCuts,
    ) -> anyhow::Result<Option<TitleFieldsByLccn>> {
        use std::io::BufRead;
        let path = self.dir.join(format!("{index_id}.jsonl"));
        let file = std::fs::File::open(&path).with_context(|| path.display().to_string())?;
        let mut by = TitleFieldsByLccn::new();
        for line in std::io::BufReader::new(file).lines() {
            add_title_fields(&mut by, &serde_json::from_str(&line?)?, cuts)?;
        }
        Ok(Some(by))
    }

    fn backend(&self) -> &'static str {
        "memory"
    }

    fn stats(&self) -> Arc<SinkStats> {
        self.stats.clone()
    }
}
