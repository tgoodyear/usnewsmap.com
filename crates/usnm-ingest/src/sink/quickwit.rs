//! The Quickwit ingest client: creating an index on the writer node, posting its
//! documents and checking the node took them all (08 §8.4.1).

use std::collections::BTreeSet;
use std::sync::atomic::AtomicU32;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use serde_json::Value;

use usnm_core::text_layout::TextLayout;

use super::{
    main_template, Decades, IndexSink, MainLayout, QuickwitNode, SinkStats, TitleFieldsByLccn,
    WriterProcess, WriterTuning,
};
use crate::merges::{self, IndexLayout, MergeWait, NodeEvents};

/// Titles a `title_fields` search can tell apart: more than LoC has.
const TITLE_BUCKETS: u64 = 50_000;

/// A `title_fields` search's answer, by LCCN.
fn title_fields_of(v: &Value) -> anyhow::Result<TitleFieldsByLccn> {
    let buckets = v["aggregations"]["titles"]["buckets"]
        .as_array()
        .context("the search answered no title buckets")?;
    anyhow::ensure!(
        (buckets.len() as u64) < TITLE_BUCKETS,
        "the index has {TITLE_BUCKETS} titles or more: too many to check"
    );
    let keys = |b: &Value, name: &str| -> BTreeSet<String> {
        b[name]["buckets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| match &k["key"] {
                Value::String(s) if !s.is_empty() => Some(s.clone()),
                _ => None,
            })
            .collect()
    };
    let mut out = TitleFieldsByLccn::new();
    for b in buckets {
        let lccn = b["key"].as_str().context("a title bucket without a key")?;
        let shards = b["shards"]["buckets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|k| k["key"].as_f64().map(|n| n as u64))
            .collect();
        let ordinal = |name: &str| {
            b[name]["value"]
                .as_f64()
                .map(|n| (n as u64) >> 32)
                .with_context(|| format!("{lccn} has no {name} sort_key"))
        };
        out.insert(
            lccn.to_owned(),
            crate::finish::TitleFields {
                places: keys(b, "places"),
                shards,
                states: keys(b, "states"),
                languages: keys(b, "languages"),
                ordinals: (ordinal("first")?, ordinal("last")?),
            },
        );
    }
    Ok(out)
}

/// What a writer node that doesn't see itself as a searcher yet answers a
/// search with (Quickwit 0.9.1), just after it starts.
const NO_SEARCHER: &str = "there are no available searcher nodes in the cluster";

/// Documents per ingest request, bounded well under Quickwit's 10 MiB body limit.
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

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

    /// Kafka (`crate::kafka`) only with v1: the restart-based final merges
    /// (`merges::seal_restarting`) are v1's; v2's `merges::seal` would wait
    /// for `_ingest-source`, which a Kafka build never uses.
    pub fn check_kafka(self, kafka: bool) -> anyhow::Result<()> {
        if kafka && self != Self::V1 {
            bail!(
                "USNM_WRITER_KAFKA needs {}=v1, not `{}`",
                Self::VAR,
                self.name()
            );
        }
        Ok(())
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
    /// The broker a partitioned main index's pages go through (`crate::kafka`).
    kafka: Option<crate::kafka::Kafka>,
    /// The index with a Kafka topic and source, when it is the current one.
    kafka_index: Option<String>,
    /// When the writer node this release runs started, in seconds since the
    /// epoch: splits staged before it are a stopped writer's.
    node_started: Option<i64>,
    /// The current index is a stopped release's, taken up by `attach`: its
    /// Kafka source and topic may be gone already.
    resumed: bool,
    /// While a finish replays its build, the lines that say how the
    /// writer's merges go; `finish` stops it.
    replay_watch: Option<merges::MergeWatch>,
}

/// Posts ingest requests to one index on the writer node: the request,
/// v1/v2's retries, and the check that the node took every document.
#[derive(Clone)]
pub struct Poster {
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

/// JSON lines to one index: in v1 requests of at most [`CHUNK_BYTES`], one
/// at a time, or to its Kafka topic (`crate::kafka`):
/// [`QuickwitSink::line_sender`].
pub enum LineSender {
    Http { poster: Poster, buf: Vec<u8> },
    Kafka(Box<crate::kafka::Producer>),
}

impl LineSender {
    pub async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        let (poster, buf) = match self {
            Self::Kafka(p) => return p.add_line(line).await,
            Self::Http { poster, buf } => (poster, buf),
        };
        if line.len() + 1 > CHUNK_BYTES {
            bail!(
                "a document is {} bytes, over the {CHUNK_BYTES}-byte ingest request limit",
                line.len() + 1
            );
        }
        if buf.len() + line.len() + 1 > CHUNK_BYTES {
            let body = bytes::Bytes::from(std::mem::take(buf));
            poster.post("auto", body).await?;
        }
        buf.extend_from_slice(line);
        buf.push(b'\n');
        Ok(())
    }

    /// The writer lock's flag, for Kafka's watch to stop on.
    pub fn set_lease_lost(&mut self, lost: Option<Arc<std::sync::atomic::AtomicBool>>) {
        if let Self::Kafka(p) = self {
            p.set_lease_lost(lost);
        }
    }

    /// A decade file's last line went in: Kafka moves to the next partition.
    pub async fn end_file(&mut self) -> anyhow::Result<()> {
        match self {
            Self::Kafka(p) => p.end_file().await,
            Self::Http { .. } => Ok(()),
        }
    }

    /// The lines not sent yet. Over HTTP, never none once a line was added (a
    /// request goes only when the next line wouldn't fit): the release gives
    /// them back to the sink, whose `finish` sends them with the forced
    /// commit, as when the pages went out in line. Kafka sends its last
    /// records, waits until the index holds them all (`Producer::drain`, so
    /// a stall at the end gets the watch's restart too), and returns none.
    pub async fn into_unsent(self) -> anyhow::Result<Vec<u8>> {
        match self {
            Self::Kafka(mut p) => {
                p.drain().await?;
                Ok(Vec::new())
            }
            Self::Http { buf, .. } => Ok(buf),
        }
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
            kafka: None,
            kafka_index: None,
            node_started: None,
            resumed: false,
            replay_watch: None,
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
        self.node_started = Some(node.started);
        self
    }

    /// Send a partitioned main index's pages through `kafka`
    /// (`crate::kafka`); the rest stays on the ingest API.
    pub fn kafka(mut self, kafka: Option<crate::kafka::Kafka>) -> Self {
        self.kafka = kafka;
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
    /// reads on: to the index's Kafka topic when it has one, otherwise the
    /// same requests `add_line` would post, one at a time.
    pub async fn line_sender(&self) -> anyhow::Result<LineSender> {
        if let (Some(k), Some(id)) = (&self.kafka, &self.kafka_index) {
            if self.index.as_ref() == Some(id) {
                // The writer node this release runs can be restarted for a
                // stalled source; one it doesn't run can't.
                let restart = self.process.as_ref().map(|p| {
                    Arc::new(ProcessRestart {
                        process: Arc::clone(p),
                        stats: Arc::clone(&self.stats),
                    }) as Arc<dyn merges::Restart>
                });
                let watch = crate::kafka::Watch::new(self.http.clone(), &self.base, id, restart)?;
                return Ok(LineSender::Kafka(Box::new(
                    k.producer(id, Arc::clone(&self.stats), Some(watch)).await?,
                )));
            }
        }
        Ok(LineSender::Http {
            poster: self.poster()?,
            buf: Vec::new(),
        })
    }

    async fn add_source(&self, index_id: &str, yaml: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .post(format!("{}/api/v1/indexes/{index_id}/sources", self.base))
            .header("content-type", "application/yaml")
            .body(yaml.to_owned())
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            bail!(
                "adding a source to `{index_id}` returned {status}: {}",
                resp.text()
                    .await
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        Ok(())
    }

    async fn delete_source(&self, index_id: &str, source_id: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .delete(format!(
                "{}/api/v1/indexes/{index_id}/sources/{source_id}",
                self.base
            ))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            bail!(
                "deleting source `{source_id}` of `{index_id}` returned {status}: {}",
                resp.text()
                    .await
                    .unwrap_or_default()
                    .chars()
                    .take(300)
                    .collect::<String>()
            );
        }
        Ok(())
    }

    /// [`QuickwitSink::delete_source`], for a source that may be gone.
    async fn delete_source_if_exists(&self, index_id: &str, source_id: &str) -> anyhow::Result<()> {
        if self
            .node()
            .sources(index_id)
            .await?
            .iter()
            .any(|s| s == source_id)
        {
            self.delete_source(index_id, source_id).await
        } else {
            tracing::info!(index = index_id, source = source_id, "no source to delete");
            Ok(())
        }
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
    async fn line_sender(&mut self) -> anyhow::Result<Option<LineSender>> {
        QuickwitSink::line_sender(self).await.map(Some)
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
        // A partitioned base's decade files go through Kafka (`crate::kafka`).
        // Only a partitioned base sends decade files (`crate::decade_order`); a
        // delta tagged by decade goes in line, through v1.
        if let (Some(k), Decades::Partitioned) = (self.kafka.clone(), layout.decades) {
            self.ingest.check_kafka(true)?;
            k.create_topic(index_id).await?;
            self.add_source(index_id, &k.source_yaml(index_id)).await?;
            self.kafka_index = Some(index_id.to_owned());
            tracing::info!(index = index_id, pipelines = k.partitions, "Kafka source");
        }
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
        self.resumed = false;
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
        // The merge wait logs its own lines from here.
        self.replay_watch = None;
        // A forced commit publishes everything still buffered in the node.
        // `add` always leaves its document in the buffer, so an empty one
        // means none was added (a batch with no text): there is nothing to
        // commit, and the node rejects an empty request (411).
        if !self.buf.is_empty() {
            self.send("force").await?;
        }
        let id = self.index.clone().context("no index created")?;
        let via_kafka = self.kafka_index.as_ref() == Some(&id);
        // Kafka's backlog can take a while to index, and its pipelines' last
        // splits wait for their commit timeout (the forced commit is v1's
        // only): wait while the count grows, for the longer of 20 minutes
        // and the commit timeout plus 10 without progress. Otherwise 4 minutes.
        let patience = if via_kafka {
            let commit = u64::from(WriterTuning::from_env()?.commit_timeout_secs);
            (1200u64.max(commit + 600) / 2) as u32
        } else {
            120
        };
        let mut last = 0;
        let mut still = 0;
        while still < patience {
            let n = self.count(&id).await?;
            if n >= expected {
                last = n;
                break;
            }
            still = if n > last { 0 } else { still + 1 };
            last = n;
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
        // The Kafka source stays through the final merges, so its splits
        // get the final merge factor too (Quickwit merges by source); then
        // it and its topic go: every record is in the index.
        if via_kafka {
            // A stopped release's may be gone already (`attach`).
            if self.resumed {
                self.delete_source_if_exists(&id, "kafka").await?;
            } else {
                self.delete_source(&id, "kafka").await?;
            }
            self.kafka_index = None;
            if let Some(k) = &self.kafka {
                if self.resumed {
                    k.delete_topic_if_exists(&id).await?;
                } else {
                    k.delete_topic(&id).await?;
                }
            }
        }
        tracing::info!(
            index = %id,
            splits = layout.splits,
            "merged; the index is closed to further writes"
        );
        Ok(())
    }

    /// The stopped release's index, as `create` would have left it: its
    /// split target, and its Kafka source and topic when the release sent
    /// through Kafka. Before anything is counted or merged, its merge
    /// policy is the template's again if the release stopped with the final
    /// merges' in place, and the splits a stopped writer left staged are
    /// marked for deletion: they hold no document the published splits
    /// don't, and `publish-staged` refuses an index that has any.
    async fn attach(&mut self, index_id: &str, layout: MainLayout) -> anyhow::Result<()> {
        let started = self.node_started.with_context(|| {
            format!(
                "taking up `{index_id}` needs a writer node this command runs (--quickwit-bin): \
                 the splits staged before it started are the stopped writer's"
            )
        })?;
        let node = self.node();
        let events = self.events.as_deref();
        let config = merges::explained(events, node.index_config(index_id).await)
            .await
            .with_context(|| format!("`{index_id}` isn't in the writer's metastore"))?;
        if let Some(fixed) = merges::template_merge_factor(&config)? {
            tracing::warn!(
                index = index_id,
                "the stopped release left the final merges' merge factor in the index's config; \
                 putting the template's back"
            );
            merges::explained(events, node.update_index(index_id, &fixed).await).await?;
        }
        let staged =
            merges::explained(events, node.staged_splits_before(index_id, started).await).await?;
        if !staged.is_empty() {
            tracing::warn!(
                index = index_id,
                splits = staged.len(),
                first = ?staged.iter().take(10).collect::<Vec<_>>(),
                "marking the splits a stopped writer left staged for deletion"
            );
            merges::explained(events, node.mark_for_deletion(index_id, &staged).await).await?;
        }
        let sources = merges::explained(events, node.sources(index_id).await).await?;
        // Only a partitioned base sends through Kafka (`create`).
        let kafka = self.kafka.is_some() && layout.decades == Decades::Partitioned;
        if sources.iter().any(|s| s == "kafka") && !kafka {
            bail!(
                "`{index_id}` has a Kafka source: set USNM_WRITER_KAFKA (and its partitions) as the \
                 release had them, so the writer runs the source's pipelines and merges its splits"
            );
        }
        if kafka {
            self.ingest.check_kafka(true)?;
            self.kafka_index = Some(index_id.to_owned());
        }
        self.index = Some(index_id.to_owned());
        self.split_docs = layout.split_docs;
        self.resumed = true;
        self.replay_watch = Some(merges::MergeWatch::start(
            self.http.clone(),
            self.base.clone(),
            index_id.to_owned(),
            "replay",
            self.stats.work_dir.clone(),
        ));
        tracing::info!(
            index = index_id,
            sources = ?sources,
            split_docs = layout.split_docs,
            "took up the stopped release's index"
        );
        Ok(())
    }

    async fn has_index(&mut self, index_id: &str) -> anyhow::Result<bool> {
        let node = self.node();
        merges::explained(self.events.as_deref(), node.has_index(index_id).await).await
    }

    /// A writer just started takes a moment to see itself as a searcher;
    /// until then it answers searches with an error that says so.
    async fn published_docs(&mut self, index_id: &str) -> anyhow::Result<u64> {
        let until = tokio::time::Instant::now() + Duration::from_secs(60);
        let text = loop {
            let resp = self
                .http
                .get(format!(
                    "{}/api/v1/{index_id}/search?query=*&max_hits=0",
                    self.base
                ))
                .send()
                .await;
            let resp = merges::explained(self.events.as_deref(), resp.map_err(Into::into)).await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            if status.is_success() {
                break text;
            }
            if !text.contains(NO_SEARCHER) || tokio::time::Instant::now() >= until {
                bail!(
                    "counting `{index_id}` returned {status}: {}",
                    text.chars().take(500).collect::<String>()
                );
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        };
        let v: Value = serde_json::from_str(&text).context("search response")?;
        v["num_hits"]
            .as_u64()
            .context("search response has no num_hits")
    }

    /// From the published splits, when each holds one decade (a partitioned
    /// index's do).
    async fn decade_docs(
        &mut self,
        index_id: &str,
    ) -> anyhow::Result<Option<std::collections::BTreeMap<u16, u64>>> {
        let node = self.node();
        let splits = merges::explained(self.events.as_deref(), node.splits(index_id).await).await?;
        if splits
            .iter()
            .any(|s| s.partition_id == 0 || s.decades.len() != 1)
        {
            return Ok(None);
        }
        let mut by = std::collections::BTreeMap::new();
        for s in &splits {
            *by.entry(s.decades[0]).or_default() += s.docs;
        }
        Ok(Some(by))
    }

    /// One search of the published splits: each title's terms and its
    /// documents' lowest and highest `sort_key`.
    async fn title_fields(&mut self, index_id: &str) -> anyhow::Result<Option<TitleFieldsByLccn>> {
        let terms = |field: &str| serde_json::json!({"terms": {"field": field, "size": 64}});
        let body = serde_json::json!({
            "query": "*",
            "max_hits": 0,
            "aggs": {"titles": {
                "terms": {"field": "lccn", "size": TITLE_BUCKETS},
                "aggs": {
                    "places": terms("place_id"),
                    "shards": terms("place_shard"),
                    "states": terms("state"),
                    "languages": terms("language"),
                    "first": {"min": {"field": "sort_key"}},
                    "last": {"max": {"field": "sort_key"}},
                },
            }},
        });
        let resp = self
            .http
            .post(format!("{}/api/v1/{index_id}/search", self.base))
            .json(&body)
            .send()
            .await;
        let resp = merges::explained(self.events.as_deref(), resp.map_err(Into::into)).await?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "reading the titles of `{index_id}` returned {status}: {}",
                text.chars().take(500).collect::<String>()
            );
        }
        let v: Value = serde_json::from_str(&text).context("search response")?;
        title_fields_of(&v).map(Some)
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

/// The sink's writer, owned, for a Kafka source that stalled
/// (`crate::kafka::Watch`): the progress line follows the new process.
struct ProcessRestart {
    process: Arc<WriterProcess>,
    stats: Arc<SinkStats>,
}

#[async_trait]
impl merges::Restart for ProcessRestart {
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

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    use super::*;
    use crate::sink::add_title_fields;

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

    #[test]
    fn reads_what_each_titles_documents_carry() {
        // As Quickwit 0.9.1 answers `title_fields`' search.
        let v = serde_json::json!({"num_hits": 3, "aggregations": {"titles": {"buckets": [{
            "key": "sn1", "doc_count": 3,
            "places": {"buckets": [{"key": "P1", "doc_count": 3}]},
            "shards": {"buckets": [{"key": 3.0, "doc_count": 3}]},
            "states": {"buckets": [{"key": "IL", "doc_count": 3}]},
            "languages": {"buckets": [{"key": "eng", "doc_count": 3}, {"key": "", "doc_count": 1}]},
            "first": {"value": ((7u64 << 32) | (1 << 16) | 1) as f64},
            "last": {"value": ((7u64 << 32) | (1 << 16) | 12) as f64},
        }]}}});
        let got = title_fields_of(&v).unwrap();
        assert_eq!(
            got["sn1"],
            crate::finish::TitleFields {
                places: BTreeSet::from(["P1".to_owned()]),
                shards: BTreeSet::from([3]),
                states: BTreeSet::from(["IL".to_owned()]),
                languages: BTreeSet::from(["eng".to_owned()]),
                ordinals: (7, 7),
            }
        );
        // A document's own fields fold the same way.
        let mut by = TitleFieldsByLccn::new();
        for seq in [1u64, 12] {
            add_title_fields(
                &mut by,
                &serde_json::json!({
                    "doc_id": "d", "lccn": "sn1", "place_id": "P1", "place_shard": 3,
                    "state": "IL", "language": ["eng", ""], "sort_key": (7u64 << 32) | (1 << 16) | seq,
                }),
            )
            .unwrap();
        }
        assert_eq!(by, got);
        assert!(title_fields_of(&serde_json::json!({"num_hits": 0})).is_err());
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
        IngestApi::V1.check_kafka(true).unwrap();
        IngestApi::V2.check_kafka(false).unwrap();
        let err = IngestApi::V2.check_kafka(true).unwrap_err().to_string();
        assert!(err.contains("USNM_WRITER_INGEST=v1"), "{err}");
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
        let sender = IndexSink::line_sender(&mut s).await.unwrap();
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
        let mut order = DecadeOrder::new(
            dir.path(),
            2,
            IndexSink::line_sender(&mut s).await.unwrap(),
            None,
        )
        .unwrap();
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
            IndexSink::line_sender(&mut s).await.unwrap(),
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
        let mut order = DecadeOrder::new(
            dir.path(),
            2,
            IndexSink::line_sender(&mut s).await.unwrap(),
            None,
        )
        .unwrap();
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
