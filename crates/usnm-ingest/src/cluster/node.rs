//! Starting a cluster node (#239): Container Apps gives each replica an IP
//! of its own (100.100.x.x, reachable only inside the environment), and an
//! app's name resolves to a service IP that carries only its ingress port,
//! so Quickwit's UDP gossip and gRPC must use replica IPs. Those change
//! whenever a replica restarts, so the nodes find each other through a seed
//! registry: one blob per node, `seeds/{node_id}.json`, holding its current
//! gossip address.
//!
//! At start a node:
//! 1. finds its replica IP (its hostname's address, as `hostname -i`);
//! 2. writes its entry;
//! 3. reads every other node's entry, and passes their addresses to Quickwit
//!    as `QW_PEER_SEEDS`, with its own IP as `QW_ADVERTISE_ADDRESS`;
//! 4. becomes Quickwit (exec), with the node config filled in.
//!
//! Every node writes before it reads, so of two nodes starting together at
//! least one sees the other's new entry, and gossip needs only one live
//! seed: they join. A node that restarts at a new IP reads the others'
//! current addresses and joins through them; they learn its new address by
//! gossip, and its old one stops answering and is dropped as dead. An entry
//! left by a replica that is gone is a seed that never answers, which
//! Quickwit tolerates.

use std::collections::BTreeMap;
use std::net::{IpAddr, ToSocketAddrs};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use usnm_store::ObjectStore;

/// The node config every member fills in (infra/quickwit/cluster-node.yaml).
pub const NODE_TEMPLATE: &str = include_str!("../../../../infra/quickwit/cluster-node.yaml");

/// Where the entries live in the registry store.
pub const SEEDS_PREFIX: &str = "seeds";

/// Quickwit services a node may run (`enabled_services`).
const SERVICES: &[&str] = &[
    "control_plane",
    "indexer",
    "janitor",
    "metastore",
    "searcher",
];

/// One node's settings, from its flags.
#[derive(Debug, Clone)]
pub struct NodeSpec {
    pub node_id: String,
    pub services: Vec<String>,
    /// REST and gossip (UDP); gRPC is the next port.
    pub rest_port: u16,
    pub data_dir: PathBuf,
    pub metastore: String,
    pub index_root: String,
    pub storage_account: String,
    /// `searcher.max_num_concurrent_split_searches`; `None` keeps the
    /// sidecar's value (infra/quickwit/searcher.yaml).
    pub split_searches: Option<u32>,
}

/// The template's line for concurrent split searches (the sidecar's value).
const SPLIT_SEARCHES_LINE: &str = "max_num_concurrent_split_searches: 8";

impl NodeSpec {
    /// The node config, from [`NODE_TEMPLATE`].
    pub fn config(&self) -> anyhow::Result<String> {
        if !usnm_store::is_safe_segment(&self.node_id) {
            bail!("`{}` isn't a usable node id", self.node_id);
        }
        if self.services.is_empty() {
            bail!("a node needs at least one service");
        }
        for s in &self.services {
            if !SERVICES.contains(&s.as_str()) {
                bail!("unknown Quickwit service `{s}` (one of {SERVICES:?})");
            }
        }
        let grpc = self
            .rest_port
            .checked_add(1)
            .context("the REST port must be below 65535")?;
        let out = NODE_TEMPLATE
            .replace("__NODE_ID__", &self.node_id)
            .replace("__SERVICES__", &self.services.join(", "))
            .replace("__REST_PORT__", &self.rest_port.to_string())
            .replace("__GRPC_PORT__", &grpc.to_string())
            .replace("__DATA_DIR__", &self.data_dir.display().to_string())
            .replace("__METASTORE__", &self.metastore)
            .replace("__INDEX_ROOT__", &self.index_root)
            .replace("__STORAGE_ACCOUNT__", &self.storage_account);
        let out = match self.split_searches {
            Some(n) => {
                anyhow::ensure!(n > 0, "concurrent split searches must be at least 1");
                anyhow::ensure!(
                    out.contains(SPLIT_SEARCHES_LINE),
                    "the node config has no `{SPLIT_SEARCHES_LINE}` line to change"
                );
                out.replace(
                    SPLIT_SEARCHES_LINE,
                    &format!("max_num_concurrent_split_searches: {n}"),
                )
            }
            None => out,
        };
        if let Some(line) = out
            .lines()
            .find(|l| !l.trim_start().starts_with('#') && l.contains("__"))
        {
            bail!("the node config still has a placeholder: {line}");
        }
        Ok(out)
    }
}

/// A node's entry in the seed registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Registration {
    pub node_id: String,
    /// `ip:port`, the node's gossip address (its REST port).
    pub addr: String,
    pub registered_at: DateTime<Utc>,
    /// The replica's name (`HOSTNAME`), for the record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub replica: Option<String>,
}

fn entry_path(node_id: &str) -> String {
    format!("{SEEDS_PREFIX}/{node_id}.json")
}

/// Write (or replace) this node's entry.
pub async fn register(store: &dyn ObjectStore, reg: &Registration) -> anyhow::Result<()> {
    store
        .put(
            &entry_path(&reg.node_id),
            serde_json::to_vec(reg)?,
            "application/json",
        )
        .await
        .with_context(|| format!("writing {}", entry_path(&reg.node_id)))
}

/// Every node's entry, by node id. An entry that doesn't parse is skipped
/// with a warning: one bad blob mustn't keep a node from starting.
pub async fn registered(store: &dyn ObjectStore) -> anyhow::Result<BTreeMap<String, Registration>> {
    let mut out = BTreeMap::new();
    for path in store.list(SEEDS_PREFIX).await? {
        if !path.ends_with(".json") {
            continue;
        }
        let Some(bytes) = store.get(&path).await? else {
            continue;
        };
        match serde_json::from_slice::<Registration>(&bytes) {
            Ok(r) => {
                out.insert(r.node_id.clone(), r);
            }
            Err(e) => tracing::warn!(path, error = %e, "skipping a seed entry that doesn't parse"),
        }
    }
    Ok(out)
}

/// The other nodes' addresses, sorted: the peer seeds.
pub fn seeds(all: &BTreeMap<String, Registration>, self_id: &str) -> Vec<String> {
    let mut out: Vec<String> = all
        .values()
        .filter(|r| r.node_id != self_id)
        .map(|r| r.addr.clone())
        .collect();
    out.sort();
    out.dedup();
    out
}

/// The address another replica reaches this one at: of `candidates` (the
/// hostname's addresses), the first IPv4 address that is neither loopback
/// nor link-local, else the first such IPv6 one. A workload-profiles
/// environment's Consumption replicas resolve their hostname to 100.100.x.x;
/// in 2023 some also listed 127.0.0.1 and 169.254.x.x (#238).
pub fn pick_address(candidates: &[IpAddr]) -> Option<IpAddr> {
    let usable = |ip: &&IpAddr| match ip {
        IpAddr::V4(v4) => !(v4.is_loopback() || v4.is_link_local() || v4.is_unspecified()),
        IpAddr::V6(v6) => {
            !(v6.is_loopback() || v6.is_unspecified() || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    };
    candidates
        .iter()
        .filter(usable)
        .find(|ip| ip.is_ipv4())
        .or_else(|| candidates.iter().find(|ip| usable(ip)))
        .copied()
}

/// This replica's address: its hostname resolved, as `hostname -i` does.
pub fn replica_address() -> anyhow::Result<IpAddr> {
    let host = std::fs::read_to_string("/proc/sys/kernel/hostname")
        .ok()
        .map(|h| h.trim().to_owned())
        .filter(|h| !h.is_empty())
        .or_else(|| std::env::var("HOSTNAME").ok().filter(|h| !h.is_empty()))
        .context("can't tell this replica's hostname; pass --advertise")?;
    let found: Vec<IpAddr> = (host.as_str(), 0)
        .to_socket_addrs()
        .with_context(|| format!("resolving this replica's hostname `{host}`"))?
        .map(|a| a.ip())
        .collect();
    pick_address(&found).with_context(|| {
        format!("`{host}` resolves to no address another replica can reach: {found:?}")
    })
}

/// The environment Quickwit runs with: what it inherited, without any
/// `QW_*` (the official image sets QW_LISTEN_ADDRESS, QW_DATA_DIR and
/// QW_CONFIG, which would override the config) and without AZURE_CLIENT_ID
/// (it selects the node's user-assigned identity for the registry; Quickwit
/// authenticates with the system-assigned one, 08 §8.2), plus its address,
/// its seeds and its CPU count.
pub fn quickwit_env(
    inherited: impl IntoIterator<Item = (String, String)>,
    advertise: IpAddr,
    seeds: &[String],
    cpus: Option<u32>,
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = inherited
        .into_iter()
        .filter(|(k, _)| !k.starts_with("QW_") && k != "AZURE_CLIENT_ID")
        .collect();
    env.push(("QW_ADVERTISE_ADDRESS".into(), advertise.to_string()));
    if !seeds.is_empty() {
        env.push(("QW_PEER_SEEDS".into(), seeds.join(",")));
    }
    // Quickwit sizes its thread pools from this, before the cgroup.
    if let Some(n) = cpus {
        env.push(("QW_NUM_CPUS".into(), n.to_string()));
    }
    env.push(("QW_DISABLE_TELEMETRY".into(), "1".into()));
    env
}

/// Register, find the peers, write the config and run Quickwit in this
/// process's place. Returns only on failure.
pub async fn start(
    registry: &dyn ObjectStore,
    spec: &NodeSpec,
    advertise: IpAddr,
    cpus: Option<u32>,
    quickwit: &Path,
) -> anyhow::Result<()> {
    let config = spec.config()?;
    std::fs::create_dir_all(&spec.data_dir)
        .with_context(|| format!("creating {}", spec.data_dir.display()))?;
    let reg = Registration {
        node_id: spec.node_id.clone(),
        addr: std::net::SocketAddr::new(advertise, spec.rest_port).to_string(),
        registered_at: Utc::now(),
        replica: std::env::var("HOSTNAME").ok(),
    };
    register(registry, &reg).await?;
    let all = registered(registry).await?;
    let peers = seeds(&all, &spec.node_id);
    tracing::info!(
        node = %spec.node_id,
        addr = %reg.addr,
        services = %spec.services.join(","),
        seeds = %peers.join(","),
        "starting a cluster node"
    );
    let path = spec.data_dir.join("node.yaml");
    std::fs::write(&path, config).with_context(|| format!("writing {}", path.display()))?;
    let env = quickwit_env(std::env::vars(), advertise, &peers, cpus);
    let mut cmd = std::process::Command::new(quickwit);
    cmd.env_clear()
        .envs(env)
        .args(["run", "--config"])
        .arg(&path);
    exec(cmd, quickwit)
}

#[cfg(unix)]
fn exec(mut cmd: std::process::Command, bin: &Path) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let err = cmd.exec();
    Err(err).with_context(|| format!("starting {}", bin.display()))
}

#[cfg(not(unix))]
fn exec(mut cmd: std::process::Command, bin: &Path) -> anyhow::Result<()> {
    let status = cmd
        .status()
        .with_context(|| format!("starting {}", bin.display()))?;
    bail!("{} exited: {status}", bin.display())
}

#[cfg(test)]
mod tests {
    use std::net::{Ipv4Addr, Ipv6Addr};

    use super::*;

    fn spec(services: &[&str]) -> NodeSpec {
        NodeSpec {
            node_id: "qw-0".into(),
            services: services.iter().map(|s| s.to_string()).collect(),
            rest_port: 7280,
            data_dir: "/work/qwdata".into(),
            metastore: "azure://qw-cluster".into(),
            index_root: "azure://qw-cluster".into(),
            storage_account: "stusnmdabc123".into(),
            split_searches: None,
        }
    }

    #[test]
    fn concurrent_split_searches_can_be_set() {
        let mut s = spec(&["searcher"]);
        assert!(s
            .config()
            .unwrap()
            .contains("max_num_concurrent_split_searches: 8"));
        s.split_searches = Some(24);
        let c = s.config().unwrap();
        assert!(c.contains("max_num_concurrent_split_searches: 24"), "{c}");
        assert!(!c.contains("max_num_concurrent_split_searches: 8"));
        s.split_searches = Some(0);
        assert!(s.config().is_err());
    }

    #[test]
    fn fills_in_the_node_config() {
        let yaml = spec(&[
            "metastore",
            "control_plane",
            "janitor",
            "indexer",
            "searcher",
        ])
        .config()
        .unwrap();
        for line in [
            "node_id: qw-0",
            "enabled_services: [metastore, control_plane, janitor, indexer, searcher]",
            "  listen_port: 7280",
            "grpc_listen_port: 7281",
            "data_dir: /work/qwdata",
            "metastore_uri: azure://qw-cluster",
            "default_index_root_uri: azure://qw-cluster",
            "    account: stusnmdabc123",
            "listen_address: 0.0.0.0",
        ] {
            assert!(yaml.lines().any(|l| l == line), "{line} in\n{yaml}");
        }
        // Never the production index (08 §8.4.1).
        assert!(!yaml.contains("qw-index"));
    }

    #[test]
    fn refuses_bad_node_settings() {
        assert!(spec(&[]).config().is_err());
        assert!(spec(&["searcher", "coordinator"]).config().is_err());
        let mut s = spec(&["searcher"]);
        s.node_id = "../qw".into();
        assert!(s.config().is_err());
        s.node_id = "qw-1".into();
        s.rest_port = 65535;
        assert!(s.config().is_err());
    }

    /// The nodes search as the API's sidecar does: the same searcher settings.
    #[test]
    fn searcher_settings_match_the_sidecar() {
        let section = |yaml: &str| -> Vec<String> {
            yaml.lines()
                .skip_while(|l| !l.starts_with("searcher:"))
                .skip(1)
                .take_while(|l| l.starts_with(' ') || l.trim().is_empty())
                .map(str::trim)
                .filter(|l| !l.is_empty() && !l.starts_with('#'))
                .map(str::to_owned)
                .collect()
        };
        let sidecar = section(include_str!("../../../../infra/quickwit/searcher.yaml"));
        assert!(!sidecar.is_empty());
        assert_eq!(section(NODE_TEMPLATE), sidecar);
    }

    #[test]
    fn picks_the_replica_address() {
        let lo = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let link = IpAddr::V4(Ipv4Addr::new(169, 254, 1, 2));
        let replica = IpAddr::V4(Ipv4Addr::new(100, 100, 196, 158));
        let v6 = IpAddr::V6("fd00::5".parse::<Ipv6Addr>().unwrap());
        let v6_link = IpAddr::V6("fe80::1".parse::<Ipv6Addr>().unwrap());
        assert_eq!(pick_address(&[lo, link, replica]), Some(replica));
        assert_eq!(pick_address(&[v6, replica]), Some(replica));
        assert_eq!(pick_address(&[lo, v6_link, v6]), Some(v6));
        assert_eq!(pick_address(&[lo, link]), None);
    }

    #[test]
    fn quickwit_gets_its_address_and_seeds_and_nothing_inherited_from_qw() {
        let inherited = [
            ("PATH", "/usr/bin"),
            ("QW_LISTEN_ADDRESS", "0.0.0.0"),
            ("QW_CONFIG", "/quickwit/config/quickwit.yaml"),
            ("QW_PEER_SEEDS", "10.0.0.9:7280"),
            ("AZURE_CLIENT_ID", "abc"),
        ]
        .map(|(k, v)| (k.to_owned(), v.to_owned()));
        let ip = IpAddr::V4(Ipv4Addr::new(100, 100, 1, 2));
        let env: BTreeMap<String, String> = quickwit_env(
            inherited,
            ip,
            &["100.100.1.3:7280".into(), "100.100.1.4:7280".into()],
            Some(2),
        )
        .into_iter()
        .collect();
        assert_eq!(env["PATH"], "/usr/bin");
        assert_eq!(env["QW_ADVERTISE_ADDRESS"], "100.100.1.2");
        assert_eq!(env["QW_PEER_SEEDS"], "100.100.1.3:7280,100.100.1.4:7280");
        assert_eq!(env["QW_NUM_CPUS"], "2");
        assert!(!env.contains_key("QW_LISTEN_ADDRESS") && !env.contains_key("QW_CONFIG"));
        assert!(!env.contains_key("AZURE_CLIENT_ID"));
        // The first node to start has no seeds.
        let env = quickwit_env([], ip, &[], None);
        assert!(!env
            .iter()
            .any(|(k, _)| k == "QW_PEER_SEEDS" || k == "QW_NUM_CPUS"));
    }

    #[tokio::test]
    async fn the_registry_holds_one_entry_per_node() {
        let dir = tempfile::tempdir().unwrap();
        let store = usnm_store::open(dir.path().to_str().unwrap()).unwrap();
        let reg = |id: &str, addr: &str| Registration {
            node_id: id.into(),
            addr: addr.into(),
            registered_at: Utc::now(),
            replica: None,
        };
        register(store.as_ref(), &reg("qw-0", "100.100.0.1:7280"))
            .await
            .unwrap();
        register(store.as_ref(), &reg("qw-1", "100.100.0.2:7280"))
            .await
            .unwrap();
        // A restart replaces the node's entry.
        register(store.as_ref(), &reg("qw-1", "100.100.0.9:7280"))
            .await
            .unwrap();
        store
            .put("seeds/junk.json", b"not json".to_vec(), "application/json")
            .await
            .unwrap();
        let all = registered(store.as_ref()).await.unwrap();
        assert_eq!(all.len(), 2);
        assert_eq!(seeds(&all, "qw-0"), ["100.100.0.9:7280"]);
        assert_eq!(seeds(&all, "qw-1"), ["100.100.0.1:7280"]);
        assert_eq!(seeds(&all, "qw-2").len(), 2);
    }
}
