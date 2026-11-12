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

/// A ledger in a file. It holds only the current day: the first count of a new
/// day replaces the old one. Every operation holds an exclusive `flock` on
/// `<file>.lock`, and a count is written to a temporary file and renamed into
/// place, so a crash leaves the old count or the new one, never a cut-off file.
#[derive(Debug)]
pub struct FileLedger {
    path: PathBuf,
}

impl FileLedger {
    pub fn new(path: PathBuf) -> Self {
        Self { path }
    }

    fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self.path.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    }

    /// Runs `f` with the ledger locked.
    fn locked<T>(&self, f: impl FnOnce() -> std::io::Result<T>) -> std::io::Result<T> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.sibling(".lock"))?;
        rustix::fs::flock(&lock, rustix::fs::FlockOperation::LockExclusive)
            .map_err(std::io::Error::from)?;
        f()
    }

    /// The day on file. No file is a fresh ledger; a file that is empty or can't be
    /// read is damaged, and is an error, never a fresh day: a budget that forgot
    /// what it spent is no budget.
    fn read(&self) -> std::io::Result<Day> {
        match std::fs::read_to_string(&self.path) {
            Ok(text) => serde_json::from_str(&text)
                .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e)),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Day::default()),
            Err(e) => Err(e),
        }
    }

    fn write(&self, day: &Day) -> std::io::Result<()> {
        use std::io::Write;
        let tmp = self.sibling(".tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec(day).map_err(std::io::Error::other)?)?;
        file.sync_all()?;
        std::fs::rename(&tmp, &self.path)
    }
}

impl ByteLedger for FileLedger {
    fn used(&self, day: &str) -> std::io::Result<u64> {
        self.locked(|| {
            let d = self.read()?;
            Ok(if d.day == day { d.bytes } else { 0 })
        })
    }

    fn add(&self, day: &str, n: u64) -> std::io::Result<u64> {
        self.locked(|| {
            let mut d = self.read()?;
            if d.day != day {
                d = Day {
                    day: day.to_owned(),
                    bytes: 0,
                };
            }
            d.bytes = d.bytes.saturating_add(n);
            self.write(&d)?;
            Ok(d.bytes)
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
            // Kept, to try again; until it is recorded the host counts as spent.
            self.pending.fetch_add(n, Ordering::Relaxed);
            tracing::warn!(host = %self.label, bytes = n, error = %e, "can't record bytes against the daily budget");
        }
    }

    /// Why nothing more goes to the host today, if that is so: the budget is
    /// spent, or the ledger can't say (a budget that can't be read isn't taken as unspent).
    pub fn spent(&self) -> Option<String> {
        self.flush();
        if self.pending.load(Ordering::Relaxed) > 0 {
            return Some(format!(
                "{} daily byte budget can't be updated; nothing more from it until the count is recorded",
                self.label
            ));
        }
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
