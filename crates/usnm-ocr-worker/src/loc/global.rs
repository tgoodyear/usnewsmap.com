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
    /// No request before this (a `Retry-After` hold), in ms since the epoch.
    #[serde(default)]
    pub hold_until_ms: u64,
    /// Bumped by every hold and slow-down, so a caller that reserved a slot
    /// before one takes a new slot instead of sending in the old one.
    #[serde(default)]
    pub generation: u64,
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

/// Milliseconds since the epoch, `ahead` from now.
pub fn epoch_ms_in(ahead: Duration) -> u64 {
    now_ms() + u64::try_from(ahead.as_millis()).unwrap_or(u64::MAX)
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
    // A new (empty) file starts from now. One that can't be read was cut off
    // or damaged: never let that undo a hold or a slow-down, so it starts
    // slowed, held for a slow gap, until an operator resets it.
    let mut state: State = if text.trim().is_empty() {
        State::default()
    } else {
        serde_json::from_str(&text).unwrap_or_else(|_| {
            let now = now_ms();
            State {
                next_ms: now + u64::try_from(SLOW_GAP.as_millis()).unwrap_or(u64::MAX),
                slowed: Some(Slowed {
                    at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                    why: "the pacer's state file couldn't be read".into(),
                }),
                sent: 0,
                hold_until_ms: now,
                generation: 1,
            }
        })
    };
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

    /// Reserve the next slot at or after `not_before_ms`; returns it and the
    /// generation it was taken in.
    pub fn reserve_at(&self, not_before_ms: u64) -> std::io::Result<(u64, u64)> {
        let (normal, slow) = (self.normal, self.slow);
        with_state(&self.path, |s| {
            let at = s
                .next_ms
                .max(now_ms())
                .max(s.hold_until_ms)
                .max(not_before_ms);
            let gap = if s.slowed.is_some() { slow } else { normal };
            s.next_ms = at + u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
            s.sent += 1;
            (at, s.generation)
        })
    }

    #[cfg(test)]
    fn reserve(&self) -> std::io::Result<u64> {
        self.reserve_at(0).map(|(at, _)| at)
    }

    /// Whether a slot taken in `generation` still stands: no hold or
    /// slow-down came since, and no hold is running.
    fn still_good(&self, generation: u64) -> bool {
        with_state(&self.path, |s| {
            s.generation == generation && s.hold_until_ms <= now_ms()
        })
        .unwrap_or(false)
    }

    /// Wait for a slot in the host's budget at or after `not_before_ms`
    /// (ms since the epoch); returns when it was. A hold or slow-down that
    /// lands while waiting sends the caller back for a new slot.
    pub async fn admit(&self, not_before_ms: u64) -> u64 {
        loop {
            let me = self.clone();
            let (at, generation) = match tokio::task::spawn_blocking(move || {
                me.reserve_at(not_before_ms)
            })
            .await
            {
                Ok(Ok(r)) => r,
                other => {
                    // Without the file there is no shared budget: wait the slow
                    // gap rather than send unpaced.
                    if let Ok(Err(e)) = other {
                        tracing::warn!(path = %self.path.display(), error = %e, "the global LoC pacer's file failed; waiting the slow gap");
                    }
                    tokio::time::sleep(self.slow).await;
                    return now_ms();
                }
            };
            let wait = at.saturating_sub(now_ms());
            if wait > 0 {
                tokio::time::sleep(Duration::from_millis(wait)).await;
            }
            let me = self.clone();
            if tokio::task::spawn_blocking(move || me.still_good(generation))
                .await
                .unwrap_or(false)
            {
                return at;
            }
        }
    }

    /// Wait for the next slot in the host's budget.
    pub async fn wait(&self) {
        self.admit(0).await;
    }

    /// Count `n` more requests already sent (redirect hops a client followed
    /// on its own): the next slots move on by `n` gaps.
    pub fn charge(&self, n: u64) {
        let (normal, slow) = (self.normal, self.slow);
        let res = with_state(&self.path, |s| {
            let gap = if s.slowed.is_some() { slow } else { normal };
            let gap = u64::try_from(gap.as_millis()).unwrap_or(u64::MAX);
            s.next_ms = s.next_ms.max(now_ms()) + n * gap;
            s.sent += n;
        });
        if let Err(e) = res {
            tracing::warn!(error = %e, "can't charge the global LoC pacer");
        }
    }

    /// Hold every process for at least `d` from now.
    pub fn hold(&self, d: Duration) {
        let until = now_ms() + u64::try_from(d.as_millis()).unwrap_or(u64::MAX);
        if let Err(e) = with_state(&self.path, |s| {
            s.next_ms = s.next_ms.max(until);
            s.hold_until_ms = s.hold_until_ms.max(until);
            s.generation += 1;
        }) {
            tracing::warn!(error = %e, "can't record a hold in the global LoC pacer");
        }
    }

    /// LoC pushed back: every process drops to the slow rate, for good.
    pub fn slow_down(&self, why: &str) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let res = with_state(&self.path, |s| {
            if s.slowed.is_none() {
                s.generation += 1;
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
    fn a_hold_or_slow_down_invalidates_slots_already_taken() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        let a = GlobalPacer::with_gaps(
            p.clone(),
            Duration::from_millis(10),
            Duration::from_millis(50),
        );
        let (_, g) = a.reserve_at(0).unwrap();
        assert!(a.still_good(g));
        a.slow_down("HTTP 429");
        assert!(!a.still_good(g));
        let (_, g) = a.reserve_at(0).unwrap();
        a.hold(Duration::from_secs(60));
        assert!(!a.still_good(g), "a hold after the slot was taken");
    }

    #[test]
    fn a_damaged_file_starts_slowed_and_an_empty_one_does_not() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        std::fs::write(&p, b"{\"next_ms\": 12").unwrap();
        let a = GlobalPacer::new(p.clone());
        assert!(a.state().unwrap().slowed.is_some());
        std::fs::write(&p, b"").unwrap();
        assert!(a.state().unwrap().slowed.is_none());
    }

    #[test]
    fn not_before_and_charges_move_slots() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        let a = GlobalPacer::with_gaps(p, Duration::from_millis(1000), Duration::from_millis(5000));
        let later = now_ms() + 30_000;
        let (at, _) = a.reserve_at(later).unwrap();
        assert!(at >= later);
        a.charge(2);
        let (next, _) = a.reserve_at(0).unwrap();
        assert!(next >= at + 3000, "{next} {at}");
    }

    #[test]
    fn the_default_gaps_are_45_and_10_a_minute() {
        assert!(60_000 / NORMAL_GAP.as_millis() <= 45);
        assert_eq!(60_000 / SLOW_GAP.as_millis(), 10);
    }
}
