//! A partitioned base's pages through Kafka (#283): the on-prem writer reads
//! them with a Kafka source of several indexing pipelines instead of the v1
//! ingest API's one. Measured on pve-big (20,000 of the build's pages, 2.2
//! GB): v1 accepted about 130 pages/s on one node, a Kafka source with 1, 2
//! and 4 pipelines indexed 341, 509 and 754.
//!
//! Only the main index's decade files go this way (`crate::decade_order`):
//! each whole file to one partition, the next file to the next partition, so
//! every pipeline reads long runs of one decade and its splits stay one
//! decade each. Everything else (the last pages, the Japanese and Congress
//! indexes, the forced commit, the count and the restart-based final merges)
//! stays on v1. The source keeps its position in the metastore and commits
//! it with each split, so a writer restart resumes without indexing a record
//! twice. A record that may have been written twice (a produce whose answer
//! was lost) isn't retried: the release fails, and the exact count would
//! catch a duplicate either way.
//!
//! The producer keeps within reach of what the index has taken ([`Watch`]):
//! the topic's retention deletes records whether or not they were read, and
//! in run e (#283) the consumers stopped after a broker stall while the
//! release sent on for hours, so most of the pages expired unread.
//!
//! `USNM_WRITER_KAFKA` names the broker (`host:port`), with
//! `USNM_WRITER_INGEST=v1`; `USNM_WRITER_KAFKA_PARTITIONS` the partitions and
//! pipelines (default 8); `USNM_WRITER_KAFKA_MAX_LAG` the records the index
//! may be behind before the producer waits (default 1,000,000), and
//! `USNM_WRITER_KAFKA_STALL_SECS` how long its checkpoint may stand still
//! with records waiting (default 900).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use rskafka::client::partition::{Compression, OffsetAt, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use rskafka::BackoffConfig;

use crate::merges::Restart;
use crate::sink::SinkStats;

pub const VAR: &str = "USNM_WRITER_KAFKA";
pub const PARTITIONS_VAR: &str = "USNM_WRITER_KAFKA_PARTITIONS";
const DEFAULT_PARTITIONS: i32 = 8;
/// Bytes of records per produce request, under the broker's 16 MB message limit.
const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// The topic's record size limit, set when the release creates it.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
/// The source's consumer group session timeout (librdkafka's default is 45 s).
const CONSUMER_SESSION_MS: u32 = 300_000;
const CONSUMER_HEARTBEAT_MS: u32 = 10_000;
const CONSUMER_MAX_POLL_MS: u32 = 900_000;
pub const MAX_LAG_VAR: &str = "USNM_WRITER_KAFKA_MAX_LAG";
pub const STALL_VAR: &str = "USNM_WRITER_KAFKA_STALL_SECS";
/// Records the index may be behind before the producer waits: about half an
/// hour of sending, far inside the topic's retention.
const DEFAULT_MAX_LAG: i64 = 1_000_000;
/// How long the index's checkpoint may stand still while records wait. The
/// pipelines commit every `commit_timeout_secs` (30 s), so a quarter of an
/// hour without one is a stall.
const DEFAULT_STALL_SECS: u64 = 900;
/// Writer restarts for stalls in one release: each needs progress after the
/// one before.
const MAX_RESTARTS: u32 = 3;

/// Where the pages go: the broker and the partition count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Kafka {
    pub bootstrap: String,
    pub partitions: i32,
}

impl Kafka {
    /// From the environment; `None` when `USNM_WRITER_KAFKA` is unset.
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self::from_lookup(get(VAR), get(PARTITIONS_VAR))
    }

    pub fn from_lookup(
        bootstrap: Option<String>,
        partitions: Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(bootstrap) = bootstrap.map(|b| b.trim().to_owned()) else {
            return Ok(None);
        };
        let ok_host = bootstrap
            .rsplit_once(':')
            .is_some_and(|(h, p)| !h.is_empty() && p.parse::<u16>().is_ok_and(|p| p > 0));
        if !ok_host {
            bail!("{VAR} must be host:port, not `{bootstrap}`");
        }
        let partitions = match partitions {
            None => DEFAULT_PARTITIONS,
            Some(p) => match p.trim().parse::<i32>() {
                Ok(n @ 1..=64) => n,
                _ => bail!("{PARTITIONS_VAR} must be 1 to 64, not `{p}`"),
            },
        };
        Ok(Some(Self {
            bootstrap,
            partitions,
        }))
    }

    /// The topic for `index_id`.
    pub fn topic(index_id: &str) -> String {
        format!("usnm-{index_id}")
    }

    /// The indexer's CPU capacity the pipelines need (4 cores' worth each,
    /// Quickwit's `PIPELINE_FULL_CAPACITY`, or it schedules fewer).
    pub fn cpu_capacity(&self) -> i32 {
        4 * self.partitions
    }

    async fn client(&self) -> anyhow::Result<Client> {
        ClientBuilder::new(vec![self.bootstrap.clone()])
            .build()
            .await
            .with_context(|| format!("connecting to Kafka at {}", self.bootstrap))
    }

    /// A client that never sends a request again: `rskafka` retries I/O and
    /// connection errors by default, with no deadline, and a produce whose
    /// answer was lost may have been appended. A zero deadline ends the
    /// retries at the first error (a throttle, which appended nothing, is
    /// still waited out).
    async fn producer_client(&self) -> anyhow::Result<Client> {
        ClientBuilder::new(vec![self.bootstrap.clone()])
            .backoff_config(no_retries())
            .build()
            .await
            .with_context(|| format!("connecting to Kafka at {}", self.bootstrap))
    }

    /// Create `index_id`'s topic, empty: one that exists already is an
    /// earlier attempt's, whose records would be indexed again.
    pub async fn create_topic(&self, index_id: &str) -> anyhow::Result<()> {
        let topic = Self::topic(index_id);
        let client = self.client().await?;
        if client
            .list_topics()
            .await
            .context("listing Kafka topics")?
            .iter()
            .any(|t| t.name == topic)
        {
            bail!("Kafka topic {topic} exists already: delete it, or build a new version");
        }
        client
            .controller_client()?
            .create_topic(&topic, self.partitions, 1, 30_000)
            .await
            .with_context(|| format!("creating Kafka topic {topic}"))?;
        tracing::info!(topic, partitions = self.partitions, "Kafka topic");
        Ok(())
    }

    /// Delete `index_id`'s topic once the index holds every record and the
    /// source is gone: the next build of a version of the same name (a
    /// rehearsal, a retry the same day) needs the name, and the records
    /// are in the index.
    pub async fn delete_topic(&self, index_id: &str) -> anyhow::Result<()> {
        let topic = Self::topic(index_id);
        self.client()
            .await?
            .controller_client()?
            .delete_topic(&topic, 30_000)
            .await
            .with_context(|| format!("deleting Kafka topic {topic}"))?;
        tracing::info!(topic, "Kafka topic deleted");
        Ok(())
    }

    /// The index's Kafka source, as Quickwit takes it: one pipeline per
    /// partition, from the start of the topic, with consumer timeouts long
    /// enough to outlast a broker stalled on a busy disk (45 s in run e,
    /// #283), after which librdkafka's consumers left the group and never
    /// came back.
    pub fn source_yaml(&self, index_id: &str) -> String {
        format!(
            "version: 0.8\nsource_id: kafka\nsource_type: kafka\nnum_pipelines: {}\nparams:\n  \
             topic: {}\n  client_params:\n    bootstrap.servers: {}\n    \
             auto.offset.reset: earliest\n    fetch.message.max.bytes: {MAX_MESSAGE_BYTES}\n    \
             max.partition.fetch.bytes: {MAX_MESSAGE_BYTES}\n    \
             session.timeout.ms: {CONSUMER_SESSION_MS}\n    \
             heartbeat.interval.ms: {CONSUMER_HEARTBEAT_MS}\n    \
             max.poll.interval.ms: {CONSUMER_MAX_POLL_MS}\n",
            self.partitions,
            Self::topic(index_id),
            self.bootstrap
        )
    }

    /// A producer to `index_id`'s topic, kept within reach of the index by
    /// `watch` when given.
    pub async fn producer(
        &self,
        index_id: &str,
        stats: Arc<SinkStats>,
        watch: Option<Watch>,
    ) -> anyhow::Result<Producer> {
        let topic = Self::topic(index_id);
        let client = self.producer_client().await?;
        let mut partitions = Vec::new();
        let mut offsets = Vec::new();
        for p in 0..self.partitions {
            let pc = client
                .partition_client(topic.clone(), p, UnknownTopicHandling::Retry)
                .await
                .with_context(|| format!("Kafka partition {p} of {topic}"))?;
            offsets.push(
                pc.get_offset(OffsetAt::Latest)
                    .await
                    .with_context(|| format!("Kafka partition {p} of {topic}: its end"))?,
            );
            partitions.push(pc);
        }
        Ok(Producer {
            kafka: self.clone(),
            topic,
            partitions,
            offsets,
            next: 0,
            batch: Vec::new(),
            batch_bytes: 0,
            stats,
            watch,
            lease_lost: None,
        })
    }
}

/// The backoff that stops at the first error.
pub fn no_retries() -> BackoffConfig {
    BackoffConfig {
        deadline: Some(std::time::Duration::ZERO),
        ..BackoffConfig::default()
    }
}

/// How long a produce whose answer was lost is worked out for: the broker
/// can stall for seconds when the disk is busy (21 October 2026, #283).
const LOST_PRODUCE_SECS: u64 = 600;

/// What a partition's end says about a produce whose answer was lost, given
/// the end before it (`expected`) and its record count: this producer is
/// the topic's only one, and a produce to one partition is one record
/// batch, appended whole or not at all.
#[derive(Debug, PartialEq, Eq)]
enum Lost {
    /// The batch is there: go on.
    Written,
    /// Nothing was appended: send it again.
    NotWritten,
    /// Anything else: someone else wrote, or records went missing.
    Unexplained,
}

fn lost(expected: i64, n: i64, end: i64) -> Lost {
    if end == expected + n {
        Lost::Written
    } else if end == expected {
        Lost::NotWritten
    } else {
        Lost::Unexplained
    }
}

/// Keeps a producer within reach of what the index has taken. Before a
/// produce (at most every `poll`), and at the end until the index has every
/// record ([`Producer::drain`]), it reads the index's Kafka checkpoint from
/// the writer node (the position each partition's published splits reach)
/// and:
/// - fails at once if a partition's oldest held record is past the index's
///   position: records expired unread;
/// - waits while the index is more than `max_lag` records behind;
/// - when the checkpoint stands still for `stall` with records waiting,
///   restarts the writer (its source resumes from the checkpoint), at most
///   [`MAX_RESTARTS`] times and only after progress since the last one, and
///   otherwise fails.
pub struct Watch {
    http: reqwest::Client,
    /// The index's metadata on the writer node.
    url: String,
    restart: Option<Arc<dyn Restart>>,
    max_lag: i64,
    stall: Duration,
    /// After a restart, how long the new writer gets to show progress: it
    /// starts, its consumers join the group (a member that left uncleanly
    /// holds it for up to the session timeout), and a pipeline publishes a
    /// split.
    restart_grace: Duration,
    poll: Duration,
    /// How long one read of the checkpoint may take: a writer that doesn't
    /// answer is one that stands still.
    read_timeout: Duration,
    last_check: Option<Instant>,
    /// The positions last read, kept when a read fails.
    last_next: Vec<i64>,
    /// The index's position summed over the partitions, when it last moved.
    indexed: i64,
    moved: Instant,
    restarts: u32,
    /// A restart that hasn't been followed by progress yet.
    restarted: bool,
    /// The producer is held until the index catches up.
    waiting: bool,
}

impl Watch {
    /// For `index` on the writer node at `base`, with the settings from the
    /// environment.
    pub fn new(
        http: reqwest::Client,
        base: &str,
        index: &str,
        restart: Option<Arc<dyn Restart>>,
    ) -> anyhow::Result<Self> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        let max_lag = match get(MAX_LAG_VAR) {
            None => DEFAULT_MAX_LAG,
            Some(v) => match v.trim().parse::<i64>() {
                Ok(n) if n > 0 => n,
                _ => bail!("{MAX_LAG_VAR} must be a positive number of records, not `{v}`"),
            },
        };
        let stall = match get(STALL_VAR) {
            None => DEFAULT_STALL_SECS,
            Some(v) => match v.trim().parse::<u64>() {
                Ok(n) if n >= 60 => n,
                _ => bail!("{STALL_VAR} must be at least 60 seconds, not `{v}`"),
            },
        };
        // A pipeline publishes once per commit timeout, so the checkpoint
        // can stand still that long while healthy (as `finish` allows).
        let commit = u64::from(crate::sink::WriterTuning::from_env()?.commit_timeout_secs);
        let stall = stall.max(commit + 600);
        Ok(Self::with(
            http,
            base,
            index,
            restart,
            max_lag,
            Duration::from_secs(stall),
            Duration::from_millis(u64::from(CONSUMER_SESSION_MS)) + Duration::from_secs(120),
            Duration::from_secs(10),
            Duration::from_secs(30),
        ))
    }

    // The settings `new` reads from the environment, given; for the tests.
    #[allow(clippy::too_many_arguments)]
    fn with(
        http: reqwest::Client,
        base: &str,
        index: &str,
        restart: Option<Arc<dyn Restart>>,
        max_lag: i64,
        stall: Duration,
        restart_grace: Duration,
        poll: Duration,
        read_timeout: Duration,
    ) -> Self {
        Self {
            http,
            url: format!("{}/api/v1/indexes/{index}", base.trim_end_matches('/')),
            restart,
            max_lag,
            stall,
            restart_grace,
            poll,
            read_timeout,
            last_check: None,
            last_next: Vec::new(),
            indexed: -1,
            moved: Instant::now(),
            restarts: 0,
            restarted: false,
            waiting: false,
        }
    }

    fn due(&self) -> bool {
        self.last_check.is_none_or(|t| t.elapsed() >= self.poll)
    }

    /// The next offset the index reads in each of `n` partitions.
    async fn positions(&self, n: usize) -> anyhow::Result<Vec<i64>> {
        let resp = self
            .http
            .get(&self.url)
            .timeout(self.read_timeout)
            .send()
            .await
            .context("reading the index's Kafka checkpoint")?;
        let status = resp.status();
        let text = resp.text().await.unwrap_or_default();
        if !status.is_success() {
            bail!(
                "reading the index's Kafka checkpoint returned {status}: {}",
                text.chars().take(200).collect::<String>()
            );
        }
        let v: serde_json::Value = serde_json::from_str(&text).context("the index's metadata")?;
        next_offsets(&v["checkpoint"]["kafka"], n)
    }

    /// One look at the index against `ends`, the producer's partition ends,
    /// and `earliest`, each partition's oldest held record: `Ok(true)` once
    /// the index is within `target` records of the ends, `Ok(false)` to look
    /// again after `poll`. Fails if records expired unread, the writer lock
    /// was lost, or the checkpoint stood still past its restarts.
    async fn check(
        &mut self,
        ends: &[i64],
        earliest: &[i64],
        target: i64,
        lease_lost: Option<&std::sync::atomic::AtomicBool>,
    ) -> anyhow::Result<bool> {
        let lost = || lease_lost.is_some_and(|l| l.load(std::sync::atomic::Ordering::SeqCst));
        if lost() {
            bail!("the writer lock could not be renewed; the Kafka producer stopped");
        }
        // A failed read (a writer that hangs or is restarting) counts as
        // the index standing still: the stall rules below decide.
        let next = match self.positions(ends.len()).await {
            Ok(next) => {
                self.last_next.clone_from(&next);
                next
            }
            Err(e) => {
                tracing::warn!(error = %format!("{e:#}"), "Kafka: the index's checkpoint couldn't be read");
                if self.last_next.len() != ends.len() {
                    self.last_next = vec![0; ends.len()];
                }
                self.last_next.clone()
            }
        };
        self.last_check = Some(Instant::now());
        for (p, (&at, &first)) in next.iter().zip(earliest).enumerate() {
            if first > at {
                bail!(
                    "Kafka partition {p}: records {at} to {} expired before the index read \
                     them (its oldest held record is {first})",
                    first - 1
                );
            }
        }
        let sum: i64 = next.iter().sum();
        let lag: i64 = ends.iter().zip(&next).map(|(e, i)| (e - i).max(0)).sum();
        if sum > self.indexed || lag == 0 {
            self.indexed = self.indexed.max(sum);
            self.moved = Instant::now();
            self.restarted = false;
        }
        let allowed = if self.restarted {
            self.stall.max(self.restart_grace)
        } else {
            self.stall
        };
        if lag > 0 && self.moved.elapsed() >= allowed {
            let secs = self.moved.elapsed().as_secs();
            match &self.restart {
                Some(r) if !self.restarted && self.restarts < MAX_RESTARTS => {
                    if lost() {
                        bail!("the writer lock could not be renewed; the writer isn't restarted");
                    }
                    tracing::warn!(
                        lag,
                        secs,
                        restarts = self.restarts + 1,
                        "Kafka: the index's checkpoint stood still; restarting the writer"
                    );
                    r.restart()
                        .await
                        .context("restarting the writer for a stalled Kafka source")?;
                    self.restarts += 1;
                    self.restarted = true;
                    self.moved = Instant::now();
                }
                _ => bail!(
                    "indexing from Kafka stalled: the index's checkpoint hasn't moved for \
                     {secs} s with {lag} records waiting (writer restarts: {})",
                    self.restarts
                ),
            }
        }
        if lag <= target {
            if self.waiting {
                tracing::info!(lag, "Kafka: the index caught up; sending again");
                self.waiting = false;
            }
            return Ok(true);
        }
        if !self.waiting {
            tracing::info!(lag, target, "Kafka: waiting for the index to catch up");
            self.waiting = true;
        }
        Ok(false)
    }
}

/// The next offset to read in each of `n` partitions, from a Kafka source's
/// checkpoint (`{"00000000000000000003": "00000000000002123600", …}`: each
/// partition's last offset taken). A partition not in it starts at 0.
fn next_offsets(checkpoint: &serde_json::Value, n: usize) -> anyhow::Result<Vec<i64>> {
    let mut next = vec![0i64; n];
    let Some(map) = checkpoint.as_object() else {
        return Ok(next);
    };
    for (k, v) in map {
        let p: usize = k
            .trim()
            .parse()
            .with_context(|| format!("checkpoint partition `{k}`"))?;
        let at = v.as_str().unwrap_or_default().trim();
        if p >= n || at.is_empty() {
            continue;
        }
        let last: i64 = at
            .parse()
            .with_context(|| format!("checkpoint position `{at}` for partition {p}"))?;
        next[p] = last + 1;
    }
    Ok(next)
}

/// Records to one partition at a time; [`Producer::end_file`] moves on.
pub struct Producer {
    kafka: Kafka,
    topic: String,
    partitions: Vec<PartitionClient>,
    /// Each partition's end as this producer last saw it.
    offsets: Vec<i64>,
    next: usize,
    batch: Vec<Record>,
    batch_bytes: usize,
    stats: Arc<SinkStats>,
    watch: Option<Watch>,
    /// Set when the writer lock couldn't be renewed (`crate::decade_order`).
    lease_lost: Option<Arc<std::sync::atomic::AtomicBool>>,
}

impl Producer {
    pub async fn add_line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if line.len() > MAX_MESSAGE_BYTES - 1024 {
            bail!(
                "a document is {} bytes, over the Kafka record limit",
                line.len()
            );
        }
        if self.batch_bytes + line.len() > BATCH_BYTES {
            self.produce().await?;
        }
        self.batch_bytes += line.len();
        self.batch.push(Record {
            key: None,
            value: Some(line.to_vec()),
            headers: BTreeMap::new(),
            timestamp: chrono::Utc::now(),
        });
        Ok(())
    }

    /// The file's last records, then the next partition for the next file.
    pub async fn end_file(&mut self) -> anyhow::Result<()> {
        self.produce().await?;
        self.next = (self.next + 1) % self.partitions.len();
        Ok(())
    }

    async fn produce(&mut self) -> anyhow::Result<()> {
        if self.batch.is_empty() {
            return Ok(());
        }
        if self.watch.as_ref().is_some_and(Watch::due) {
            let target = self.watch.as_ref().map_or(0, |w| w.max_lag);
            self.keep_up(target).await?;
        }
        let records = std::mem::take(&mut self.batch);
        let (n, bytes) = (records.len() as u64, self.batch_bytes as u64);
        self.batch_bytes = 0;
        let p = self.next;
        let expected = self.offsets[p];
        // The client never retries by itself (`no_retries`): a produce whose
        // answer was lost may have been appended, and sending it blind could
        // duplicate it. The partition's end says which (`lost`).
        let first = self.partitions[p]
            .produce(records.clone(), Compression::Zstd)
            .await;
        let err = match first {
            Ok(_) => {
                self.offsets[p] = expected + n as i64;
                self.stats.sent(n, bytes);
                return Ok(());
            }
            Err(e) => anyhow::Error::new(e),
        };
        let started = std::time::Instant::now();
        let mut wait = std::time::Duration::from_secs(1);
        let mut last = err;
        loop {
            if started.elapsed().as_secs() > LOST_PRODUCE_SECS {
                return Err(last).with_context(|| {
                    format!(
                        "producing to Kafka partition {p}: unresolved after {LOST_PRODUCE_SECS} s"
                    )
                });
            }
            tracing::warn!(partition = p, error = %format!("{last:#}"), "Kafka produce failed; checking the partition's end");
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(std::time::Duration::from_secs(30));
            // A new connection: the old one may be poisoned.
            let pc = match self.reconnect(p).await {
                Ok(pc) => pc,
                Err(e) => {
                    last = e;
                    continue;
                }
            };
            let end = match pc.get_offset(OffsetAt::Latest).await {
                Ok(end) => end,
                Err(e) => {
                    last = e.into();
                    continue;
                }
            };
            self.partitions[p] = pc;
            match lost(expected, n as i64, end) {
                Lost::Written => {
                    tracing::warn!(partition = p, records = n, "the lost produce was written");
                }
                Lost::NotWritten => {
                    if let Err(e) = self.partitions[p]
                        .produce(records.clone(), Compression::Zstd)
                        .await
                    {
                        last = e.into();
                        continue;
                    }
                    tracing::warn!(partition = p, records = n, "sent the lost produce again");
                }
                Lost::Unexplained => bail!(
                    "Kafka partition {p} ends at {end}, not {expected} or {}: \
                     a lost produce can't be accounted for",
                    expected + n as i64
                ),
            }
            self.offsets[p] = expected + n as i64;
            self.stats.sent(n, bytes);
            return Ok(());
        }
    }

    /// The writer lock's flag, for the watch to stop on.
    pub fn set_lease_lost(&mut self, lost: Option<Arc<std::sync::atomic::AtomicBool>>) {
        self.lease_lost = lost;
    }

    /// Wait, under the watch, until the index is within `target` records of
    /// what this producer has sent.
    async fn keep_up(&mut self, target: i64) -> anyhow::Result<()> {
        loop {
            let earliest = self.earliest().await?;
            let Some(w) = self.watch.as_mut() else {
                return Ok(());
            };
            if w.check(&self.offsets, &earliest, target, self.lease_lost.as_deref())
                .await?
            {
                return Ok(());
            }
            tokio::time::sleep(w.poll).await;
        }
    }

    /// The last records, then a wait until the index holds every one, under
    /// the watch: a stall in the last stretch gets the same restart as one
    /// earlier, not a failed count after the merges' wait.
    pub async fn drain(&mut self) -> anyhow::Result<()> {
        self.produce().await?;
        if self.watch.is_some() {
            self.keep_up(0).await?;
            tracing::info!("Kafka: the index has every record sent");
        }
        Ok(())
    }

    /// Each partition's oldest record still held, to tell an expired one.
    /// Only reads, so a closed connection is opened again and the read
    /// retried: the producer's own connections never retry (`no_retries`),
    /// and the broker closes one that is idle for 10 minutes.
    async fn earliest(&mut self) -> anyhow::Result<Vec<i64>> {
        let mut out = Vec::with_capacity(self.partitions.len());
        for p in 0..self.partitions.len() {
            let started = Instant::now();
            let mut wait = Duration::from_secs(1);
            let at = loop {
                let err = match self.partitions[p].get_offset(OffsetAt::Earliest).await {
                    Ok(at) => break at,
                    Err(e) => anyhow::Error::new(e),
                };
                if started.elapsed().as_secs() > LOST_PRODUCE_SECS {
                    return Err(err).with_context(|| {
                        format!("Kafka partition {p} of {}: its start", self.topic)
                    });
                }
                tokio::time::sleep(wait).await;
                wait = (wait * 2).min(Duration::from_secs(30));
                if let Ok(pc) = self.reconnect(p).await {
                    self.partitions[p] = pc;
                }
            };
            out.push(at);
        }
        Ok(out)
    }

    async fn reconnect(&self, p: usize) -> anyhow::Result<PartitionClient> {
        let client = self.kafka.producer_client().await?;
        client
            .partition_client(self.topic.clone(), p as i32, UnknownTopicHandling::Retry)
            .await
            .with_context(|| format!("Kafka partition {p} of {}", self.topic))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_come_from_the_environment() {
        assert_eq!(Kafka::from_lookup(None, None).unwrap(), None);
        let k = Kafka::from_lookup(Some("127.0.0.1:9092".into()), None)
            .unwrap()
            .unwrap();
        assert_eq!((k.partitions, k.cpu_capacity()), (8, 32));
        let k = Kafka::from_lookup(Some("kafka.local:9092".into()), Some("4".into()))
            .unwrap()
            .unwrap();
        assert_eq!(k.partitions, 4);
        for bad in ["9092", "host:", ":9092", "host:x"] {
            assert!(Kafka::from_lookup(Some(bad.into()), None).is_err(), "{bad}");
        }
        for bad in ["0", "65", "x"] {
            assert!(
                Kafka::from_lookup(Some("h:1".into()), Some(bad.into())).is_err(),
                "{bad}"
            );
        }
    }

    #[test]
    fn a_lost_produce_is_told_by_the_partition_end() {
        assert_eq!(lost(100, 10, 110), Lost::Written);
        assert_eq!(lost(100, 10, 100), Lost::NotWritten);
        assert_eq!(lost(100, 10, 105), Lost::Unexplained);
        assert_eq!(lost(100, 10, 120), Lost::Unexplained);
        assert_eq!(lost(100, 10, 99), Lost::Unexplained);
    }

    #[test]
    fn a_checkpoint_gives_each_partitions_next_offset() {
        let cp = serde_json::json!({
            "00000000000000000000": "00000000000002159999",
            "00000000000000000002": "00000000000002099999",
            "00000000000000000003": "",
        });
        assert_eq!(next_offsets(&cp, 4).unwrap(), [2_160_000, 0, 2_100_000, 0]);
        assert_eq!(next_offsets(&serde_json::Value::Null, 2).unwrap(), [0, 0]);
        assert!(next_offsets(&serde_json::json!({"x": "1"}), 2).is_err());
    }

    #[test]
    fn the_source_outlasts_a_stalled_broker() {
        let k = Kafka::from_lookup(Some("127.0.0.1:9092".into()), Some("4".into()))
            .unwrap()
            .unwrap();
        let y = k.source_yaml("pages-base-20261022-1");
        assert!(y.contains("session.timeout.ms: 300000"), "{y}");
        assert!(y.contains("max.poll.interval.ms: 900000"), "{y}");
        assert!(y.contains("heartbeat.interval.ms: 10000"), "{y}");
    }

    /// A writer node whose index checkpoint is `cp` (one offset per
    /// partition, the last taken), and a restart that counts.
    async fn checkpoint_node(cp: Arc<std::sync::Mutex<Vec<i64>>>) -> String {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            "/api/v1/indexes/idx",
            get(move || {
                let cp = cp.lock().unwrap().clone();
                async move {
                    let m: serde_json::Map<String, serde_json::Value> = cp
                        .iter()
                        .enumerate()
                        .filter(|(_, &v)| v >= 0)
                        .map(|(p, v)| (format!("{p:020}"), serde_json::json!(format!("{v:020}"))))
                        .collect();
                    Json(serde_json::json!({"checkpoint": {"kafka": m}}))
                }
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[derive(Default)]
    struct Counted(std::sync::atomic::AtomicU32);

    #[async_trait::async_trait]
    impl Restart for Counted {
        async fn restart(&self) -> anyhow::Result<()> {
            self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(())
        }
    }

    fn watch(base: &str, restart: Arc<Counted>, max_lag: i64) -> Watch {
        Watch::with(
            reqwest::Client::new(),
            base,
            "idx",
            Some(restart as Arc<dyn Restart>),
            max_lag,
            Duration::from_millis(150),
            Duration::from_millis(300),
            Duration::from_millis(10),
            Duration::from_millis(50),
        )
    }

    /// The producer's loop, without Kafka: `earliest` stays as given.
    async fn keep_up(w: &mut Watch, ends: &[i64], earliest: &[i64]) -> anyhow::Result<()> {
        let target = w.max_lag;
        while !w.check(ends, earliest, target, None).await? {
            tokio::time::sleep(w.poll).await;
        }
        Ok(())
    }

    #[tokio::test]
    async fn a_lost_writer_lock_stops_the_watch() {
        let cp = Arc::new(std::sync::Mutex::new(vec![99]));
        let base = checkpoint_node(Arc::clone(&cp)).await;
        let restarts = Arc::new(Counted::default());
        let mut w = watch(&base, Arc::clone(&restarts), 50);
        let lost = std::sync::atomic::AtomicBool::new(true);
        let err = w
            .check(&[1_000], &[0], 50, Some(&lost))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("writer lock"), "{err}");
        assert_eq!(restarts.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Caught up to the last record: the drain's target.
        cp.lock().unwrap()[0] = 999;
        assert!(w.check(&[1_000], &[0], 0, None).await.unwrap());
    }

    #[tokio::test]
    async fn the_producer_waits_while_the_index_is_far_behind() {
        let cp = Arc::new(std::sync::Mutex::new(vec![99, 99]));
        let base = checkpoint_node(Arc::clone(&cp)).await;
        let restarts = Arc::new(Counted::default());
        let mut w = watch(&base, Arc::clone(&restarts), 500);
        // 1,800 records behind: it waits until the index catches up.
        let catch_up = {
            let cp = Arc::clone(&cp);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(60)).await;
                *cp.lock().unwrap() = vec![799, 799];
            })
        };
        let t = Instant::now();
        keep_up(&mut w, &[1_000, 1_000], &[0, 0]).await.unwrap();
        assert!(t.elapsed() >= Duration::from_millis(50));
        catch_up.await.unwrap();
        assert_eq!(restarts.0.load(std::sync::atomic::Ordering::SeqCst), 0);
        // Within the limit: no wait.
        keep_up(&mut w, &[1_000, 1_000], &[0, 0]).await.unwrap();
    }

    #[tokio::test]
    async fn a_stalled_index_restarts_the_writer_once_then_fails() {
        let cp = Arc::new(std::sync::Mutex::new(vec![99, -1]));
        let base = checkpoint_node(Arc::clone(&cp)).await;
        let restarts = Arc::new(Counted::default());
        let mut w = watch(&base, Arc::clone(&restarts), 50);
        let err = keep_up(&mut w, &[1_000, 1_000], &[0, 0])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("stalled"), "{err}");
        assert_eq!(restarts.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn progress_after_a_restart_allows_another() {
        let cp = Arc::new(std::sync::Mutex::new(vec![99]));
        let base = checkpoint_node(Arc::clone(&cp)).await;
        let restarts = Arc::new(Counted::default());
        let mut w = watch(&base, Arc::clone(&restarts), 50);
        let mover = {
            let cp = Arc::clone(&cp);
            let restarts = Arc::clone(&restarts);
            tokio::spawn(async move {
                // After each restart, the index moves a little, then stalls.
                for round in 1..=2u32 {
                    while restarts.0.load(std::sync::atomic::Ordering::SeqCst) < round {
                        tokio::time::sleep(Duration::from_millis(5)).await;
                    }
                    tokio::time::sleep(Duration::from_millis(20)).await;
                    cp.lock().unwrap()[0] += 10;
                }
            })
        };
        let err = keep_up(&mut w, &[1_000], &[0])
            .await
            .unwrap_err()
            .to_string();
        mover.await.unwrap();
        assert!(err.contains("stalled"), "{err}");
        assert_eq!(restarts.0.load(std::sync::atomic::Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_writer_that_does_not_answer_is_restarted() {
        use axum::{routing::get, Router};
        let app = Router::new().route(
            "/api/v1/indexes/idx",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                "{}"
            }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        let restarts = Arc::new(Counted::default());
        let mut w = watch(&format!("http://{addr}"), Arc::clone(&restarts), 50);
        // Not an error: it waits, restarts the writer, then fails as stalled.
        let err = keep_up(&mut w, &[1_000], &[0])
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("stalled"), "{err}");
        assert_eq!(restarts.0.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn records_expired_unread_fail_at_once() {
        let cp = Arc::new(std::sync::Mutex::new(vec![2_159_999, 2_099_999]));
        let base = checkpoint_node(Arc::clone(&cp)).await;
        let restarts = Arc::new(Counted::default());
        let mut w = watch(&base, Arc::clone(&restarts), 10_000_000);
        let err = keep_up(&mut w, &[5_946_669, 5_912_000], &[4_556_906, 0])
            .await
            .unwrap_err()
            .to_string();
        assert!(
            err.contains("partition 0") && err.contains("expired"),
            "{err}"
        );
    }

    #[test]
    fn the_producer_never_retries() {
        assert_eq!(no_retries().deadline, Some(std::time::Duration::ZERO));
    }

    #[test]
    fn the_source_reads_the_topic_with_a_pipeline_per_partition() {
        let k = Kafka::from_lookup(Some("127.0.0.1:9092".into()), Some("4".into()))
            .unwrap()
            .unwrap();
        let y = k.source_yaml("pages-base-20261020-1");
        assert!(y.contains("source_type: kafka"));
        assert!(y.contains("num_pipelines: 4"));
        assert!(y.contains("topic: usnm-pages-base-20261020-1"));
        assert!(y.contains("bootstrap.servers: 127.0.0.1:9092"));
        assert!(y.contains("auto.offset.reset: earliest"));
    }
}
