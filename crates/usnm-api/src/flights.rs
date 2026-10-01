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
//! Searches also take one of `compute_concurrency` slots for as long as
//! they run, which bounds how many can outlive their requests.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use futures::future::{BoxFuture, Shared};
use tokio::sync::Semaphore;

use crate::error::ApiError;

/// The result of one computation, shared by everyone waiting on it.
pub(crate) type Flight = Shared<BoxFuture<'static, Result<Arc<Vec<u8>>, ApiError>>>;

pub struct Flights {
    running: Mutex<HashMap<String, Flight>>,
    /// Slots for search computations.
    pub(crate) slots: Arc<Semaphore>,
}

impl Flights {
    pub(crate) fn new(search_slots: usize) -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(search_slots.max(1))),
        }
    }

    /// The map, even if a thread panicked while holding it: every change
    /// to it is a single insert or remove, so it is never half-updated.
    pub(crate) fn lock(&self) -> MutexGuard<'_, HashMap<String, Flight>> {
        self.running.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The computation running for `key`, if any.
    pub(crate) fn get(&self, key: &str) -> Option<Flight> {
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
