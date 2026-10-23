//! What the writer node's output says about the release: full disks, failed
//! merges and the state of each index's merge pipelines.

use std::collections::BTreeMap;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::bail;

use super::{INGEST_SOURCE, INGEST_V1_SOURCE};

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
    /// Each index's `_ingest-source` merge pipeline, by index id.
    pipelines: BTreeMap<String, Pipeline>,
    /// Each index's `_ingest-api-source` merge pipeline starts, by index id:
    /// how many, and the last one's line (it has the merge policy).
    v1_spawns: BTreeMap<String, (u32, String)>,
    /// The first failure of each index's `_ingest-api-source` merge
    /// pipeline, by index id.
    v1_failed: BTreeMap<String, String>,
}

/// What the writer's output has said about one index's `_ingest-source`
/// merge pipeline. The flags are about its current run: Quickwit logs
/// "spawning merge pipeline" for a new pipeline and for each restart.
#[derive(Debug, Default)]
struct Pipeline {
    spawns: u32,
    /// The indexing service told it to run its final merges and stop.
    stopping: bool,
    /// It failed: the line. Quickwit restarts it unless it was `stopping`.
    failed: Option<String>,
    /// It ran its final merges and exited.
    completed: bool,
}

/// Where an index's final merges stand, from the writer's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinalMerges {
    /// Not asked for yet: the pipeline runs, or Quickwit will restart it.
    Pending,
    /// The pipeline was told to stop and is merging what is left.
    Running,
    /// The pipeline ran its final merges and exited.
    Done,
    /// The pipeline failed after it was told to stop: Quickwit won't restart
    /// it, so nothing more will merge. The writer's line.
    Stopped(String),
}

/// What a writer line says about an `_ingest-source` merge pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PipelineEvent {
    Spawned,
    Stopping,
    Failed,
    Completed,
}

impl PipelineEvent {
    fn of(line: &str) -> Option<Self> {
        if !line.contains(&format!("source_id={INGEST_SOURCE}")) {
            return None;
        }
        [
            ("spawning merge pipeline", Self::Spawned),
            ("shutting down orphan merge pipeline", Self::Stopping),
            ("merge pipeline failed", Self::Failed),
            ("merge pipeline completed successfully", Self::Completed),
        ]
        .into_iter()
        .find_map(|(text, event)| line.contains(text).then_some(event))
    }
}

/// The first 300 characters of a line, for an error message.
pub(super) fn excerpt(line: &str) -> String {
    line.chars().take(300).collect()
}

impl NodeEvents {
    pub fn observe(&self, line: &str) {
        let disk_full = line.contains("No space left on device") || line.contains("os error 28");
        let merge_failed = line.contains("failed to merge splits")
            || line.contains("merge scheduler service is dead");
        let event = PipelineEvent::of(line);
        let v1_source = format!("source_id={INGEST_V1_SOURCE}");
        let v1 = line.split_whitespace().any(|w| w == v1_source);
        let v1_spawn = v1 && line.contains("spawning merge pipeline");
        let v1_failed = v1 && line.contains("merge pipeline failed");
        if !(disk_full || merge_failed || event.is_some() || v1_spawn || v1_failed) {
            return;
        }
        let mut e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if disk_full && e.disk_full.is_none() {
            e.disk_full = Some(excerpt(line));
        }
        if merge_failed && e.merge_failed.is_none() {
            e.merge_failed = Some(excerpt(line));
        }
        // `index_uid=pages-delta-20261008-1:01M3…`: the field, not the span
        // (`spawn_merge_pipeline{index_uid=…`).
        let id = line
            .split_whitespace()
            .find_map(|w| w.strip_prefix("index_uid="))
            .and_then(|uid| uid.split(':').next());
        if let (true, Some(id)) = (v1_spawn, id) {
            let s = e.v1_spawns.entry(id.to_owned()).or_default();
            *s = (s.0 + 1, line.to_owned());
        }
        if let (true, Some(id)) = (v1_failed, id) {
            e.v1_failed
                .entry(id.to_owned())
                .or_insert_with(|| excerpt(line));
        }
        if let (Some(event), Some(id)) = (event, id) {
            let p = e.pipelines.entry(id.to_owned()).or_default();
            match event {
                PipelineEvent::Spawned => {
                    *p = Pipeline {
                        spawns: p.spawns + 1,
                        ..Pipeline::default()
                    }
                }
                PipelineEvent::Stopping => p.stopping = true,
                PipelineEvent::Failed => p.failed = Some(excerpt(line)),
                PipelineEvent::Completed => p.completed = true,
            }
        }
    }

    /// Record that the writer process has ended, and why (`sink::describe_exit`).
    pub fn writer_exited(&self, why: &str) {
        let mut e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.exited.get_or_insert_with(|| why.to_owned());
    }

    /// Forget that the writer exited: the release stopped it to start it
    /// again (`sink::WriterProcess::restart`).
    pub fn clear_exit(&self) {
        let mut e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.exited = None;
    }

    /// Why the writer process ended, if it has.
    pub fn exit(&self) -> Option<String> {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.exited.clone()
    }

    /// Fails once `index_id`'s ingest v1 merge pipeline has failed: it is
    /// not followed like v2's, so its merges could otherwise look settled
    /// with splits left unmerged.
    pub fn check_v1(&self, index_id: &str) -> anyhow::Result<()> {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(line) = e.v1_failed.get(index_id) {
            bail!(
                "the merge pipeline of `{index_id}` failed; not publishing a half-merged index. \
                 Its output: {line}"
            );
        }
        Ok(())
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

    /// Where `index_id`'s final merges stand.
    pub fn final_merges(&self, index_id: &str) -> FinalMerges {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        match e.pipelines.get(index_id) {
            Some(p) if p.completed => FinalMerges::Done,
            Some(Pipeline {
                stopping: true,
                failed: Some(line),
                ..
            }) => FinalMerges::Stopped(line.clone()),
            Some(p) if p.stopping => FinalMerges::Running,
            _ => FinalMerges::Pending,
        }
    }

    /// How many times Quickwit has started `index_id`'s merge pipeline.
    pub fn spawns(&self, index_id: &str) -> u32 {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.pipelines.get(index_id).map_or(0, |p| p.spawns)
    }

    /// How many times Quickwit has started `index_id`'s ingest v1 merge
    /// pipeline, and the last start's line.
    pub fn v1_spawns(&self, index_id: &str) -> (u32, Option<String>) {
        let e = self.0.lock().unwrap_or_else(|e| e.into_inner());
        e.v1_spawns
            .get(index_id)
            .map_or((0, None), |(n, line)| (*n, Some(line.clone())))
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::merges::merges_with_factor;

    #[test]
    fn watches_the_writer_output() {
        let e = NodeEvents::default();
        e.observe("2026-09-29T09:09:18Z  WARN quickwit_config: peer seeds are empty");
        assert!(e.check().is_ok());
        assert_eq!(
            e.final_merges("pages-delta-20261008-1"),
            FinalMerges::Pending
        );

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
        assert_eq!(e.final_merges("pages-delta-20261008-1"), FinalMerges::Done);
        assert_eq!(
            e.final_merges("pages-base-20261001-1"),
            FinalMerges::Pending
        );
        assert!(e.check().is_ok());

        e.observe(
            "2026-09-29T09:27:26Z ERROR quickwit_actors::spawn_builder: actor-exit \
             exit_status=Failure(No space left on device (os error 28) at path \"/work/quickwit/qwdata/indexing\")",
        );
        let err = e.check().unwrap_err().to_string();
        assert!(err.contains("ran out of disk"), "{err}");
        assert!(err.contains("os error 28"), "{err}");
    }

    /// The writer's lines from CI run 37088490670 (issue #84): the final
    /// merge's upload failed, and Quickwit left the pipeline stopped.
    #[test]
    fn follows_a_merge_pipeline_through_failures_and_restarts() {
        let id = "pages-delta-20261015-1";
        let e = NodeEvents::default();
        let spawned = "2026-10-03T02:06:57.809Z  INFO spawn_merge_pipeline{index_uid=pages-delta-20261015-1:01M3ZR7CS2DXSCJPAT30QTW46Q generation=0}: \
                       quickwit_indexing::actors::merge_pipeline: spawning merge pipeline \
                       index_uid=pages-delta-20261015-1:01M3ZR7CS2DXSCJPAT30QTW46Q source_id=_ingest-source root_dir=/tmp/x";
        let stopping = "2026-10-03T02:07:21.807Z  INFO quickwit_indexing::actors::indexing_service: shutting down \
                        orphan merge pipeline index_uid=pages-delta-20261015-1:01M3ZR7CS2DXSCJPAT30QTW46Q source_id=_ingest-source";
        let failed = "2026-10-03T02:07:22.836Z ERROR quickwit_indexing::actors::merge_pipeline: merge pipeline failed \
                      index_uid=pages-delta-20261015-1:01M3ZR7CS2DXSCJPAT30QTW46Q source_id=_ingest-source generation=1 \
                      healthy_actors=[] failed_or_unhealthy_actors=[\"MergePublisher-young-tVo8\"]";
        let completed = "2026-10-03T02:09:19.983Z  INFO quickwit_indexing::actors::merge_pipeline: merge pipeline \
                         completed successfully index_uid=pages-delta-20261015-1:01M3ZR7CS2DXSCJPAT30QTW46Q \
                         source_id=_ingest-source generation=1";
        e.observe(spawned);
        assert_eq!(
            (e.final_merges(id), e.spawns(id)),
            (FinalMerges::Pending, 1)
        );
        // A failure while it runs: Quickwit restarts it after a heartbeat.
        e.observe(failed);
        assert_eq!(e.final_merges(id), FinalMerges::Pending);
        e.observe(spawned);
        assert_eq!(
            (e.final_merges(id), e.spawns(id)),
            (FinalMerges::Pending, 2)
        );
        e.observe(stopping);
        assert_eq!(e.final_merges(id), FinalMerges::Running);
        // A failure once it was told to stop: nothing restarts it.
        e.observe(failed);
        match e.final_merges(id) {
            FinalMerges::Stopped(line) => assert!(line.contains("MergePublisher"), "{line}"),
            other => panic!("{other:?}"),
        }
        // Told to stop while it waited to be restarted: the same.
        let e2 = NodeEvents::default();
        e2.observe(spawned);
        e2.observe(failed);
        e2.observe(stopping);
        assert!(matches!(e2.final_merges(id), FinalMerges::Stopped(_)));
        // A new pipeline starts over; "failed" is not a failed merge.
        e.observe(spawned);
        assert_eq!(
            (e.final_merges(id), e.spawns(id)),
            (FinalMerges::Pending, 3)
        );
        e.observe(stopping);
        e.observe(completed);
        assert_eq!(e.final_merges(id), FinalMerges::Done);
        assert!(e.check().is_ok());
        // Another source's pipeline is not this one.
        e.observe(&spawned.replace("_ingest-source", "_ingest-api-source"));
        assert_eq!(e.spawns(id), 3);
    }

    /// Ingest v1's merge pipeline is tracked apart, with the policy it
    /// started with; a restart the release asked for is not an exit.
    #[test]
    fn follows_ingest_v1_merge_pipelines_and_restarts() {
        let id = "pages-base-20261020-1";
        let e = NodeEvents::default();
        let spawned = |factor: u32| {
            format!(
                "2026-10-19T16:04:28.154Z  INFO spawn_merge_pipeline{{index_uid={id}:01M5AEGETSRZT3Y8CFRXV1RE8X generation=0}}: \
                 quickwit_indexing::actors::merge_pipeline: spawning merge pipeline \
                 index_uid={id}:01M5AEGETSRZT3Y8CFRXV1RE8X source_id=_ingest-api-source root_dir=/work/x \
                 merge_policy=StableLogMergePolicy {{ config: StableLogMergePolicyConfig {{ min_level_num_docs: 100000, \
                 merge_factor: {factor}, max_merge_factor: 12, maturation_period: 604800s }}, split_num_docs_target: 60000 }}"
            )
        };
        assert_eq!(e.v1_spawns(id), (0, None));
        e.observe(&spawned(10));
        assert_eq!(e.v1_spawns(id).0, 1);
        // Not the v2 source's pipeline.
        assert_eq!(e.spawns(id), 0);
        e.observe(&spawned(2));
        let (n, line) = e.v1_spawns(id);
        assert_eq!(n, 2);
        let line = line.unwrap();
        assert!(merges_with_factor(&line, 2) && !merges_with_factor(&line, 12));
        // `limit_merge` prints the factor first.
        let limit = "merge_policy=ConstWriteAmplificationMergePolicy { config: \
                     ConstWriteAmplificationMergePolicyConfig { merge_factor: 2, max_merge_factor: 12, \
                     max_merge_ops: 4, maturation_period: 604800s } }";
        assert!(merges_with_factor(limit, 2));
        assert!(!merges_with_factor(
            &limit.replace("merge_factor: 2,", "merge_factor: 10,"),
            2
        ));
        // The v2 source's line doesn't count.
        e.observe(&spawned(2).replace("_ingest-api-source", "_ingest-source"));
        assert_eq!((e.v1_spawns(id).0, e.spawns(id)), (2, 1));
        e.writer_exited("the Quickwit writer exited with status 0");
        assert!(e.check().is_err());
        e.clear_exit();
        assert!(e.check().is_ok());
        // A v1 merge pipeline that fails stops a v1 release of its index.
        let failed = |id: &str, source: &str| {
            format!(
                "2026-10-19T16:05:00.000Z ERROR quickwit_indexing::actors::merge_pipeline: merge pipeline \
                 failed index_uid={id}:01M5AEGETSRZT3Y8CFRXV1RE8X source_id={source} generation=1 \
                 healthy_actors=[] failed_or_unhealthy_actors=[\"MergePublisher-young-tVo8\"]"
            )
        };
        e.observe(&failed("pages-delta-20261020-1", "_ingest-api-source"));
        e.observe(&failed(id, "_ingest-source"));
        assert!(e.check_v1(id).is_ok());
        e.observe(&failed(id, "_ingest-api-source"));
        let err = e.check_v1(id).unwrap_err().to_string();
        assert!(
            err.contains("merge pipeline of `pages-base-20261020-1` failed"),
            "{err}"
        );
        assert!(err.contains("half-merged"), "{err}");
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
}
