//! A run: workers taking jobs from a [`JobStore`] and trying each job's
//! sources in order.
//!
//! A host that pushes back, or whose budget is slowed, gets nothing more this
//! run, so a block is reported and never pressed on; jobs that have another
//! source go on with it, and the rest wait for a later run. Running out of
//! disk, or too many verification failures, stops the whole run.

use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Context;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::Instant;

use super::engine::{Engine, Fail, Gate, Got, Reservation};
use super::job::{Attempt, Job, JobStore, Status};

/// Entries kept in [`Status::failed`].
const MAX_FAILED: usize = 100;

#[derive(Debug, Clone)]
pub struct RunOptions {
    /// Where `.part` files go. A hard link gives them their final name, so
    /// this has to be on the destination's filesystem.
    pub part_dir: PathBuf,
    /// The filesystem that [`RunOptions::min_free_bytes`] is measured on.
    pub out: PathBuf,
    pub workers: usize,
    /// Stop starting jobs after this long.
    pub run_for: Duration,
    /// Leave at least this much free.
    pub min_free_bytes: u64,
    /// At most this many files fetched this run.
    pub limit: Option<usize>,
    /// Stop, and refuse to start, at this many verification failures on record.
    pub max_checksum_failures: Option<u64>,
}

fn free_bytes(path: &Path) -> u64 {
    rustix::fs::statvfs(path).map_or(0, |s| s.f_bavail.saturating_mul(s.f_frsize))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Where a run stands against its limit.
enum Limit {
    Room,
    /// Jobs in hand fill it, but they haven't all succeeded.
    Wait,
    Reached,
}

struct Track {
    status: Status,
    /// Jobs workers hold now, counted against the limit.
    in_hand: usize,
    /// Jobs downloading now, and since when any has been.
    busy: usize,
    busy_since: Option<Instant>,
    downloading: Duration,
    /// The first reason the run ended early, and its error if it is one.
    end: Option<(String, Option<anyhow::Error>)>,
}

struct Shared {
    engine: Arc<Engine>,
    store: Arc<dyn JobStore>,
    opts: RunOptions,
    start: Instant,
    deadline: Instant,
    track: Mutex<Track>,
    /// Bytes promised to transfers in flight, on top of the free-space floor.
    reserved: AtomicU64,
    /// Bumped when a worker puts a job down, for those waiting on the limit.
    freed: watch::Sender<u64>,
    /// One status write at a time, newest last.
    publishing: tokio::sync::Mutex<()>,
    /// Set by the caller's stop signal or by a reason to halt.
    halt: watch::Sender<bool>,
    outside: watch::Receiver<bool>,
}

impl Shared {
    fn track(&self) -> std::sync::MutexGuard<'_, Track> {
        self.track.lock().expect("run lock")
    }

    /// Note why the run ends, unless it already has a reason.
    fn end(&self, state: &str, err: Option<anyhow::Error>) {
        self.track().end.get_or_insert((state.to_owned(), err));
    }

    /// End the run now, in-flight transfers too.
    fn halt(&self, state: &str, err: anyhow::Error) {
        self.end(state, Some(err));
        self.halt.send_replace(true);
    }

    fn stopped_state(&self) -> &'static str {
        if *self.outside.borrow() {
            "stopped"
        } else {
            "paused"
        }
    }

    fn failed(&self, m: String) {
        let mut t = self.track();
        if t.status.failed.len() < MAX_FAILED {
            t.status.failed.push(m);
        }
    }

    /// This policy gets nothing more this run, including from jobs already
    /// waiting for it, which go on to their next source.
    fn close(&self, gate: &Gate, state: &str, err: anyhow::Error) {
        self.end(state, Some(err));
        gate.close();
    }

    fn began(&self) {
        let mut t = self.track();
        t.busy += 1;
        t.busy_since.get_or_insert_with(Instant::now);
    }

    fn ended(&self) {
        let mut t = self.track();
        t.busy -= 1;
        if t.busy == 0 {
            if let Some(since) = t.busy_since.take() {
                t.downloading += since.elapsed();
            }
        }
    }

    async fn publish(&self) {
        let _one = self.publishing.lock().await;
        let s = {
            let mut t = self.track();
            let (bytes, secs) = (t.status.bytes_this_run, t.downloading.as_secs_f64());
            t.status.elapsed_secs = self.start.elapsed().as_secs_f64();
            t.status.bytes_per_sec = if secs == 0.0 {
                0.0
            } else {
                bytes as f64 / secs
            };
            t.status.updated_at = now();
            t.status.clone()
        };
        if let Err(e) = self.store.status(&s).await {
            tracing::warn!(error = %e, "can't record the run's status");
        }
    }

    /// Count a job against the limit: one check and one count, so workers
    /// can't all pass it together.
    fn take_on(&self) -> Limit {
        let mut t = self.track();
        let (fetched, in_hand) = (t.status.fetched_this_run, t.in_hand);
        match self.opts.limit {
            Some(l) if fetched >= l => Limit::Reached,
            Some(l) if fetched + in_hand >= l => Limit::Wait,
            _ => {
                t.in_hand += 1;
                Limit::Room
            }
        }
    }

    /// Wait for the limit to answer. A job held by another worker may yet fail
    /// and leave room, so a full limit only ends the run once it's met. False
    /// if the limit is met or the run stopped.
    async fn wait_for_room(&self, stop: &watch::Receiver<bool>) -> bool {
        let mut freed = self.freed.subscribe();
        let mut stop = stop.clone();
        loop {
            match self.take_on() {
                Limit::Room => return true,
                Limit::Reached => {
                    self.end("limit", None);
                    return false;
                }
                Limit::Wait => {}
            }
            tokio::select! {
                _ = freed.changed() => {}
                _ = stop.wait_for(|s| *s) => {
                    self.end(self.stopped_state(), None);
                    return false;
                }
            }
        }
    }

    fn put_down(&self) {
        self.track().in_hand -= 1;
        self.freed.send_modify(|n| *n += 1);
    }

    /// Try `job`'s sources in order. False ends this worker.
    async fn one(&self, job: &Job, stop: &watch::Receiver<bool>) -> anyhow::Result<bool> {
        let part = self.opts.part_dir.join(format!("{}.part", job.name));
        let mut tried = false;
        for url in &job.sources {
            let gate = match self.engine.gate_for(url).await {
                Ok(g) => g,
                Err(Fail::Refused(m)) => {
                    tracing::warn!(file = %job.name, error = %m, "a source was refused");
                    self.failed(m);
                    tried = true;
                    continue;
                }
                Err(_) => continue,
            };
            let policy = gate.policy.name.clone();
            if gate.is_closed() {
                continue;
            }
            // A push-back to any process ends this host's part of the run before
            // its next job, as one to this run's own requests does.
            if let Err(e) = gate.not_slowed().await {
                self.close(&gate, "slowed", e);
                continue;
            }
            // A partial that won't be resumed goes before the space is counted.
            let have = if gate.policy.resume {
                tokio::fs::metadata(&part).await.map_or(0, |m| m.len())
            } else {
                let _ = tokio::fs::remove_file(&part).await;
                0
            };
            let need = job.size.saturating_sub(have);
            let held = Reservation::new(&self.reserved);
            let others = held.set(need);
            let room = free_bytes(&self.opts.out);
            if room
                < need
                    .saturating_add(self.opts.min_free_bytes)
                    .saturating_add(others)
            {
                self.halt(
                    "disk",
                    anyhow::anyhow!(
                        "{}: {need} bytes needed, and {} must stay free; stopping",
                        job.name,
                        self.opts.min_free_bytes
                    ),
                );
                self.store.release(job).await?;
                return Ok(false);
            }
            let t = Instant::now();
            self.began();
            let res = self
                .engine
                .fetch(job, url, &part, stop, self.deadline, &held)
                .await;
            self.ended();
            drop(held);
            match res {
                Ok(Got { written, placed }) => {
                    {
                        let mut s = self.track();
                        s.status.done += 1;
                        s.status.bytes_done += job.size;
                        if placed {
                            s.status.fetched_this_run += 1;
                            s.status.bytes_this_run += written;
                        }
                    }
                    tracing::info!(file = %job.name, source = %policy, bytes = written, placed, secs = t.elapsed().as_secs(), "file fetched");
                    let how = Attempt::Fetched {
                        source: url.clone(),
                        bytes: written,
                    };
                    self.store.finished(job, &how).await?;
                    return Ok(true);
                }
                Err(Fail::Stopped | Fail::RangeRefused) => {
                    self.end(self.stopped_state(), None);
                    self.store.release(job).await?;
                    return Ok(false);
                }
                Err(Fail::Closed) => {}
                Err(Fail::Slowed(e)) => self.close(&gate, "slowed", e),
                Err(Fail::Blocked(m)) => {
                    self.failed(m.clone());
                    let lasting = gate.policy.slow_requests_per_minute.is_some();
                    let label = &gate.policy.label;
                    let e = if lasting {
                        anyhow::anyhow!(
                            "{label} pushed back ({m}); the global pacer is at the slow rate; stopping"
                        )
                    } else {
                        anyhow::anyhow!("{label} ({m}): nothing more to it this run")
                    };
                    self.close(&gate, "blocked", e);
                    tried = true;
                }
                Err(Fail::Transient(m) | Fail::Refused(m)) => {
                    tracing::warn!(file = %job.name, error = %m, "the file failed from this source");
                    self.failed(m);
                    tried = true;
                }
                Err(Fail::Checksum(detail)) => {
                    let m = format!("{}: {detail}", job.name);
                    tracing::warn!(file = %job.name, source = %policy, detail = %detail, "verification failed; removed the partial copy");
                    self.failed(m);
                    tried = true;
                    let n = self.store.checksum_failed(job, &detail).await?;
                    self.track().status.checksum_failures = n;
                    if self.opts.max_checksum_failures.is_some_and(|l| n >= l) {
                        self.halt(
                            "checksum",
                            anyhow::anyhow!("{n} verification failures; stopping"),
                        );
                        return Ok(false);
                    }
                }
            }
        }
        if tried {
            let how = Attempt::Failed {
                message: format!("{}: no source gave it", job.name),
            };
            self.store.finished(job, &how).await?;
        } else {
            self.store.release(job).await?;
        }
        Ok(true)
    }

    async fn worker(self: Arc<Self>, stop: watch::Receiver<bool>) -> anyhow::Result<()> {
        loop {
            if *stop.borrow() {
                self.end(self.stopped_state(), None);
                return Ok(());
            }
            let Some(job) = self.store.claim().await? else {
                return Ok(());
            };
            let why = if *stop.borrow() {
                Some(self.stopped_state())
            } else if Instant::now() >= self.deadline {
                tracing::info!("the run's time is up; the next run goes on");
                Some("paused")
            } else {
                None
            };
            if let Some(state) = why {
                self.end(state, None);
                self.store.release(&job).await?;
                return Ok(());
            }
            if !self.wait_for_room(&stop).await {
                self.store.release(&job).await?;
                return Ok(());
            }
            let go_on = self.one(&job, &stop).await;
            self.put_down();
            self.publish().await;
            if !go_on? {
                return Ok(());
            }
        }
    }
}

/// Work through `store`. `status` starts from what's already done; the error
/// is the reason the run ended early, when that's an error, and the status
/// has been recorded either way.
pub async fn run(
    engine: Arc<Engine>,
    store: Arc<dyn JobStore>,
    opts: RunOptions,
    mut status: Status,
    stop: watch::Receiver<bool>,
) -> anyhow::Result<Status> {
    // A hard link gives a file its name, so find out now rather than after a download.
    let dev = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| m.dev())
            .with_context(|| p.display().to_string())
    };
    anyhow::ensure!(
        dev(&opts.part_dir)? == dev(&opts.out)?,
        "{} and {} have to be on one filesystem",
        opts.part_dir.display(),
        opts.out.display()
    );
    let failures = store.checksum_failures().await?;
    if let Some(l) = opts.max_checksum_failures {
        anyhow::ensure!(
            failures < l,
            "{failures} verification failures on record; refusing to start"
        );
    }
    status.checksum_failures = failures;
    status.state = "running".into();
    let (halt, halted) = watch::channel(*stop.borrow());
    let start = Instant::now();
    let workers = opts.workers.max(1);
    let shared = Arc::new(Shared {
        engine,
        store,
        start,
        deadline: start + opts.run_for,
        opts,
        track: Mutex::new(Track {
            status,
            in_hand: 0,
            busy: 0,
            busy_since: None,
            downloading: Duration::ZERO,
            end: None,
        }),
        reserved: AtomicU64::new(0),
        freed: watch::channel(0).0,
        publishing: tokio::sync::Mutex::new(()),
        halt,
        outside: stop.clone(),
    });
    shared.publish().await;
    let forward = {
        let shared = Arc::clone(&shared);
        let mut outside = stop;
        tokio::spawn(async move {
            if outside.wait_for(|s| *s).await.is_ok() {
                shared.halt.send_replace(true);
            }
        })
    };
    let mut set = JoinSet::new();
    for _ in 0..workers {
        set.spawn(Arc::clone(&shared).worker(halted.clone()));
    }
    let mut failed = None;
    while let Some(r) = set.join_next().await {
        if let Err(e) = r.map_err(anyhow::Error::from).and_then(|r| r) {
            failed.get_or_insert(e);
        }
    }
    forward.abort();
    let result = {
        let mut t = shared.track();
        match t.end.take() {
            Some((state, err)) => {
                t.status.state = state;
                err.or(failed).map_or(Ok(()), Err)
            }
            None => {
                t.status.state = if t.status.done == t.status.archives {
                    "done".into()
                } else {
                    "incomplete".into()
                };
                failed.map_or(Ok(()), Err)
            }
        }
    };
    shared.publish().await;
    let s = shared.track().status.clone();
    result.map(|()| s)
}
