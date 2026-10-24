//! The global LoC pacer (#962): one budget for every LoC request on the
//! host, whichever process sends it.
//!
//! The maintainer set LoC's whole budget at 45 requests a minute (24 October
//! 2026): OCR's scans, the quality pass's and the bulk OCR archives together.
//! Each process's own [`super::Pacer`]s still space requests to one server;
//! this one spaces every request to any LoC server, across processes, through
//! a small state file under an exclusive `flock`. A caller takes the next
//! free slot and writes the slot after it, so N processes share one rate.
//!
//! A 429, a 403 or a challenge page from LoC switches the file to the slow
//! rate (10 a minute, LoC's own guideline on loc.gov/legal) for every
//! process, and it stays slow until an operator resets it
//! (`usnm-ocr-worker loc-pacer --file <f> --reset`): the switch is never
//! undone automatically. Holds (`Retry-After`) apply to every process too.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 45 requests a minute.
pub const NORMAL_GAP: Duration = Duration::from_millis(1334);
/// 10 requests a minute.
pub const SLOW_GAP: Duration = Duration::from_millis(6000);

/// What the state file holds.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct State {
    /// The next free slot, in ms since the Unix epoch.
    pub next_ms: u64,
    /// Set once LoC pushed back: the slow rate applies from then on.
    #[serde(default)]
    pub slowed: Option<Slowed>,
    /// Requests let through, for the measured rate.
    #[serde(default)]
    pub sent: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slowed {
    /// RFC 3339.
    pub at: String,
    pub why: String,
}

#[derive(Debug, Clone)]
pub struct GlobalPacer {
    path: PathBuf,
    normal: Duration,
    slow: Duration,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// Runs `f` on the state under the file's exclusive lock and writes it back.
fn with_state<T>(path: &Path, f: impl FnOnce(&mut State) -> T) -> std::io::Result<T> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut file: File = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)?;
    rustix::fs::flock(&file, rustix::fs::FlockOperation::LockExclusive)
        .map_err(std::io::Error::from)?;
    let mut text = String::new();
    file.read_to_string(&mut text)?;
    // An empty or unreadable file starts again from now; never faster than
    // asked, since the gap still applies to the next slot.
    let mut state: State = serde_json::from_str(&text).unwrap_or_default();
    let out = f(&mut state);
    let body = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&body)?;
    file.sync_data()?;
    // The lock goes with the file.
    Ok(out)
}

impl GlobalPacer {
    pub fn new(path: PathBuf) -> Self {
        Self::with_gaps(path, NORMAL_GAP, SLOW_GAP)
    }

    pub fn with_gaps(path: PathBuf, normal: Duration, slow: Duration) -> Self {
        Self { path, normal, slow }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reserve the next slot; the caller waits until it.
    fn reserve(&self) -> std::io::Result<u64> {
        let (normal, slow) = (self.normal, self.slow);
        with_state(&self.path, |s| {
            let now = now_ms();
            let at = s.next_ms.max(now);
            let gap = if s.slowed.is_some() { slow } else { normal };
            s.next_ms = at + u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
            s.sent += 1;
            at
        })
    }

    /// Wait for this request's slot in the host's budget.
    pub async fn wait(&self) {
        let me = self.clone();
        let at = match tokio::task::spawn_blocking(move || me.reserve()).await {
            Ok(Ok(at)) => at,
            Ok(Err(e)) => {
                // Without the file there is no shared budget: wait the slow
                // gap rather than send unpaced.
                tracing::warn!(path = %self.path.display(), error = %e, "the global LoC pacer's file failed; waiting the slow gap");
                tokio::time::sleep(self.slow).await;
                return;
            }
            Err(_) => {
                tokio::time::sleep(self.slow).await;
                return;
            }
        };
        let wait = at.saturating_sub(now_ms());
        if wait > 0 {
            tokio::time::sleep(Duration::from_millis(wait)).await;
        }
    }

    /// Hold every process for at least `d` from now.
    pub fn hold(&self, d: Duration) {
        let until = now_ms() + u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        if let Err(e) = with_state(&self.path, |s| s.next_ms = s.next_ms.max(until)) {
            tracing::warn!(error = %e, "can't record a hold in the global LoC pacer");
        }
    }

    /// LoC pushed back: every process drops to the slow rate, for good.
    pub fn slow_down(&self, why: &str) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let res = with_state(&self.path, |s| {
            if s.slowed.is_none() {
                s.slowed = Some(Slowed {
                    at: at.clone(),
                    why: why.to_owned(),
                });
                true
            } else {
                false
            }
        });
        match res {
            Ok(true) => tracing::error!(
                why,
                path = %self.path.display(),
                "LoC pushed back: the global pacer drops to 10 requests a minute until reset"
            ),
            Ok(false) => {}
            Err(e) => {
                tracing::warn!(error = %e, "can't record the slow-down in the global LoC pacer")
            }
        }
    }

    /// The state as it is now.
    pub fn state(&self) -> std::io::Result<State> {
        with_state(&self.path, |s| s.clone())
    }

    /// Back to the normal rate (an operator's decision).
    pub fn reset(&self) -> std::io::Result<()> {
        with_state(&self.path, |s| s.slowed = None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slots_are_a_gap_apart_across_handles() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        let a = GlobalPacer::with_gaps(
            p.clone(),
            Duration::from_millis(1000),
            Duration::from_millis(5000),
        );
        let b = GlobalPacer::with_gaps(
            p.clone(),
            Duration::from_millis(1000),
            Duration::from_millis(5000),
        );
        let s1 = a.reserve().unwrap();
        let s2 = b.reserve().unwrap();
        let s3 = a.reserve().unwrap();
        assert_eq!(s2 - s1, 1000);
        assert_eq!(s3 - s2, 1000);
        assert_eq!(a.state().unwrap().sent, 3);
    }

    #[test]
    fn a_push_back_slows_every_handle_until_reset() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        let a = GlobalPacer::with_gaps(
            p.clone(),
            Duration::from_millis(1000),
            Duration::from_millis(5000),
        );
        let b = GlobalPacer::with_gaps(
            p.clone(),
            Duration::from_millis(1000),
            Duration::from_millis(5000),
        );
        a.slow_down("HTTP 429");
        let s1 = b.reserve().unwrap();
        let s2 = b.reserve().unwrap();
        assert_eq!(s2 - s1, 5000);
        let st = a.state().unwrap();
        assert_eq!(st.slowed.unwrap().why, "HTTP 429");
        // A second push-back keeps the first reason.
        b.slow_down("HTTP 403");
        assert_eq!(a.state().unwrap().slowed.unwrap().why, "HTTP 429");
        a.reset().unwrap();
        let s3 = b.reserve().unwrap();
        let s4 = b.reserve().unwrap();
        assert_eq!(s4 - s3, 1000);
    }

    #[test]
    fn a_hold_moves_the_next_slot() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        let a = GlobalPacer::with_gaps(p, Duration::from_millis(10), Duration::from_millis(50));
        let before = now_ms();
        a.hold(Duration::from_secs(60));
        let s = a.reserve().unwrap();
        assert!(s >= before + 60_000);
    }

    #[test]
    fn the_default_gaps_are_45_and_10_a_minute() {
        assert!(60_000 / NORMAL_GAP.as_millis() <= 45);
        assert_eq!(60_000 / SLOW_GAP.as_millis(), 10);
    }
}
