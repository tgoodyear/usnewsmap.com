//! Merging a new index before it is published (05 §5.5.1, 08 §8.4.1).
//!
//! Every cold search opens each split of the indexes it names, so a sealed
//! index should be a few large splits, not the hundreds the writer cuts
//! while it ingests. The writer node merges them in the background; the
//! release waits for it in two steps before it publishes:
//!
//! 1. **Settle:** with the index still open, until the merge planner has
//!    nothing running or queued and the split list has stopped changing.
//! 2. **Finalize:** the index's ingest source is disabled, which shuts its
//!    merge pipeline down; on the way out, the merge policy merges the small
//!    splits that are left (`max_finalize_merge_operations`). The release
//!    waits for the pipeline to finish and the splits to settle again.
//!
//! The wait is bounded, and the result is checked: the splits must hold
//! exactly the documents sent, and no more of them than the merge policy
//! leaves. The writer's output is watched for a full disk and for failed
//! merges; either stops the release, so nothing is published half merged.

use std::collections::BTreeSet;
use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, Context};
use serde::Serialize;
use serde_json::Value;

use crate::progress;
use crate::sink::INDEX_TEMPLATE;

/// The ingest API's source in every index (Quickwit's ingest v2).
pub const INGEST_SOURCE: &str = "_ingest-source";

/// Splits listed per request.
const PAGE: usize = 1000;

/// A setting from the index template's `indexing_settings`, e.g.
/// `split_num_docs_target`: the merge checks read the policy the indexes
/// are created with, so the two can't drift apart.
pub fn template_setting(key: &str) -> Option<u64> {
    INDEX_TEMPLATE.lines().find_map(|line| {
        let (k, v) = line.trim().split_once(':')?;
        (k == key).then(|| v.trim().parse().ok()).flatten()
    })
}

/// The most splits a fully merged index of `docs` documents can have.
/// Splits reach `split_num_docs_target` documents and stop merging, so there
/// are at most `docs / target` of those. Below that size, every split left
/// after the planner settles is in a final merge (at most
/// `max_finalize_merge_operations`) except fewer than three, the smallest
/// final merge.
pub fn max_splits(docs: u64) -> u64 {
    let target = template_setting("split_num_docs_target").unwrap_or(10_000_000);
    let finals = template_setting("max_finalize_merge_operations").unwrap_or(0);
    docs / target.max(1) + finals + 2
}

/// What the writer node's output has said about the release so far: read
/// line by line as the node writes it (`sink::forward`).
#[derive(Debug, Default)]
pub struct NodeEvents(Mutex<Events>);

#[derive(Debug, Default)]
struct Events {
    /// Why the writer process ended, once it has.
    exited: Option<String>,
    disk_full: Option<String>,
    merge_failed: Option<String>,
    /// Indexes whose `_ingest-source` merge pipeline has finished for good.
    finished: BTreeSet<String>,
}

/// The first 300 characters of a line, for an error message.
fn excerpt(line: &str) -> String {
    line.chars().take(300).collect()
}

impl NodeEvents {
    pub fn observe(&self, line: &str) {
        let disk_full = line.contains("No space left on device") || line.contains("os error 28");
        let merge_failed = line.contains("failed to merge splits")
            || line.contains("merge scheduler service is dead");
        let finished = line.contains("merge pipeline completed successfully")
            && line.contains(&format!("source_id={INGEST_SOURCE}"));
        if !(disk_full || merge_failed || finished) {
            return;
        }
        let mut e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if disk_full && e.disk_full.is_none() {
            e.disk_full = Some(excerpt(line));
        }
        if merge_failed && e.merge_failed.is_none() {
            e.merge_failed = Some(excerpt(line));
        }
        if finished {
            // `index_uid=pages-delta-20261008-1:01M3…`
            if let Some(id) = line
                .split_whitespace()
                .find_map(|w| w.strip_prefix("index_uid="))
                .and_then(|uid| uid.split(':').next())
            {
                e.finished.insert(id.to_owned());
            }
        }
    }

    /// Record that the writer process has ended, and why (`sink::describe_exit`).
    pub fn writer_exited(&self, why: &str) {
        let mut e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.exited.get_or_insert_with(|| why.to_owned());
    }

    /// Why the writer process ended, if it has.
    pub fn exit(&self) -> Option<String> {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.exited.clone()
    }

    /// Fails once the writer has exited, run out of disk, or failed a merge.
    pub fn check(&self) -> anyhow::Result<()> {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(why) = &e.exited {
            bail!("{why}; not publishing");
        }
        if let Some(line) = &e.disk_full {
            bail!("the Quickwit writer ran out of disk; not publishing. Its output: {line}");
        }
        if let Some(line) = &e.merge_failed {
            bail!(
                "a Quickwit merge failed; not publishing a half-merged index. Its output: {line}"
            );
        }
        Ok(())
    }

    /// `err`, from a request to the writer, with the reason the writer
    /// exited in front if it has (or does within `grace`): a writer the
    /// kernel killed shows up first as "connection refused", and the exit
    /// status takes a moment to be reaped.
    pub async fn explain(&self, err: anyhow::Error, grace: Duration) -> anyhow::Error {
        let until = tokio::time::Instant::now() + grace;
        loop {
            if let Some(why) = self.exit() {
                return err.context(why);
            }
            if tokio::time::Instant::now() >= until {
                return err;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Whether `index_id`'s merge pipeline has run its final merges and exited.
    pub fn finished(&self, index_id: &str) -> bool {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.finished.contains(index_id)
    }
}

/// `(running, queued)` merge operations, from the node's Prometheus metrics.
/// Quickwit registers the gauges with the first merge, so a missing one is 0.
pub fn merge_gauges(metrics: &str) -> (u64, u64) {
    let gauge = |name: &str| {
        metrics
            .lines()
            .find_map(|line| {
                let value = line.strip_prefix(name)?.strip_prefix(' ')?;
                let v: f64 = value.trim().parse().ok()?;
                (v >= 0.0).then_some(v as u64)
            })
            .unwrap_or(0)
    };
    (
        gauge("quickwit_indexing_ongoing_merge_operations"),
        gauge("quickwit_indexing_pending_merge_operations"),
    )
}

/// How long a failed request to the writer waits for its exit to be reaped.
pub const EXIT_GRACE: Duration = Duration::from_secs(5);

/// `r`, with the writer's exit in front of an error if it has exited.
pub async fn explained<T>(events: Option<&NodeEvents>, r: anyhow::Result<T>) -> anyhow::Result<T> {
    match (r, events) {
        (Err(e), Some(ev)) => Err(ev.explain(e, EXIT_GRACE).await),
        (r, _) => r,
    }
}

/// Decides when merging has settled: no merge running or queued, and the
/// same splits, for `needed` polls in a row.
#[derive(Debug)]
pub struct Settle {
    needed: u32,
    stable: u32,
    last: Option<Vec<String>>,
}

impl Settle {
    pub fn new(needed: u32) -> Self {
        Self {
            needed: needed.max(1),
            stable: 0,
            last: None,
        }
    }

    /// Record one poll; `true` once settled. `split_ids` must be sorted.
    pub fn observe(&mut self, idle: bool, split_ids: &[String]) -> bool {
        let same = self.last.as_deref() == Some(split_ids);
        self.stable = if idle && same { self.stable + 1 } else { 0 };
        if !same {
            self.last = Some(split_ids.to_vec());
        }
        self.stable >= self.needed
    }
}

/// One published split.
#[derive(Debug, Clone, PartialEq)]
pub struct Split {
    pub id: String,
    pub docs: u64,
    pub bytes: u64,
}

/// An index's published splits, as the release log and manifest report them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexLayout {
    pub index_id: String,
    pub splits: u64,
    pub docs: u64,
    /// The split files' total size.
    pub bytes: u64,
    pub smallest_split_docs: u64,
    pub largest_split_docs: u64,
}

impl IndexLayout {
    pub fn of(index_id: &str, splits: &[Split]) -> Self {
        Self {
            index_id: index_id.to_owned(),
            splits: splits.len() as u64,
            docs: splits.iter().map(|s| s.docs).sum(),
            bytes: splits.iter().map(|s| s.bytes).sum(),
            smallest_split_docs: splits.iter().map(|s| s.docs).min().unwrap_or(0),
            largest_split_docs: splits.iter().map(|s| s.docs).max().unwrap_or(0),
        }
    }

    /// One "index layout" line.
    pub fn log(&self) {
        tracing::info!(
            index = %self.index_id,
            splits = self.splits,
            docs = self.docs,
            mb = (self.bytes as f64 / (1024.0 * 1024.0)).round() as u64,
            smallest_split_docs = self.smallest_split_docs,
            largest_split_docs = self.largest_split_docs,
            "index layout"
        );
    }
}

/// The splits in one page of `GET /api/v1/indexes/{id}/splits`.
fn parse_splits(v: &Value) -> anyhow::Result<Vec<Split>> {
    v["splits"]
        .as_array()
        .context("the split list has no `splits`")?
        .iter()
        .map(|s| {
            Ok(Split {
                id: s["split_id"]
                    .as_str()
                    .context("a split has no id")?
                    .to_owned(),
                docs: s["num_docs"].as_u64().context("a split has no num_docs")?,
                // The footer ends the split file.
                bytes: s["footer_offsets"]["end"].as_u64().unwrap_or(0),
            })
        })
        .collect()
}

/// The writer node's REST API, for merges and splits.
pub struct Node<'a> {
    pub http: &'a reqwest::Client,
    pub base: &'a str,
}

impl Node<'_> {
    /// Every published split of `index_id`, sorted by id.
    pub async fn splits(&self, index_id: &str) -> anyhow::Result<Vec<Split>> {
        let mut all = Vec::new();
        loop {
            let v: Value = self
                .http
                .get(format!("{}/api/v1/indexes/{index_id}/splits", self.base))
                .query(&[
                    ("split_states", "Published"),
                    ("offset", &all.len().to_string()),
                    ("limit", &PAGE.to_string()),
                ])
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("listing the splits of `{index_id}`"))?
                .json()
                .await?;
            let page = parse_splits(&v)?;
            let n = page.len();
            all.extend(page);
            if n < PAGE {
                break;
            }
        }
        all.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(all)
    }

    async fn gauges(&self) -> anyhow::Result<(u64, u64)> {
        let text = self
            .http
            .get(format!("{}/metrics", self.base))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        Ok(merge_gauges(&text))
    }

    /// Stop ingesting into `index_id`, which runs its final merges.
    async fn close(&self, index_id: &str) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(format!(
                "{}/api/v1/indexes/{index_id}/sources/{INGEST_SOURCE}/toggle",
                self.base
            ))
            .json(&serde_json::json!({"enable": false}))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "disabling ingest into `{index_id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        Ok(())
    }
}

/// How the release waits for merges.
#[derive(Debug, Clone)]
pub struct MergeWait {
    /// Between polls of the node's metrics and split list.
    pub poll: Duration,
    /// Polls in a row with nothing changing before merging counts as settled.
    pub stable_polls: u32,
    /// For both steps together; past it the release fails.
    pub timeout: Duration,
    /// Without the node's output to say the final merges are done, how long
    /// to wait after disabling the source before trusting a quiet node.
    pub finalize_grace: Duration,
}

impl Default for MergeWait {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(10),
            stable_polls: 3,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            finalize_grace: Duration::from_secs(60),
        }
    }
}

/// 4 hours: the merges left when a full rebuild of the corpus stops
/// ingesting (about 1.7 hours for 23.8M pages), with room to spare (08 §8.4).
pub const DEFAULT_TIMEOUT_SECS: u64 = 14400;

/// Lines of "release merges" progress, at most this often.
const LOG_EVERY: Duration = Duration::from_secs(30);

/// Wait until `index_id` is merged as far as its merge policy goes, close
/// it, and check the result (see the module docs). `expected` is the number
/// of documents sent; `work_dir` is where the writer keeps its data. Nothing
/// counts as sealed after `wait.timeout`, and a request to the writer that
/// hangs past it (plus one poll) is abandoned.
pub async fn seal(
    node: &Node<'_>,
    index_id: &str,
    expected: u64,
    events: Option<&NodeEvents>,
    wait: &MergeWait,
    work_dir: Option<&Path>,
) -> anyhow::Result<IndexLayout> {
    let polls = poll_until_sealed(node, index_id, expected, events, wait, work_dir);
    match tokio::time::timeout(wait.timeout + wait.poll, polls).await {
        Ok(result) => result,
        Err(_) => bail!(
            "merges into `{index_id}` did not finish within {} s (a request to the writer was \
             still waiting); not publishing",
            wait.timeout.as_secs()
        ),
    }
}

async fn poll_until_sealed(
    node: &Node<'_>,
    index_id: &str,
    expected: u64,
    events: Option<&NodeEvents>,
    wait: &MergeWait,
    work_dir: Option<&Path>,
) -> anyhow::Result<IndexLayout> {
    let start = tokio::time::Instant::now();
    let deadline = start + wait.timeout;
    let mut last_log: Option<tokio::time::Instant> = None;
    let mut step = "settle";
    let mut closed_at: Option<tokio::time::Instant> = None;
    let mut settle = Settle::new(wait.stable_polls);
    loop {
        if let Some(e) = events {
            e.check()?;
        }
        let (running, queued) = explained(events, node.gauges().await).await?;
        let splits = explained(events, node.splits(index_id).await).await?;
        let ids: Vec<String> = splits.iter().map(|s| s.id.clone()).collect();
        let settled = settle.observe(running == 0 && queued == 0, &ids);
        if last_log.is_none_or(|t| t.elapsed() >= LOG_EVERY) {
            last_log = Some(tokio::time::Instant::now());
            tracing::info!(
                index = index_id,
                step,
                splits = splits.len(),
                merges_running = running,
                merges_queued = queued,
                elapsed_secs = start.elapsed().as_secs(),
                disk_free_mb = work_dir
                    .and_then(progress::disk_free)
                    .map(|b| b / (1024 * 1024)),
                "release merges"
            );
        }
        match closed_at {
            None if settled => {
                tracing::info!(
                    index = index_id,
                    splits = splits.len(),
                    "merges settled; closing the index for its final merges"
                );
                explained(events, node.close(index_id).await).await?;
                closed_at = Some(tokio::time::Instant::now());
                step = "finalize";
                settle = Settle::new(wait.stable_polls);
            }
            Some(at) if settled => {
                // An index that was never written has no ingest-source
                // pipeline, so no "completed" line will come; there is
                // nothing for it to merge.
                let done = expected == 0
                    || match events {
                        Some(e) => e.finished(index_id),
                        None => at.elapsed() >= wait.finalize_grace,
                    };
                if done && tokio::time::Instant::now() <= deadline {
                    return check(index_id, expected, &splits);
                }
            }
            _ => {}
        }
        if tokio::time::Instant::now() + wait.poll > deadline {
            bail!(
                "merges into `{index_id}` did not finish within {} s ({step}: {} splits, {running} \
                 merges running, {queued} queued); not publishing",
                wait.timeout.as_secs(),
                splits.len()
            );
        }
        tokio::time::sleep(wait.poll).await;
    }
}

/// The merged index holds every document, in no more splits than the
/// merge policy leaves.
fn check(index_id: &str, expected: u64, splits: &[Split]) -> anyhow::Result<IndexLayout> {
    let layout = IndexLayout::of(index_id, splits);
    if layout.docs != expected {
        bail!(
            "after merging, `{index_id}` holds {} documents in {} splits, expected {expected}",
            layout.docs,
            layout.splits
        );
    }
    let ceiling = max_splits(expected);
    if layout.splits > ceiling {
        bail!(
            "merging left `{index_id}` in {} splits; its merge policy leaves at most {ceiling} \
             for {expected} documents. Not publishing a half-merged index",
            layout.splits
        );
    }
    Ok(layout)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::Arc;

    use super::*;

    #[test]
    fn the_template_sets_the_merge_policy_the_checks_assume() {
        assert_eq!(template_setting("split_num_docs_target"), Some(100_000));
        assert_eq!(template_setting("max_finalize_merge_operations"), Some(5));
        assert!(INDEX_TEMPLATE.contains("type: limit_merge"));
        assert_eq!(template_setting("no_such_setting"), None);
        // 274,480 documents: two full splits, up to five final merges, two left over.
        assert_eq!(max_splits(274_480), 2 + 5 + 2);
        assert_eq!(max_splits(71_764), 7);
        // The whole corpus: about 240 splits.
        assert_eq!(max_splits(23_800_000), 238 + 7);
    }

    #[test]
    fn reads_the_merge_gauges() {
        let metrics = "# HELP quickwit_indexing_ongoing_merge_operations Number of ongoing merge operations\n\
                       # TYPE quickwit_indexing_ongoing_merge_operations gauge\n\
                       quickwit_indexing_ongoing_merge_operations 1\n\
                       quickwit_indexing_pending_merge_operations 3\n\
                       quickwit_indexing_pending_merge_bytes 12345\n";
        assert_eq!(merge_gauges(metrics), (1, 3));
        assert_eq!(
            merge_gauges(
                "quickwit_indexing_pending_merge_operations 2\nquickwit_indexing_ongoing_merge_operations 0.0\n"
            ),
            (0, 2)
        );
        // A labelled series isn't the total; before the first merge there
        // are no gauges at all.
        assert_eq!(
            merge_gauges("quickwit_indexing_ongoing_merge_operations{x=\"y\"} 2\n"),
            (0, 0)
        );
        assert_eq!(merge_gauges(""), (0, 0));
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn settles_after_enough_quiet_polls() {
        let mut s = Settle::new(3);
        let a = ids(&["a", "b", "c"]);
        let merged = ids(&["d"]);
        assert!(!s.observe(true, &a)); // first sight: nothing to compare yet
        assert!(!s.observe(false, &a)); // a merge started
        assert!(!s.observe(true, &merged)); // it finished: the splits changed
        assert!(!s.observe(true, &merged));
        assert!(!s.observe(true, &merged));
        assert!(s.observe(true, &merged));
        // A change starts the count again.
        assert!(!s.observe(true, &a));
    }

    #[test]
    fn watches_the_writer_output() {
        let e = NodeEvents::default();
        e.observe("2026-09-29T09:09:18Z  WARN quickwit_config: peer seeds are empty");
        assert!(e.check().is_ok());
        assert!(!e.finished("pages-delta-20261008-1"));

        e.observe(
            "2026-10-02T06:59:13Z  INFO quickwit_indexing::actors::merge_pipeline: merge pipeline \
             completed successfully index_uid=pages-delta-20261008-1:01M3XP9DEKWZK8TW8TXB27ZM7Y \
             source_id=_ingest-source generation=1",
        );
        // The ingest API v1 source's pipeline is another one.
        e.observe(
            "2026-10-02T06:59:13Z  INFO quickwit_indexing::actors::merge_pipeline: merge pipeline \
             completed successfully index_uid=pages-base-20261001-1:01M3XP9DEKWZK8TW8TXB27ZM7Y \
             source_id=_ingest-api-source generation=1",
        );
        assert!(e.finished("pages-delta-20261008-1"));
        assert!(!e.finished("pages-base-20261001-1"));
        assert!(e.check().is_ok());

        e.observe(
            "2026-09-29T09:27:26Z ERROR quickwit_actors::spawn_builder: actor-exit \
             exit_status=Failure(No space left on device (os error 28) at path \"/work/quickwit/qwdata/indexing\")",
        );
        let err = e.check().unwrap_err().to_string();
        assert!(err.contains("ran out of disk"), "{err}");
        assert!(err.contains("os error 28"), "{err}");
    }

    #[test]
    fn a_failed_merge_stops_the_release() {
        let e = NodeEvents::default();
        e.observe(
            "2026-09-29T14:03:59Z ERROR merge{merge_split_id=01M3PQ9H}:merge_executor: \
             quickwit_indexing::actors::merge_executor: failed to merge splits task=Merge(…)",
        );
        let err = e.check().unwrap_err().to_string();
        assert!(err.contains("half-merged"), "{err}");
    }

    #[test]
    fn layout_and_checks() {
        let splits = vec![
            Split {
                id: "a".into(),
                docs: 1_000_000,
                bytes: 23_000_000_000,
            },
            Split {
                id: "b".into(),
                docs: 40_000,
                bytes: 920_000_000,
            },
        ];
        let l = check("idx", 1_040_000, &splits).unwrap();
        assert_eq!(
            l,
            IndexLayout {
                index_id: "idx".into(),
                splits: 2,
                docs: 1_040_000,
                bytes: 23_920_000_000,
                smallest_split_docs: 40_000,
                largest_split_docs: 1_000_000,
            }
        );
        let err = check("idx", 1_040_001, &splits).unwrap_err().to_string();
        assert!(err.contains("expected 1040001"), "{err}");

        let many: Vec<Split> = (0..20)
            .map(|i| Split {
                id: format!("s{i:02}"),
                docs: 7_000,
                bytes: 1,
            })
            .collect();
        let err = check("idx", 140_000, &many).unwrap_err().to_string();
        assert!(err.contains("at most 8"), "{err}");
        assert_eq!(IndexLayout::of("empty", &[]).splits, 0);
    }

    #[test]
    fn parses_a_split_list() {
        let v = serde_json::json!({"offset": 0, "size": 1, "splits": [{
            "split_state": "Published", "split_id": "01M3XPHH", "num_docs": 40000,
            "footer_offsets": {"start": 919982920, "end": 920075297}, "num_merge_ops": 1,
        }]});
        assert_eq!(
            parse_splits(&v).unwrap(),
            vec![Split {
                id: "01M3XPHH".into(),
                docs: 40000,
                bytes: 920_075_297
            }]
        );
        assert!(parse_splits(&serde_json::json!({"message": "index `x` not found"})).is_err());
    }

    /// A fake writer node: merges run for a few polls, then the planner
    /// settles; closing the index runs one final merge.
    #[derive(Default)]
    struct Fake {
        polls: AtomicU32,
        closed_at: Mutex<Option<u32>>,
    }

    impl Fake {
        fn now(&self) -> u32 {
            self.polls.load(Ordering::SeqCst)
        }
        fn phase(&self) -> (u64, Vec<(&'static str, u64)>) {
            let t = self.now();
            match *self.closed_at.lock().unwrap() {
                // Final merge: two polls running, then one split.
                Some(c) if t < c + 2 => (1, vec![("m1", 900), ("s9", 50), ("s10", 50)]),
                Some(_) => (0, vec![("f1", 1000)]),
                // Before closing: merging for three polls, then three splits.
                None if t < 3 => (1, vec![("s1", 400), ("s2", 500), ("s9", 50), ("s10", 50)]),
                None => (0, vec![("m1", 900), ("s9", 50), ("s10", 50)]),
            }
        }
    }

    async fn fake_node(finish_line: Option<Arc<NodeEvents>>) -> (String, Arc<Fake>) {
        use axum::routing::{get, put};
        let fake = Arc::new(Fake::default());
        let f1 = fake.clone();
        let f2 = fake.clone();
        let f3 = fake.clone();
        let app = axum::Router::new()
            .route(
                "/metrics",
                get(move || async move {
                    f1.polls.fetch_add(1, Ordering::SeqCst);
                    let (running, _) = f1.phase();
                    format!(
                        "quickwit_indexing_ongoing_merge_operations {running}\n\
                         quickwit_indexing_pending_merge_operations 0\n"
                    )
                }),
            )
            .route(
                "/api/v1/indexes/{id}/splits",
                get(move || async move {
                    let (_, splits) = f2.phase();
                    axum::Json(serde_json::json!({"splits": splits.iter().map(|(id, docs)| {
                        serde_json::json!({"split_id": id, "num_docs": docs, "footer_offsets": {"end": docs * 10}})
                    }).collect::<Vec<_>>()}))
                }),
            )
            .route(
                "/api/v1/indexes/{id}/sources/_ingest-source/toggle",
                put(move || async move {
                    *f3.closed_at.lock().unwrap() = Some(f3.now());
                    if let Some(e) = &finish_line {
                        e.observe(
                            "INFO merge pipeline completed successfully index_uid=idx:01M3 \
                             source_id=_ingest-source generation=1",
                        );
                    }
                    "null"
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), fake)
    }

    fn quick() -> MergeWait {
        MergeWait {
            poll: Duration::from_millis(5),
            stable_polls: 2,
            timeout: Duration::from_secs(10),
            finalize_grace: Duration::from_millis(50),
        }
    }

    #[tokio::test]
    async fn waits_for_merges_then_the_final_merge() {
        let events = Arc::new(NodeEvents::default());
        let (base, fake) = fake_node(Some(events.clone())).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let layout = seal(&node, "idx", 1000, Some(&events), &quick(), None)
            .await
            .unwrap();
        assert_eq!((layout.splits, layout.docs), (1, 1000));
        // Closed only after the planner had settled on three splits.
        assert!(fake.closed_at.lock().unwrap().unwrap() >= 5);
    }

    #[tokio::test]
    async fn without_the_node_output_waits_out_the_grace_period() {
        let (base, _) = fake_node(None).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let started = std::time::Instant::now();
        let layout = seal(&node, "idx", 1000, None, &quick(), None)
            .await
            .unwrap();
        assert_eq!(layout.splits, 1);
        assert!(started.elapsed() >= quick().finalize_grace);
    }

    #[tokio::test]
    async fn gives_up_at_the_timeout() {
        // The final merge never reports done.
        let events = Arc::new(NodeEvents::default());
        let (base, _) = fake_node(None).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let wait = MergeWait {
            timeout: Duration::from_millis(300),
            ..quick()
        };
        let err = seal(&node, "idx", 1000, Some(&events), &wait, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not finish"), "{err}");
        assert!(err.contains("finalize"), "{err}");
    }

    #[tokio::test]
    async fn a_writer_that_stops_answering_hits_the_same_deadline() {
        use axum::routing::get;
        let app = axum::Router::new().route(
            "/metrics",
            get(|| async {
                tokio::time::sleep(Duration::from_secs(30)).await;
                ""
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let base = format!("http://{addr}");
        let node = Node {
            http: &http,
            base: &base,
        };
        let wait = MergeWait {
            timeout: Duration::from_millis(200),
            ..quick()
        };
        let started = std::time::Instant::now();
        let err = seal(&node, "idx", 1000, None, &wait, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("did not finish within"), "{err}");
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_full_disk_stops_the_wait() {
        let events = Arc::new(NodeEvents::default());
        events.observe("ERROR No space left on device (os error 28)");
        let (base, fake) = fake_node(None).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let err = seal(&node, "idx", 1000, Some(&events), &quick(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ran out of disk"), "{err}");
        assert!(fake.closed_at.lock().unwrap().is_none());
    }

    #[tokio::test]
    async fn a_writer_that_dies_is_named_instead_of_the_failed_request() {
        let events = NodeEvents::default();
        let refused = anyhow::anyhow!("error sending request: Connection refused");
        // Not exited (yet): the request's own error, after the grace period.
        let err = events
            .explain(anyhow::anyhow!("timed out"), Duration::from_millis(50))
            .await;
        assert_eq!(format!("{err:#}"), "timed out");

        events.writer_exited("the Quickwit writer was killed by signal 9 (SIGKILL)");
        // Only the first exit counts.
        events.writer_exited("later");
        let err = format!("{:#}", events.explain(refused, Duration::ZERO).await);
        assert!(
            err.starts_with("the Quickwit writer was killed by signal 9 (SIGKILL): "),
            "{err}"
        );
        assert!(err.contains("Connection refused"), "{err}");
        let err = events.check().unwrap_err().to_string();
        assert!(
            err.contains("signal 9") && err.contains("not publishing"),
            "{err}"
        );
    }

    #[tokio::test]
    async fn the_merge_wait_reports_a_writer_that_died() {
        use axum::http::StatusCode;
        use axum::routing::get;
        // The node stops answering once it has "died".
        let events = Arc::new(NodeEvents::default());
        let polls = Arc::new(AtomicU32::new(0));
        let (ev, p) = (events.clone(), polls.clone());
        let app = axum::Router::new()
            .route(
                "/metrics",
                get(move || async move {
                    if p.fetch_add(1, Ordering::SeqCst) < 2 {
                        return (
                            StatusCode::OK,
                            "quickwit_indexing_ongoing_merge_operations 1\n",
                        );
                    }
                    ev.writer_exited("the Quickwit writer was killed by signal 9 (SIGKILL)");
                    (StatusCode::BAD_GATEWAY, "")
                }),
            )
            .route(
                "/api/v1/indexes/{id}/splits",
                get(|| async { axum::Json(serde_json::json!({"splits": []})) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let base = format!("http://{addr}");
        let node = Node {
            http: &http,
            base: &base,
        };
        let err = seal(&node, "idx", 1000, Some(&events), &quick(), None)
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("signal 9"), "{err}");
        assert_eq!(polls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_merge_that_loses_documents_is_not_published() {
        let events = Arc::new(NodeEvents::default());
        let (base, _) = fake_node(Some(events.clone())).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let err = seal(&node, "idx", 1001, Some(&events), &quick(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected 1001"), "{err}");
    }
}
