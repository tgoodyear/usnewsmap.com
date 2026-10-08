//! The cluster as a node sees it, and what each node's metrics say.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::Context;
use serde::Serialize;
use serde_json::Value;

/// One member of the cluster, from `GET /api/v1/cluster` on any node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Member {
    pub node_id: String,
    /// Changes when the node restarts.
    pub generation: u64,
    /// `ip:port` it gossips on.
    pub gossip: String,
    /// Its REST API, `http://{ip}:{grpc port - 1}`: every node's config sets
    /// gRPC to the REST port plus one (infra/quickwit/cluster-node.yaml).
    pub rest_url: Option<String>,
    pub services: Vec<String>,
    pub ready: bool,
}

impl Member {
    pub fn runs(&self, service: &str) -> bool {
        // Quickwit lists `control-plane` for the `control_plane` service.
        let service = service.replace('_', "-");
        self.services.iter().any(|s| s.replace('_', "-") == service)
    }
}

/// The members a node knows of, live or ready (dead ones left out), by id.
pub fn parse_members(v: &Value) -> anyhow::Result<Vec<Member>> {
    let ids = |key: &str| -> BTreeMap<String, u64> {
        v[key]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|n| {
                        Some((
                            n["node_id"].as_str()?.to_owned(),
                            n["generation_id"].as_u64().unwrap_or(0),
                        ))
                    })
                    .collect()
            })
            .unwrap_or_default()
    };
    let ready = ids("ready_nodes");
    let live = ids("live_nodes");
    let states = v["chitchat_state_snapshot"]["node_states"]
        .as_array()
        .context("the cluster state has no node_states")?;
    let mut out = Vec::new();
    for s in states {
        let id = &s["chitchat_id"];
        let Some(node_id) = id["node_id"].as_str() else {
            continue;
        };
        let generation = id["generation_id"].as_u64().unwrap_or(0);
        let is_ready = ready.get(node_id) == Some(&generation);
        if !is_ready && live.get(node_id) != Some(&generation) {
            continue;
        }
        let kv = &s["key_values"];
        let value = |k: &str| kv[k]["value"].as_str().map(str::to_owned);
        let rest_url = value("grpc_advertise_addr").and_then(|g| {
            let addr: std::net::SocketAddr = g.parse().ok()?;
            Some(format!(
                "http://{}",
                std::net::SocketAddr::new(addr.ip(), addr.port().checked_sub(1)?)
            ))
        });
        out.push(Member {
            node_id: node_id.to_owned(),
            generation,
            gossip: id["gossip_advertise_addr"]
                .as_str()
                .unwrap_or_default()
                .to_owned(),
            rest_url,
            services: value("enabled_services")
                .map(|s| s.split(',').map(|x| x.trim().to_owned()).collect())
                .unwrap_or_default(),
            ready: is_ready,
        });
    }
    out.sort_by(|a, b| a.node_id.cmp(&b.node_id));
    Ok(out)
}

/// The members `root` knows of.
pub async fn members(http: &reqwest::Client, root: &str) -> anyhow::Result<Vec<Member>> {
    let v: Value = http
        .get(format!("{}/api/v1/cluster", root.trim_end_matches('/')))
        .timeout(Duration::from_secs(10))
        .send()
        .await?
        .error_for_status()
        .context("reading the cluster's members")?
        .json()
        .await?;
    parse_members(&v)
}

/// Wait until `root` sees `searchers` ready searcher nodes (and every node
/// it knows is ready), or `wait` passes. Returns the members either way.
pub async fn wait_ready(
    http: &reqwest::Client,
    root: &str,
    searchers: usize,
    wait: Duration,
) -> anyhow::Result<(Vec<Member>, bool)> {
    let deadline = tokio::time::Instant::now() + wait;
    loop {
        let found = members(http, root).await;
        if let Ok(m) = &found {
            let ready = m.iter().filter(|m| m.ready && m.runs("searcher")).count();
            if ready >= searchers && m.iter().all(|m| m.ready) {
                return Ok((m.clone(), true));
            }
        }
        if tokio::time::Instant::now() >= deadline {
            return found.map(|m| (m, false));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}

/// The sum of a Prometheus metric's samples whose labels include all of
/// `labels` (samples without labels count only when `labels` is empty).
pub fn metric_sum(text: &str, name: &str, labels: &[(&str, &str)]) -> f64 {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|line| {
            let rest = line.strip_prefix(name)?;
            let (label_text, value) = match rest.strip_prefix('{') {
                Some(r) => {
                    let (l, v) = r.split_once('}')?;
                    (l, v)
                }
                None if rest.starts_with(' ') => ("", rest),
                None => return None,
            };
            let have = parse_labels(label_text);
            let matches = labels
                .iter()
                .all(|(k, v)| have.get(*k).map(String::as_str) == Some(*v));
            if !matches {
                return None;
            }
            value.split_whitespace().next()?.parse::<f64>().ok()
        })
        .filter(|v| v.is_finite())
        .sum()
}

fn parse_labels(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    let mut rest = text;
    while let Some((key, after)) = rest.split_once("=\"") {
        let mut value = String::new();
        let mut chars = after.char_indices();
        let mut end = after.len();
        while let Some((i, c)) = chars.next() {
            match c {
                '\\' => {
                    if let Some((_, n)) = chars.next() {
                        value.push(n);
                    }
                }
                '"' => {
                    end = i + 1;
                    break;
                }
                c => value.push(c),
            }
        }
        out.insert(key.trim_start_matches(',').trim().to_owned(), value);
        rest = &after[end.min(after.len())..];
    }
    out
}

/// Counters read from every node: leaf searches and the splits they read,
/// root searches, documents indexed, merges.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct NodeCounters {
    pub leaf_requests: f64,
    pub leaf_splits: f64,
    pub leaf_split_secs: f64,
    pub root_requests: f64,
    pub docs_indexed: f64,
    pub merges_running: f64,
    pub merges_queued: f64,
    pub footer_cache_bytes: f64,
    pub footer_cache_hits: f64,
    pub footer_cache_misses: f64,
}

impl NodeCounters {
    pub fn parse(text: &str, index_id: Option<&str>) -> Self {
        let ok = [("status", "success")];
        let mut docs = vec![("docs_processed_status", "valid")];
        if let Some(id) = index_id {
            docs.push(("index", id));
        }
        let footer = [("component_name", "splitfooter")];
        NodeCounters {
            leaf_requests: metric_sum(text, "quickwit_search_leaf_search_requests_total", &ok),
            leaf_splits: metric_sum(text, "quickwit_search_leaf_search_targeted_splits_sum", &ok),
            leaf_split_secs: metric_sum(
                text,
                "quickwit_search_leaf_search_split_duration_secs_sum",
                &[],
            ),
            root_requests: metric_sum(text, "quickwit_search_root_search_requests_total", &ok),
            docs_indexed: metric_sum(text, "quickwit_indexing_processed_docs_total", &docs),
            // Gauges Quickwit can report as -0.
            merges_running: metric_sum(text, "quickwit_indexing_ongoing_merge_operations", &[])
                .max(0.0),
            merges_queued: metric_sum(text, "quickwit_indexing_pending_merge_operations", &[])
                .max(0.0),
            footer_cache_bytes: metric_sum(text, "quickwit_cache_in_cache_num_bytes", &footer),
            footer_cache_hits: metric_sum(text, "quickwit_cache_cache_hits_total", &footer),
            footer_cache_misses: metric_sum(text, "quickwit_cache_cache_misses_total", &footer),
        }
    }

    /// What changed from `before` (gauges keep their latest value).
    pub fn since(&self, before: &NodeCounters) -> NodeCounters {
        NodeCounters {
            leaf_requests: self.leaf_requests - before.leaf_requests,
            leaf_splits: self.leaf_splits - before.leaf_splits,
            leaf_split_secs: self.leaf_split_secs - before.leaf_split_secs,
            root_requests: self.root_requests - before.root_requests,
            docs_indexed: self.docs_indexed - before.docs_indexed,
            merges_running: self.merges_running,
            merges_queued: self.merges_queued,
            footer_cache_bytes: self.footer_cache_bytes,
            footer_cache_hits: self.footer_cache_hits - before.footer_cache_hits,
            footer_cache_misses: self.footer_cache_misses - before.footer_cache_misses,
        }
    }
}

/// Each member's counters, by node id. A node that doesn't answer is left out.
pub async fn counters(
    http: &reqwest::Client,
    members: &[Member],
    index_id: Option<&str>,
) -> BTreeMap<String, NodeCounters> {
    let mut out = BTreeMap::new();
    for m in members {
        let Some(url) = &m.rest_url else { continue };
        let text = async {
            http.get(format!("{url}/metrics"))
                .timeout(Duration::from_secs(5))
                .send()
                .await?
                .error_for_status()?
                .text()
                .await
        }
        .await;
        match text {
            Ok(t) => {
                out.insert(m.node_id.clone(), NodeCounters::parse(&t, index_id));
            }
            Err(e) => {
                tracing::warn!(node = %m.node_id, error = %e, "can't read the node's metrics")
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_members_a_node_knows() {
        let v = serde_json::json!({
            "cluster_id": "usnm-qwcluster",
            "self_node_id": "qw-0",
            "ready_nodes": [
                {"node_id": "qw-0", "generation_id": 7, "gossip_advertise_addr": "100.100.0.1:7280"},
                {"node_id": "qw-1", "generation_id": 9, "gossip_advertise_addr": "100.100.0.2:7280"}
            ],
            "live_nodes": [
                {"node_id": "qw-2", "generation_id": 3, "gossip_advertise_addr": "100.100.0.3:7280"}
            ],
            "dead_nodes": [
                {"node_id": "qw-2", "generation_id": 1, "gossip_advertise_addr": "100.100.0.9:7280"}
            ],
            "chitchat_state_snapshot": {"node_states": [
                {"chitchat_id": {"node_id": "qw-0", "generation_id": 7, "gossip_advertise_addr": "100.100.0.1:7280"},
                 "key_values": {
                    "enabled_services": {"value": "searcher,metastore,control-plane,janitor,indexer"},
                    "grpc_advertise_addr": {"value": "100.100.0.1:7281"}}},
                {"chitchat_id": {"node_id": "qw-1", "generation_id": 9, "gossip_advertise_addr": "100.100.0.2:7280"},
                 "key_values": {
                    "enabled_services": {"value": "searcher"},
                    "grpc_advertise_addr": {"value": "100.100.0.2:7281"}}},
                {"chitchat_id": {"node_id": "qw-2", "generation_id": 3, "gossip_advertise_addr": "100.100.0.3:7280"},
                 "key_values": {"enabled_services": {"value": "searcher,indexer"}}},
                {"chitchat_id": {"node_id": "qw-2", "generation_id": 1, "gossip_advertise_addr": "100.100.0.9:7280"},
                 "key_values": {"enabled_services": {"value": "searcher"}}}
            ]}
        });
        let m = parse_members(&v).unwrap();
        assert_eq!(m.len(), 3, "{m:?}");
        assert_eq!(m[0].node_id, "qw-0");
        assert!(m[0].ready && m[0].runs("control_plane") && m[0].runs("indexer"));
        assert_eq!(m[0].rest_url.as_deref(), Some("http://100.100.0.1:7280"));
        assert_eq!(m[1].services, ["searcher"]);
        // Live but not ready, the dead generation dropped.
        assert_eq!((m[2].generation, m[2].ready), (3, false));
        assert_eq!(m[2].rest_url, None);
    }

    #[test]
    fn sums_metrics_by_label() {
        let text = "\
# HELP quickwit_search_leaf_search_targeted_splits_sum x
quickwit_search_leaf_search_targeted_splits_sum 0
quickwit_search_leaf_search_targeted_splits_sum{status=\"success\"} 40
quickwit_search_leaf_search_targeted_splits_sum{status=\"error\"} 2
quickwit_search_leaf_search_targeted_splits_count{status=\"success\"} 4
quickwit_indexing_processed_docs_total{index=\"a\",docs_processed_status=\"valid\"} 100
quickwit_indexing_processed_docs_total{index=\"b\",docs_processed_status=\"valid\"} 7
quickwit_indexing_processed_docs_total{index=\"a\",docs_processed_status=\"doc_mapper_error\"} 1
quickwit_indexing_ongoing_merge_operations 2
quickwit_cache_in_cache_num_bytes{component_name=\"splitfooter\"} 1.5e6
";
        let ok = [("status", "success")];
        assert_eq!(
            metric_sum(text, "quickwit_search_leaf_search_targeted_splits_sum", &ok),
            40.0
        );
        assert_eq!(
            metric_sum(text, "quickwit_search_leaf_search_targeted_splits_sum", &[]),
            42.0
        );
        let c = NodeCounters::parse(text, Some("a"));
        assert_eq!(c.docs_indexed, 100.0);
        assert_eq!(NodeCounters::parse(text, None).docs_indexed, 107.0);
        assert_eq!(c.merges_running, 2.0);
        assert_eq!(c.footer_cache_bytes, 1.5e6);
        let later = NodeCounters {
            leaf_splits: 50.0,
            ..c.clone()
        };
        assert_eq!(later.since(&c).leaf_splits, 10.0);
        // A metric whose name only starts the same isn't counted.
        assert_eq!(metric_sum(text, "quickwit_search_leaf_search", &[]), 0.0);
    }

    #[test]
    fn parses_escaped_label_values() {
        let l = parse_labels(r#"a="x\"y",b="z""#);
        assert_eq!(l["a"], "x\"y");
        assert_eq!(l["b"], "z");
    }
}
