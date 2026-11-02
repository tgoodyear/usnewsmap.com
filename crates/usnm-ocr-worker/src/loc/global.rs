//! The global LoC pacer (#962): one budget for every LoC request on the
//! host, whichever process sends it.
//!
//! The maintainer set LoC's whole budget at 20 requests a minute (2 November
//! 2026, after LoC sent challenge pages at about 30; 45 from 24 October): OCR's scans, the quality pass's and the
//! bulk OCR archives together. LoC publishes 150 a minute for its storage
//! services and 150 for its image services, each with a one-hour block past
//! it, and may block below those when it is under load.
//! Each process's own [`super::Pacer`]s still space requests to one server;
//! this one spaces every request to any LoC server, across processes, through
//! a small state file under an exclusive `flock`. A caller takes the next
//! free slot and writes the slot after it, so N processes share one rate.
//!
//! A 429, a 403 or a challenge page from LoC holds the budget past the block
//! and switches the file to the slow rate (12 a minute) for every process for
//! [`SLOW_FOR`] (24 hours); another push-back meanwhile starts the 24 hours
//! again. Once they pass, the first caller lifts it and logs that. An operator
//! can lift it sooner (`usnm-ocr-worker loc-pacer --file <f> --reset`). A file
//! slowed by an older release, with no end time, stays slow until reset.
//! Holds (`Retry-After`) apply to every process too.
//!
//! This file format is the first implementation of the fetcher's
//! [`Budget`](crate::fetcher::Budget), and any host policy can have one, so
//! "LoC" in what follows is the case it was built for.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// 20 requests a minute.
pub const NORMAL_GAP: Duration = Duration::from_millis(3000);
/// 12 requests a minute.
pub const SLOW_GAP: Duration = Duration::from_millis(5000);
/// How long the slow rate lasts after LoC's last push-back.
pub const SLOW_FOR: Duration = Duration::from_secs(24 * 3600);

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
    /// When the slow rate ends, in ms since the epoch. None: only a reset ends
    /// it (a file an older release slowed, or a budget with no time limit).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub until_ms: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct GlobalPacer {
    path: PathBuf,
    normal: Duration,
    slow: Duration,
    /// How long a push-back keeps the slow rate; None: until reset.
    slow_for: Option<Duration>,
    /// How logs name the budget's host.
    label: String,
    /// When set, a file holding the time of this user's last request (ms since
    /// the epoch), so a restarted run keeps its own gap from it.
    stamp: Option<PathBuf>,
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
/// A slow-down whose time is up is lifted first; it is returned for the log.
fn with_state<T>(
    path: &Path,
    f: impl FnOnce(&mut State) -> T,
) -> std::io::Result<(T, Option<Slowed>)> {
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
                    until_ms: None,
                }),
                sent: 0,
                hold_until_ms: now,
                generation: 1,
            }
        })
    };
    let lifted = match &state.slowed {
        Some(s) if s.until_ms.is_some_and(|u| u <= now_ms()) => {
            state.generation += 1;
            state.slowed.take()
        }
        _ => None,
    };
    let out = f(&mut state);
    let body = serde_json::to_vec(&state).map_err(std::io::Error::other)?;
    file.set_len(0)?;
    file.seek(SeekFrom::Start(0))?;
    file.write_all(&body)?;
    file.sync_data()?;
    // The lock goes with the file.
    Ok((out, lifted))
}

impl GlobalPacer {
    pub fn new(path: PathBuf) -> Self {
        Self::with_gaps(path, NORMAL_GAP, SLOW_GAP)
    }

    pub fn with_gaps(path: PathBuf, normal: Duration, slow: Duration) -> Self {
        Self {
            path,
            normal,
            slow,
            slow_for: Some(SLOW_FOR),
            label: "LoC".into(),
            stamp: None,
        }
    }

    /// How long a push-back keeps the slow rate; None: until an operator resets it.
    pub fn slow_for(mut self, d: Option<Duration>) -> Self {
        self.slow_for = d;
        self
    }

    /// [`with_state`], logging a slow-down it lifted.
    fn locked<T>(&self, f: impl FnOnce(&mut State) -> T) -> std::io::Result<T> {
        let (out, lifted) = with_state(&self.path, f)?;
        if let Some(s) = lifted {
            tracing::warn!(
                since = %s.at,
                why = %s.why,
                path = %self.path.display(),
                "{}'s slow rate after a push-back is over: the global pacer is back to {} requests a minute",
                self.label,
                60_000 / self.normal.as_millis().max(1)
            );
        }
        Ok(out)
    }

    pub fn labelled(mut self, label: &str) -> Self {
        label.clone_into(&mut self.label);
        self
    }

    pub fn with_stamp(mut self, stamp: PathBuf) -> Self {
        self.stamp = Some(stamp);
        self
    }

    pub(crate) fn slow_gap(&self) -> Duration {
        self.slow
    }

    pub(crate) fn read_stamp(&self) -> Option<u64> {
        let text = std::fs::read_to_string(self.stamp.as_ref()?).ok()?;
        text.trim().parse().ok()
    }

    pub(crate) fn write_stamp(&self) {
        if let Some(stamp) = &self.stamp {
            if let Err(e) = std::fs::write(stamp, now_ms().to_string()) {
                tracing::warn!(path = %stamp.display(), error = %e, "can't record the request's time");
            }
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reserve the next slot at or after `not_before_ms`; returns it and the
    /// generation it was taken in.
    pub fn reserve_at(&self, not_before_ms: u64) -> std::io::Result<(u64, u64)> {
        let (normal, slow) = (self.normal, self.slow);
        self.locked(|s| {
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
    pub(crate) fn is_still_good(&self, generation: u64) -> bool {
        self.locked(|s| s.generation == generation && s.hold_until_ms <= now_ms())
            .unwrap_or(false)
    }

    /// Count `n` more requests already sent (redirect hops a client followed
    /// on its own): the next slots move on by `n` gaps.
    pub fn charge(&self, n: u64) {
        let (normal, slow) = (self.normal, self.slow);
        let res = self.locked(|s| {
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
        if let Err(e) = self.locked(|s| {
            s.next_ms = s.next_ms.max(until);
            s.hold_until_ms = s.hold_until_ms.max(until);
            s.generation += 1;
        }) {
            tracing::warn!(error = %e, "can't record a hold in the global LoC pacer");
        }
    }

    /// LoC pushed back: every process drops to the slow rate for the budget's
    /// slow time ([`SLOW_FOR`] for LoC), counted from this push-back. A
    /// push-back while slowed keeps the first reason and starts the time again;
    /// one with no end time (until reset) stays that way.
    pub fn slow_down(&self, why: &str) {
        let at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let until = self.slow_for.map(epoch_ms_in);
        let res = self.locked(|s| match &mut s.slowed {
            None => {
                s.generation += 1;
                s.slowed = Some(Slowed {
                    at: at.clone(),
                    why: why.to_owned(),
                    until_ms: until,
                });
                (true, until)
            }
            Some(old) => {
                if let (Some(was), Some(new)) = (old.until_ms, until) {
                    old.until_ms = Some(was.max(new));
                }
                (false, old.until_ms)
            }
        });
        let rate = 60_000 / self.slow.as_millis().max(1);
        let until_text = |u: Option<u64>| {
            u.and_then(|ms| chrono::DateTime::from_timestamp_millis(i64::try_from(ms).ok()?))
                .map_or_else(
                    || "until reset".to_owned(),
                    |t| {
                        format!(
                            "until {}",
                            t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                        )
                    },
                )
        };
        match res {
            Ok((true, u)) => tracing::error!(
                why,
                path = %self.path.display(),
                "{} pushed back: the global pacer drops to {rate} requests a minute {}",
                self.label,
                until_text(u)
            ),
            Ok((false, u)) => tracing::error!(
                why,
                path = %self.path.display(),
                "{} pushed back again: the global pacer stays at {rate} requests a minute {}",
                self.label,
                until_text(u)
            ),
            Err(e) => {
                tracing::warn!(error = %e, "can't record the slow-down in the global LoC pacer")
            }
        }
    }

    /// The state as it is now.
    pub fn state(&self) -> std::io::Result<State> {
        self.locked(|s| s.clone())
    }

    /// Back to the normal rate (an operator's decision).
    pub fn reset(&self) -> std::io::Result<()> {
        self.locked(|s| s.slowed = None)
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
    fn a_push_back_slows_every_handle_until_its_time_is_up_or_a_reset() {
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
        assert!(a.is_still_good(g));
        a.slow_down("HTTP 429");
        assert!(!a.is_still_good(g));
        let (_, g) = a.reserve_at(0).unwrap();
        a.hold(Duration::from_secs(60));
        assert!(!a.is_still_good(g), "a hold after the slot was taken");
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
    fn the_default_gaps_are_20_and_12_a_minute_and_the_slow_rate_lasts_a_day() {
        assert_eq!(60_000 / NORMAL_GAP.as_millis(), 20);
        assert_eq!(60_000 / SLOW_GAP.as_millis(), 12);
        assert_eq!(SLOW_FOR, Duration::from_secs(86_400));
    }

    #[test]
    fn a_push_back_slows_for_a_day_then_lifts_itself() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        let a = GlobalPacer::new(p.clone());
        let before = now_ms();
        a.slow_down("HTTP 429");
        let st = a.state().unwrap();
        let until = st.slowed.unwrap().until_ms.unwrap();
        assert!(until >= before + 86_400_000 && until <= now_ms() + 86_400_000);
        // A second push-back an instant later starts the day again from then.
        a.slow_down("HTTP 403");
        let st = a.state().unwrap().slowed.unwrap();
        assert_eq!(st.why, "HTTP 429");
        assert!(st.until_ms.unwrap() >= until);
        // The day is up: the next caller lifts it, and the normal gap is back.
        let g0 = a.state().unwrap().generation;
        with_state(&p, |s| {
            s.slowed.as_mut().unwrap().until_ms = Some(now_ms() - 1);
        })
        .unwrap();
        let (s1, g1) = a.reserve_at(0).unwrap();
        assert!(g1 > g0, "a lift is a new generation");
        let (s2, _) = a.reserve_at(0).unwrap();
        assert_eq!(u128::from(s2 - s1), NORMAL_GAP.as_millis());
        assert!(a.state().unwrap().slowed.is_none());
    }

    #[test]
    fn a_slow_down_with_no_end_stays_until_reset() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("pacer.json");
        // As an older release wrote it: no end time.
        std::fs::write(
            &p,
            br#"{"next_ms":0,"slowed":{"at":"2026-10-25T20:25:37Z","why":"HTTP 503 (challenge page)"},"sent":1,"hold_until_ms":0,"generation":1}"#,
        )
        .unwrap();
        let a = GlobalPacer::new(p.clone());
        let (s1, _) = a.reserve_at(0).unwrap();
        let (s2, _) = a.reserve_at(0).unwrap();
        assert_eq!(u128::from(s2 - s1), SLOW_GAP.as_millis());
        // Another push-back gives it no end time either.
        a.slow_down("HTTP 429");
        assert_eq!(a.state().unwrap().slowed.unwrap().until_ms, None);
        // A budget with no time limit behaves the same from its first push-back.
        let q = dir.path().join("q.json");
        let b = GlobalPacer::new(q).slow_for(None);
        b.slow_down("HTTP 429");
        assert_eq!(b.state().unwrap().slowed.unwrap().until_ms, None);
        a.reset().unwrap();
        assert!(a.state().unwrap().slowed.is_none());
    }
}
