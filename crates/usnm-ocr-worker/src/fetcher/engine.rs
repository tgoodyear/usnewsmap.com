//! One download, from one source: every request paced and within its host's
//! policy, the bytes written to a `.part`, checked, then given their name.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::bail;
use reqwest::StatusCode;
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{watch, Semaphore};
use tokio::time::Instant;

use super::budget::{Budget, Budgets};
use super::job::Job;
use super::plain;
use super::policy::{Policies, Policy, ServingProbe};
use crate::loc::global::epoch_ms_in;
use crate::loc::{Pacer, USER_AGENT};

/// Redirects followed for one request.
const MAX_HOPS: usize = 5;
/// Measured against [`Policy::min_bytes_per_sec`].
const STALL_WINDOW: Duration = Duration::from_secs(180);
/// The byte cap never goes under this, however it was lowered.
const MIN_BYTE_CAP: u64 = 1_000_000;
/// A failed serving probe counts as this many seconds.
const PROBE_FAILED_SECS: f64 = 99.0;
/// Probes in a median.
const PROBES: usize = 5;

/// Why a source didn't give the file.
#[derive(Debug)]
pub enum Fail {
    /// The host pushed back, or kept failing: this run asks it nothing more.
    Blocked(String),
    /// Worth trying again.
    Transient(String),
    /// This source can't serve this request: no policy, or a refusal that isn't push-back.
    Refused(String),
    /// The bytes aren't the listed ones; the copy has gone.
    Checksum(String),
    /// Stopped, or the run's time is up, before a request went out.
    Stopped,
    /// The budget was slowed (a push-back to any process) while this request
    /// waited for its turn; it didn't go out.
    Slowed(anyhow::Error),
    /// The server won't give the rest of this `.part`.
    RangeRefused,
}

/// A file in place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Got {
    /// Bytes that came over the network for it.
    pub written: u64,
    /// False if a verified copy was already at the destination.
    pub placed: bool,
}

/// What every download in a run shares.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Names the caller in a slow-down's recorded reason.
    pub who: String,
    /// When present, a number in this file lowers byte caps at the next attempt.
    pub rate_file: Option<PathBuf>,
}

/// A policy's byte-rate cap, shared by its transfers.
#[derive(Debug)]
pub(super) struct ByteCap {
    cap: AtomicU64,
    /// The cap as last refreshed; 0 means no cap.
    now: AtomicU64,
    next: Mutex<Option<Instant>>,
    file: Option<PathBuf>,
}

impl ByteCap {
    pub(super) fn new(cap: Option<u64>, file: Option<PathBuf>) -> Self {
        let c = cap.unwrap_or(0);
        let me = Self {
            cap: AtomicU64::new(c),
            now: AtomicU64::new(c),
            next: Mutex::new(None),
            file,
        };
        me.refresh();
        me
    }

    pub(super) fn current(&self) -> Option<u64> {
        Some(self.now.load(Ordering::Relaxed)).filter(|c| *c > 0)
    }

    /// Read the lowering file, which can only ever lower the cap.
    pub(super) fn refresh(&self) {
        let cap = self.cap.load(Ordering::Relaxed);
        if cap == 0 {
            return;
        }
        let asked = self
            .file
            .as_ref()
            .and_then(|f| std::fs::read_to_string(f).ok())
            .and_then(|t| t.trim().parse::<u64>().ok());
        let c = asked.map_or(cap, |a| a.min(cap));
        self.now.store(c.max(MIN_BYTE_CAP), Ordering::Relaxed);
    }

    /// Halve the cap, down to `floor`, for the rest of the run.
    fn halve(&self, floor: u64) -> Option<u64> {
        let cap = self.cap.load(Ordering::Relaxed);
        (cap > floor).then(|| {
            let lower = (cap / 2).max(floor);
            self.cap.store(lower, Ordering::Relaxed);
            self.refresh();
            lower
        })
    }

    /// Wait until `n` more bytes fit the cap. Up to a second of unused rate
    /// carries over, so timer granularity doesn't lower the real rate.
    pub(super) async fn take(&self, n: u64) {
        let Some(cap) = self.current() else {
            return;
        };
        let cost = Duration::from_secs_f64(n as f64 / cap as f64);
        let at = {
            let mut next = self.next.lock().expect("byte cap lock");
            let now = Instant::now();
            let earliest = now.checked_sub(Duration::from_secs(1)).unwrap_or(now);
            let at = next.map_or(now, |t| t.max(earliest));
            *next = Some(at + cost);
            at
        };
        tokio::time::sleep_until(at).await;
    }
}

/// The last few probes' times, and whether their median says serving is slow.
#[derive(Debug, Default)]
pub(super) struct Probes(Vec<f64>);

impl Probes {
    pub(super) fn slow_after(&mut self, took: f64, limit: f64) -> bool {
        self.0.push(took);
        if self.0.len() < PROBES {
            return false;
        }
        let mut sorted = self.0.clone();
        sorted.sort_by(f64::total_cmp);
        let slow = sorted[PROBES / 2] > limit;
        if slow {
            self.0.clear();
        } else {
            self.0.remove(0);
        }
        slow
    }
}

/// Lowers `cap` while the serving path is slow, and never raises it.
fn guard(http: reqwest::Client, probe: ServingProbe, cap: &Arc<ByteCap>) {
    let cap = Arc::downgrade(cap);
    tokio::spawn(async move {
        let mut probes = Probes::default();
        loop {
            tokio::time::sleep(Duration::from_secs(probe.every_secs.max(1))).await;
            let Some(cap) = cap.upgrade() else {
                return;
            };
            let t = std::time::Instant::now();
            let ok = matches!(http.get(&probe.url).send().await, Ok(r) if r.status().is_success());
            let took = if ok {
                t.elapsed().as_secs_f64()
            } else {
                PROBE_FAILED_SECS
            };
            if probes.slow_after(took, probe.slow_secs) {
                if let Some(lower) = cap.halve(probe.floor_bytes_per_sec) {
                    tracing::warn!(cap = lower, "serving is slow; lowering the byte cap");
                }
            }
        }
    });
}

/// One policy's state in this process.
#[derive(Debug)]
pub(super) struct Gate {
    pub(super) policy: Policy,
    pub(super) budget: Arc<dyn Budget>,
    pacer: Pacer,
    /// When the last request went: checked again before each send and logged,
    /// so the journal shows every request's spacing.
    last_sent: Mutex<Option<Instant>>,
    slots: Semaphore,
    pub(super) cap: Arc<ByteCap>,
    in_a_row: AtomicU32,
}

/// `ms` since the epoch as an `Instant`, if it isn't too far back to say.
/// A time in the future (the clock moved back) counts as now: a full gap.
pub(super) fn instant_of(ms: u64) -> Option<Instant> {
    let ago = Duration::from_millis(epoch_ms_in(Duration::ZERO).saturating_sub(ms));
    Instant::now().checked_sub(ago)
}

impl Gate {
    /// An error once this budget has been slowed (a push-back to any process):
    /// nothing goes to the host until an operator resets it.
    pub(super) async fn not_slowed(&self) -> anyhow::Result<()> {
        if let Some(slowed) = self.budget.state().await?.slowed {
            bail!(
                "the global {} pacer has been slowed since {} ({}); this waits for `loc-pacer --reset`",
                self.policy.label,
                slowed.at,
                slowed.why
            );
        }
        Ok(())
    }

    /// Wait for this request's turn. Stops before any request once `stop` is
    /// set or `deadline` has passed, including while waiting.
    async fn turn(
        &self,
        stop: &watch::Receiver<bool>,
        deadline: Instant,
        at: &str,
    ) -> Result<(), Fail> {
        let mut s = stop.clone();
        tokio::select! {
            () = self.pacer.wait() => {}
            _ = s.wait_for(|s| *s) => return Err(Fail::Stopped),
            () = tokio::time::sleep_until(deadline) => return Err(Fail::Stopped),
        }
        // Belt and braces over the pacer: never two requests closer than the gap.
        let gap = self.policy.min_gap();
        let prev = *self.last_sent.lock().expect("last_sent lock");
        if let Some(p) = prev {
            let since = p.elapsed();
            if since < gap {
                let mut s = stop.clone();
                tokio::select! {
                    () = tokio::time::sleep(gap - since) => {}
                    _ = s.wait_for(|s| *s) => return Err(Fail::Stopped),
                    () = tokio::time::sleep_until(deadline) => return Err(Fail::Stopped),
                }
            }
        }
        if *stop.borrow() || Instant::now() >= deadline {
            return Err(Fail::Stopped);
        }
        // The waits above can outlast a push-back to another process: checked again
        // just before the request (each redirect hop too), not only between jobs.
        self.not_slowed().await.map_err(Fail::Slowed)?;
        let now = Instant::now();
        let since = prev.map(|p| (now - p).as_secs_f64());
        *self.last_sent.lock().expect("last_sent lock") = Some(now);
        self.budget.mark_sent().await;
        tracing::info!(url = %at, policy = %self.policy.name, since_last_request_secs = since, "fetch request");
        Ok(())
    }
}

/// The `Retry-After` of a response, at most `max`.
fn retry_after(resp: &reqwest::Response, max: Duration) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| usnm_core::retry_after::parse(v, std::time::SystemTime::now()))
        .map(|d| d.min(max))
}

fn is_html(resp: &reqwest::Response) -> bool {
    let h = resp.headers();
    h.get("cf-mitigated").is_some()
        || h.get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                let v = v.to_ascii_lowercase();
                v.contains("text/html") || v.contains("application/xhtml")
            })
}

/// Where a 206 says its body starts.
fn range_start(resp: &reqwest::Response) -> Option<u64> {
    let v = resp.headers().get(reqwest::header::CONTENT_RANGE)?;
    let rest = v.to_str().ok()?.strip_prefix("bytes ")?;
    rest.split('-').next()?.parse().ok()
}

async fn remove(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

/// After a transfer ends early: a `.part` that will resume has to be complete
/// on disk before the next attempt reads it.
async fn keep_or_remove(mut file: tokio::fs::File, part: &Path, resume: bool) {
    if resume {
        let _ = file.flush().await;
    } else {
        drop(file);
        remove(part).await;
    }
}

/// Feed all of `path` to `hash`; false if stopped first.
async fn hash_part(
    path: &Path,
    hash: &mut Sha256,
    stop: &watch::Receiver<bool>,
) -> std::io::Result<bool> {
    let mut f = tokio::fs::File::open(path).await?;
    let mut buf = vec![0u8; 1 << 20];
    loop {
        if *stop.borrow() {
            return Ok(false);
        }
        let n = f.read(&mut buf).await?;
        if n == 0 {
            return Ok(true);
        }
        hash.update(&buf[..n]);
    }
}

/// Verified bytes get their final name. The name is linked rather than
/// renamed over, so a good copy already there is never replaced; one of the
/// wrong size is known bad and is.
pub(super) fn publish(part: &Path, job: &Job) -> std::io::Result<bool> {
    match std::fs::hard_link(part, &job.dest) {
        Ok(()) => {
            std::fs::remove_file(part)?;
            Ok(true)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            if std::fs::metadata(&job.dest)?.len() == job.size {
                std::fs::remove_file(part)?;
                Ok(false)
            } else {
                std::fs::rename(part, &job.dest)?;
                Ok(true)
            }
        }
        Err(e) => Err(e),
    }
}

/// Fetches files within host policies.
pub struct Engine {
    http: reqwest::Client,
    probe_http: reqwest::Client,
    policies: Policies,
    budgets: Box<dyn Budgets>,
    gates: tokio::sync::Mutex<HashMap<String, Arc<Gate>>>,
    opts: EngineOptions,
}

impl Engine {
    pub fn new(
        policies: Policies,
        budgets: Box<dyn Budgets>,
        opts: EngineOptions,
    ) -> anyhow::Result<Self> {
        Ok(Self {
            http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .connect_timeout(Duration::from_secs(30))
                .read_timeout(Duration::from_secs(120))
                .redirect(reqwest::redirect::Policy::none())
                .build()?,
            probe_http: reqwest::Client::builder()
                .user_agent(USER_AGENT)
                .timeout(Duration::from_secs(10))
                .build()?,
            policies,
            budgets,
            gates: tokio::sync::Mutex::new(HashMap::new()),
            opts,
        })
    }

    /// `url`'s policy state, set up on first use.
    pub(super) async fn gate(&self, url: &reqwest::Url) -> Result<Arc<Gate>, Fail> {
        let policy = self.policies.for_url(url).map_err(Fail::Refused)?;
        let mut gates = self.gates.lock().await;
        if let Some(g) = gates.get(&policy.name) {
            return Ok(Arc::clone(g));
        }
        let budget = self
            .budgets
            .budget(policy)
            .map_err(|e| Fail::Refused(format!("{e:#}")))?;
        let last = budget.last_sent_ms().await.and_then(instant_of);
        let gap = policy.min_gap();
        let pacer = Pacer::with_budget(gap, Some(Arc::clone(&budget)));
        // The previous run's remainder of the gap goes on the pacer first, so the first
        // request waits it out before it takes a slot in the host's budget.
        if let Some(left) = last.and_then(|l| gap.checked_sub(l.elapsed())) {
            pacer.hold(left);
        }
        let cap = Arc::new(ByteCap::new(
            policy.max_bytes_per_sec,
            self.opts.rate_file.clone(),
        ));
        if let (Some(p), true) = (&policy.serving_probe, cap.current().is_some()) {
            guard(self.probe_http.clone(), p.clone(), &cap);
        }
        let g = Arc::new(Gate {
            policy: policy.clone(),
            budget,
            pacer,
            last_sent: Mutex::new(last),
            slots: Semaphore::new(policy.concurrency as usize),
            cap,
            in_a_row: AtomicU32::new(0),
        });
        gates.insert(policy.name.clone(), Arc::clone(&g));
        Ok(g)
    }

    pub(super) async fn gate_for(&self, url: &str) -> Result<Arc<Gate>, Fail> {
        let u = reqwest::Url::parse(url).map_err(|e| Fail::Refused(format!("{url}: {e}")))?;
        self.gate(&u).await
    }

    /// An error if `url`'s budget has been slowed, or no policy covers it.
    pub async fn check_not_slowed(&self, url: &str) -> anyhow::Result<()> {
        let gate = self.gate_for(url).await.map_err(|e| match e {
            Fail::Refused(m) => anyhow::anyhow!(m),
            other => anyhow::anyhow!("{other:?}"),
        })?;
        gate.not_slowed().await
    }

    /// GET `url`, following redirects by hand, each hop paced and refused if
    /// it leaves the policy `url` started in. `from` asks for the file from
    /// that byte on.
    pub async fn open(
        &self,
        url: &str,
        from: Option<u64>,
        stop: &watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<reqwest::Response, Fail> {
        let mut at = reqwest::Url::parse(url).map_err(|e| Fail::Refused(format!("{url}: {e}")))?;
        let first = self.gate(&at).await?;
        for _ in 0..MAX_HOPS {
            let shown = plain(&at);
            let here = self.policies.for_url(&at).map_err(Fail::Refused)?;
            if here.name != first.policy.name {
                return Err(Fail::Refused(format!(
                    "{shown}: a redirect out of {}'s policy",
                    first.policy.label
                )));
            }
            first.turn(stop, deadline, &shown).await?;
            let mut req = self.http.get(at.clone());
            if let Some(n) = from {
                req = req.header(reqwest::header::RANGE, format!("bytes={n}-"));
            }
            let resp = req
                .send()
                .await
                .map_err(|e| Fail::Transient(format!("{shown}: {}", e.without_url())))?;
            let status = resp.status();
            if status.is_redirection() {
                at = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| at.join(l).ok())
                    .ok_or_else(|| {
                        Fail::Transient(format!("{shown}: a redirect with no location"))
                    })?;
                continue;
            }
            let rules = &first.policy.push_back;
            let html = rules.html && is_html(&resp);
            if html || rules.statuses.contains(&status.as_u16()) {
                let d = retry_after(&resp, Duration::from_secs(rules.max_hold_secs))
                    .unwrap_or(Duration::from_secs(rules.default_hold_secs));
                first.budget.hold(d).await;
                let why = if html {
                    format!("HTTP {} with an HTML page", status.as_u16())
                } else {
                    format!("HTTP {}", status.as_u16())
                };
                if first.policy.slow_requests_per_minute.is_some() {
                    first
                        .budget
                        .slow_down(&format!("{}: {why}", self.opts.who))
                        .await;
                }
                return Err(Fail::Blocked(format!("{shown}: {why}")));
            }
            if status == StatusCode::RANGE_NOT_SATISFIABLE && from.is_some() {
                return Err(Fail::RangeRefused);
            }
            if !status.is_success() {
                let msg = format!("{shown}: HTTP {}", status.as_u16());
                if status.is_server_error() {
                    // This process's pacer, not the shared budget: a 503 is load.
                    first.pacer.hold(
                        retry_after(&resp, Duration::from_secs(rules.max_hold_secs))
                            .unwrap_or(Duration::ZERO)
                            .max(Duration::from_secs(first.policy.server_error_hold_secs)),
                    );
                    return Err(Fail::Transient(msg));
                }
                return Err(Fail::Refused(msg));
            }
            return Ok(resp);
        }
        Err(Fail::Transient(format!(
            "{}: too many redirects",
            reqwest::Url::parse(url).map_or_else(|_| url.to_owned(), |u| plain(&u))
        )))
    }

    /// `job` from `url` into `part` and on to its destination, with the
    /// policy's retries. Waits for one of the policy's transfer slots.
    pub async fn fetch(
        &self,
        job: &Job,
        url: &str,
        part: &Path,
        stop: &watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Got, Fail> {
        let gate = self.gate_for(url).await?;
        let mut s = stop.clone();
        let _slot = tokio::select! {
            p = gate.slots.acquire() => p.map_err(|_| Fail::Stopped)?,
            _ = s.wait_for(|s| *s) => return Err(Fail::Stopped),
            () = tokio::time::sleep_until(deadline) => return Err(Fail::Stopped),
        };
        let mut tries: u32 = 0;
        loop {
            gate.cap.refresh();
            let m = match self.attempt(&gate, job, url, part, stop, deadline).await {
                Ok(g) => {
                    gate.in_a_row.store(0, Ordering::Relaxed);
                    return Ok(g);
                }
                Err(Fail::RangeRefused) => {
                    remove(part).await;
                    continue;
                }
                Err(Fail::Transient(m)) => m,
                Err(e) => return Err(e),
            };
            tries += 1;
            let streak = gate.in_a_row.fetch_add(1, Ordering::Relaxed) + 1;
            if gate
                .policy
                .max_failures_in_a_row
                .is_some_and(|l| streak >= l)
            {
                return Err(Fail::Blocked(format!(
                    "{streak} transient failures in a row, last on {}: {m}",
                    job.name
                )));
            }
            if tries >= gate.policy.attempts {
                return Err(Fail::Transient(m));
            }
            tracing::warn!(file = %job.name, attempt = tries, error = %m, "transient failure; trying again");
            let wait = gate.policy.retry_backoff_secs * u64::from(tries);
            let mut s = stop.clone();
            tokio::select! {
                () = tokio::time::sleep(Duration::from_secs(wait)) => {}
                _ = s.wait_for(|s| *s) => return Err(Fail::Stopped),
            }
        }
    }

    async fn attempt(
        &self,
        gate: &Gate,
        job: &Job,
        url: &str,
        part: &Path,
        stop: &watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<Got, Fail> {
        let resume = gate.policy.resume;
        let name = &job.name;
        let mut have = if resume {
            tokio::fs::metadata(part).await.map_or(0, |m| m.len())
        } else {
            0
        };
        if !resume || have > job.size {
            remove(part).await;
            have = 0;
        }
        let mut hash = Sha256::new();
        if have > 0 {
            let done = hash_part(part, &mut hash, stop)
                .await
                .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
            if !done {
                return Err(Fail::Stopped);
            }
        }
        let mut written = 0;
        if have < job.size {
            let mut resp = self
                .open(url, (have > 0).then_some(have), stop, deadline)
                .await?;
            if have > 0 {
                if resp.status() != StatusCode::PARTIAL_CONTENT {
                    // The server sent the whole file, so the part starts over.
                    have = 0;
                    hash = Sha256::new();
                } else if range_start(&resp) != Some(have) {
                    remove(part).await;
                    return Err(Fail::Transient(format!(
                        "{name}: the source resumed from somewhere else"
                    )));
                }
            }
            if let Some(len) = resp.content_length() {
                if len + have != job.size {
                    return Err(Fail::Transient(format!(
                        "{name}: the source offers {} bytes, the job lists {}",
                        len + have,
                        job.size
                    )));
                }
            }
            let mut file = tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .append(have > 0)
                .truncate(have == 0)
                .open(part)
                .await
                .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
            let mut window = Instant::now();
            let mut in_window: u64 = 0;
            let mut s = stop.clone();
            loop {
                let got = tokio::select! {
                    c = resp.chunk() => Some(c),
                    () = async { let _ = s.wait_for(|s| *s).await; } => None,
                };
                let Some(got) = got else {
                    keep_or_remove(file, part, resume).await;
                    return Err(Fail::Stopped);
                };
                let chunk = match got {
                    Ok(Some(c)) => c,
                    Ok(None) => break,
                    Err(e) => {
                        keep_or_remove(file, part, resume).await;
                        return Err(Fail::Transient(format!("{name}: {}", e.without_url())));
                    }
                };
                have += chunk.len() as u64;
                written += chunk.len() as u64;
                if have > job.size {
                    remove(part).await;
                    return Err(Fail::Transient(format!(
                        "{name}: longer than the listed {} bytes",
                        job.size
                    )));
                }
                hash.update(&chunk);
                file.write_all(&chunk)
                    .await
                    .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
                gate.cap.take(chunk.len() as u64).await;
                in_window += chunk.len() as u64;
                if window.elapsed() >= STALL_WINDOW {
                    if let Some(min) = gate.policy.min_bytes_per_sec {
                        if in_window < min.saturating_mul(STALL_WINDOW.as_secs()) {
                            keep_or_remove(file, part, resume).await;
                            return Err(Fail::Transient(format!("{name}: stalled")));
                        }
                    }
                    window = Instant::now();
                    in_window = 0;
                }
            }
            file.sync_all()
                .await
                .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
        }
        let got = usnm_core::digest::hex(&hash.finalize());
        if have != job.size || got != job.sha256 {
            remove(part).await;
            let detail = if have == job.size {
                format!("sha256 {got} expected {}", job.sha256)
            } else {
                format!("size {have} expected {}", job.size)
            };
            return Err(Fail::Checksum(detail));
        }
        let placed = publish(part, job)
            .map_err(|e| Fail::Transient(format!("{}: {e}", job.dest.display())))?;
        Ok(Got { written, placed })
    }
}
