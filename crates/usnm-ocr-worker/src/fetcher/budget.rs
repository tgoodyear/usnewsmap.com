//! Pacer budgets, behind an interface so where they live can change without
//! the fetcher knowing.
//!
//! A [`Budget`] is one policy's shared request rate: slots, holds and the
//! lasting slow-down, seen alike by every process using it. Each operation
//! has to be atomic against the others. The first implementation is
//! [`GlobalPacer`], a state file under `flock`.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Context;
use async_trait::async_trait;

use super::policy::Policy;
use crate::loc::global::{GlobalPacer, State};

/// One policy's shared budget.
#[async_trait]
pub trait Budget: Send + Sync + std::fmt::Debug {
    /// Take the next free slot at or after `not_before_ms` (ms since the
    /// epoch), past any hold; returns it and the generation it was taken in.
    async fn reserve(&self, not_before_ms: u64) -> std::io::Result<(u64, u64)>;
    /// Whether a slot taken in `generation` still stands: no hold or
    /// slow-down came since, and no hold is running.
    async fn still_good(&self, generation: u64) -> bool;
    /// Hold every user of the budget for at least `d` from now.
    async fn hold(&self, d: Duration);
    /// The host pushed back: the slow rate, for everyone, until [`Budget::reset`].
    async fn slow_down(&self, why: &str);
    async fn state(&self) -> std::io::Result<State>;
    /// Back to the normal rate. Only ever an operator's decision.
    async fn reset(&self) -> std::io::Result<()>;
    /// How long to wait before trying again when the budget can't be reached.
    fn retry_gap(&self) -> Duration;
    /// When this budget's user last sent, across restarts, in ms since the epoch.
    async fn last_sent_ms(&self) -> Option<u64> {
        None
    }
    /// Record that a request is going out now.
    async fn mark_sent(&self) {}

    /// Wait for a slot at or after `not_before_ms`; returns when it was. A
    /// hold or slow-down that lands while waiting sends the caller back for a
    /// new slot.
    async fn admit(&self, not_before_ms: u64) -> u64 {
        loop {
            let (at, generation) = match self.reserve(not_before_ms).await {
                Ok(r) => r,
                Err(e) => {
                    // Without the budget a request could break it: wait, then try for
                    // a slot again. Nothing goes out until the budget answers.
                    tracing::warn!(budget = ?self, error = %e, "the budget failed; waiting before trying again");
                    tokio::time::sleep(self.retry_gap()).await;
                    continue;
                }
            };
            let wait = at.saturating_sub(crate::loc::global::epoch_ms_in(Duration::ZERO));
            if wait > 0 {
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
            if self.still_good(generation).await {
                return at;
            }
        }
    }
}

#[async_trait]
impl Budget for GlobalPacer {
    async fn reserve(&self, not_before_ms: u64) -> std::io::Result<(u64, u64)> {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.reserve_at(not_before_ms))
            .await
            .map_err(std::io::Error::other)?
    }

    async fn still_good(&self, generation: u64) -> bool {
        let me = self.clone();
        tokio::task::spawn_blocking(move || me.is_still_good(generation))
            .await
            .unwrap_or(false)
    }

    async fn hold(&self, d: Duration) {
        GlobalPacer::hold(self, d);
    }

    async fn slow_down(&self, why: &str) {
        GlobalPacer::slow_down(self, why);
    }

    async fn state(&self) -> std::io::Result<State> {
        GlobalPacer::state(self)
    }

    async fn reset(&self) -> std::io::Result<()> {
        GlobalPacer::reset(self)
    }

    fn retry_gap(&self) -> Duration {
        self.slow_gap()
    }

    async fn last_sent_ms(&self) -> Option<u64> {
        self.read_stamp()
    }

    async fn mark_sent(&self) {
        self.write_stamp();
    }
}

/// Where each policy's budget comes from.
pub trait Budgets: Send + Sync {
    fn budget(&self, policy: &Policy) -> anyhow::Result<Arc<dyn Budget>>;
}

/// Budgets in state files: a named file for a policy that has one, otherwise
/// `<dir>/<policy>.json`.
#[derive(Debug, Default)]
pub struct FileBudgets {
    dir: Option<PathBuf>,
    named: HashMap<String, (PathBuf, Option<PathBuf>)>,
}

impl FileBudgets {
    pub fn new(dir: Option<PathBuf>) -> Self {
        Self {
            dir,
            named: HashMap::new(),
        }
    }

    /// `policy`'s budget is in `file`, and `stamp` keeps its last request's time.
    pub fn with_file(mut self, policy: &str, file: PathBuf, stamp: Option<PathBuf>) -> Self {
        self.named.insert(policy.to_owned(), (file, stamp));
        self
    }
}

impl Budgets for FileBudgets {
    fn budget(&self, policy: &Policy) -> anyhow::Result<Arc<dyn Budget>> {
        let (file, stamp) = match self.named.get(&policy.name) {
            Some((f, s)) => (f.clone(), s.clone()),
            None => {
                let dir = self
                    .dir
                    .as_ref()
                    .with_context(|| format!("no budget file for policy {}", policy.name))?;
                (dir.join(format!("{}.json", policy.name)), None)
            }
        };
        let mut g = GlobalPacer::with_gaps(file, policy.normal_gap(), policy.slow_gap())
            .labelled(&policy.label);
        if let Some(s) = stamp {
            g = g.with_stamp(s);
        }
        Ok(Arc::new(g))
    }
}
