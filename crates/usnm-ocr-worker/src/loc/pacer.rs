//! The pacer: a minimum gap between requests to one of LoC's servers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use crate::fetcher::Budget;

/// A minimum gap between requests, shared by every task holding it. Each
/// caller reserves the next free slot, so N concurrent callers are spaced
/// `gap` apart, not sent together.
#[derive(Debug)]
pub struct Pacer {
    gap: Duration,
    next: Mutex<Option<Instant>>,
    /// Set by [`Pacer::hold`]: no request before this.
    held: Mutex<Option<Instant>>,
    /// The host's budget across every LoC server and process, waited for
    /// after this pacer's own slot (#962).
    global: Option<Arc<dyn Budget>>,
    /// With a global budget, one caller at a time goes through both waits,
    /// so a slot this pacer gave out can't go stale while another caller
    /// waits for the host's budget (tokio's mutex is fair: callers keep
    /// their order).
    admission: tokio::sync::Mutex<()>,
}

impl Pacer {
    pub fn new(gap: Duration) -> Self {
        Self {
            gap,
            next: Mutex::new(None),
            held: Mutex::new(None),
            global: None,
            admission: tokio::sync::Mutex::new(()),
        }
    }

    /// The same, inside the host's global budget.
    pub fn with_global<B: Budget + 'static>(gap: Duration, global: Option<Arc<B>>) -> Self {
        Self::with_budget(gap, global.map(|g| g as Arc<dyn Budget>))
    }

    /// The same, with any budget.
    pub fn with_budget(gap: Duration, global: Option<Arc<dyn Budget>>) -> Self {
        Self {
            global,
            ..Self::new(gap)
        }
    }

    fn reserve(&self) -> Instant {
        let mut next = self.next.lock().expect("pacer lock");
        let now = Instant::now();
        let at = next.map_or(now, |n| n.max(now));
        *next = Some(at + self.gap);
        at
    }

    /// Wait for this caller's slot. A caller already waiting when a hold
    /// is set checks it on waking and takes a new slot after it. With a
    /// global budget, the caller waits for this pacer's slot first and only
    /// then takes the host's next free slot, so one slow pacer (the bulk
    /// archives' minute, or a server's hold) never reserves the host's budget
    /// ahead and stalls the others; this pacer's next slot then counts from
    /// the moment the request actually goes, so both gaps hold.
    pub async fn wait(&self) {
        let Some(g) = &self.global else {
            loop {
                tokio::time::sleep_until(self.reserve()).await;
                if self.held_until().is_none() {
                    return;
                }
            }
        };
        let _one = self.admission.lock().await;
        loop {
            tokio::time::sleep_until(self.reserve()).await;
            if self.held_until().is_some() {
                continue;
            }
            g.admit(0).await;
            let sent = Instant::now();
            {
                let mut next = self.next.lock().expect("pacer lock");
                *next = Some(next.map_or(sent + self.gap, |n| n.max(sent + self.gap)));
            }
            if self.held_until().is_none() {
                return;
            }
        }
    }

    /// Until when callers are held, if a hold is still running.
    pub fn held_until(&self) -> Option<Instant> {
        let held = *self.held.lock().expect("pacer lock");
        held.filter(|h| *h > Instant::now())
    }

    /// Hold every caller for at least `d` from now (the server said to wait).
    pub fn hold(&self, d: Duration) {
        let until = Instant::now() + d;
        let mut held = self.held.lock().expect("pacer lock");
        *held = Some(held.map_or(until, |h| h.max(until)));
        let mut next = self.next.lock().expect("pacer lock");
        *next = Some(next.map_or(until, |n| n.max(until)));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::loc::global::GlobalPacer;

    /// With a global budget held past this pacer's slot, the next request
    /// still waits this pacer's whole gap after the first actually went.
    #[tokio::test]
    async fn both_gaps_hold_after_a_global_hold() {
        let dir = tempfile::tempdir().unwrap();
        let g = Arc::new(GlobalPacer::with_gaps(
            dir.path().join("p.json"),
            Duration::from_millis(10),
            Duration::from_millis(50),
        ));
        let p = Pacer::with_global(Duration::from_millis(300), Some(Arc::clone(&g)));
        let t0 = std::time::Instant::now();
        g.hold(Duration::from_millis(400));
        p.wait().await;
        let first = t0.elapsed();
        assert!(first >= Duration::from_millis(390), "{first:?}");
        p.wait().await;
        let gap = t0.elapsed() - first;
        // Measured after each wait returns, so a busy test runner can shave a little off.
        assert!(gap >= Duration::from_millis(250), "{gap:?}");
    }

    /// Concurrent callers behind a held global budget still leave this
    /// pacer's gap between them once it opens.
    #[tokio::test]
    async fn concurrent_callers_keep_the_gap_after_a_global_hold() {
        let dir = tempfile::tempdir().unwrap();
        let g = Arc::new(GlobalPacer::with_gaps(
            dir.path().join("p.json"),
            Duration::from_millis(10),
            Duration::from_millis(50),
        ));
        let p = Arc::new(Pacer::with_global(
            Duration::from_millis(200),
            Some(Arc::clone(&g)),
        ));
        let t0 = std::time::Instant::now();
        g.hold(Duration::from_millis(500));
        let tasks: Vec<_> = (0..3)
            .map(|_| {
                let p = Arc::clone(&p);
                tokio::spawn(async move {
                    p.wait().await;
                    t0.elapsed()
                })
            })
            .collect();
        let mut at = Vec::new();
        for t in tasks {
            at.push(t.await.unwrap());
        }
        at.sort();
        assert!(at[0] >= Duration::from_millis(490), "{at:?}");
        assert!(at[1] - at[0] >= Duration::from_millis(150), "{at:?}");
        assert!(at[2] - at[1] >= Duration::from_millis(150), "{at:?}");
    }
}
