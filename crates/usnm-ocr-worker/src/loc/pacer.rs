//! The pacer: a minimum gap between requests to one of LoC's servers.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::time::Instant;

use super::global::GlobalPacer;

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
    global: Option<Arc<GlobalPacer>>,
}

impl Pacer {
    pub fn new(gap: Duration) -> Self {
        Self {
            gap,
            next: Mutex::new(None),
            held: Mutex::new(None),
            global: None,
        }
    }

    /// The same, inside the host's global budget.
    pub fn with_global(gap: Duration, global: Option<Arc<GlobalPacer>>) -> Self {
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
    /// global budget, the slot is admitted by both: the global slot is taken
    /// at or after this pacer's, and this pacer's next slot counts from the
    /// moment the request actually goes, so both gaps hold.
    pub async fn wait(&self) {
        loop {
            let slot = self.reserve();
            if let Some(g) = &self.global {
                let ahead = slot.saturating_duration_since(Instant::now());
                let not_before = super::global::epoch_ms_in(ahead);
                g.admit(not_before).await;
                let sent = Instant::now();
                let mut next = self.next.lock().expect("pacer lock");
                *next = Some(next.map_or(sent + self.gap, |n| n.max(sent + self.gap)));
            } else {
                tokio::time::sleep_until(slot).await;
            }
            let held = *self.held.lock().expect("pacer lock");
            if held.is_none_or(|h| h <= Instant::now()) {
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
        g.hold(Duration::from_millis(400));
        let t0 = std::time::Instant::now();
        p.wait().await;
        let first = t0.elapsed();
        assert!(first >= Duration::from_millis(390), "{first:?}");
        p.wait().await;
        let gap = t0.elapsed() - first;
        assert!(gap >= Duration::from_millis(290), "{gap:?}");
    }
}
