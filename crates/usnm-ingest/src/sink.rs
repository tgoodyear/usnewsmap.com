//! Where index documents go: JSONL files (the API's memory backend, local
//! development and tests) or a Quickwit writer node (08 §8.4.1).

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use opentelemetry::KeyValue;
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};

use usnm_core::text_layout::TextLayout;

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

/// How hard the writer may work for one run (#172): Quickwit's indexing heap
/// and commit timeout for the new index, and the node's in-memory ingest
/// queue. The defaults are the index template's and [`INGEST_QUEUE_MEMORY`],
/// sized for the Consumption profile's 7.5 GiB container. On the E4 profile
/// (26 GiB, a full rebuild) the ingest job sets more through
/// `USNM_WRITER_HEAP`, `USNM_WRITER_COMMIT_SECS` and `USNM_WRITER_QUEUE`: with
/// a heap that holds a whole 30,000-page split and a commit timeout longer than
/// it takes to index one, the indexer writes splits at the target size, so
/// they need little merging. At 30 s, a full rebuild's indexer cut splits of
/// about 7,000 pages, and merging them cost the writer its memory and much of
/// its CPU.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WriterTuning {
    pub heap: String,
    pub commit_timeout_secs: u32,
    pub queue: String,
}

/// How a main index lays out its pages by decade (05 §5.5.5, #123). Only
/// this run's new main index takes it; the Japanese index never does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Decades {
    /// No `decade` field: the template as it is.
    #[default]
    Off,
    /// The `decade` field, recorded as a split tag (`tag_fields`): a delta
    /// on a version laid out by decade. Its splits mix decades as before,
    /// but a search skips each split that holds none of its decades.
    Tagged,
    /// Also `partition_key: decade`, so every split holds one decade: a
    /// full base built with `--partition-decade`.
    Partitioned,
}

/// The template lines [`Decades`] adds to: the doc mapping's timestamp
/// field and the `year` field mapping.
const TEMPLATE_TIMESTAMP: &str = "timestamp_field: date";
const TEMPLATE_YEAR: &str = "- { name: year, type: u64, fast: true, indexed: true }";

/// Partitions an indexer keeps apart at once, more than the 15 decades
/// (1820s to 1960s, `usnm_core::decade`). Past it, Quickwit 0.9.1 puts the
/// pages of further decades into one shared split, which a search can't
/// skip. Each partition has its own in-memory split while the indexer
/// fills, all within the one indexing heap.
pub const MAX_NUM_PARTITIONS: u32 = 32;

impl Decades {
    /// Whether the pages carry the `decade` field.
    pub fn on(self) -> bool {
        self != Decades::Off
    }

    /// `template` with the `decade` field, and its tag and partitioning as
    /// this layout has them. `Off` leaves it as it is. Each line it adds to
    /// must be there exactly once, so a template change can't silently
    /// drop the layout.
    pub fn apply(self, template: &str) -> anyhow::Result<String> {
        if self == Decades::Off {
            return Ok(template.to_owned());
        }
        let mut found = (0, 0);
        let mut out = Vec::new();
        for line in template.lines() {
            let indent = &line[..line.len() - line.trim_start().len()];
            out.push(line.to_owned());
            match line.trim() {
                TEMPLATE_TIMESTAMP => {
                    found.0 += 1;
                    out.push(format!(
                        "{indent}# Decade partitions (05 §5.5.5, #123): searches name their \
                         decades (`decade:IN [...]`), and Quickwit skips the splits of the others."
                    ));
                    out.push(format!("{indent}tag_fields: [decade]"));
                    if self == Decades::Partitioned {
                        out.push(format!("{indent}partition_key: decade"));
                        out.push(format!("{indent}max_num_partitions: {MAX_NUM_PARTITIONS}"));
                    }
                }
                TEMPLATE_YEAR => {
                    found.1 += 1;
                    out.push(format!(
                        "{indent}# The page's decade, from 1820 (usnm_core::decade::of_year)."
                    ));
                    out.push(format!(
                        "{indent}- {{ name: decade, type: u64, fast: false, indexed: true, stored: false }}"
                    ));
                }
                _ => {}
            }
        }
        if found != (1, 1) {
            bail!(
                "the index template needs exactly one `{TEMPLATE_TIMESTAMP}` and one `{TEMPLATE_YEAR}` line to add the decade to, not {} and {}",
                found.0,
                found.1
            );
        }
        let mut out = out.join("\n");
        if template.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }
}

/// How this run's new main index lays its pages out: by decade (05
/// §5.5.5), in which text fields (05 §5.5.7), with the page languages or
/// not (05 §5.5.8) and in splits of how many pages. Only the main index
/// takes it; the Japanese index keeps its own mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MainLayout {
    pub decades: Decades,
    pub text: TextLayout,
    /// The `page_language` and `page_language_set` fields.
    pub page_language: bool,
    /// The split target in place of the template's (`--split-docs`, #283):
    /// only a full base takes one. `None` keeps the template's.
    pub split_docs: Option<u64>,
}

impl From<Decades> for MainLayout {
    fn from(decades: Decades) -> Self {
        MainLayout {
            decades,
            ..MainLayout::default()
        }
    }
}

impl MainLayout {
    /// The pages a merge stops at in this index: `split_docs`, or the
    /// template's `split_num_docs_target`.
    pub fn split_target(&self) -> u64 {
        self.split_docs
            .unwrap_or_else(merges::template_split_target)
    }
}

/// The main index's config for this run: the template with the writer's
/// tuning, the split target, the decade layout, the text fields and the
/// page languages.
pub fn main_template(tuning: &WriterTuning, layout: MainLayout) -> anyhow::Result<String> {
    let mut template = text_fields(layout.text, &tuning.apply(INDEX_TEMPLATE)?)?;
    if let Some(docs) = layout.split_docs {
        template = split_target(docs, &template)?;
    }
    let template = page_language_fields(layout.page_language, &template)?;
    layout.decades.apply(&template)
}

/// The template line the page-language fields follow: the title languages.
const TEMPLATE_LANGUAGE: &str =
    "- { name: language, type: array<text>, tokenizer: raw, fast: true }";

/// `template` with the page-language fields (05 §5.5.8, #289) after the
/// title languages' when `on`; as it is otherwise. That line must be there
/// exactly once, so a template change can't silently drop the fields.
pub fn page_language_fields(on: bool, template: &str) -> anyhow::Result<String> {
    use usnm_core::page_language::{FIELD, SET_FIELD};
    if !on {
        return Ok(template.to_owned());
    }
    let mut found = 0;
    let mut out = Vec::new();
    for line in template.lines() {
        let indent = &line[..line.len() - line.trim_start().len()];
        out.push(line.to_owned());
        if line.trim() == TEMPLATE_LANGUAGE {
            found += 1;
            out.push(format!(
                "{indent}# The page's own languages, found from its words (05 §5.5.8, #289): \
                 `lang=` matches them"
            ));
            out.push(format!(
                "{indent}# and the counts by language read them. Sorted; a mixed page has two."
            ));
            out.push(format!(
                "{indent}- {{ name: {FIELD}, type: array<text>, tokenizer: raw, fast: true }}"
            ));
            out.push(format!(
                "{indent}# The same languages as one keyword (`eng_ger`, `none`): the filter's \
                 complement side."
            ));
            out.push(format!(
                "{indent}- {{ name: {SET_FIELD}, type: text, tokenizer: raw, indexed: true, \
                 stored: false, fast: false }}"
            ));
        }
    }
    if found != 1 {
        bail!(
            "the index template needs exactly one `{TEMPLATE_LANGUAGE}` line to add the page \
             languages after, not {found}"
        );
    }
    let mut out = out.join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The template line [`split_target`] replaces.
const TEMPLATE_SPLIT_TARGET: &str = "split_num_docs_target: 30000";

/// The split targets [`split_target`] takes are below this. A
/// million-page split (about 23 GB before the common-word pairs) killed
/// the writer while it uploaded in October 2026 (08 §8.4).
pub const MAX_SPLIT_DOCS: u64 = 1_000_000;

/// `template` with `split_num_docs_target: docs` (#283): a merge stops
/// taking splits once it holds that many pages, so a larger target leaves
/// fewer, larger splits. The line must be there exactly once, so a
/// template change can't silently keep the template's target.
pub fn split_target(docs: u64, template: &str) -> anyhow::Result<String> {
    if docs == 0 || docs >= MAX_SPLIT_DOCS {
        bail!("the split target must be above 0 and below {MAX_SPLIT_DOCS}, not {docs}");
    }
    let found = template
        .lines()
        .filter(|l| l.trim() == TEMPLATE_SPLIT_TARGET)
        .count();
    if found != 1 {
        bail!("the index template needs exactly one `{TEMPLATE_SPLIT_TARGET}` line, not {found}");
    }
    let mut out = template
        .lines()
        .map(|l| {
            if l.trim() == TEMPLATE_SPLIT_TARGET {
                let indent = &l[..l.len() - l.trim_start().len()];
                format!("{indent}split_num_docs_target: {docs}")
            } else {
                l.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The template's text field mappings for [`TextLayout::Separate`], in the
/// order they appear: LoC's text and pairs, American Stories' text and pairs.
const TEMPLATE_TEXT_FIELDS: [&str; 4] = [
    "- { name: text, type: text, tokenizer: usnm_text, record: position, stored: true, fieldnorms: false }",
    "- { name: text_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
    "- { name: text_as, type: text, tokenizer: usnm_text, record: position, stored: true, fieldnorms: false }",
    "- { name: text_as_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
];
const TEMPLATE_SEARCH_FIELDS: &str = "default_search_fields: [text]";

/// `template` with the text fields of `layout` (05 §5.5.7). `Separate`
/// leaves it as it is. `Single` keeps `text` and `text_as` stored but not
/// indexed, drops their pairs, and adds `text_all` and `text_all_cg`, each
/// with the comment lines above it replaced. Each line it changes must be
/// there exactly once, so a template change can't silently keep a searched
/// field of one text.
pub fn text_fields(layout: TextLayout, template: &str) -> anyhow::Result<String> {
    if layout == TextLayout::Separate {
        return Ok(template.to_owned());
    }
    let mut found = [0usize; 5];
    let mut out: Vec<String> = Vec::new();
    for line in template.lines() {
        let indent = &line[..line.len() - line.trim_start().len()];
        let trimmed = line.trim();
        if let Some(i) = TEMPLATE_TEXT_FIELDS.iter().position(|f| *f == trimmed) {
            found[i] += 1;
            // The comment above the field described its old role.
            while out
                .last()
                .is_some_and(|l| l.trim_start().starts_with('#') && l.starts_with(indent))
            {
                out.pop();
            }
            if i == 0 {
                for l in [
                    "# One searched field for both texts (05 §5.5.7, #283). LoC's text (or",
                    "# our OCR's Latin text, #203) and American Stories' (05 §5.5.4) are",
                    "# stored for snippets and `matched_in`, not searched.",
                    "- { name: text, type: text, indexed: false, stored: true }",
                    "- { name: text_as, type: text, indexed: false, stored: true }",
                    "# Both texts' words, one token per word as the release folds them,",
                    "# with 32 positions no query can match between the texts",
                    "# (usnm_core::text_layout::index_fields), and their common-word",
                    "# pairs (05 §5.5.3), made separately for each text.",
                    "- { name: text_all, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
                    "- { name: text_all_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
                ] {
                    out.push(format!("{indent}{l}"));
                }
            }
        } else if trimmed == TEMPLATE_SEARCH_FIELDS {
            found[4] += 1;
            out.push(format!(
                "{indent}default_search_fields: [{}]",
                usnm_core::text_layout::FIELD
            ));
        } else {
            out.push(line.to_owned());
        }
    }
    if found != [1; 5] {
        bail!(
            "the index template needs each of its four text fields and `{TEMPLATE_SEARCH_FIELDS}` \
             exactly once to lay them out in one field, not {found:?}"
        );
    }
    let mut out = out.join("\n");
    if template.ends_with('\n') {
        out.push('\n');
    }
    Ok(out)
}

/// The template lines [`WriterTuning`] replaces.
const TEMPLATE_HEAP: &str = "heap_size: 1GiB";
const TEMPLATE_COMMIT: &str = "commit_timeout_secs: 30";

impl Default for WriterTuning {
    fn default() -> Self {
        WriterTuning {
            heap: "1GiB".into(),
            commit_timeout_secs: 30,
            queue: INGEST_QUEUE_MEMORY.into(),
        }
    }
}

impl WriterTuning {
    /// From the environment, the defaults for what isn't set.
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|k| std::env::var(k).ok().filter(|v| !v.is_empty()))
    }

    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let d = WriterTuning::default();
        // A whole number of MiB or GiB above zero, with exactly one unit.
        let size = |key: &str, default: String| -> anyhow::Result<String> {
            let Some(v) = get(key) else {
                return Ok(default);
            };
            let amount = v.strip_suffix("GiB").or_else(|| v.strip_suffix("MiB"));
            match amount {
                Some(n)
                    if n.bytes().all(|b| b.is_ascii_digit())
                        && n.parse::<u64>().is_ok_and(|n| n > 0) =>
                {
                    Ok(v)
                }
                _ => bail!("{key} must be a size like 6GiB or 512MiB, not `{v}`"),
            }
        };
        let commit_timeout_secs = match get("USNM_WRITER_COMMIT_SECS") {
            None => d.commit_timeout_secs,
            Some(v) => match v.parse::<u32>() {
                Ok(n) if (1..=3600).contains(&n) => n,
                _ => bail!("USNM_WRITER_COMMIT_SECS must be 1 to 3600 seconds, not `{v}`"),
            },
        };
        Ok(WriterTuning {
            heap: size("USNM_WRITER_HEAP", d.heap)?,
            commit_timeout_secs,
            queue: size("USNM_WRITER_QUEUE", d.queue)?,
        })
    }

    /// `template` with this run's heap and commit timeout. Each must be an
    /// active setting line (not a comment) and there must be exactly one of
    /// each, so a template change can't silently drop the tuning.
    pub fn apply(&self, template: &str) -> anyhow::Result<String> {
        let mut found = (0, 0);
        let lines: Vec<String> = template
            .lines()
            .map(|line| {
                let indent = &line[..line.len() - line.trim_start().len()];
                match line.trim() {
                    TEMPLATE_HEAP => {
                        found.0 += 1;
                        format!("{indent}heap_size: {}", self.heap)
                    }
                    TEMPLATE_COMMIT => {
                        found.1 += 1;
                        format!("{indent}commit_timeout_secs: {}", self.commit_timeout_secs)
                    }
                    _ => line.to_owned(),
                }
            })
            .collect();
        if found != (1, 1) {
            bail!(
                "the index template needs exactly one `{TEMPLATE_HEAP}` and one `{TEMPLATE_COMMIT}` setting to tune, not {} and {}",
                found.0,
                found.1
            );
        }
        let mut out = lines.join("\n");
        if template.ends_with('\n') {
            out.push('\n');
        }
        Ok(out)
    }
}
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

/// Which of Quickwit's ingest APIs the release writes through
/// (`USNM_WRITER_INGEST`, #283).
///
/// Ingest v2 (the default) delivers at least once. Its router gives each
/// persist to the ingester 6 s and then sends the whole request again, up
/// to 5 times, but a persist that timed out isn't cancelled: it waits for
/// the ingester's lock and appends its documents anyway. When the lock was
/// held for longer (26 s, once in a 57-hour full rebuild), the node answered
/// 200 and kept two to four copies of a request's pages: 121 extra
/// documents in 23.7 million, which the exact count caught.
///
/// Ingest v1 (`?use_legacy_ingest=true`) appends a request to the node's
/// queue once, before it answers. It answers 429 when the queue is full and
/// 404 before the index's queue exists, both before it appends anything;
/// those requests are sent again, and nothing else is. Its source can't be
/// closed, so the release runs its final merges by restarting the writer
/// (`merges::seal_restarting`). It needs a writer node the release runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum IngestApi {
    #[default]
    V2,
    V1,
}

impl IngestApi {
    pub const VAR: &'static str = "USNM_WRITER_INGEST";

    /// `v2` (or unset or empty) or `v1`.
    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value.map(str::trim) {
            None | Some("") | Some("v2") => Ok(Self::V2),
            Some("v1") => Ok(Self::V1),
            Some(other) => bail!("{} must be `v1` or `v2`, not `{other}`", Self::VAR),
        }
    }

    pub fn from_env() -> anyhow::Result<Self> {
        Self::parse(std::env::var(Self::VAR).ok().as_deref())
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::V2 => "v2",
            Self::V1 => "v1",
        }
    }

    /// Whether a request answered `status` was certainly not appended, so
    /// sending it again can't duplicate anything.
    fn retry(self, status: reqwest::StatusCode) -> bool {
        use reqwest::StatusCode as S;
        match self {
            // At least once: `finish` checks the count, so a duplicate
            // fails the release rather than go unnoticed.
            Self::V2 => status == S::SERVICE_UNAVAILABLE || status == S::TOO_MANY_REQUESTS,
            Self::V1 => status == S::TOO_MANY_REQUESTS || status == S::NOT_FOUND,
        }
    }
}

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
    fn line_sender(&self) -> Option<LineSender> {
        None
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
    /// The Quickwit writer node's process, for its memory use: 0 for none.
    /// A restart (ingest v1's final merges) changes it.
    node_pid: AtomicU32,
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
    pub fn node_pid(&self) -> Option<u32> {
        Some(self.node_pid.load(Ordering::Relaxed)).filter(|&p| p != 0)
    }
    fn set_node_pid(&self, pid: Option<u32>) {
        self.node_pid.store(pid.unwrap_or(0), Ordering::Relaxed);
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
    /// The new index's split target when it isn't the template's (a main
    /// index with `MainLayout::split_docs`), for the merge checks.
    split_docs: Option<u64>,
    /// The writer node's output, when this release runs the node.
    events: Option<Arc<NodeEvents>>,
    /// The writer node's process, when this release runs the node: ingest
    /// v1 restarts it for the final merges.
    process: Option<Arc<WriterProcess>>,
    ingest: IngestApi,
}

/// Posts ingest requests to one index on the writer node: the request,
/// v1/v2's retries, and the check that the node took every document.
#[derive(Clone)]
struct Poster {
    http: reqwest::Client,
    base: String,
    id: String,
    ingest: IngestApi,
    retry_initial: Duration,
    retry_for: Duration,
    stats: Arc<SinkStats>,
    events: Option<Arc<NodeEvents>>,
}

impl Poster {
    async fn post(&self, commit: &str, body: bytes::Bytes) -> anyhow::Result<()> {
        if let Some(e) = &self.events {
            e.check()?;
        }
        let id = self.id.as_str();
        let url = match self.ingest {
            IngestApi::V2 => format!("{}/api/v1/{id}/ingest?commit={commit}", self.base),
            IngestApi::V1 => format!(
                "{}/api/v1/{id}/ingest?commit={commit}&use_legacy_ingest=true",
                self.base
            ),
        };
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
            // A request with no answer (a timeout, a dropped connection) may
            // have been taken: it is never sent again.
            let resp = merges::explained(self.events.as_deref(), sent).await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // The whole request is sent again.
            if !self.ingest.retry(status) || tokio::time::Instant::now() + pause > deadline {
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
        let docs = body.iter().filter(|&&b| b == b'\n').count() as u64;
        // v1 doesn't count rejected documents (it drops them as it indexes);
        // the final count catches them.
        let accepted = match self.ingest {
            IngestApi::V2 => v["num_rejected_docs"].as_u64() == Some(0),
            IngestApi::V1 => v["num_docs_for_processing"].as_u64() == Some(docs),
        };
        if !accepted {
            bail!(
                "ingest into `{id}` did not take all {docs} documents: {}",
                text.chars().take(500).collect::<String>()
            );
        }
        self.stats.sent(docs, body.len() as u64);
        Ok(())
    }
}

/// JSON lines to one index, in requests of at most [`CHUNK_BYTES`], one at
/// a time: [`QuickwitSink::line_sender`].
pub struct LineSender {
    poster: Poster,
    buf: Vec<u8>,
}

impl LineSender {
    pub async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if line.len() + 1 > CHUNK_BYTES {
            bail!(
                "a document is {} bytes, over the {CHUNK_BYTES}-byte ingest request limit",
                line.len() + 1
            );
        }
        if self.buf.len() + line.len() + 1 > CHUNK_BYTES {
            let body = bytes::Bytes::from(std::mem::take(&mut self.buf));
            self.poster.post("auto", body).await?;
        }
        self.buf.extend_from_slice(line);
        self.buf.push(b'\n');
        Ok(())
    }

    /// The lines not sent yet (never none once a line was added: a request
    /// goes only when the next line wouldn't fit). The release gives them
    /// back to the sink, whose `finish` sends them with the forced commit,
    /// as when the pages went out in line.
    pub fn into_unsent(self) -> Vec<u8> {
        self.buf
    }
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
            split_docs: None,
            events: None,
            process: None,
            ingest: IngestApi::default(),
        })
    }

    /// Report the free disk of `node`'s work directory and its memory in
    /// release progress, and stop the release if its output reports a full
    /// disk or a failed merge. Call before sending anything.
    pub fn watching(mut self, node: &QuickwitNode) -> Self {
        self.stats = Arc::new(SinkStats {
            work_dir: Some(node.work_dir.clone()),
            node_pid: AtomicU32::new(node.pid().unwrap_or(0)),
            ..SinkStats::default()
        });
        self.events = Some(node.events.clone());
        self.process = Some(node.process.clone());
        self
    }

    /// Write through `api` (`IngestApi::from_env` in a release).
    pub fn ingest(mut self, api: IngestApi) -> Self {
        self.ingest = api;
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
        let body = bytes::Bytes::from(std::mem::take(&mut self.buf));
        self.poster()?.post(commit, body).await
    }

    /// What posting a request to this sink's index needs, owned.
    fn poster(&self) -> anyhow::Result<Poster> {
        Ok(Poster {
            http: self.http.clone(),
            base: self.base.clone(),
            id: self.index.clone().context("no index created")?,
            ingest: self.ingest,
            retry_initial: self.retry_initial,
            retry_for: self.retry_for,
            stats: Arc::clone(&self.stats),
            events: self.events.clone(),
        })
    }

    /// An owned sender of JSON lines to this sink's index, for
    /// `crate::decade_order` to send a decade's file while the release
    /// reads on. It posts the same requests `add_line` would, one at a time.
    pub fn line_sender(&self) -> anyhow::Result<LineSender> {
        Ok(LineSender {
            poster: self.poster()?,
            buf: Vec::new(),
        })
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
    fn line_sender(&self) -> Option<LineSender> {
        QuickwitSink::line_sender(self).ok()
    }

    async fn create(&mut self, index_id: &str, layout: MainLayout) -> anyhow::Result<()> {
        // The main index takes this run's writer tuning and layout; the
        // Japanese index (about 11k pages) keeps its template's.
        let tuning = WriterTuning::from_env()?;
        if tuning != WriterTuning::default() {
            tracing::info!(heap = %tuning.heap, commit_timeout_secs = tuning.commit_timeout_secs, queue = %tuning.queue, "writer tuning");
        }
        if layout.decades.on() {
            tracing::info!(index = index_id, layout = ?layout.decades, "decade layout");
        }
        if layout.text != TextLayout::Separate {
            tracing::info!(
                index = index_id,
                text_layout = layout.text.version(),
                "one searched text field"
            );
        }
        if let Some(docs) = layout.split_docs {
            tracing::info!(index = index_id, split_docs = docs, "split target");
        }
        let template = main_template(&tuning, layout)?;
        self.create_with(index_id, &template).await?;
        self.split_docs = layout.split_docs;
        Ok(())
    }

    async fn create_with(&mut self, index_id: &str, template: &str) -> anyhow::Result<()> {
        let config = template
            .replace("${INDEX_ID}", index_id)
            .replace("${INDEX_URI}", &format!("{}/{index_id}", self.index_root));
        let sent = self
            .http
            .post(format!("{}/api/v1/indexes", self.base))
            .header("content-type", "application/yaml")
            .body(config)
            .send()
            .await
            .map_err(anyhow::Error::from);
        let resp = merges::explained(self.events.as_deref(), sent).await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "creating index `{index_id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        self.index = Some(index_id.to_owned());
        self.split_docs = None;
        Ok(())
    }

    async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
        self.add_line(&serde_json::to_vec(doc)?).await
    }

    async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if line.len() + 1 > CHUNK_BYTES {
            let doc_id = serde_json::from_slice::<Value>(line)
                .ok()
                .and_then(|d| d["doc_id"].as_str().map(str::to_owned));
            bail!(
                "document `{}` is {} bytes, over the {CHUNK_BYTES}-byte ingest request limit",
                doc_id.as_deref().unwrap_or("?"),
                line.len() + 1
            );
        }
        // Send first if this document would push the request over the limit.
        if self.buf.len() + line.len() + 1 > CHUNK_BYTES {
            self.send("auto").await?;
        }
        self.buf.extend_from_slice(line);
        self.buf.push(b'\n');
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
        let split_target = self
            .split_docs
            .unwrap_or_else(merges::template_split_target);
        let layout = match self.ingest {
            IngestApi::V2 => {
                merges::seal(
                    &self.node(),
                    &id,
                    expected,
                    split_target,
                    self.events.as_deref(),
                    &self.merge_wait,
                    self.stats.work_dir.as_deref(),
                )
                .await?
            }
            IngestApi::V1 => {
                let (Some(process), Some(events)) = (&self.process, &self.events) else {
                    bail!(
                        "{}=v1 runs the final merges by restarting the writer: it needs a writer \
                         the release runs (--quickwit-bin)",
                        IngestApi::VAR
                    );
                };
                let writer = Restarting {
                    process,
                    stats: &self.stats,
                };
                merges::seal_restarting(
                    &self.node(),
                    &writer,
                    &id,
                    expected,
                    split_target,
                    events,
                    &self.merge_wait,
                    self.stats.work_dir.as_deref(),
                )
                .await?
            }
        };
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

    async fn engine(&mut self) -> Option<String> {
        let v: Value = self
            .http
            .get(format!("{}/api/v1/version", self.base))
            .send()
            .await
            .ok()?
            .error_for_status()
            .ok()?
            .json()
            .await
            .ok()?;
        let version = v["build"]["version"].as_str()?;
        Some(match v["build"]["commit_short_hash"].as_str() {
            Some(c) if !c.is_empty() => format!("Quickwit {version} ({c})"),
            _ => format!("Quickwit {version}"),
        })
    }

    async fn layout(&mut self, index_ids: &[String]) -> anyhow::Result<Vec<IndexLayout>> {
        let node = self.node();
        let mut out = Vec::new();
        for id in index_ids {
            let splits = merges::explained(self.events.as_deref(), node.splits(id).await).await?;
            out.push(IndexLayout::of(id, &splits));
        }
        Ok(out)
    }

    fn stats(&self) -> Arc<SinkStats> {
        self.stats.clone()
    }
}

/// The sink's writer, restarted for ingest v1's final merges: the progress
/// line follows the new process's memory.
struct Restarting<'a> {
    process: &'a WriterProcess,
    stats: &'a SinkStats,
}

#[async_trait]
impl merges::Restart for Restarting<'_> {
    async fn restart(&self) -> anyhow::Result<()> {
        let restarted = self.process.restart().await;
        self.stats.set_node_pid(self.process.pid());
        restarted
    }
}

/// A node restarted directly (the tests of `merges::seal_restarting`).
#[async_trait]
impl merges::Restart for QuickwitNode {
    async fn restart(&self) -> anyhow::Result<()> {
        self.process.restart().await
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
    work_dir: PathBuf,
    events: Arc<NodeEvents>,
    /// Shared with the sink, which restarts it for ingest v1's final merges.
    process: Arc<WriterProcess>,
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
        let mut config = format!(
            "version: 0.8\ncluster_id: usnm-writer\nnode_id: writer\nlisten_address: 127.0.0.1\n\
             rest:\n  listen_port: {port}\ngrpc_listen_port: {}\ndata_dir: {}\n\
             metastore_uri: {metastore}\ndefault_index_root_uri: {index_root}\n\
             ingest_api:\n  shard_throughput_limit: {SHARD_THROUGHPUT_LIMIT}\n  \
             max_queue_memory_usage: {queue}\n  \
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

    #[tokio::test]
    async fn engine_is_the_nodes_version() {
        let app = axum::Router::new().route(
            "/api/v1/version",
            axum::routing::get(|| async {
                axum::Json(serde_json::json!({
                    "build": {"version": "0.9.1", "commit_short_hash": "abc1234"}
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut s = QuickwitSink::new(&format!("http://{addr}"), "file:///tmp/x").unwrap();
        assert_eq!(
            s.engine().await.as_deref(),
            Some("Quickwit 0.9.1 (abc1234)")
        );
        // Nothing listening: unknown, not an error.
        let mut gone = QuickwitSink::new("http://127.0.0.1:9", "file:///tmp/x").unwrap();
        assert_eq!(gone.engine().await, None);
    }

    #[test]
    fn writer_tuning_defaults_and_overrides() {
        let none = |_: &str| None;
        assert_eq!(
            WriterTuning::from_lookup(none).unwrap(),
            WriterTuning::default()
        );
        // The defaults leave the template as it is.
        assert_eq!(
            WriterTuning::default().apply(INDEX_TEMPLATE).unwrap(),
            INDEX_TEMPLATE
        );
        let e4 = |k: &str| match k {
            "USNM_WRITER_HEAP" => Some("6GiB".to_string()),
            "USNM_WRITER_COMMIT_SECS" => Some("600".to_string()),
            "USNM_WRITER_QUEUE" => Some("4GiB".to_string()),
            _ => None,
        };
        let t = WriterTuning::from_lookup(e4).unwrap();
        assert_eq!(
            (t.heap.as_str(), t.commit_timeout_secs, t.queue.as_str()),
            ("6GiB", 600, "4GiB")
        );
        let yaml = t.apply(INDEX_TEMPLATE).unwrap();
        assert!(yaml.contains("heap_size: 6GiB") && yaml.contains("commit_timeout_secs: 600"));
        assert!(!yaml.contains("heap_size: 1GiB") && !yaml.contains("commit_timeout_secs: 30"));
        // Everything else is the template's.
        assert_eq!(yaml.lines().count(), INDEX_TEMPLATE.lines().count());
    }

    #[test]
    fn decade_layouts_add_the_field_and_its_tag_or_partitions() {
        assert_eq!(Decades::Off.apply(INDEX_TEMPLATE).unwrap(), INDEX_TEMPLATE);
        let tagged = Decades::Tagged.apply(INDEX_TEMPLATE).unwrap();
        let partitioned = Decades::Partitioned.apply(INDEX_TEMPLATE).unwrap();
        for yaml in [&tagged, &partitioned] {
            assert!(yaml.contains("\n  tag_fields: [decade]\n"), "{yaml}");
            assert!(yaml.contains(
                "\n    - { name: decade, type: u64, fast: false, indexed: true, stored: false }\n"
            ));
            // Only the doc mapping changes: the indexing settings, which the
            // merge checks read, are the template's.
            let settings = |t: &str| t[t.find("indexing_settings:").unwrap()..].to_owned();
            assert_eq!(settings(yaml), settings(INDEX_TEMPLATE));
            // The template's lines, in order, with the new ones between them.
            let mut lines = yaml.lines();
            assert!(INDEX_TEMPLATE.lines().all(|l| lines.any(|y| y == l)));
        }
        assert!(!tagged.contains("partition_key"));
        assert!(partitioned.contains("\n  partition_key: decade\n  max_num_partitions: 32\n"));
        // With the writer's tuning too.
        let t = WriterTuning {
            heap: "6GiB".into(),
            commit_timeout_secs: 120,
            queue: "4GiB".into(),
        };
        let yaml = main_template(&t, Decades::Partitioned.into()).unwrap();
        assert!(yaml.contains("heap_size: 6GiB") && yaml.contains("partition_key: decade"));
        // A template without the lines it adds to is refused.
        assert!(Decades::Tagged.apply("version: 0.9").is_err());
        let twice = format!("{INDEX_TEMPLATE}\n  timestamp_field: date\n");
        assert!(Decades::Partitioned.apply(&twice).is_err());
    }

    #[test]
    fn one_text_field_keeps_the_texts_stored_and_searches_text_all() {
        assert_eq!(
            text_fields(TextLayout::Separate, INDEX_TEMPLATE).unwrap(),
            INDEX_TEMPLATE
        );
        let yaml = text_fields(TextLayout::Single, INDEX_TEMPLATE).unwrap();
        let fields: Vec<&str> = yaml
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("- { name: text"))
            .collect();
        assert_eq!(
            fields,
            [
                "- { name: text, type: text, indexed: false, stored: true }",
                "- { name: text_as, type: text, indexed: false, stored: true }",
                "- { name: text_all, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
                "- { name: text_all_cg, type: text, tokenizer: whitespace, record: position, stored: false, fieldnorms: false }",
            ]
        );
        assert!(
            yaml.contains("\n  default_search_fields: [text_all]"),
            "{yaml}"
        );
        assert!(!yaml.contains("default_search_fields: [text]"));
        // The comments about the old fields went with them.
        assert!(!yaml.contains("Searched only when"), "{yaml}");
        assert!(!yaml.contains("Phrases holding a common"), "{yaml}");
        // Everything but the text fields is the template's.
        let settings = |t: &str| t[t.find("    - { name: date").unwrap()..].to_owned();
        assert_eq!(
            settings(&yaml).replace("[text_all]", "[text]"),
            settings(INDEX_TEMPLATE)
        );
        assert!(
            yaml.starts_with(&INDEX_TEMPLATE[..INDEX_TEMPLATE.find("field_mappings:").unwrap()])
        );
        // Strict, as every main index: a document with a field of the other
        // layout (`text_cg` here, `text_all` in the template) is refused.
        assert!(yaml.contains("\n  mode: strict\n"));
        assert!(!INDEX_TEMPLATE.contains("name: text_all"));
        // With the writer's tuning, a decade layout and a split target too.
        let t = WriterTuning {
            heap: "6GiB".into(),
            commit_timeout_secs: 120,
            queue: "4GiB".into(),
        };
        let layout = MainLayout {
            decades: Decades::Partitioned,
            text: TextLayout::Single,
            page_language: false,
            split_docs: Some(120_000),
        };
        let yaml = main_template(&t, layout).unwrap();
        assert!(yaml.contains("\n  split_num_docs_target: 120000\n"));
        assert!(yaml.contains("heap_size: 6GiB") && yaml.contains("partition_key: decade"));
        assert!(yaml.contains("name: text_all,") && yaml.contains("name: decade,"));
        // A template without one of the lines it changes is refused.
        let no_pairs = INDEX_TEMPLATE.replace(TEMPLATE_TEXT_FIELDS[1], "");
        assert!(text_fields(TextLayout::Single, &no_pairs).is_err());
        assert!(text_fields(TextLayout::Single, "version: 0.9").is_err());
    }

    #[test]
    fn page_languages_add_two_keyword_fields() {
        assert_eq!(
            page_language_fields(false, INDEX_TEMPLATE).unwrap(),
            INDEX_TEMPLATE
        );
        let yaml = page_language_fields(true, INDEX_TEMPLATE).unwrap();
        let fields: Vec<&str> = yaml
            .lines()
            .map(str::trim)
            .filter(|l| l.starts_with("- { name: page_language") || *l == TEMPLATE_LANGUAGE)
            .collect();
        assert_eq!(
            fields,
            [
                TEMPLATE_LANGUAGE,
                "- { name: page_language, type: array<text>, tokenizer: raw, fast: true }",
                "- { name: page_language_set, type: text, tokenizer: raw, indexed: true, stored: false, fast: false }",
            ]
        );
        // The template's lines, in order, with the new ones between them.
        let mut lines = yaml.lines();
        assert!(INDEX_TEMPLATE.lines().all(|l| lines.any(|y| y == l)));
        assert_eq!(yaml.lines().count(), INDEX_TEMPLATE.lines().count() + 5);
        // With every other layout.
        let t = WriterTuning {
            heap: "6GiB".into(),
            commit_timeout_secs: 120,
            queue: "4GiB".into(),
        };
        let layout = MainLayout {
            decades: Decades::Partitioned,
            text: TextLayout::Single,
            page_language: true,
            split_docs: Some(120_000),
        };
        let yaml = main_template(&t, layout).unwrap();
        for field in [
            "name: text_all,",
            "name: decade,",
            "name: page_language_set,",
            "split_num_docs_target: 120000\n",
        ] {
            assert!(yaml.contains(field), "{field}");
        }
        assert!(page_language_fields(true, "version: 0.9").is_err());
    }

    #[test]
    fn a_split_target_replaces_the_templates() {
        let default = MainLayout::default();
        assert_eq!(default.split_target(), 30_000);
        assert_eq!(
            main_template(&WriterTuning::default(), default).unwrap(),
            INDEX_TEMPLATE
        );
        let layout = MainLayout {
            split_docs: Some(90_000),
            ..MainLayout::default()
        };
        assert_eq!(layout.split_target(), 90_000);
        let yaml = main_template(&WriterTuning::default(), layout).unwrap();
        assert_eq!(
            yaml,
            INDEX_TEMPLATE.replace(
                "  split_num_docs_target: 30000\n",
                "  split_num_docs_target: 90000\n"
            )
        );
        assert!(split_target(0, INDEX_TEMPLATE).is_err());
        assert!(split_target(MAX_SPLIT_DOCS, INDEX_TEMPLATE).is_err());
        // A template without the line, or with it twice, is refused.
        assert!(split_target(90_000, "version: 0.9").is_err());
        let twice = format!("{INDEX_TEMPLATE}  {TEMPLATE_SPLIT_TARGET}\n");
        assert!(split_target(90_000, &twice).is_err());
    }

    #[test]
    fn writer_tuning_refuses_bad_values_and_untunable_templates() {
        for (k, v) in [
            ("USNM_WRITER_HEAP", "6"),
            ("USNM_WRITER_HEAP", "6GB"),
            ("USNM_WRITER_HEAP", "0GiB"),
            ("USNM_WRITER_HEAP", "00GiB"),
            ("USNM_WRITER_HEAP", "6GiBGiB"),
            ("USNM_WRITER_HEAP", "6GiBMiB"),
            ("USNM_WRITER_HEAP", "GiB"),
            ("USNM_WRITER_HEAP", "-6GiB"),
            ("USNM_WRITER_QUEUE", "lots"),
            ("USNM_WRITER_COMMIT_SECS", "0"),
            ("USNM_WRITER_COMMIT_SECS", "7200"),
            ("USNM_WRITER_COMMIT_SECS", "ten"),
        ] {
            let got = WriterTuning::from_lookup(|key| (key == k).then(|| v.to_string()));
            assert!(got.is_err(), "{k}={v}");
        }
        assert!(WriterTuning::default().apply("version: 0.9").is_err());
        // A commented-out setting doesn't count, and neither do two.
        let commented = INDEX_TEMPLATE.replace("heap_size: 1GiB", "# heap_size: 1GiB");
        assert!(WriterTuning::default().apply(&commented).is_err());
        let twice = format!("{INDEX_TEMPLATE}\n  commit_timeout_secs: 30\n");
        assert!(WriterTuning::default().apply(&twice).is_err());
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

    #[test]
    fn the_ingest_api_is_v2_unless_set_to_v1() {
        assert_eq!(IngestApi::parse(None).unwrap(), IngestApi::V2);
        assert_eq!(IngestApi::parse(Some("")).unwrap(), IngestApi::V2);
        assert_eq!(IngestApi::parse(Some("v2")).unwrap(), IngestApi::V2);
        assert_eq!(IngestApi::parse(Some(" v1 ")).unwrap(), IngestApi::V1);
        let err = IngestApi::parse(Some("legacy")).unwrap_err().to_string();
        assert!(err.contains("USNM_WRITER_INGEST"), "{err}");
        for api in [IngestApi::V1, IngestApi::V2] {
            assert_eq!(IngestApi::parse(Some(api.name())).unwrap(), api);
        }
    }

    /// What a fake writer does with one ingest request.
    #[derive(Debug, Clone, Copy)]
    enum Answer {
        /// Answers with this status without taking the documents.
        Refuse(u16),
        /// Takes the documents, then answers with this status: what an
        /// ingest v2 router does when one of its persists times out and
        /// another one (or the client's resend) succeeds.
        TakeThen(u16),
        /// Takes the documents twice and answers 200: the router's own
        /// persist retry, with the timed-out persist still appending
        /// (the #283 rebuild).
        TakeTwice,
        /// Takes the documents and answers 200 after the client gave up.
        TakeThenStall,
        /// Takes the documents and answers 200 after a second.
        TakeAfterASecond,
    }

    /// The fake writer's index: the query string of every ingest request
    /// and the documents it holds.
    #[derive(Default)]
    struct FakeIndex {
        requests: Vec<String>,
        docs: Vec<Vec<u8>>,
    }

    /// A fake writer node that answers ingest requests with `answers` in
    /// turn (then takes them and answers 200), as `api` would, and counts
    /// the documents it holds for searches.
    async fn fake_writer(
        api: IngestApi,
        answers: Vec<Answer>,
    ) -> (QuickwitSink, Arc<Mutex<FakeIndex>>) {
        use axum::extract::{RawQuery, State};
        use axum::http::StatusCode;
        type Shared = (
            Arc<Mutex<FakeIndex>>,
            Arc<Mutex<VecDeque<Answer>>>,
            IngestApi,
        );
        async fn ingest(
            State((index, answers, api)): State<Shared>,
            RawQuery(query): RawQuery,
            body: axum::body::Bytes,
        ) -> (StatusCode, String) {
            let lines: Vec<Vec<u8>> = body
                .split(|&b| b == b'\n')
                .filter(|l| !l.is_empty())
                .map(<[u8]>::to_vec)
                .collect();
            let answer = answers.lock().unwrap().pop_front();
            let take = |times: usize| {
                let mut i = index.lock().unwrap();
                for _ in 0..times {
                    i.docs.extend(lines.iter().cloned());
                }
            };
            index
                .lock()
                .unwrap()
                .requests
                .push(query.unwrap_or_default());
            let ok = match api {
                IngestApi::V2 => format!(
                    r#"{{"num_docs_for_processing":{n},"num_ingested_docs":{n},"num_rejected_docs":0}}"#,
                    n = lines.len()
                ),
                IngestApi::V1 => format!(r#"{{"num_docs_for_processing":{}}}"#, lines.len()),
            };
            let status = |code: u16| StatusCode::from_u16(code).unwrap();
            let error = r#"{"message":"ingest failed"}"#.to_owned();
            match answer {
                Some(Answer::Refuse(code)) => (status(code), error),
                Some(Answer::TakeThen(code)) => {
                    take(1);
                    (status(code), error)
                }
                Some(Answer::TakeTwice) => {
                    take(2);
                    (StatusCode::OK, ok)
                }
                Some(Answer::TakeThenStall) => {
                    take(1);
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    (StatusCode::OK, ok)
                }
                Some(Answer::TakeAfterASecond) => {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    take(1);
                    (StatusCode::OK, ok)
                }
                None => {
                    take(1);
                    (StatusCode::OK, ok)
                }
            }
        }
        async fn search(State((index, _, _)): State<Shared>) -> String {
            format!(r#"{{"num_hits":{}}}"#, index.lock().unwrap().docs.len())
        }
        let index: Arc<Mutex<FakeIndex>> = Arc::default();
        let shared: Shared = (
            index.clone(),
            Arc::new(Mutex::new(VecDeque::from(answers))),
            api,
        );
        let app = axum::Router::new()
            .route("/api/v1/{id}/ingest", axum::routing::post(ingest))
            .route("/api/v1/{id}/search", axum::routing::get(search))
            // Room for the release's 8 MiB requests (axum's default is 2 MB).
            .layer(axum::extract::DefaultBodyLimit::max(2 * CHUNK_BYTES))
            .with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut s = QuickwitSink::new(&format!("http://{addr}"), "file:///tmp/x")
            .unwrap()
            .ingest(api);
        s.index = Some("i".into());
        s.retry_initial = Duration::from_millis(1);
        s.retry_for = Duration::from_secs(5);
        s.http = reqwest::Client::builder()
            .timeout(Duration::from_millis(500))
            .build()
            .unwrap();
        (s, index)
    }

    /// Pages of about 1 MB with a decade, so the background sender posts
    /// several automatic requests.
    fn big_docs(n: u32) -> Vec<Value> {
        (0..n)
            .map(|i| {
                serde_json::json!({"doc_id": format!("p{i}"), "decade": 1850 + 10 * (i % 3), "pad": "x".repeat(1_000_000)})
            })
            .collect()
    }

    /// A decade's full file goes out in the background in the order the
    /// in-line path sends it, every page once, the forced commit last.
    #[tokio::test]
    async fn decades_sent_in_the_background_keep_their_order() {
        use crate::decade_order::DecadeOrder;
        let docs = big_docs(40);
        // In line: the order the release sent before.
        let (mut inline, inline_index) = fake_writer(IngestApi::V1, vec![]).await;
        inline.http = reqwest::Client::new();
        let dir = tempfile::tempdir().unwrap();
        let mut order = DecadeOrder::new(dir.path(), 3, None, None).unwrap();
        for d in &docs {
            order.add(&mut inline, d).await.unwrap();
        }
        order.finish(&mut inline).await.unwrap();
        inline.send("force").await.unwrap();
        // In the background.
        let (mut s, index) = fake_writer(IngestApi::V1, vec![]).await;
        s.http = reqwest::Client::new();
        let dir = tempfile::tempdir().unwrap();
        let sender = IndexSink::line_sender(&s);
        assert!(sender.is_some());
        let mut order = DecadeOrder::new(dir.path(), 3, sender, None).unwrap();
        for d in &docs {
            order.add(&mut s, d).await.unwrap();
        }
        let stats = order.finish(&mut s).await.unwrap();
        assert_eq!((stats.docs, stats.runs), (40, 15));
        assert!(stats.peak_bytes > 0);
        // The last pages are back in the sink, for `finish`'s forced commit.
        assert!(!s.buf.is_empty());
        let before_force = index.lock().unwrap().requests.len();
        assert!(before_force >= 3, "{before_force} automatic requests");
        s.send("force").await.unwrap();
        let i = index.lock().unwrap();
        let want = inline_index.lock().unwrap();
        assert_eq!(i.docs.len(), 40);
        assert_eq!(i.docs, want.docs);
        assert!(i.requests[..before_force]
            .iter()
            .all(|r| r == "commit=auto&use_legacy_ingest=true"));
        assert_eq!(
            i.requests.last().map(String::as_str),
            Some("commit=force&use_legacy_ingest=true")
        );
        assert_eq!(s.stats().docs_sent(), 40);
    }

    /// The background sender's failure stops the release.
    #[tokio::test]
    async fn a_failed_background_send_fails_the_release() {
        use crate::decade_order::DecadeOrder;
        let (mut s, _) = fake_writer(IngestApi::V1, vec![Answer::Refuse(500)]).await;
        s.http = reqwest::Client::new();
        let dir = tempfile::tempdir().unwrap();
        let mut order = DecadeOrder::new(dir.path(), 2, IndexSink::line_sender(&s), None).unwrap();
        let mut err = None;
        for d in &big_docs(40) {
            if let Err(e) = order.add(&mut s, d).await {
                err = Some(e);
                break;
            }
        }
        let err = match err {
            Some(e) => e,
            None => order.finish(&mut s).await.unwrap_err(),
        };
        assert!(format!("{err:#}").contains("returned 500"), "{err:#}");
    }

    /// A lost writer lock stops the background sender at its next line,
    /// even while the release itself is busy elsewhere.
    #[tokio::test]
    async fn a_lost_lease_stops_the_background_sender() {
        use crate::decade_order::DecadeOrder;
        let (mut s, index) = fake_writer(IngestApi::V1, vec![Answer::TakeAfterASecond; 40]).await;
        s.http = reqwest::Client::new();
        let dir = tempfile::tempdir().unwrap();
        let lost = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let mut order = DecadeOrder::new(
            dir.path(),
            2,
            IndexSink::line_sender(&s),
            Some(Arc::clone(&lost)),
        )
        .unwrap();
        for d in &big_docs(20) {
            order.add(&mut s, d).await.unwrap();
        }
        lost.store(true, std::sync::atomic::Ordering::SeqCst);
        let err = order.finish(&mut s).await.unwrap_err();
        assert!(
            format!("{err:#}").contains("writer lock could not be renewed"),
            "{err:#}"
        );
        let seen = index.lock().unwrap().requests.len();
        tokio::time::sleep(Duration::from_millis(1500)).await;
        assert_eq!(index.lock().unwrap().requests.len(), seen);
    }

    /// A release that gives up while the sender waits on the writer (its
    /// lock lost) stops the sender: nothing is posted after that.
    #[tokio::test]
    async fn a_cancelled_finish_stops_the_sender() {
        use crate::decade_order::DecadeOrder;
        let (mut s, index) = fake_writer(IngestApi::V1, vec![Answer::TakeAfterASecond; 40]).await;
        s.http = reqwest::Client::new();
        let dir = tempfile::tempdir().unwrap();
        let mut order = DecadeOrder::new(dir.path(), 2, IndexSink::line_sender(&s), None).unwrap();
        for d in &big_docs(30) {
            order.add(&mut s, d).await.unwrap();
        }
        let cancelled =
            tokio::time::timeout(Duration::from_millis(300), order.finish(&mut s)).await;
        assert!(cancelled.is_err(), "finish should still be waiting");
        let seen = index.lock().unwrap().requests.len();
        drop(order);
        tokio::time::sleep(Duration::from_millis(2500)).await;
        let after = index.lock().unwrap().requests.len();
        assert_eq!(
            after, seen,
            "the sender kept posting after the release gave up"
        );
    }

    /// Two pages, one request.
    const TWO: &[u8] = b"{\"doc_id\":\"a\"}\n{\"doc_id\":\"b\"}\n";

    /// The #283 symptom: every request was answered 200 once and counted
    /// once, but the writer kept extra copies. Only the count sees them.
    #[tokio::test]
    async fn v2_duplicates_that_the_writer_accepted_once_fail_the_count() {
        let (mut s, index) = fake_writer(IngestApi::V2, vec![Answer::TakeTwice]).await;
        s.buf = TWO.to_vec();
        s.send("auto").await.unwrap();
        assert_eq!(s.stats().docs_sent(), 2);
        assert_eq!(index.lock().unwrap().docs.len(), 4);
        let err = s.finish(2).await.unwrap_err().to_string();
        assert!(err.contains("holds 4 documents, expected 2"), "{err}");
    }

    /// v2 resends a request the node answered 503 after it took it: at
    /// least once, and the count fails the release.
    #[tokio::test]
    async fn v2_resends_a_request_it_cannot_tell_was_taken() {
        let (mut s, index) = fake_writer(IngestApi::V2, vec![Answer::TakeThen(503)]).await;
        s.buf = TWO.to_vec();
        s.send("auto").await.unwrap();
        {
            let i = index.lock().unwrap();
            assert_eq!((i.requests.len(), i.docs.len()), (2, 4));
        }
        let err = s.finish(2).await.unwrap_err().to_string();
        assert!(err.contains("holds 4 documents, expected 2"), "{err}");
    }

    #[tokio::test]
    async fn v1_sends_through_the_legacy_ingest_api() {
        let (mut s, index) = fake_writer(IngestApi::V1, vec![]).await;
        s.buf = TWO.to_vec();
        s.send("auto").await.unwrap();
        let i = index.lock().unwrap();
        assert_eq!(i.requests, ["commit=auto&use_legacy_ingest=true"]);
        assert_eq!(i.docs.len(), 2);
        assert_eq!(s.stats().docs_sent(), 2);
    }

    /// 429 (queue full) and 404 (no queue yet) come before v1 appends
    /// anything: those are sent again, and the index holds each page once.
    #[tokio::test]
    async fn v1_retries_only_what_was_not_taken() {
        let (mut s, index) = fake_writer(
            IngestApi::V1,
            vec![
                Answer::Refuse(404),
                Answer::Refuse(429),
                Answer::Refuse(429),
            ],
        )
        .await;
        s.buf = TWO.to_vec();
        s.send("auto").await.unwrap();
        assert_eq!(index.lock().unwrap().requests.len(), 4);
        assert_eq!(index.lock().unwrap().docs.len(), 2);
        let stats = s.stats();
        assert_eq!((stats.docs_sent(), stats.retries_429()), (2, 2));
        assert_eq!(stats.retries_503(), 0);
    }

    /// Any other answer, and no answer at all, may come after v1 took the
    /// request: it is never sent again, and the release stops.
    #[tokio::test]
    async fn v1_never_resends_a_request_that_may_have_been_taken() {
        for answer in [
            Answer::TakeThen(503),
            Answer::TakeThen(500),
            Answer::TakeThen(408),
            Answer::TakeThenStall,
        ] {
            let (mut s, index) = fake_writer(IngestApi::V1, vec![answer]).await;
            s.buf = TWO.to_vec();
            let err = s.send("auto").await.unwrap_err();
            let i = index.lock().unwrap();
            assert_eq!(i.requests.len(), 1, "{answer:?}: {err:#}");
            assert_eq!(i.docs.len(), 2, "{answer:?}");
            assert_eq!(s.stats().docs_sent(), 0, "{answer:?}");
        }
        // A v2 writer that times out is not resent either.
        let (mut s, index) = fake_writer(IngestApi::V2, vec![Answer::TakeThenStall]).await;
        s.buf = TWO.to_vec();
        assert!(s.send("auto").await.is_err());
        assert_eq!(index.lock().unwrap().requests.len(), 1);
    }

    /// v1 must say it took every page of the request.
    #[tokio::test]
    async fn v1_checks_the_documents_it_took() {
        let (mut s, _) = fake_writer(IngestApi::V1, vec![]).await;
        s.buf = TWO.to_vec();
        s.buf.extend_from_slice(b"\n");
        // Three newlines, two documents: the node counts two.
        let err = s.send("auto").await.unwrap_err().to_string();
        assert!(err.contains("did not take all 3 documents"), "{err}");
    }

    /// v1's final merges restart the writer: without one there is nothing
    /// to restart, and the release stops before it publishes.
    #[tokio::test]
    async fn v1_needs_a_writer_it_can_restart() {
        let (mut s, _) = fake_writer(IngestApi::V1, vec![]).await;
        s.buf = TWO.to_vec();
        let err = s.finish(2).await.unwrap_err().to_string();
        assert!(err.contains("--quickwit-bin"), "{err}");
    }
}
