//! The experimental search cluster (#238, #239; docs/operations.md, "Search
//! cluster experiment"): Quickwit nodes as Container Apps, each one replica,
//! and the tooling that measures them on a sample of the corpus.
//!
//! - [`node`]: a node's start. It advertises the replica's IP, registers it
//!   in the seed registry (a blob per node), takes the other nodes' entries
//!   as peer seeds, and becomes Quickwit.
//! - [`members`]: the cluster as a node sees it (`/api/v1/cluster`), and the
//!   counters each node's Prometheus endpoint reports.
//! - [`sample`]: a fixed share of the published version's pages, as the
//!   documents a full release would build for them (NDJSON parts).
//! - [`load`]: those documents into a new index on the cluster, timed step
//!   by step: sending, committed, merges settled, sealed.
//! - [`bench`]: the benchmark searches against the cluster's root, the way
//!   the API runs them (`usnm_search::plan::aggregate`), cold, warm and at
//!   rising concurrency, with each node's share of the leaf work.
//!
//! The `usnm-qwcluster` binary runs each of them.

pub mod bench;
pub mod load;
pub mod members;
pub mod node;
pub mod sample;

/// One JSON report line in the job's console (`qwcluster report`), which
/// `scripts/qwcluster-report.py` reads back from Log Analytics.
pub fn log_report(kind: &str, report: &serde_json::Value) {
    tracing::info!(kind, report = %report, "qwcluster report");
}
