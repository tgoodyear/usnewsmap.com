//! How fast the merges go, and the "release merges" progress line.

use std::path::Path;
use std::time::Duration;

use crate::progress;

use super::node::Gauges;
use super::{Node, Split, LOG_EVERY};

/// How long the merge wait looks back to say how fast merging goes.
const BURN_WINDOW: Duration = Duration::from_secs(30 * 60);

/// How fast the merges get through what is left (#283): the bytes of the
/// immature splits, and the bytes merges write, sampled at each poll of the
/// merge wait and compared over the last [`BURN_WINDOW`]. For the wait's
/// lines, so a long one says how far it has to go.
#[derive(Debug, Default)]
pub struct Burndown {
    /// (when, immature bytes, bytes merged so far) per poll, oldest first.
    samples: std::collections::VecDeque<(tokio::time::Instant, u64, Option<u64>)>,
}

/// What [`Burndown::rates`] reports.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Burn {
    /// How fast the immature splits' bytes went down, in MB an hour.
    pub immature_mb_per_hour: Option<f64>,
    /// How fast merges wrote, in MB a second.
    pub merge_write_mb_per_sec: Option<f64>,
    /// Minutes left for the immature bytes at that rate.
    pub eta_mins: Option<u64>,
}

impl Burndown {
    /// Record one poll: at `at`, `immature` bytes in immature splits, and
    /// the writer's merges' written bytes so far, if it says.
    pub fn observe(&mut self, at: tokio::time::Instant, immature: u64, merged: Option<u64>) {
        // A restarted writer counts its written bytes from zero again.
        if merged.is_some_and(|m| self.samples.back().and_then(|s| s.2).is_some_and(|b| m < b)) {
            for s in &mut self.samples {
                s.2 = None;
            }
        }
        self.samples.push_back((at, immature, merged));
        while self
            .samples
            .get(1)
            .is_some_and(|s| at.duration_since(s.0) >= BURN_WINDOW)
        {
            self.samples.pop_front();
        }
    }

    /// The rates over the samples kept, once they span a minute.
    pub fn rates(&self) -> Burn {
        let (Some(first), Some(last)) = (self.samples.front(), self.samples.back()) else {
            return Burn::default();
        };
        let secs = last.0.duration_since(first.0).as_secs_f64();
        if secs < 60.0 {
            return Burn::default();
        }
        const MB: f64 = 1024.0 * 1024.0;
        let fell = first.1 as f64 - last.1 as f64;
        let per_hour = fell / MB / secs * 3600.0;
        let written = self
            .samples
            .iter()
            .find_map(|s| s.2.map(|b| (s.0, b)))
            .zip(last.2)
            .map(|((t, from), to)| {
                (
                    to.saturating_sub(from) as f64,
                    last.0.duration_since(t).as_secs_f64(),
                )
            })
            .filter(|(_, secs)| *secs >= 60.0)
            .map(|(bytes, secs)| (bytes / MB / secs * 10.0).round() / 10.0);
        Burn {
            immature_mb_per_hour: Some(per_hour.round()),
            merge_write_mb_per_sec: written,
            eta_mins: (fell > 0.0).then(|| (last.1 as f64 / (fell / secs) / 60.0).round() as u64),
        }
    }
}

/// How many of `splits` are immature, and their bytes.
pub(super) fn immature(splits: &[Split]) -> (u64, u64) {
    splits
        .iter()
        .filter(|s| s.immature)
        .fold((0, 0), |(n, b), s| (n + 1, b + s.bytes))
}

/// One "release merges" line: where `index_id`'s merges are, `elapsed`
/// into the wait's `step`, and how fast they go ([`Burndown`]).
pub(super) fn log_wait(
    index_id: &str,
    step: &str,
    elapsed: Duration,
    g: &Gauges,
    splits: &[Split],
    burndown: &Burndown,
    work_dir: Option<&Path>,
) {
    let burn = burndown.rates();
    let (immature_splits, immature_bytes) = immature(splits);
    tracing::info!(
        index = index_id,
        step,
        splits = splits.len(),
        merges_running = g.running,
        merges_queued = g.queued,
        elapsed_secs = elapsed.as_secs(),
        immature_splits,
        immature_mb = immature_bytes / (1024 * 1024),
        pending_merge_mb = g.pending_bytes.map(|b| b / (1024 * 1024)),
        immature_mb_per_hour = burn.immature_mb_per_hour,
        merge_write_mb_per_sec = burn.merge_write_mb_per_sec,
        eta_mins = burn.eta_mins,
        disk_free_mb = work_dir
            .and_then(progress::disk_free)
            .map(|b| b / (1024 * 1024)),
        wal_disk_free_mb = progress::wal_disk_free().map(|b| b / (1024 * 1024)),
        "release merges"
    );
}

/// Logs [`log_wait`]'s line for an index every [`LOG_EVERY`] until dropped:
/// a finish's writer merges while the finish replays its build, before the
/// merge wait proper starts (`crate::finish`).
pub struct MergeWatch(tokio::task::JoinHandle<()>);

impl Drop for MergeWatch {
    fn drop(&mut self) {
        self.0.abort();
    }
}

impl MergeWatch {
    /// Watch `index_id` on the writer at `base`, as step `step`. A poll that
    /// fails is logged and the next one tried.
    pub fn start(
        http: reqwest::Client,
        base: String,
        index_id: String,
        step: &'static str,
        work_dir: Option<std::path::PathBuf>,
    ) -> Self {
        use tracing::Instrument;
        Self(tokio::spawn(
            async move {
                let node = Node {
                    http: &http,
                    base: &base,
                };
                let start = tokio::time::Instant::now();
                let mut burndown = Burndown::default();
                loop {
                    match (node.gauges().await, node.splits(&index_id).await) {
                        (Ok(g), Ok(splits)) => {
                            burndown.observe(
                                tokio::time::Instant::now(),
                                immature(&splits).1,
                                g.merged_bytes,
                            );
                            log_wait(
                                &index_id,
                                step,
                                start.elapsed(),
                                &g,
                                &splits,
                                &burndown,
                                work_dir.as_deref(),
                            );
                        }
                        (Err(e), _) | (_, Err(e)) => {
                            tracing::warn!(index = %index_id, error = %format!("{e:#}"), "could not read the writer's merges")
                        }
                    }
                    tokio::time::sleep(LOG_EVERY).await;
                }
            }
            .instrument(tracing::Span::current()),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_burn_down_says_how_fast_the_backlog_goes() {
        let mb = 1024 * 1024;
        let t0 = tokio::time::Instant::now();
        let at = |secs| t0 + Duration::from_secs(secs);
        let mut b = Burndown::default();
        b.observe(at(0), 1000 * mb, Some(0));
        b.observe(at(30), 990 * mb, Some(30 * mb));
        assert_eq!(b.rates(), Burn::default(), "under a minute: no rate yet");
        // 100 MB in 10 minutes: 600 MB an hour, 900 MB left, 90 minutes.
        b.observe(at(600), 900 * mb, Some(600 * mb));
        assert_eq!(
            b.rates(),
            Burn {
                immature_mb_per_hour: Some(600.0),
                merge_write_mb_per_sec: Some(1.0),
                eta_mins: Some(90),
            }
        );
        // The window moves on; a restarted writer's counter starts again.
        b.observe(at(600 + 1800), 300 * mb, Some(5 * mb));
        b.observe(at(600 + 1900), 290 * mb, Some(105 * mb));
        let r = b.rates();
        assert_eq!(r.merge_write_mb_per_sec, Some(1.0));
        assert_eq!(r.immature_mb_per_hour, Some(1156.0));
        assert_eq!(r.eta_mins, Some(15));
        // Nothing merged: no time left to say.
        let mut idle = Burndown::default();
        idle.observe(at(0), 5 * mb, None);
        idle.observe(at(120), 5 * mb, None);
        assert_eq!(idle.rates().eta_mins, None);
        assert_eq!(idle.rates().immature_mb_per_hour, Some(0.0));
        assert_eq!(idle.rates().merge_write_mb_per_sec, None);
    }
}
