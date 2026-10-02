//! What the ingest job is doing, for the status page's "Right now" line
//! (`ops/activity`, 06 §6.3.6).
//!
//! `run`, `titles-sync` and `release` keep one [`Reporter`]. It writes the
//! item on every step change and pause, and a heartbeat every
//! [`HEARTBEAT`] while the command runs, from its own task, so a long
//! await (a 65-minute pause for LoC, a merge wait) still shows as live and
//! an execution the platform killed shows as stopped once the heartbeats
//! end. Writes are best effort: a failed one is logged and the job goes on.
//! The backfill job's curate workers don't report here; the status page
//! reads their leases instead.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::state::{Activity, MergeProgress, Outcome, State, Step};

/// Between heartbeats while a command runs. The API counts an execution
/// that hasn't written for 10 minutes as stopped.
pub const HEARTBEAT: Duration = Duration::from_secs(60);

/// Longest a write may take (Cosmos retries 429s for minutes).
const WRITE_TIMEOUT: Duration = Duration::from_secs(10);

/// Reports one command's steps; cheap to clone. [`Reporter::off`] does nothing.
#[derive(Clone, Default)]
pub struct Reporter(Option<Arc<Inner>>);

struct Inner {
    state: State,
    doc: Mutex<Activity>,
    /// Held from the snapshot through the write, so a slow heartbeat can't
    /// land after a newer step, pause or end and put the old one back.
    writing: tokio::sync::Mutex<()>,
}

impl std::fmt::Debug for Reporter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.0.is_some() {
            "Reporter(on)"
        } else {
            "Reporter(off)"
        })
    }
}

impl Reporter {
    /// A reporter that records nothing (tests, and commands that don't report).
    pub fn off() -> Self {
        Self(None)
    }

    /// Start reporting `command` at `step`, and write it.
    pub async fn start(state: State, command: &str, owner: &str, step: Step) -> Self {
        let now = Utc::now();
        let r = Self(Some(Arc::new(Inner {
            state,
            doc: Mutex::new(Activity {
                command: command.to_owned(),
                owner: owner.to_owned(),
                started_at: now,
                step,
                step_started_at: now,
                updated_at: now,
                done: None,
                total: None,
                paused_until: None,
                index_version: None,
                merge: None,
                ended_at: None,
                outcome: None,
                error: None,
            }),
            writing: tokio::sync::Mutex::new(()),
        })));
        r.write().await;
        r
    }

    fn update(&self, f: impl FnOnce(&mut Activity)) -> bool {
        match &self.0 {
            Some(inner) => {
                f(&mut inner.doc.lock().unwrap_or_else(|e| e.into_inner()));
                true
            }
            None => false,
        }
    }

    /// A copy of what was last recorded (tests).
    pub fn snapshot(&self) -> Option<Activity> {
        self.0
            .as_ref()
            .map(|i| i.doc.lock().unwrap_or_else(|e| e.into_inner()).clone())
    }

    /// Move to `step`, clearing the previous step's counts, and write it.
    pub async fn step(&self, step: Step) {
        let changed = self.update(|a| {
            if a.step != step {
                a.step = step;
                a.step_started_at = Utc::now();
                a.done = None;
                a.total = None;
                a.paused_until = None;
                a.merge = None;
            }
        });
        if changed {
            self.write().await;
        }
    }

    /// The step's progress; written with the next heartbeat.
    pub fn progress(&self, done: u64, total: u64) {
        self.update(|a| {
            a.done = Some(done);
            a.total = Some(total);
        });
    }

    /// The version being built; written with the next write.
    pub fn version(&self, version: &str) {
        self.update(|a| a.index_version = Some(version.to_owned()));
    }

    /// LoC rate limited titles-sync, which sends nothing until `until`
    /// (`None`: it resumed). Written at once.
    pub async fn paused_until(&self, until: Option<DateTime<Utc>>) {
        if self.update(|a| a.paused_until = until) {
            self.write().await;
        }
    }

    /// Where the merge wait is; written with the next heartbeat.
    pub fn merges(&self, m: MergeProgress) {
        self.update(|a| a.merge = Some(m));
    }

    /// Record how the command ended.
    pub async fn end(&self, outcome: Outcome, error: Option<String>) {
        let ended = self.update(|a| {
            a.ended_at = Some(Utc::now());
            a.outcome = Some(outcome);
            a.paused_until = None;
            a.error = error.map(|e| e.chars().take(2000).collect());
        });
        if ended {
            self.write().await;
        }
    }

    /// Write the item now; errors and timeouts are logged only.
    pub async fn write(&self) {
        let Some(inner) = &self.0 else { return };
        let _writing = inner.writing.lock().await;
        let doc = {
            let mut a = inner.doc.lock().unwrap_or_else(|e| e.into_inner());
            a.updated_at = Utc::now();
            a.clone()
        };
        match tokio::time::timeout(WRITE_TIMEOUT, inner.state.set_activity(&doc)).await {
            Ok(Ok(true)) => {}
            Ok(Ok(false)) => tracing::warn!(
                "another ingest execution is reporting its activity; not overwriting it"
            ),
            Ok(Err(e)) => {
                tracing::warn!(error = %format!("{e:#}"), "could not record the job's activity; continuing")
            }
            Err(_) => tracing::warn!("recording the job's activity timed out; continuing"),
        }
    }

    /// Write a heartbeat every `interval` until the guard is dropped.
    pub fn every(&self, interval: Duration) -> Heartbeat {
        let r = self.clone();
        Heartbeat(self.0.as_ref().map(|_| {
            tokio::spawn(async move {
                let mut tick = tokio::time::interval(interval);
                tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                tick.tick().await;
                loop {
                    tick.tick().await;
                    r.write().await;
                }
            })
        }))
    }
}

/// Stops the heartbeats when dropped.
pub struct Heartbeat(Option<tokio::task::JoinHandle<()>>);

impl Drop for Heartbeat {
    fn drop(&mut self) {
        if let Some(h) = &self.0 {
            h.abort();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::{DocStore, MemoryDocs};
    use crate::state::{ACTIVITY, OPS};

    async fn stored(docs: &MemoryDocs) -> serde_json::Value {
        docs.get(OPS, ACTIVITY, ACTIVITY)
            .await
            .unwrap()
            .unwrap()
            .doc
    }

    #[tokio::test]
    async fn records_steps_pauses_and_the_end() {
        let docs = Arc::new(MemoryDocs::default());
        let r = Reporter::start(State::new(docs.clone()), "run", "host-1-abc", Step::Listing).await;
        let v = stored(&docs).await;
        assert_eq!(
            (v["step"].as_str(), v["kind"].as_str()),
            (Some("listing"), Some(ACTIVITY))
        );

        r.step(Step::Titles).await;
        r.progress(342, 3464);
        let until = Utc::now() + chrono::Duration::minutes(65);
        r.paused_until(Some(until)).await;
        let v = stored(&docs).await;
        assert_eq!(v["step"], "titles");
        assert_eq!(
            (v["done"].as_u64(), v["total"].as_u64()),
            (Some(342), Some(3464))
        );
        assert!(v["paused_until"].is_string());

        // A new step clears the last one's counts.
        r.step(Step::Indexing).await;
        r.version("pages-v1");
        r.merges(MergeProgress {
            step: "settle".into(),
            splits: 40,
            merges_running: 2,
            merges_queued: 1,
        });
        r.step(Step::Merging).await;
        let v = stored(&docs).await;
        assert_eq!(
            (v["step"].as_str(), v["done"].is_null()),
            (Some("merging"), true)
        );
        assert_eq!(v["index_version"], "pages-v1");

        r.end(Outcome::Failed, Some("boom".into())).await;
        let v = stored(&docs).await;
        assert_eq!(
            (v["outcome"].as_str(), v["error"].as_str()),
            (Some("failed"), Some("boom"))
        );
        assert!(v["ended_at"].is_string());
    }

    #[tokio::test]
    async fn an_overlapping_execution_doesnt_overwrite_a_live_one() {
        let docs = Arc::new(MemoryDocs::default());
        let first = Reporter::start(State::new(docs.clone()), "run", "first", Step::Titles).await;
        let second =
            Reporter::start(State::new(docs.clone()), "run", "second", Step::Listing).await;
        second.step(Step::Indexing).await;
        let v = stored(&docs).await;
        assert_eq!(
            (v["owner"].as_str(), v["step"].as_str()),
            (Some("first"), Some("titles"))
        );
        // Once the first has ended, the next one takes over.
        first.end(Outcome::TitlesLeft, None).await;
        second.write().await;
        let v = stored(&docs).await;
        assert_eq!(
            (v["owner"].as_str(), v["step"].as_str()),
            (Some("second"), Some("indexing"))
        );
    }

    #[tokio::test]
    async fn heartbeats_keep_the_item_fresh() {
        let docs = Arc::new(MemoryDocs::default());
        let r = Reporter::start(State::new(docs.clone()), "run", "o", Step::Titles).await;
        let first = stored(&docs).await["updated_at"].clone();
        let beat = r.every(Duration::from_millis(10));
        tokio::time::sleep(Duration::from_millis(60)).await;
        drop(beat);
        assert_ne!(stored(&docs).await["updated_at"], first);
    }

    #[tokio::test]
    async fn off_does_nothing() {
        let r = Reporter::off();
        r.step(Step::Titles).await;
        r.progress(1, 2);
        r.end(Outcome::Published, None).await;
        assert!(r.snapshot().is_none());
        drop(r.every(Duration::from_millis(1)));
    }
}
