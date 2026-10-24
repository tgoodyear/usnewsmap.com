//! The pacer: a minimum gap between requests to one of LoC's servers.

use std::sync::Mutex;
use std::time::Duration;

use tokio::time::Instant;

/// A minimum gap between requests, shared by every task holding it. Each
/// caller reserves the next free slot, so N concurrent callers are spaced
/// `gap` apart, not sent together.
#[derive(Debug)]
pub struct Pacer {
    gap: Duration,
    next: Mutex<Option<Instant>>,
    /// Set by [`Pacer::hold`]: no request before this.
    held: Mutex<Option<Instant>>,
}

impl Pacer {
    pub fn new(gap: Duration) -> Self {
        Self {
            gap,
            next: Mutex::new(None),
            held: Mutex::new(None),
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
    /// is set checks it on waking and takes a new slot after it.
    pub async fn wait(&self) {
        loop {
            tokio::time::sleep_until(self.reserve()).await;
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
