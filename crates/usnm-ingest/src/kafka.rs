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
//! `USNM_WRITER_KAFKA` names the broker (`host:port`), with
//! `USNM_WRITER_INGEST=v1`; `USNM_WRITER_KAFKA_PARTITIONS` the partitions and
//! pipelines (default 8).

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{bail, Context};
use rskafka::client::partition::{Compression, PartitionClient, UnknownTopicHandling};
use rskafka::client::{Client, ClientBuilder};
use rskafka::record::Record;
use rskafka::BackoffConfig;

use crate::sink::SinkStats;

pub const VAR: &str = "USNM_WRITER_KAFKA";
pub const PARTITIONS_VAR: &str = "USNM_WRITER_KAFKA_PARTITIONS";
const DEFAULT_PARTITIONS: i32 = 8;
/// Bytes of records per produce request, under the broker's 16 MB message limit.
const BATCH_BYTES: usize = 4 * 1024 * 1024;
/// The topic's record size limit, set when the release creates it.
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;

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
    /// partition, from the start of the topic.
    pub fn source_yaml(&self, index_id: &str) -> String {
        format!(
            "version: 0.8\nsource_id: kafka\nsource_type: kafka\nnum_pipelines: {}\nparams:\n  \
             topic: {}\n  client_params:\n    bootstrap.servers: {}\n    \
             auto.offset.reset: earliest\n    fetch.message.max.bytes: {MAX_MESSAGE_BYTES}\n    \
             max.partition.fetch.bytes: {MAX_MESSAGE_BYTES}\n",
            self.partitions,
            Self::topic(index_id),
            self.bootstrap
        )
    }

    /// A producer to `index_id`'s topic.
    pub async fn producer(
        &self,
        index_id: &str,
        stats: Arc<SinkStats>,
    ) -> anyhow::Result<Producer> {
        let topic = Self::topic(index_id);
        let client = self.producer_client().await?;
        let mut partitions = Vec::new();
        for p in 0..self.partitions {
            partitions.push(
                client
                    .partition_client(topic.clone(), p, UnknownTopicHandling::Retry)
                    .await
                    .with_context(|| format!("Kafka partition {p} of {topic}"))?,
            );
        }
        Ok(Producer {
            partitions,
            next: 0,
            batch: Vec::new(),
            batch_bytes: 0,
            stats,
        })
    }
}

/// The writer node in a Quickwit cluster with remote indexers (#936):
/// `USNM_WRITER_ADVERTISE` names this host on the LAN (resolved when the node
/// starts, so no address is configured), `USNM_WRITER_PEERS` the other
/// nodes (`host:port`, comma separated), `USNM_WRITER_KAFKA_LOCAL_PIPELINES`
/// how many of the Kafka source's pipelines this node takes (the control
/// plane gives the rest to the peers), and `USNM_WRITER_CLUSTER_DONE` the
/// file the release writes once the index holds every page, which stops the
/// remote indexers (`scripts/onprem/indexer.sh`) before the final merges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cluster {
    pub advertise: String,
    pub peers: Vec<String>,
    pub local_pipelines: i32,
    pub done_marker: Option<std::path::PathBuf>,
}

impl Cluster {
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let get = |k: &str| std::env::var(k).ok().filter(|v| !v.trim().is_empty());
        Self::from_lookup(
            get("USNM_WRITER_ADVERTISE"),
            get("USNM_WRITER_PEERS"),
            get("USNM_WRITER_KAFKA_LOCAL_PIPELINES"),
            get("USNM_WRITER_CLUSTER_DONE"),
        )
    }

    pub fn from_lookup(
        advertise: Option<String>,
        peers: Option<String>,
        local: Option<String>,
        done: Option<String>,
    ) -> anyhow::Result<Option<Self>> {
        let Some(advertise) = advertise.map(|a| a.trim().to_owned()) else {
            return Ok(None);
        };
        let peers: Vec<String> = peers
            .unwrap_or_default()
            .split(',')
            .map(|p| p.trim().to_owned())
            .filter(|p| !p.is_empty())
            .collect();
        if peers.is_empty() {
            bail!("USNM_WRITER_ADVERTISE needs USNM_WRITER_PEERS: the remote indexers, host:port");
        }
        for p in &peers {
            let ok = p
                .rsplit_once(':')
                .is_some_and(|(h, port)| !h.is_empty() && port.parse::<u16>().is_ok_and(|n| n > 0));
            if !ok {
                bail!("USNM_WRITER_PEERS: `{p}` isn't host:port");
            }
        }
        let local_pipelines = match local {
            None => 1,
            Some(v) => match v.trim().parse::<i32>() {
                Ok(n @ 1..=64) => n,
                _ => bail!("USNM_WRITER_KAFKA_LOCAL_PIPELINES must be 1 to 64, not `{v}`"),
            },
        };
        Ok(Some(Self {
            advertise,
            peers,
            local_pipelines,
            done_marker: done.map(std::path::PathBuf::from),
        }))
    }

    /// The advertised host's IPv4 address, looked up now.
    pub fn advertise_ip(&self) -> anyhow::Result<std::net::IpAddr> {
        use std::net::ToSocketAddrs;
        (self.advertise.as_str(), 0)
            .to_socket_addrs()
            .with_context(|| format!("resolving USNM_WRITER_ADVERTISE `{}`", self.advertise))?
            .map(|a| a.ip())
            .find(std::net::IpAddr::is_ipv4)
            .with_context(|| format!("`{}` has no IPv4 address", self.advertise))
    }

    /// The node config lines that make the writer a cluster member.
    pub fn node_yaml(&self) -> anyhow::Result<String> {
        let peers: Vec<String> = self.peers.iter().map(|p| format!("  - {p}")).collect();
        Ok(format!(
            "advertise_address: {}\npeer_seeds:\n{}\n",
            self.advertise_ip()?,
            peers.join("\n")
        ))
    }
}

/// The backoff that stops at the first error.
pub fn no_retries() -> BackoffConfig {
    BackoffConfig {
        deadline: Some(std::time::Duration::ZERO),
        ..BackoffConfig::default()
    }
}

/// Records to one partition at a time; [`Producer::end_file`] moves on.
pub struct Producer {
    partitions: Vec<PartitionClient>,
    next: usize,
    batch: Vec<Record>,
    batch_bytes: usize,
    stats: Arc<SinkStats>,
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
        let records = std::mem::take(&mut self.batch);
        let (n, bytes) = (records.len() as u64, self.batch_bytes as u64);
        self.batch_bytes = 0;
        let partition = &self.partitions[self.next];
        // Not retried: a produce whose answer was lost may have been
        // written, and a second one would duplicate it.
        partition
            .produce(records, Compression::Zstd)
            .await
            .with_context(|| format!("producing to Kafka partition {}", partition.partition()))?;
        self.stats.sent(n, bytes);
        Ok(())
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
    fn a_cluster_needs_peers_and_takes_few_pipelines_here() {
        assert_eq!(Cluster::from_lookup(None, None, None, None).unwrap(), None);
        assert!(Cluster::from_lookup(Some("localhost".into()), None, None, None).is_err());
        let c = Cluster::from_lookup(
            Some("localhost".into()),
            Some("index2.example.org:7380, index3.example.org:7380".into()),
            None,
            Some("/srv/usnm/runs/x/cluster-done".into()),
        )
        .unwrap()
        .unwrap();
        assert_eq!(c.peers.len(), 2);
        assert_eq!(c.local_pipelines, 1);
        let y = c.node_yaml().unwrap();
        assert!(
            y.contains("peer_seeds:\n  - index2.example.org:7380"),
            "{y}"
        );
        assert!(y.starts_with("advertise_address: 127."), "{y}");
        assert!(Cluster::from_lookup(Some("h".into()), Some("nope".into()), None, None).is_err());
        assert!(Cluster::from_lookup(
            Some("h".into()),
            Some("h2:1".into()),
            Some("0".into()),
            None
        )
        .is_err());
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
