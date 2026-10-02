//! Response computations in flight (06 §6.5).
//!
//! A response the in-process cache doesn't hold is computed by a task of
//! its own, not by the request that asked for it, so a slow search keeps
//! going after its visitor stops waiting (the visitor gets `202 Accepted`
//! and asks again). Identical requests share one computation: the first
//! registers it here under its cache key, and the others wait on the same
//! result. When the computation ends, its task stores a successful body in
//! the in-process cache and only then removes the entry, so a request finds
//! either the computation or its result, never neither. Errors, timeouts
//! included, are handed to whoever is waiting and never cached.
//!
//! A search takes one of `compute_concurrency` slots for as long as it
//! runs, after its persistent-cache read misses, which bounds how many run
//! at once. A computation that nobody has waited on for `abandon_after` (15 s)
//! is cancelled, so a visitor who changes the search or leaves frees its
//! slot. The warm-up has a slot of its own.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::future::{BoxFuture, Shared};
use tokio::sync::Semaphore;
use tokio::time::Instant;

use crate::error::ApiError;

/// The result of one computation, shared by everyone waiting on it.
pub(crate) type Flight = Shared<BoxFuture<'static, Result<Arc<Vec<u8>>, ApiError>>>;

/// A computation in flight and who is waiting on it.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) flight: Flight,
    pub(crate) interest: Arc<Interest>,
}

/// How many requests wait on a computation, and since when none has.
pub(crate) struct Interest {
    waiters: AtomicUsize,
    last_left: Mutex<Instant>,
}

impl Interest {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            waiters: AtomicUsize::new(0),
            last_left: Mutex::new(Instant::now()),
        })
    }

    /// Count a request as waiting until the guard drops.
    pub(crate) fn watch(self: &Arc<Self>) -> Watching {
        self.waiters.fetch_add(1, Ordering::SeqCst);
        Watching(self.clone())
    }

    /// Whether nobody has waited on the computation for `after`.
    pub(crate) fn abandoned(&self, after: Duration) -> bool {
        self.waiters.load(Ordering::SeqCst) == 0
            && self
                .last_left
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .elapsed()
                >= after
    }
}

/// One request waiting on a computation (see [`Interest::watch`]).
pub(crate) struct Watching(Arc<Interest>);

impl Drop for Watching {
    fn drop(&mut self) {
        *self.0.last_left.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
        self.0.waiters.fetch_sub(1, Ordering::SeqCst);
    }
}

pub struct Flights {
    running: Mutex<HashMap<String, Entry>>,
    /// Slots for visitors' search computations.
    pub(crate) slots: Arc<Semaphore>,
    /// The warm-up's own slot, so visitors' searches can't starve it (it
    /// runs one query at a time anyway).
    pub(crate) warm_up_slot: Arc<Semaphore>,
}

impl Flights {
    pub(crate) fn new(search_slots: usize) -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(search_slots.max(1))),
            warm_up_slot: Arc::new(Semaphore::new(1)),
        }
    }

    /// The map, even if a thread panicked while holding it: every change
    /// to it is a single insert or remove, so it is never half-updated.
    pub(crate) fn lock(&self) -> MutexGuard<'_, HashMap<String, Entry>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The computation running for `key`, if any.
    pub(crate) fn get(&self, key: &str) -> Option<Entry> {
        self.lock().get(key).cloned()
    }

    /// Computations running now.
    pub fn len(&self) -> usize {
        self.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Search slots free now.
    pub fn free_slots(&self) -> usize {
        self.slots.available_permits()
    }
}

/// Removes a computation's entry when its task ends, however it ends (a
/// panic included), so a key can never be stuck on a dead computation.
pub(crate) struct Landing {
    pub(crate) state: Arc<crate::AppState>,
    pub(crate) key: String,
}

impl Drop for Landing {
    fn drop(&mut self) {
        self.state.flights.lock().remove(&self.key);
    }
}
