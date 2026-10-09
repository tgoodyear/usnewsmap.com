//! The Quickwit searcher's thread pools and Tokio runtimes, read from its
//! Prometheus endpoint (`GET /metrics`) with its caches
//! ([`crate::cache_metrics`]), so the API can tell a cold search waiting on
//! the search pool's cores from one waiting on downloads (#251, 08 §8.1.2).
//!
//! Quickwit 0.9.1 names a metric `quickwit_{subsystem}_{name}`
//! (`quickwit-metrics/src/lib.rs`):
//!
//! | metric                                                           | type    | what                        |
//! |------------------------------------------------------------------|---------|-----------------------------|
//! | `quickwit_thread_pool_ongoing_tasks`                             | gauge   | tasks a pool is running now |
//! | `quickwit_thread_pool_pending_tasks`                             | gauge   | tasks queued for a pool now |
//! | `quickwit_runtime_tokio_worker_busy_duration_milliseconds_total` | counter | busy ms since start         |
//! | `quickwit_runtime_tokio_worker_threads`                          | gauge   | a runtime's worker threads  |
//!
//! The pools (`quickwit-common/src/thread_pool/`, label `pool`) are `search`
//! (`quickwit-search/src/lib.rs`: each split's query and aggregation, one
//! split per thread, `RAYON_NUM_THREADS` threads or one per CPU) and
//! `small_tasks` (`thread_pool/simple.rs`: short CPU work, a third of the
//! CPUs and at least 2 threads). Quickwit doesn't report a pool's size. The
//! runtimes (`quickwit-common/src/runtimes.rs`, label `runtime_type`) are
//! `main` (`quickwit-cli/src/main.rs`: REST, gRPC, and the searcher's
//! downloads and split opening, `QW_TOKIO_RUNTIME_NUM_THREADS` threads, else
//! a third of the CPUs rounded up) and the actors' `blocking` and
//! `non_blocking`, which show up only once something starts them: a
//! searcher-only node, like the sidecar, shows only `main`. Quickwit adds to
//! the busy counter once a second. Each metric also has a series without the
//! label (always 0), which is ignored, and a pool shows up only once Quickwit
//! has used it.

use std::collections::BTreeMap;

use serde::Serialize;

use crate::cache_metrics::{label, parse_value, samples};

/// A searcher thread pool the API reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Pool {
    /// Split searches: `quickwit-search/src/lib.rs`.
    Search,
    /// Short CPU work: `quickwit-common/src/thread_pool/simple.rs`.
    SmallTasks,
}

impl Pool {
    pub const ALL: [Pool; 2] = [Pool::Search, Pool::SmallTasks];

    /// Quickwit's `pool`, which the API also reports it under (the `pool`
    /// attribute).
    pub fn label(self) -> &'static str {
        match self {
            Pool::Search => "search",
            Pool::SmallTasks => "small_tasks",
        }
    }

    fn from_label(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|p| p.label() == name)
    }
}

/// A pool's tasks at the scrape.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct PoolTasks {
    /// Running on the pool's threads.
    pub ongoing: u64,
    /// Queued for a thread.
    pub pending: u64,
}

/// A searcher Tokio runtime the API reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Runtime {
    /// REST, gRPC, downloads and split opening: `quickwit-cli/src/main.rs`.
    Main,
    /// The actors' CPU-heavy runtime: `quickwit-common/src/runtimes.rs`.
    Blocking,
    /// The actors' other runtime: `quickwit-common/src/runtimes.rs`.
    NonBlocking,
}

impl Runtime {
    pub const ALL: [Runtime; 3] = [Runtime::Main, Runtime::Blocking, Runtime::NonBlocking];

    /// Quickwit's `runtime_type`, which the API also reports it under (the
    /// `runtime` attribute).
    pub fn label(self) -> &'static str {
        match self {
            Runtime::Main => "main",
            Runtime::Blocking => "blocking",
            Runtime::NonBlocking => "non_blocking",
        }
    }

    fn from_label(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|r| r.label() == name)
    }
}

/// One runtime's numbers. The busy time runs from the searcher's start.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RuntimeStats {
    /// Milliseconds its worker threads were busy, summed over the threads.
    pub busy_ms: u64,
    pub threads: u64,
}

/// The pools and runtimes a scrape found.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ThreadReport {
    pub pools: BTreeMap<Pool, PoolTasks>,
    pub runtimes: BTreeMap<Runtime, RuntimeStats>,
}

/// Pick the searcher's pools and runtimes out of Prometheus text. Lines it
/// doesn't know, or can't read, are skipped.
pub fn parse(text: &str) -> ThreadReport {
    let mut report = ThreadReport::default();
    for (name, labels, value) in samples(text) {
        match name {
            "quickwit_thread_pool_ongoing_tasks" | "quickwit_thread_pool_pending_tasks" => {
                let Some(pool) = label(labels, "pool").and_then(|p| Pool::from_label(&p)) else {
                    continue;
                };
                let Some(value) = parse_value(value) else {
                    continue;
                };
                let tasks = report.pools.entry(pool).or_default();
                if name == "quickwit_thread_pool_ongoing_tasks" {
                    tasks.ongoing = value;
                } else {
                    tasks.pending = value;
                }
            }
            "quickwit_runtime_tokio_worker_busy_duration_milliseconds_total"
            | "quickwit_runtime_tokio_worker_threads" => {
                let Some(runtime) =
                    label(labels, "runtime_type").and_then(|r| Runtime::from_label(&r))
                else {
                    continue;
                };
                let Some(value) = parse_value(value) else {
                    continue;
                };
                let stats = report.runtimes.entry(runtime).or_default();
                if name == "quickwit_runtime_tokio_worker_threads" {
                    stats.threads = value;
                } else {
                    stats.busy_ms = value;
                }
            }
            _ => {}
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The same `/metrics` as `cache_metrics`' test: Quickwit 0.9.1, a
    /// searcher-only node, idle after two searches.
    const QUICKWIT_091: &str = include_str!("../tests/data/quickwit-0.9.1-metrics.txt");

    #[test]
    fn reads_quickwit_091() {
        let r = parse(QUICKWIT_091);
        assert_eq!(
            r.pools,
            BTreeMap::from([
                (Pool::Search, PoolTasks::default()),
                (Pool::SmallTasks, PoolTasks::default()),
            ])
        );
        // Only the main runtime: a searcher doesn't start the actors' ones.
        assert_eq!(
            r.runtimes,
            BTreeMap::from([(
                Runtime::Main,
                RuntimeStats {
                    busy_ms: 13,
                    threads: 3,
                }
            )])
        );
    }

    #[test]
    fn skips_what_it_does_not_know() {
        let text = "\
# TYPE quickwit_thread_pool_ongoing_tasks gauge
quickwit_thread_pool_ongoing_tasks 0
quickwit_thread_pool_ongoing_tasks{pool=\"search\"} 3
quickwit_thread_pool_pending_tasks{pool=\"search\"} 41
quickwit_thread_pool_pending_tasks{pool=\"priority_order_test\"} 9
quickwit_thread_pool_ongoing_tasks{pool=\"small_tasks\"} -1
quickwit_thread_pool_pending_tasks{status=\"pending\"} 5
quickwit_runtime_tokio_worker_busy_duration_milliseconds_total 0
quickwit_runtime_tokio_worker_busy_duration_milliseconds_total{runtime_type=\"main\"} 581234
quickwit_runtime_tokio_worker_threads{runtime_type=\"main\"} 1
quickwit_runtime_tokio_worker_threads{runtime_type=\"blocking\"} 2.0 1700000000000
quickwit_runtime_tokio_worker_threads{runtime_type=\"elsewhere\"} 8
quickwit_runtime_tokio_worker_busy_ratio{runtime_type=\"main\"} 0.97
quickwit_runtime_tokio_scheduled_tasks{runtime_type=\"main\"} 12
quickwit_search_leaf_search_single_split_tasks{status=\"pending\"} 7
quickwit_runtime_tokio_worker_threads{runtime_type=\"non_blocking\"
garbage
";
        let r = parse(text);
        assert_eq!(
            r.pools,
            BTreeMap::from([(
                Pool::Search,
                PoolTasks {
                    ongoing: 3,
                    pending: 41,
                }
            )])
        );
        assert_eq!(
            r.runtimes,
            BTreeMap::from([
                (
                    Runtime::Main,
                    RuntimeStats {
                        busy_ms: 581_234,
                        threads: 1,
                    }
                ),
                (
                    Runtime::Blocking,
                    RuntimeStats {
                        busy_ms: 0,
                        threads: 2,
                    }
                ),
            ])
        );
        assert_eq!(parse(""), ThreadReport::default());
    }
}
