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
//! Quickwit 0.9.1 doesn't restart a merge pipeline that fails once it has
//! been told to stop, and its uploader can fail a merge: to move the merged
//! split into its local cache, it measures the merge's scratch folder while
//! the merge executor deletes the input splits it downloaded there, and a
//! folder that disappears between the listing and its size fails the upload
//! ("No such file or directory"). The pipeline then never finishes its final
//! merges and the splits never change (issue #84). The release watches the writer's output for that, and for a
//! node that stays idle without finishing, and runs the final merges again:
//! it re-enables the source, sends one document the index's strict mapping
//! rejects (so a source whose shards were closed and deleted gets a new,
//! empty one and Quickwit starts its pipelines), waits for the new merge
//! pipeline, and disables the source again. A new pipeline reads the index's
//! unmerged splits from the metastore. After three tries the release fails.
//!
//! The wait is bounded, and the result is checked: the splits must hold
//! exactly the documents sent, and no more of them than the merge policy
//! leaves. The writer's output is watched for a full disk and for failed
//! merges; either stops the release, so nothing is published half merged.

mod events;
mod layout;
mod node;
mod watch;

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{bail, Context};
use serde_json::Value;

use crate::activity::Reporter;
use crate::sink::INDEX_TEMPLATE;
use crate::state::MergeProgress;

pub use events::{explained, FinalMerges, NodeEvents, EXIT_GRACE};
pub use layout::{IndexLayout, Split};
pub use node::{merge_gauges, Node};
pub use watch::{Burn, Burndown, MergeWatch};

use events::excerpt;
use layout::check;
use watch::{immature, log_wait};

/// The ingest API's source in every index (Quickwit's ingest v2).
pub const INGEST_SOURCE: &str = "_ingest-source";
/// Ingest v1's source in every index (`sink::IngestApi::V1`).
pub const INGEST_V1_SOURCE: &str = "_ingest-api-source";

/// A setting from the index template's `indexing_settings`, e.g.
/// `split_num_docs_target`: the merge checks read the policy the indexes
/// are created with, so the two can't drift apart.
pub fn template_setting(key: &str) -> Option<u64> {
    INDEX_TEMPLATE.lines().find_map(|line| {
        let (k, v) = line.trim().split_once(':')?;
        (k == key).then(|| v.trim().parse().ok()).flatten()
    })
}

/// The template's `split_num_docs_target`: the target of every index but a
/// main index built with another (`crate::sink::MainLayout::split_docs`).
pub fn template_split_target() -> u64 {
    template_setting("split_num_docs_target").unwrap_or(10_000_000)
}

/// The most splits a fully merged index of `docs` documents can have, when
/// its `split_num_docs_target` is `target`. Splits reach `target` documents
/// and stop merging, so there are at most `docs / target` of those. Below
/// that size, every split left after the planner settles is in a final
/// merge (at most `max_finalize_merge_operations`) except fewer than three,
/// the smallest final merge.
pub fn max_splits(docs: u64, target: u64) -> u64 {
    let finals = template_setting("max_finalize_merge_operations").unwrap_or(0);
    docs / target.max(1) + finals + 2
}

/// The most splits a fully merged index can have when its splits are
/// partitioned (05 §5.5.5): Quickwit merges only splits of one partition,
/// and runs its final merges in each, so every partition can be left with
/// its own [`max_splits`]. `docs` are the documents of each partition.
pub fn max_partitioned_splits(docs: impl IntoIterator<Item = u64>, target: u64) -> u64 {
    docs.into_iter().map(|d| max_splits(d, target)).sum()
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

/// Tries at the final merges before the release gives up.
pub const FINAL_MERGE_TRIES: u32 = 3;

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
    /// With it: how long the node may stay idle after the source is disabled
    /// without the final merges finishing before they are run again, and how
    /// long a reopened index may take to start its pipeline. Quickwit asks a
    /// pipeline to stop within 30 s (its actor heartbeat), and its merges
    /// show as running.
    pub finalize_stall: Duration,
    /// Where the wait's step and counts are recorded for the status page
    /// (`ops/activity`).
    pub report: Reporter,
}

impl Default for MergeWait {
    fn default() -> Self {
        Self {
            poll: Duration::from_secs(10),
            stable_polls: 3,
            timeout: Duration::from_secs(DEFAULT_TIMEOUT_SECS),
            finalize_grace: Duration::from_secs(60),
            finalize_stall: Duration::from_secs(600),
            report: Reporter::off(),
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
/// of documents sent, `split_target` the index's `split_num_docs_target`
/// ([`template_split_target`] unless the index was created with another);
/// `work_dir` is where the writer keeps its data. Nothing counts as sealed
/// after `wait.timeout`, and a request to the writer that hangs past it
/// (plus one poll) is abandoned.
pub async fn seal(
    node: &Node<'_>,
    index_id: &str,
    expected: u64,
    split_target: u64,
    events: Option<&NodeEvents>,
    wait: &MergeWait,
    work_dir: Option<&Path>,
) -> anyhow::Result<IndexLayout> {
    let closed = AtomicBool::new(false);
    let polls = poll_until_sealed(
        node,
        index_id,
        (expected, split_target),
        events,
        wait,
        work_dir,
        &closed,
    );
    match tokio::time::timeout(wait.timeout + wait.poll, polls).await {
        Ok(result) => result,
        Err(_) => {
            // A rerun of the final merges may have reopened the index: close
            // it again before the caller stops the writer.
            let reclosed = if closed.load(Ordering::SeqCst) {
                match tokio::time::timeout(CLOSE_TIMEOUT, node.close(index_id)).await {
                    Ok(Ok(())) => String::new(),
                    Ok(Err(e)) => format!(", and closing it again failed: {e:#}"),
                    Err(_) => ", and closing it again got no answer".to_owned(),
                }
            } else {
                String::new()
            };
            bail!(
                "merges into `{index_id}` did not finish within {} s (a request to the writer was \
                 still waiting){reclosed}; not publishing",
                wait.timeout.as_secs()
            )
        }
    }
}

/// `expected`: the documents sent and the index's split target, as for
/// [`seal`].
async fn poll_until_sealed(
    node: &Node<'_>,
    index_id: &str,
    (expected, split_target): (u64, u64),
    events: Option<&NodeEvents>,
    wait: &MergeWait,
    work_dir: Option<&Path>,
    closed: &AtomicBool,
) -> anyhow::Result<IndexLayout> {
    let start = tokio::time::Instant::now();
    let deadline = start + wait.timeout;
    let mut last_log: Option<tokio::time::Instant> = None;
    let mut step = "settle";
    let mut closed_at: Option<tokio::time::Instant> = None;
    let mut settle = Settle::new(wait.stable_polls);
    let mut tries = 0;
    let mut burndown = Burndown::default();
    loop {
        if let Some(e) = events {
            e.check()?;
        }
        let g = explained(events, node.gauges().await).await?;
        let (running, queued) = (g.running, g.queued);
        let splits = explained(events, node.splits(index_id).await).await?;
        burndown.observe(
            tokio::time::Instant::now(),
            immature(&splits).1,
            g.merged_bytes,
        );
        let ids: Vec<String> = splits.iter().map(|s| s.id.clone()).collect();
        let settled = settle.observe(running == 0 && queued == 0, &ids);
        wait.report.merges(MergeProgress {
            step: step.to_owned(),
            splits: splits.len() as u64,
            merges_running: running,
            merges_queued: queued,
        });
        if last_log.is_none_or(|t| t.elapsed() >= LOG_EVERY) {
            last_log = Some(tokio::time::Instant::now());
            log_wait(
                index_id,
                step,
                start.elapsed(),
                &g,
                &splits,
                &burndown,
                work_dir,
            );
        }
        match closed_at {
            None if settled => {
                tracing::info!(
                    index = index_id,
                    splits = splits.len(),
                    "merges settled; closing the index for its final merges"
                );
                closed.store(true, Ordering::SeqCst);
                explained(events, node.close(index_id).await).await?;
                closed_at = Some(tokio::time::Instant::now());
                tries = 1;
                step = "finalize";
                settle = Settle::new(wait.stable_polls);
            }
            Some(at) if settled => {
                // An index that was never written has no ingest-source
                // pipeline, so no "completed" line will come; there is
                // nothing for it to merge.
                let state = match events {
                    _ if expected == 0 => FinalMerges::Done,
                    Some(e) => e.final_merges(index_id),
                    None if at.elapsed() >= wait.finalize_grace => FinalMerges::Done,
                    None => FinalMerges::Pending,
                };
                let why = match state {
                    FinalMerges::Done if tokio::time::Instant::now() <= deadline => {
                        return check(index_id, expected, split_target, &splits);
                    }
                    FinalMerges::Done => None,
                    FinalMerges::Stopped(line) => {
                        Some(format!("its merge pipeline failed: {line}"))
                    }
                    // Idle all this time with the source disabled: whatever
                    // happened, nothing is merging and nothing will.
                    _ if events.is_some() && at.elapsed() >= wait.finalize_stall => Some(format!(
                        "the writer stayed idle for {} s without finishing them",
                        at.elapsed().as_secs()
                    )),
                    _ => None,
                };
                if let (Some(why), Some(e)) = (why, events) {
                    if tries >= FINAL_MERGE_TRIES {
                        bail!(
                            "the final merges of `{index_id}` did not finish in {tries} tries ({why}); \
                             not publishing a half-merged index"
                        );
                    }
                    tracing::warn!(
                        index = index_id,
                        splits = splits.len(),
                        attempt = tries + 1,
                        "the final merges stopped ({why}); reopening the index to run them again"
                    );
                    rerun_final_merges(node, index_id, e, wait, deadline).await?;
                    closed_at = Some(tokio::time::Instant::now());
                    tries += 1;
                    settle = Settle::new(wait.stable_polls);
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

/// Reopen `index_id`, wait for Quickwit to start a new merge pipeline for
/// it, and close it again: the new pipeline runs the final merges. `seal`
/// does this when they stop; public for the tests against a real writer.
pub async fn rerun_final_merges(
    node: &Node<'_>,
    index_id: &str,
    events: &NodeEvents,
    wait: &MergeWait,
    deadline: tokio::time::Instant,
) -> anyhow::Result<()> {
    let spawns = events.spawns(index_id);
    let started = tokio::time::Instant::now();
    let until = deadline.min(started + wait.finalize_stall);
    let no_pipeline = || {
        anyhow::anyhow!(
            "reopened `{index_id}` to run its final merges again, but the writer started no \
             merge pipeline for it within {} s; not publishing a half-merged index",
            started.elapsed().as_secs()
        )
    };
    // Closed again however this ends, so a later writer run starts nothing
    // for it: below, or by the guard if `seal` gives up (and drops this)
    // while a request is still waiting.
    let mut guard = CloseOnDrop::new(node, index_id);
    let reopened = tokio::time::timeout_at(until, async {
        explained(Some(events), node.reopen(index_id).await).await?;
        while events.spawns(index_id) == spawns {
            events.check()?;
            if tokio::time::Instant::now() + wait.poll > until {
                return Err(no_pipeline());
            }
            tokio::time::sleep(wait.poll).await;
        }
        Ok(())
    })
    .await
    .unwrap_or_else(|_| Err(no_pipeline()));
    let closed = match tokio::time::timeout(CLOSE_TIMEOUT, node.close(index_id)).await {
        Ok(r) => r,
        Err(_) => Err(anyhow::anyhow!(
            "the writer did not answer within {} s",
            CLOSE_TIMEOUT.as_secs()
        )),
    };
    guard.armed = closed.is_err();
    match (reopened, closed) {
        (Err(e), Err(c)) => Err(anyhow::anyhow!(
            "{e:#}; closing it again also failed: {c:#}"
        )),
        (Err(e), _) => Err(e),
        (Ok(()), closed) => explained(Some(events), closed).await,
    }
}

/// How long disabling a reopened index's source may take.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(30);

/// Disables an index's ingest source when dropped while armed, from a task
/// of its own: `rerun_final_merges` reopens the index, and the release must
/// not leave it open if `seal` gives up on it halfway.
struct CloseOnDrop {
    http: reqwest::Client,
    base: String,
    index_id: String,
    armed: bool,
}

impl CloseOnDrop {
    fn new(node: &Node<'_>, index_id: &str) -> Self {
        Self {
            http: node.http.clone(),
            base: node.base.to_owned(),
            index_id: index_id.to_owned(),
            armed: true,
        }
    }
}

impl Drop for CloseOnDrop {
    fn drop(&mut self) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if !self.armed {
            return;
        }
        let (http, base, index_id) = (
            self.http.clone(),
            std::mem::take(&mut self.base),
            std::mem::take(&mut self.index_id),
        );
        rt.spawn(async move {
            let node = Node {
                http: &http,
                base: &base,
            };
            match tokio::time::timeout(CLOSE_TIMEOUT, node.close(&index_id)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => {
                    tracing::warn!(index = %index_id, "could not close the reopened index: {e:#}")
                }
                Err(_) => tracing::warn!(
                    index = %index_id,
                    "could not close the reopened index: the writer did not answer"
                ),
            }
        });
    }
}

/// Stops the writer node and starts it again on the same data, for
/// [`seal_restarting`].
#[async_trait::async_trait]
pub trait Restart: Send + Sync {
    async fn restart(&self) -> anyhow::Result<()>;
}

/// The merge factor of ingest v1's final merges: any two splits of a
/// partition below the split target merge, so at most one is left in each.
pub const FINAL_MERGE_FACTOR: u64 = 2;

/// `config` with its merge policy's `merge_factor` set to `factor` (and its
/// `max_merge_factor` at least that).
pub fn with_merge_factor(config: &Value, factor: u64) -> anyhow::Result<Value> {
    let mut out = config.clone();
    let policy = out["indexing_settings"]["merge_policy"]
        .as_object_mut()
        .context("the index config has no indexing_settings.merge_policy")?;
    let kind = policy.get("type").and_then(Value::as_str).unwrap_or("");
    if !matches!(kind, "stable_log" | "limit_merge") {
        bail!("final merges need the stable_log merge policy, not `{kind}`");
    }
    policy.insert("merge_factor".into(), factor.into());
    let max = policy
        .get("max_merge_factor")
        .and_then(Value::as_u64)
        .unwrap_or(factor);
    policy.insert("max_merge_factor".into(), max.max(factor).into());
    Ok(out)
}

/// `config` with the template's merge factor back, when a release that
/// stopped during its final merges left [`FINAL_MERGE_FACTOR`] in it
/// ([`seal_restarting`] puts the config back, but that request can fail
/// too); `None` when it has the template's already. Any other factor is
/// refused: nothing this code writes sets one.
pub fn template_merge_factor(config: &Value) -> anyhow::Result<Option<Value>> {
    let want =
        template_setting("merge_factor").context("the index template has no merge_factor")?;
    let have = config["indexing_settings"]["merge_policy"]["merge_factor"].as_u64();
    if have == Some(want) {
        return Ok(None);
    }
    if have != Some(FINAL_MERGE_FACTOR) {
        bail!(
            "the index's merge_factor is {have:?}: neither the template's {want} nor the final \
             merges' {FINAL_MERGE_FACTOR}"
        );
    }
    with_merge_factor(config, want).map(Some)
}

/// [`seal`] for an index written through ingest v1, whose source can't be
/// disabled to run its final merges (Quickwit manages it): settle, give the
/// index a merge factor of [`FINAL_MERGE_FACTOR`], restart the writer (only
/// a new merge pipeline takes a new policy), wait for the index's new merge
/// pipeline to start with it, settle again, put the config back and check.
/// The splits left are below `merge_factor` in every partition, so a later
/// writer's merge pipeline has nothing to merge either way. A release that
/// fails or times out after changing the config puts it back first, and
/// says whether that worked; one cancelled outright tries from a task of its
/// own ([`RestoreOnDrop`]).
#[allow(clippy::too_many_arguments)]
pub async fn seal_restarting(
    node: &Node<'_>,
    writer: &dyn Restart,
    index_id: &str,
    expected: u64,
    split_target: u64,
    events: &NodeEvents,
    wait: &MergeWait,
    work_dir: Option<&Path>,
) -> anyhow::Result<IndexLayout> {
    let start = tokio::time::Instant::now();
    let deadline = start + wait.timeout;
    let guard = RestoreOnDrop::new(node, index_id);
    let sealing = async {
        let ev = Some(events);
        let splits = settled(
            node, index_id, ev, wait, "settle", start, deadline, work_dir,
        )
        .await?;
        events.check_v1(index_id)?;
        if expected == 0 && splits.is_empty() {
            return check(index_id, expected, split_target, &splits);
        }
        let original = explained(ev, node.index_config(index_id).await).await?;
        let finals = with_merge_factor(&original, FINAL_MERGE_FACTOR)?;
        // Before the request: one whose answer is lost may still apply.
        guard.arm(&original);
        explained(ev, node.update_index(index_id, &finals).await).await?;
        let (before, _) = events.v1_spawns(index_id);
        tracing::info!(
            index = index_id,
            splits = splits.len(),
            merge_factor = FINAL_MERGE_FACTOR,
            "merges settled; restarting the writer for the final merges"
        );
        writer.restart().await?;
        let until = deadline.min(tokio::time::Instant::now() + wait.finalize_stall);
        loop {
            events.check()?;
            events.check_v1(index_id)?;
            match events.v1_spawns(index_id) {
                (n, Some(line)) if n > before && merges_with_factor(&line, FINAL_MERGE_FACTOR) => {
                    break
                }
                (n, Some(line)) if n > before => bail!(
                    "the restarted writer merges `{index_id}` without the final merge factor; not \
                     publishing a half-merged index. Its line: {}",
                    excerpt(&line)
                ),
                _ => {}
            }
            if tokio::time::Instant::now() + wait.poll > until {
                bail!(
                    "the restarted writer started no merge pipeline for `{index_id}`; not \
                     publishing a half-merged index"
                );
            }
            tokio::time::sleep(wait.poll).await;
        }
        let splits = settled(
            node, index_id, ev, wait, "finalize", start, deadline, work_dir,
        )
        .await?;
        events.check_v1(index_id)?;
        explained(ev, node.update_index(index_id, &original).await).await?;
        guard.disarm();
        check(index_id, expected, split_target, &splits)
    };
    let result = match tokio::time::timeout(wait.timeout + wait.poll, sealing).await {
        Ok(result) => result,
        Err(_) => Err(anyhow::anyhow!(
            "merges into `{index_id}` did not finish within {} s (a request to the writer was \
             still waiting); not publishing",
            wait.timeout.as_secs()
        )),
    };
    let Err(e) = result else { return result };
    // The guard stays armed until the request is done: a release cancelled
    // during it still puts the config back from the guard's own task.
    let Some(original) = guard.pending() else {
        return Err(e);
    };
    let restored =
        match tokio::time::timeout(RESTORE_TIMEOUT, node.update_index(index_id, &original)).await {
            Ok(r) => r,
            Err(_) => Err(anyhow::anyhow!(
                "the writer did not answer within {} s",
                RESTORE_TIMEOUT.as_secs()
            )),
        };
    guard.disarm();
    match restored {
        Ok(()) => Err(anyhow::anyhow!(
            "{e:#}; `{index_id}`'s merge policy was put back"
        )),
        Err(r) => Err(anyhow::anyhow!(
            "{e:#}; `{index_id}` keeps merge_factor {FINAL_MERGE_FACTOR} in the metastore: putting its \
             merge policy back failed ({r:#}). Prune the failed index, or PUT its config back \
             (docs/operations.md, \"Duplicate-safe ingest\")"
        )),
    }
}

/// How long putting an index's merge policy back may take.
const RESTORE_TIMEOUT: Duration = Duration::from_secs(30);

/// The config [`seal_restarting`] changed, to put back if it doesn't: from
/// `seal_restarting` itself on an error or a timeout, and from a task of its
/// own if the release is cancelled (dropped) while it is armed.
struct RestoreOnDrop {
    http: reqwest::Client,
    base: String,
    index_id: String,
    original: std::sync::Mutex<Option<Value>>,
}

impl RestoreOnDrop {
    fn new(node: &Node<'_>, index_id: &str) -> Self {
        Self {
            http: node.http.clone(),
            base: node.base.to_owned(),
            index_id: index_id.to_owned(),
            original: std::sync::Mutex::new(None),
        }
    }

    fn slot(&self) -> std::sync::MutexGuard<'_, Option<Value>> {
        self.original.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn arm(&self, original: &Value) {
        *self.slot() = Some(original.clone());
    }

    /// The config to put back, if it still has to be.
    fn pending(&self) -> Option<Value> {
        self.slot().clone()
    }

    fn disarm(&self) -> Option<Value> {
        self.slot().take()
    }
}

impl Drop for RestoreOnDrop {
    fn drop(&mut self) {
        let Some(original) = self.disarm() else {
            return;
        };
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            tracing::warn!(
                index = %self.index_id,
                "could not put back the merge policy of a cancelled release's index (no runtime)"
            );
            return;
        };
        let (http, base, index_id) = (
            self.http.clone(),
            std::mem::take(&mut self.base),
            std::mem::take(&mut self.index_id),
        );
        rt.spawn(async move {
            let node = Node {
                http: &http,
                base: &base,
            };
            match tokio::time::timeout(RESTORE_TIMEOUT, node.update_index(&index_id, &original))
                .await
            {
                Ok(Ok(())) => {
                    tracing::info!(index = %index_id, "put back the merge policy of a cancelled release's index")
                }
                Ok(Err(e)) => tracing::warn!(
                    index = %index_id,
                    "could not put back the merge policy of a cancelled release's index: {e:#}"
                ),
                Err(_) => tracing::warn!(
                    index = %index_id,
                    "could not put back the merge policy of a cancelled release's index: the \
                     writer did not answer"
                ),
            }
        });
    }
}

/// Whether a "spawning merge pipeline" line's policy has `factor`:
/// `stable_log` prints `{ min_level_num_docs: .., merge_factor: 2, ..`,
/// `limit_merge` prints `{ merge_factor: 2, max_merge_factor: ..`.
fn merges_with_factor(line: &str, factor: u64) -> bool {
    line.contains(&format!(" merge_factor: {factor},"))
}

/// Poll until `index_id`'s merges have settled ([`Settle`]), reporting
/// `step`; its splits then.
#[allow(clippy::too_many_arguments)]
async fn settled(
    node: &Node<'_>,
    index_id: &str,
    events: Option<&NodeEvents>,
    wait: &MergeWait,
    step: &str,
    start: tokio::time::Instant,
    deadline: tokio::time::Instant,
    work_dir: Option<&Path>,
) -> anyhow::Result<Vec<Split>> {
    let mut settle = Settle::new(wait.stable_polls);
    let mut last_log: Option<tokio::time::Instant> = None;
    let mut burndown = Burndown::default();
    loop {
        if let Some(e) = events {
            e.check()?;
        }
        let g = explained(events, node.gauges().await).await?;
        let (running, queued) = (g.running, g.queued);
        let splits = explained(events, node.splits(index_id).await).await?;
        burndown.observe(
            tokio::time::Instant::now(),
            immature(&splits).1,
            g.merged_bytes,
        );
        let ids: Vec<String> = splits.iter().map(|s| s.id.clone()).collect();
        let done = settle.observe(running == 0 && queued == 0, &ids);
        wait.report.merges(MergeProgress {
            step: step.to_owned(),
            splits: splits.len() as u64,
            merges_running: running,
            merges_queued: queued,
        });
        if last_log.is_none_or(|t| t.elapsed() >= LOG_EVERY) {
            last_log = Some(tokio::time::Instant::now());
            log_wait(
                index_id,
                step,
                start.elapsed(),
                &g,
                &splits,
                &burndown,
                work_dir,
            );
        }
        if done {
            return Ok(splits);
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

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU32, Ordering};
    use std::sync::{Arc, Mutex};

    use super::*;

    /// The template's split target.
    const T: u64 = 30_000;

    #[test]
    fn the_template_sets_the_merge_policy_the_checks_assume() {
        assert_eq!(template_setting("split_num_docs_target"), Some(30_000));
        assert_eq!(template_setting("max_finalize_merge_operations"), Some(5));
        assert!(INDEX_TEMPLATE.contains("type: limit_merge"));
        assert_eq!(template_setting("no_such_setting"), None);
        let target = template_split_target();
        assert_eq!(target, 30_000);
        // 274,480 documents: nine full splits, up to five final merges, two left over.
        assert_eq!(max_splits(274_480, target), 9 + 5 + 2);
        assert_eq!(max_splits(51_764, target), 1 + 7);
        // The whole corpus: about 800 splits.
        assert_eq!(max_splits(23_800_000, target), 793 + 7);
        // At a larger target (`--split-docs`), fewer.
        assert_eq!(max_splits(23_800_000, 120_000), 198 + 7);
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
    fn final_merges_lower_only_the_merge_factor() {
        // As Quickwit 0.9.1 gives it (`GET /api/v1/indexes/{id}`).
        let config = serde_json::json!({
            "index_id": "i",
            "indexing_settings": {
                "split_num_docs_target": 60000,
                "merge_policy": {"type": "limit_merge", "merge_factor": 10, "max_merge_factor": 12,
                                 "max_merge_ops": 4, "max_finalize_merge_operations": 5,
                                 "maturation_period": "7days"},
            },
        });
        let finals = with_merge_factor(&config, FINAL_MERGE_FACTOR).unwrap();
        let mut expected = config.clone();
        expected["indexing_settings"]["merge_policy"]["merge_factor"] = 2.into();
        assert_eq!(finals, expected);
        // A max below the factor is raised to it.
        let mut low = config.clone();
        low["indexing_settings"]["merge_policy"]["max_merge_factor"] = 1.into();
        assert_eq!(
            with_merge_factor(&low, 2).unwrap()["indexing_settings"]["merge_policy"]
                ["max_merge_factor"],
            2
        );
        let mut other = config.clone();
        other["indexing_settings"]["merge_policy"] = serde_json::json!({"type": "no_merge"});
        assert!(with_merge_factor(&other, 2).is_err());
        assert!(with_merge_factor(&serde_json::json!({}), 2).is_err());
    }

    #[test]
    fn a_final_merge_factor_left_behind_goes_back_to_the_templates() {
        let config = serde_json::json!({
            "index_id": "i",
            "indexing_settings": {
                "merge_policy": {"type": "limit_merge", "merge_factor": 10, "max_merge_factor": 12},
            },
        });
        assert_eq!(template_merge_factor(&config).unwrap(), None);
        let left = with_merge_factor(&config, FINAL_MERGE_FACTOR).unwrap();
        assert_eq!(template_merge_factor(&left).unwrap(), Some(config.clone()));
        let mut odd = config.clone();
        odd["indexing_settings"]["merge_policy"]["merge_factor"] = 7.into();
        let err = template_merge_factor(&odd).unwrap_err().to_string();
        assert!(err.contains("Some(7)"), "{err}");
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

    /// A settled writer node for `seal_restarting`: its config has a
    /// `limit_merge` policy with factor 10; every PUT of it is recorded,
    /// and those after the first fail with `fail_restore`; the second takes 3 s
    /// with `slow_restore`.
    async fn config_node(
        fail_restore: bool,
        slow_restore: bool,
    ) -> (String, Arc<Mutex<Vec<Value>>>) {
        use axum::http::StatusCode;
        use axum::routing::get;
        let puts: Arc<Mutex<Vec<Value>>> = Arc::default();
        let p = puts.clone();
        let app = axum::Router::new()
            .route(
                "/metrics",
                get(|| async {
                    "quickwit_indexing_ongoing_merge_operations 0\n\
                     quickwit_indexing_pending_merge_operations 0\n"
                }),
            )
            .route(
                "/api/v1/indexes/{id}/splits",
                get(|| async {
                    axum::Json(serde_json::json!({"splits": [
                        {"split_id": "a", "num_docs": 10, "footer_offsets": {"end": 100}},
                        {"split_id": "b", "num_docs": 10, "footer_offsets": {"end": 100}},
                    ]}))
                }),
            )
            .route(
                "/api/v1/indexes/{id}",
                get(|| async {
                    axum::Json(serde_json::json!({"index_config": {
                        "index_id": "idx",
                        "indexing_settings": {"merge_policy": {
                            "type": "limit_merge", "merge_factor": 10, "max_merge_factor": 12
                        }}
                    }}))
                })
                .put(move |axum::Json(body): axum::Json<Value>| async move {
                    let n = {
                        let mut puts = p.lock().unwrap();
                        puts.push(body);
                        puts.len()
                    };
                    if slow_restore && n == 2 {
                        tokio::time::sleep(Duration::from_secs(3)).await;
                    }
                    if fail_restore && n > 1 {
                        StatusCode::INTERNAL_SERVER_ERROR
                    } else {
                        StatusCode::OK
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), puts)
    }

    /// A writer whose restart fails, or "succeeds" without a merge pipeline.
    struct FakeRestart(bool);

    #[async_trait::async_trait]
    impl Restart for FakeRestart {
        async fn restart(&self) -> anyhow::Result<()> {
            if self.0 {
                Ok(())
            } else {
                bail!("the writer did not stop")
            }
        }
    }

    fn factors(puts: &Mutex<Vec<Value>>) -> Vec<u64> {
        puts.lock()
            .unwrap()
            .iter()
            .map(|c| {
                c["indexing_settings"]["merge_policy"]["merge_factor"]
                    .as_u64()
                    .unwrap()
            })
            .collect()
    }

    async fn seal_with(restarts: bool, fail_restore: bool) -> (String, Vec<u64>) {
        let (base, puts) = config_node(fail_restore, false).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let events = NodeEvents::default();
        let err = seal_restarting(
            &node,
            &FakeRestart(restarts),
            "idx",
            20,
            T,
            &events,
            &quick(),
            None,
        )
        .await
        .unwrap_err();
        (format!("{err:#}"), factors(&puts))
    }

    #[tokio::test]
    async fn a_failed_restart_puts_the_merge_policy_back() {
        let (err, puts) = seal_with(false, false).await;
        assert!(err.contains("the writer did not stop"), "{err}");
        assert!(err.contains("merge policy was put back"), "{err}");
        assert_eq!(puts, [FINAL_MERGE_FACTOR, 10]);
    }

    #[tokio::test]
    async fn a_restart_without_final_merges_puts_the_merge_policy_back() {
        let (err, puts) = seal_with(true, false).await;
        assert!(err.contains("started no merge pipeline"), "{err}");
        assert!(err.contains("merge policy was put back"), "{err}");
        assert_eq!(puts, [FINAL_MERGE_FACTOR, 10]);
    }

    #[tokio::test]
    async fn a_merge_policy_that_cant_be_put_back_is_reported() {
        let (err, puts) = seal_with(false, true).await;
        assert!(err.contains("the writer did not stop"), "{err}");
        assert!(
            err.contains("keeps merge_factor 2 in the metastore"),
            "{err}"
        );
        assert!(err.contains("500"), "{err}");
        assert_eq!(puts, [FINAL_MERGE_FACTOR, 10]);
    }

    /// A release cancelled after the change puts the config back from a
    /// task of its own.
    #[tokio::test]
    async fn a_cancelled_release_puts_the_merge_policy_back() {
        struct Hang;
        #[async_trait::async_trait]
        impl Restart for Hang {
            async fn restart(&self) -> anyhow::Result<()> {
                std::future::pending().await
            }
        }
        let (base, puts) = config_node(false, false).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let events = NodeEvents::default();
        let wait = quick();
        let sealing = seal_restarting(&node, &Hang, "idx", 20, T, &events, &wait, None);
        assert!(tokio::time::timeout(Duration::from_millis(500), sealing)
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(5), async {
            while factors(&puts).len() < 2 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the config was put back");
        assert_eq!(factors(&puts), [FINAL_MERGE_FACTOR, 10]);
    }

    /// A release cancelled while it puts the config back after an error
    /// still has the guard's task put it back.
    #[tokio::test]
    async fn a_release_cancelled_during_the_restore_still_puts_the_merge_policy_back() {
        let (base, puts) = config_node(false, true).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let events = NodeEvents::default();
        let wait = quick();
        let sealing = seal_restarting(
            &node,
            &FakeRestart(false),
            "idx",
            20,
            T,
            &events,
            &wait,
            None,
        );
        assert!(tokio::time::timeout(Duration::from_millis(500), sealing)
            .await
            .is_err());
        tokio::time::timeout(Duration::from_secs(5), async {
            while factors(&puts).len() < 3 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the guard put the config back");
        assert_eq!(factors(&puts), [FINAL_MERGE_FACTOR, 10, 10]);
    }

    fn quick() -> MergeWait {
        MergeWait {
            poll: Duration::from_millis(5),
            stable_polls: 2,
            timeout: Duration::from_secs(10),
            finalize_grace: Duration::from_millis(50),
            finalize_stall: Duration::from_millis(300),
            report: Reporter::off(),
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
        let layout = seal(&node, "idx", 1000, T, Some(&events), &quick(), None)
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
        let layout = seal(&node, "idx", 1000, T, None, &quick(), None)
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
        let err = seal(&node, "idx", 1000, T, Some(&events), &wait, None)
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
        let err = seal(&node, "idx", 1000, T, None, &wait, None)
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
        let err = seal(&node, "idx", 1000, T, Some(&events), &quick(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("ran out of disk"), "{err}");
        assert!(fake.closed_at.lock().unwrap().is_none());
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
        let err = seal(&node, "idx", 1000, T, Some(&events), &quick(), None)
            .await
            .unwrap_err();
        let err = format!("{err:#}");
        assert!(err.contains("signal 9"), "{err}");
        assert_eq!(polls.load(Ordering::SeqCst), 3);
    }

    /// What a scripted writer does when the release closes the index.
    #[derive(Clone, Copy, PartialEq)]
    enum OnClose {
        /// Quickwit 0.9.1's uploader race (issue #84): the pipeline fails.
        Fail,
        /// Nothing at all.
        Nothing,
        /// The final merge: one split.
        Merge,
    }

    /// A writer whose final merges go as `script` says, one entry per close;
    /// reopening the index starts a new merge pipeline if `spawns`.
    struct Scripted {
        events: Arc<NodeEvents>,
        script: Vec<OnClose>,
        spawns: bool,
        toggles: Mutex<Vec<bool>>,
        probes: AtomicU32,
        merged: std::sync::atomic::AtomicBool,
    }

    const SPAWNED: &str = "INFO spawn_merge_pipeline{index_uid=idx:01M3 generation=0}: merge_pipeline: \
                           spawning merge pipeline index_uid=idx:01M3 source_id=_ingest-source root_dir=/x";
    const STOPPING: &str =
        "INFO indexing_service: shutting down orphan merge pipeline index_uid=idx:01M3 \
                            source_id=_ingest-source";
    const FAILED: &str = "ERROR merge_pipeline: merge pipeline failed index_uid=idx:01M3 \
                          source_id=_ingest-source generation=1";
    const COMPLETED: &str = "INFO merge_pipeline: merge pipeline completed successfully \
                             index_uid=idx:01M3 source_id=_ingest-source generation=1";

    async fn scripted(script: &[OnClose], spawns: bool) -> (String, Arc<Scripted>) {
        use axum::routing::{get, post, put};
        let w = Arc::new(Scripted {
            events: Arc::new(NodeEvents::default()),
            script: script.to_vec(),
            spawns,
            toggles: Mutex::default(),
            probes: AtomicU32::new(0),
            merged: Default::default(),
        });
        w.events.observe(SPAWNED);
        let (w1, w2, w3) = (w.clone(), w.clone(), w.clone());
        let app = axum::Router::new()
            .route(
                "/metrics",
                get(|| async { "quickwit_indexing_ongoing_merge_operations 0\n" }),
            )
            .route(
                "/api/v1/indexes/{id}/splits",
                get(move || async move {
                    let splits: &[(&str, u64)] = if w1.merged.load(Ordering::SeqCst) {
                        &[("m1", 1000)]
                    } else {
                        &[("s1", 400), ("s2", 600)]
                    };
                    axum::Json(
                        serde_json::json!({"splits": splits.iter().map(|(id, docs)| {
                        serde_json::json!({"split_id": id, "num_docs": docs})
                    }).collect::<Vec<_>>()}),
                    )
                }),
            )
            .route(
                "/api/v1/indexes/{id}/sources/_ingest-source/toggle",
                put(move |axum::Json(body): axum::Json<Value>| async move {
                    let enable = body["enable"].as_bool().unwrap();
                    let closes = {
                        let mut t = w2.toggles.lock().unwrap();
                        t.push(enable);
                        t.iter().filter(|e| !**e).count()
                    };
                    if !enable {
                        let step = w2
                            .script
                            .get(closes - 1)
                            .copied()
                            .unwrap_or(OnClose::Nothing);
                        if step != OnClose::Nothing {
                            w2.events.observe(STOPPING);
                        }
                        match step {
                            OnClose::Fail => w2.events.observe(FAILED),
                            OnClose::Merge => {
                                w2.merged.store(true, Ordering::SeqCst);
                                w2.events.observe(COMPLETED);
                            }
                            OnClose::Nothing => {}
                        }
                    }
                    "null"
                }),
            )
            .route(
                "/api/v1/{id}/ingest",
                post(move |body: String| async move {
                    // Only the reopening document, which the mapping rejects.
                    assert_eq!(body, "{\"usnm_reopen\": true}\n");
                    w3.probes.fetch_add(1, Ordering::SeqCst);
                    if w3.spawns {
                        w3.events.observe(SPAWNED);
                    }
                    axum::Json(serde_json::json!({
                        "num_docs_for_processing": 1, "num_ingested_docs": 0, "num_rejected_docs": 1
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}"), w)
    }

    async fn seal_scripted(
        script: &[OnClose],
        spawns: bool,
    ) -> (anyhow::Result<IndexLayout>, Arc<Scripted>) {
        let (base, w) = scripted(script, spawns).await;
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let r = seal(&node, "idx", 1000, T, Some(&w.events), &quick(), None).await;
        (r, w)
    }

    /// Issue #84: the final merge failed and Quickwit left its pipeline
    /// stopped, so the release waited out its timeout with six splits.
    #[tokio::test]
    async fn final_merges_that_fail_are_run_again() {
        let (r, w) = seal_scripted(&[OnClose::Fail, OnClose::Merge], true).await;
        let layout = r.unwrap();
        assert_eq!((layout.splits, layout.docs), (1, 1000));
        // Closed, reopened with one rejected document, closed again.
        assert_eq!(*w.toggles.lock().unwrap(), [false, true, false]);
        assert_eq!(w.probes.load(Ordering::SeqCst), 1);
        assert_eq!(w.events.spawns("idx"), 2);
    }

    #[tokio::test]
    async fn final_merges_that_never_finish_are_run_again() {
        let (r, w) = seal_scripted(&[OnClose::Nothing, OnClose::Merge], true).await;
        assert_eq!(r.unwrap().splits, 1);
        assert_eq!(*w.toggles.lock().unwrap(), [false, true, false]);
    }

    #[tokio::test]
    async fn final_merges_give_up_after_three_tries() {
        let (r, w) = seal_scripted(&[OnClose::Fail; 4], true).await;
        let err = r.unwrap_err().to_string();
        assert!(err.contains("did not finish in 3 tries"), "{err}");
        assert!(err.contains("merge pipeline failed"), "{err}");
        assert!(err.contains("half-merged"), "{err}");
        assert_eq!(
            *w.toggles.lock().unwrap(),
            [false, true, false, true, false]
        );
    }

    #[tokio::test]
    async fn a_reopened_index_that_starts_no_pipeline_is_closed_and_not_published() {
        let (r, w) = seal_scripted(&[OnClose::Fail, OnClose::Merge], false).await;
        let err = r.unwrap_err().to_string();
        assert!(err.contains("started no merge pipeline"), "{err}");
        // Sealed again all the same.
        assert_eq!(*w.toggles.lock().unwrap(), [false, true, false]);
    }

    /// Quickwit also runs the final merges by itself once the index's shards
    /// have been idle for 15 minutes and are deleted: closing the index then
    /// starts nothing, and the merges are already done.
    /// The merge wait gives up (drops the rerun) while the reopening request
    /// still waits: the index is closed again all the same.
    #[tokio::test]
    async fn a_rerun_dropped_while_reopening_closes_the_index() {
        use axum::routing::{post, put};
        let events = Arc::new(NodeEvents::default());
        events.observe(SPAWNED);
        let toggles = Arc::new(Mutex::new(Vec::new()));
        let t = toggles.clone();
        let app = axum::Router::new()
            .route(
                "/api/v1/indexes/{id}/sources/_ingest-source/toggle",
                put(move |axum::Json(body): axum::Json<Value>| async move {
                    t.lock().unwrap().push(body["enable"].as_bool().unwrap());
                    "null"
                }),
            )
            .route(
                "/api/v1/{id}/ingest",
                post(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    "{}"
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
            finalize_stall: Duration::from_secs(60),
            ..quick()
        };
        let far = tokio::time::Instant::now() + Duration::from_secs(60);
        let rerun = rerun_final_merges(&node, "idx", &events, &wait, far);
        assert!(tokio::time::timeout(Duration::from_millis(300), rerun)
            .await
            .is_err());
        let until = std::time::Instant::now() + Duration::from_secs(10);
        while toggles.lock().unwrap().len() < 2 {
            assert!(
                std::time::Instant::now() < until,
                "{:?}",
                toggles.lock().unwrap()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(*toggles.lock().unwrap(), [true, false]);
    }

    /// `seal` gives up while a rerun's reopening request still waits: it
    /// closes the index again before it returns, so the caller can stop the
    /// writer right away.
    #[tokio::test]
    async fn a_seal_that_gives_up_mid_rerun_closes_the_index_before_returning() {
        use axum::routing::{get, post, put};
        let events = Arc::new(NodeEvents::default());
        events.observe(SPAWNED);
        let toggles = Arc::new(Mutex::new(Vec::new()));
        let (ev, t) = (events.clone(), toggles.clone());
        let app = axum::Router::new()
            .route(
                "/metrics",
                get(|| async { "quickwit_indexing_ongoing_merge_operations 0\n" }),
            )
            .route(
                "/api/v1/indexes/{id}/splits",
                get(|| async {
                    axum::Json(
                        serde_json::json!({"splits": [{"split_id": "s1", "num_docs": 1000}]}),
                    )
                }),
            )
            .route(
                "/api/v1/indexes/{id}/sources/_ingest-source/toggle",
                put(move |axum::Json(body): axum::Json<Value>| async move {
                    let enable = body["enable"].as_bool().unwrap();
                    t.lock().unwrap().push(enable);
                    if !enable {
                        ev.observe(STOPPING);
                        ev.observe(FAILED);
                    }
                    "null"
                }),
            )
            .route(
                "/api/v1/{id}/ingest",
                post(|| async {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    "{}"
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
            timeout: Duration::from_millis(400),
            finalize_stall: Duration::from_secs(60),
            ..quick()
        };
        let err = seal(&node, "idx", 1000, T, Some(&events), &wait, None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("not publishing"), "{err}");
        let t = toggles.lock().unwrap().clone();
        assert_eq!(t.first(), Some(&false), "{t:?}");
        assert!(t.contains(&true), "{t:?}");
        assert_eq!(t.last(), Some(&false), "{t:?}");
    }

    #[tokio::test]
    async fn final_merges_that_ran_before_the_close_count() {
        let (base, w) = scripted(&[OnClose::Nothing], true).await;
        w.events.observe(STOPPING);
        w.events.observe(COMPLETED);
        w.merged.store(true, Ordering::SeqCst);
        let http = reqwest::Client::new();
        let node = Node {
            http: &http,
            base: &base,
        };
        let layout = seal(&node, "idx", 1000, T, Some(&w.events), &quick(), None)
            .await
            .unwrap();
        assert_eq!(layout.splits, 1);
        assert_eq!(*w.toggles.lock().unwrap(), [false]);
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
        let err = seal(&node, "idx", 1001, T, Some(&events), &quick(), None)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("expected 1001"), "{err}");
    }
}
