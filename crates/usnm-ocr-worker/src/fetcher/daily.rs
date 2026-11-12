//! A host's daily byte budget (#1413, #1415): how many bytes may come from it in
//! one UTC day. The count is kept per day in a [`ByteLedger`], so processes and
//! restarts share it, and a new UTC day starts from zero.
//!
//! A host whose policy sets no `daily_byte_budget` has no [`DailyBytes`], and
//! nothing here runs for it. Bytes are counted when a transfer attempt ends,
//! failed attempts included, so transfers already under way when the budget is
//! reached finish: the overshoot is at most what the policy's `concurrency` of
//! them still had to come.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Bytes taken from one host, by UTC day.
pub trait ByteLedger: Send + Sync + std::fmt::Debug {
    /// Bytes counted for `day` (`YYYY-MM-DD`).
    fn used(&self, day: &str) -> std::io::Result<u64>;
    /// Count `n` more bytes for `day`; returns the day's total. Atomic against
    /// other processes using the same ledger.
    fn add(&self, day: &str, n: u64) -> std::io::Result<u64>;
}

/// A ledger in this process only.
#[derive(Debug, Default)]
pub struct MemoryLedger(Mutex<(String, u64)>);

impl ByteLedger for MemoryLedger {
    fn used(&self, day: &str) -> std::io::Result<u64> {
        let l = self.0.lock().expect("ledger lock");
        Ok(if l.0 == day { l.1 } else { 0 })
    }

    fn add(&self, day: &str, n: u64) -> std::io::Result<u64> {
        let mut l = self.0.lock().expect("ledger lock");
        if l.0 != day {
            *l = (day.to_owned(), 0);
        }
        l.1 = l.1.saturating_add(n);
        Ok(l.1)
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Day {
    day: String,
    bytes: u64,
}

/// A ledger in a file, under an exclusive `flock`. It holds only the current
/// day: the first count of a new day replaces the old one.
#[derive(Debug)]
pub struct FileLedger {
    path: PathBuf,
}

impl FileLedger {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn with<T>(&self, f: impl FnOnce(&mut Day) -> T) -> std::io::Result<T> {
        use std::io::{Read, Seek, Write};
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let mut file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&self.path)?;
        rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::from)?;
        let mut text = String::new();
        file.read_to_string(&mut text)?;
        // A file that can't be read is an error, never a fresh day: a budget that
        // forgets what it spent is no budget.
        let mut day: Day = if text.trim().is_empty() {
            Day::default()
        } else {
            serde_json::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?
        };
        let out = f(&mut day);
        let body = serde_json::to_vec(&day).map_err(std::io::Error::other)?;
        file.set_len(0)?;
        file.rewind()?;
        file.write_all(&body)?;
        file.sync_all()?;
        Ok(out)
    }
}

impl ByteLedger for FileLedger {
    fn used(&self, day: &str) -> std::io::Result<u64> {
        self.with(|d| if d.day == day { d.bytes } else { 0 })
    }

    fn add(&self, day: &str, n: u64) -> std::io::Result<u64> {
        self.with(|d| {
            if d.day != day {
                *d = Day {
                    day: day.to_owned(),
                    bytes: 0,
                };
            }
            d.bytes = d.bytes.saturating_add(n);
            d.bytes
        })
    }
}

/// Where "now" comes from, so a test can cross midnight.
pub type Clock = Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>;

/// A host's budget for the day and what it has used.
pub struct DailyBytes {
    label: String,
    limit: u64,
    ledger: Arc<dyn ByteLedger>,
    clock: Clock,
    /// Bytes seen since the last count went to the ledger.
    pending: AtomicU64,
}

impl std::fmt::Debug for DailyBytes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DailyBytes")
            .field("label", &self.label)
            .field("limit", &self.limit)
            .finish_non_exhaustive()
    }
}

impl DailyBytes {
    pub fn new(label: &str, limit: u64, ledger: Arc<dyn ByteLedger>, clock: Clock) -> Self {
        Self {
            label: label.to_owned(),
            limit,
            ledger,
            clock,
            pending: AtomicU64::new(0),
        }
    }

    fn today(&self) -> String {
        (self.clock)().format("%Y-%m-%d").to_string()
    }

    /// `n` bytes came over the network; they go in the ledger at the next [`flush`](Self::flush).
    pub fn note(&self, n: u64) {
        self.pending.fetch_add(n, Ordering::Relaxed);
    }

    /// Put the bytes noted so far in the ledger, under today's date.
    pub fn flush(&self) {
        let n = self.pending.swap(0, Ordering::Relaxed);
        if n == 0 {
            return;
        }
        if let Err(e) = self.ledger.add(&self.today(), n) {
            tracing::warn!(host = %self.label, bytes = n, error = %e, "can't record bytes against the daily budget");
        }
    }

    /// Why nothing more goes to the host today, if that is so: the budget is
    /// spent, or the ledger can't say (a budget that can't be read isn't taken as unspent).
    pub fn spent(&self) -> Option<String> {
        self.flush();
        let day = self.today();
        match self.ledger.used(&day) {
            Ok(used) if used >= self.limit => Some(format!(
                "{} daily byte budget spent ({used} of {} bytes on {day} UTC); nothing more from it until the next UTC day",
                self.label, self.limit
            )),
            Ok(_) => None,
            Err(e) => Some(format!(
                "{} daily byte budget unreadable ({e}); nothing more from it until it can be read",
                self.label
            )),
        }
    }
}
