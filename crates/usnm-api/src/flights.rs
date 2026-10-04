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
//! at once. When every slot is taken it joins a first-come, first-served
//! queue of at most `search_queue` searches and keeps its place there for
//! as long as someone keeps asking about it; a `202` says how many searches
//! are ahead of it. A search that finds the queue full is refused at once
//! as busy. A computation that nobody has waited on for `abandon_after`
//! (15 s) is cancelled, so a visitor who changes the search or leaves frees
//! its slot or its place in the queue. The warm-up has a slot of its own.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use futures::future::{BoxFuture, Shared};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::time::Instant;

use crate::error::ApiError;

/// The result of one computation, shared by everyone waiting on it.
pub(crate) type Flight = Shared<BoxFuture<'static, Result<Arc<Vec<u8>>, ApiError>>>;

/// A computation in flight and who is waiting on it.
#[derive(Clone)]
pub(crate) struct Entry {
    pub(crate) flight: Flight,
    pub(crate) interest: Arc<Interest>,
    pub(crate) spot: Arc<Spot>,
}

/// Where a search computation is: waiting in the queue for a slot (with
/// its ticket), or not.
#[derive(Default)]
pub(crate) struct Spot(Mutex<Option<u64>>);

impl Spot {
    fn ticket(&self) -> MutexGuard<'_, Option<u64>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// How many requests wait on a computation, and since when none has.
pub(crate) struct Interest(Mutex<Waiting>);

struct Waiting {
    waiters: usize,
    last_left: Instant,
    /// Abandoned: no request may wait on it any more.
    closed: bool,
}

impl Interest {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self(Mutex::new(Waiting {
            waiters: 0,
            last_left: Instant::now(),
            closed: false,
        })))
    }

    fn lock(&self) -> MutexGuard<'_, Waiting> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Count a request as waiting until the guard drops; `None` if the
    /// computation has been abandoned (the request should ask again).
    pub(crate) fn watch(self: &Arc<Self>) -> Option<Watching> {
        let mut w = self.lock();
        if w.closed {
            return None;
        }
        w.waiters += 1;
        Some(Watching(self.clone()))
    }

    /// If nobody has waited on the computation for `after`, close it to new
    /// waiters and say so. One lock covers the check and the close, so a
    /// request can't start waiting on a computation that is being cancelled.
    pub(crate) fn abandon_if_idle(&self, after: Duration) -> bool {
        let mut w = self.lock();
        if w.waiters == 0 && w.last_left.elapsed() >= after {
            w.closed = true;
        }
        w.closed
    }
}

/// One request waiting on a computation (see [`Interest::watch`]).
pub(crate) struct Watching(Arc<Interest>);

impl Drop for Watching {
    fn drop(&mut self) {
        let mut w = self.0.lock();
        w.waiters -= 1;
        w.last_left = Instant::now();
    }
}

pub struct Flights {
    running: Mutex<HashMap<String, Entry>>,
    /// Slots for visitors' search computations. Tokio's semaphore hands
    /// freed permits to its waiters in the order they asked.
    slots: Arc<Semaphore>,
    /// The warm-up's own slot, so visitors' searches can't starve it (it
    /// runs one query at a time anyway).
    pub(crate) warm_up_slot: Arc<Semaphore>,
    /// Tickets of the searches waiting for a slot; lower is earlier.
    queue: Mutex<BTreeSet<u64>>,
    next_ticket: AtomicU64,
    max_queue: usize,
}

impl Flights {
    pub(crate) fn new(search_slots: usize, max_queue: usize) -> Self {
        Self {
            running: Mutex::new(HashMap::new()),
            slots: Arc::new(Semaphore::new(search_slots.max(1))),
            warm_up_slot: Arc::new(Semaphore::new(1)),
            queue: Mutex::new(BTreeSet::new()),
            next_ticket: AtomicU64::new(0),
            max_queue,
        }
    }

    fn queue(&self) -> MutexGuard<'_, BTreeSet<u64>> {
        self.queue.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// A slot for a visitor's search: at once if one is free, else after
    /// the searches queued before it, holding `spot`'s place in the queue
    /// meanwhile (cancelling the wait gives the place up). Busy when the
    /// queue is full.
    pub(crate) async fn slot(&self, spot: &Spot) -> Result<OwnedSemaphorePermit, ApiError> {
        // Never succeeds while others wait: freed permits go to waiters.
        if let Ok(permit) = self.slots.clone().try_acquire_owned() {
            return Ok(permit);
        }
        let ticket = {
            let mut queue = self.queue();
            if queue.len() >= self.max_queue {
                return Err(ApiError::Busy);
            }
            let ticket = self.next_ticket.fetch_add(1, Ordering::SeqCst);
            queue.insert(ticket);
            *spot.ticket() = Some(ticket);
            ticket
        };
        let place = Place {
            flights: self,
            spot,
            ticket,
        };
        let permit = self.slots.clone().acquire_owned().await;
        drop(place);
        // The semaphore is never closed.
        permit.map_err(|e| ApiError::Backend(e.to_string()))
    }

    /// How many searches are ahead of this one in the queue for a slot;
    /// `None` once it has left the queue (or never joined it).
    pub(crate) fn ahead(&self, spot: &Spot) -> Option<usize> {
        let ticket = (*spot.ticket())?;
        Some(self.queue().range(..ticket).count())
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

    /// Searches waiting for a slot now.
    pub fn queued(&self) -> usize {
        self.queue().len()
    }
}

/// A search's place in the queue, given up when it gets its slot or stops
/// waiting.
struct Place<'a> {
    flights: &'a Flights,
    spot: &'a Spot,
    ticket: u64,
}

impl Drop for Place<'_> {
    fn drop(&mut self) {
        self.flights.queue().remove(&self.ticket);
        *self.spot.ticket() = None;
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
