//! `usnm-ocr-worker bulk-ocr` (#962): Chronicling America's bulk OCR
//! archives, one per batch, each holding every page's `ocr.txt` and
//! `ocr.xml` (ALTO with word confidence), onto local disk.
//!
//! LoC lists them at `https://www.loc.gov/collections/chronicling-america/datasets/?fo=json`
//! (3,011 archives, 2.47 TB, each with its size and sha256) and asks for at
//! most "10 bulk requests within 10 minutes" per IP (the Chronicling America
//! guide's additional features page). So this fetches one archive at a time,
//! at least [`Options::gap`] apart (60 s by default), every request also
//! inside the host's global LoC budget ([`crate::loc::global`]) that OCR's
//! scans and the quality pass share. Redirects are followed by hand, each
//! hop paced, and only to `https://` hosts under `loc.gov`.
//!
//! A run stops starting archives after [`Options::max_hours`] (LoC reserves
//! the right to stop programs that run past 24 hours), so it runs as a
//! sequence of runs with pauses between them (a systemd timer). An archive is
//! written to `<name>.part`, checked against the listed size and sha256, and
//! renamed; a finished archive is skipped next time. A 429, a 403 or an HTML
//! page instead of the archive drops the global budget to the slow rate and
//! ends the run with an error, so a block is reported, never pressed on.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tokio::time::Instant;

use crate::loc::global::GlobalPacer;
use crate::loc::{Pacer, USER_AGENT};

pub const LIST_URL: &str = "https://www.loc.gov/collections/chronicling-america/datasets/?fo=json";

/// One archive, as LoC's datasets table lists it.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Archive {
    pub archive_name: String,
    pub batch: String,
    pub url: String,
    pub size: u64,
    pub sha256: String,
    #[serde(default)]
    pub page_count: Option<u64>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// Where the archives go.
    pub out: PathBuf,
    /// The datasets table, as LoC serves it (`?fo=json`); fetched once into
    /// `<out>/datasets.json` when missing.
    pub list_url: String,
    /// Between archive requests (and redirect hops).
    pub gap: Duration,
    /// Stop starting archives after this long.
    pub max_hours: f64,
    /// Leave at least this much free on `out`'s filesystem.
    pub min_free_bytes: u64,
    /// At most this many archives this run.
    pub limit: Option<usize>,
    /// The host's global LoC budget.
    pub global_pacer: PathBuf,
}

#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct Status {
    pub state: String,
    pub archives: usize,
    pub done: usize,
    pub fetched_this_run: usize,
    pub bytes_done: u64,
    pub bytes_total: u64,
    pub bytes_this_run: u64,
    pub elapsed_secs: f64,
    /// Bytes a second this run, while downloading.
    pub bytes_per_sec: f64,
    pub failed: Vec<String>,
    pub updated_at: String,
}

/// The archives in LoC's datasets JSON.
pub fn parse_list(v: &serde_json::Value) -> anyhow::Result<Vec<Archive>> {
    let rows = v["datasets"]
        .as_array()
        .context("the datasets JSON has no datasets array")?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let a: Archive = serde_json::from_value(r.clone()).context("a datasets row")?;
        check_name(&a.archive_name)?;
        if a.sha256.len() != 64 || !a.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("{}: not a sha256: {}", a.archive_name, a.sha256);
        }
        out.push(a);
    }
    out.sort_by(|a, b| a.archive_name.cmp(&b.archive_name));
    out.dedup_by(|a, b| a.archive_name == b.archive_name);
    Ok(out)
}

fn check_name(name: &str) -> anyhow::Result<()> {
    let ok = name.ends_with(".tar.bz2")
        && !name.starts_with('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'));
    if !ok {
        bail!("not an archive name: {name}");
    }
    Ok(())
}

/// Only `https://` on loc.gov or a host under it.
pub fn allowed(url: &reqwest::Url) -> bool {
    url.scheme() == "https"
        && url
            .host_str()
            .is_some_and(|h| h == "loc.gov" || h.ends_with(".loc.gov"))
}

fn free_bytes(path: &Path) -> u64 {
    rustix::fs::statvfs(path).map_or(0, |s| s.f_bavail.saturating_mul(s.f_frsize))
}

fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

struct Fetcher {
    http: reqwest::Client,
    pacer: Pacer,
    global: Arc<GlobalPacer>,
    /// The minimum between any two requests (archive starts, redirect hops, the list).
    gap: Duration,
    /// When the last request went: checked again before each send and logged, so the
    /// journal shows every request's spacing.
    last_sent: std::sync::Mutex<Option<Instant>>,
}

enum Fail {
    /// LoC pushed back: stop the run.
    Blocked(String),
    /// Try again next run.
    Transient(String),
    /// Stopped, or the run's time is up, before a request went out.
    Stopped,
}

/// The `Retry-After` of a response, capped at an hour.
fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| usnm_core::retry_after::parse(v, std::time::SystemTime::now()))
        .map(|d| d.min(Duration::from_secs(3600)))
}

fn is_html(resp: &reqwest::Response) -> bool {
    resp.headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.to_ascii_lowercase().contains("text/html"))
}

impl Fetcher {
    /// GET `url`, following redirects by hand, each request paced. Stops
    /// before any request once `stop` is set or `deadline` has passed,
    /// including while waiting for a slot.
    async fn open(
        &self,
        url: &str,
        stop: &watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<reqwest::Response, Fail> {
        let mut at =
            reqwest::Url::parse(url).map_err(|e| Fail::Transient(format!("{url}: {e}")))?;
        for _ in 0..5 {
            if !allowed(&at) {
                return Err(Fail::Transient(format!("{at}: not an https loc.gov URL")));
            }
            let mut stop = stop.clone();
            tokio::select! {
                () = self.pacer.wait() => {}
                _ = stop.wait_for(|s| *s) => return Err(Fail::Stopped),
                () = tokio::time::sleep_until(deadline) => return Err(Fail::Stopped),
            }
            // Belt and braces over the pacer: never two requests closer than the gap.
            let prev = *self.last_sent.lock().expect("last_sent lock");
            if let Some(p) = prev {
                let since = p.elapsed();
                if since < self.gap {
                    let mut stop = stop.clone();
                    tokio::select! {
                        () = tokio::time::sleep(self.gap - since) => {}
                        _ = stop.wait_for(|s| *s) => return Err(Fail::Stopped),
                        () = tokio::time::sleep_until(deadline) => return Err(Fail::Stopped),
                    }
                }
            }
            if *stop.borrow() || Instant::now() >= deadline {
                return Err(Fail::Stopped);
            }
            let now = Instant::now();
            let since = prev.map(|p| (now - p).as_secs_f64());
            *self.last_sent.lock().expect("last_sent lock") = Some(now);
            tracing::info!(url = %at, since_last_request_secs = since, "bulk OCR request");
            let resp = self
                .http
                .get(at.clone())
                .send()
                .await
                .map_err(|e| Fail::Transient(format!("{at}: {}", e.without_url())))?;
            let status = resp.status();
            if status.is_redirection() {
                let to = resp
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|l| l.to_str().ok())
                    .and_then(|l| at.join(l).ok())
                    .ok_or_else(|| Fail::Transient(format!("{at}: a redirect with no location")))?;
                at = to;
                continue;
            }
            // An HTML page is a challenge or block page whatever its status.
            if is_html(&resp)
                || matches!(
                    status,
                    StatusCode::TOO_MANY_REQUESTS | StatusCode::FORBIDDEN
                )
            {
                let d = retry_after(&resp).unwrap_or(Duration::from_secs(3600));
                self.global.hold(d);
                let why = if is_html(&resp) {
                    format!("HTTP {} with an HTML page", status.as_u16())
                } else {
                    format!("HTTP {}", status.as_u16())
                };
                self.global.slow_down(&format!("bulk OCR: {why}"));
                return Err(Fail::Blocked(format!("{at}: {why}")));
            }
            if !status.is_success() {
                if status.is_server_error() {
                    // This run's own pacer, not the host's budget: a 503 is load.
                    self.pacer.hold(
                        retry_after(&resp)
                            .unwrap_or(Duration::ZERO)
                            .max(Duration::from_secs(300)),
                    );
                }
                return Err(Fail::Transient(format!("{at}: HTTP {}", status.as_u16())));
            }
            return Ok(resp);
        }
        Err(Fail::Transient(format!("{url}: too many redirects")))
    }

    /// Download `a` into `out`, checked; returns the bytes written.
    async fn fetch(
        &self,
        a: &Archive,
        out: &Path,
        stop: &watch::Receiver<bool>,
        deadline: Instant,
    ) -> Result<u64, Fail> {
        let path = out.join(&a.archive_name);
        let part = out.join(format!("{}.part", a.archive_name));
        let mut resp = self.open(&a.url, stop, deadline).await?;
        if let Some(len) = resp.content_length() {
            if len != a.size {
                return Err(Fail::Transient(format!(
                    "{}: LoC offers {len} bytes, the table lists {}",
                    a.archive_name, a.size
                )));
            }
        }
        let mut file = tokio::fs::File::create(&part)
            .await
            .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
        let mut hash = Sha256::new();
        let mut n: u64 = 0;
        loop {
            if *stop.borrow() {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Stopped);
            }
            let chunk = match resp.chunk().await {
                Ok(Some(c)) => c,
                Ok(None) => break,
                Err(e) => {
                    let _ = tokio::fs::remove_file(&part).await;
                    return Err(Fail::Transient(format!(
                        "{}: {}",
                        a.archive_name,
                        e.without_url()
                    )));
                }
            };
            n += chunk.len() as u64;
            if n > a.size {
                let _ = tokio::fs::remove_file(&part).await;
                return Err(Fail::Transient(format!(
                    "{}: longer than the listed {} bytes",
                    a.archive_name, a.size
                )));
            }
            hash.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
        }
        file.sync_all()
            .await
            .map_err(|e| Fail::Transient(format!("{}: {e}", part.display())))?;
        drop(file);
        let got = usnm_core::digest::hex(&hash.finalize());
        if n != a.size || got != a.sha256.to_ascii_lowercase() {
            let _ = tokio::fs::remove_file(&part).await;
            return Err(Fail::Transient(format!(
                "{}: {n} bytes with sha256 {got}, not the listed {} and {}",
                a.archive_name, a.size, a.sha256
            )));
        }
        tokio::fs::rename(&part, &path)
            .await
            .map_err(|e| Fail::Transient(format!("{}: {e}", path.display())))?;
        Ok(n)
    }
}

/// An archive already on disk with the listed size (its sha256 was checked
/// before the rename that put it there).
fn done(out: &Path, a: &Archive) -> bool {
    std::fs::metadata(out.join(&a.archive_name)).is_ok_and(|m| m.len() == a.size)
}

async fn write_status(out: &Path, s: &Status) {
    let tmp = out.join("bulk-status.json.tmp");
    if let Ok(b) = serde_json::to_vec_pretty(s) {
        if tokio::fs::write(&tmp, b).await.is_ok() {
            let _ = tokio::fs::rename(&tmp, out.join("bulk-status.json")).await;
        }
    }
}

pub async fn run(opts: Options, stop: watch::Receiver<bool>) -> anyhow::Result<Status> {
    tokio::fs::create_dir_all(&opts.out).await?;
    // One bulk process at a time.
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(opts.out.join(".bulk.lock"))?;
    rustix::fs::flock(&lock, rustix::fs::FlockOperation::NonBlockingLockExclusive).map_err(
        |_| {
            anyhow::anyhow!(
                "another bulk-ocr run holds {}",
                opts.out.join(".bulk.lock").display()
            )
        },
    )?;
    let global = Arc::new(GlobalPacer::new(opts.global_pacer.clone()));
    // After a push-back the budget is at the slow rate for every process; the bulk
    // archives can wait for an operator's reset rather than add to it.
    if let Some(slowed) = global.state()?.slowed {
        bail!(
            "the global LoC pacer has been slowed since {} ({}); bulk OCR waits for `loc-pacer --reset`",
            slowed.at,
            slowed.why
        );
    }
    let f = Fetcher {
        http: reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .connect_timeout(Duration::from_secs(30))
            .read_timeout(Duration::from_secs(120))
            .redirect(reqwest::redirect::Policy::none())
            .build()?,
        pacer: Pacer::with_global(opts.gap, Some(Arc::clone(&global))),
        global: Arc::clone(&global),
        gap: opts.gap,
        last_sent: std::sync::Mutex::new(None),
    };
    let list_path = opts.out.join("datasets.json");
    if !list_path.exists() {
        let far = Instant::now() + Duration::from_secs(24 * 3600);
        let mut resp = f
            .open(&opts.list_url, &stop, far)
            .await
            .map_err(|e| match e {
                Fail::Blocked(m) | Fail::Transient(m) => anyhow::anyhow!("the datasets list: {m}"),
                Fail::Stopped => anyhow::anyhow!("stopped"),
            })?;
        let mut body = Vec::new();
        while let Some(c) = resp.chunk().await? {
            body.extend_from_slice(&c);
            if body.len() > 64 << 20 {
                bail!("the datasets list is over 64 MiB");
            }
        }
        let v: serde_json::Value =
            serde_json::from_slice(&body).context("the datasets list isn't JSON")?;
        parse_list(&v)?;
        tokio::fs::write(&list_path, &body).await?;
    }
    let v: serde_json::Value = serde_json::from_slice(&tokio::fs::read(&list_path).await?)?;
    let archives = parse_list(&v)?;
    let start = Instant::now();
    let deadline = start + Duration::from_secs_f64(opts.max_hours * 3600.0);
    let mut s = Status {
        state: "running".into(),
        archives: archives.len(),
        bytes_total: archives.iter().map(|a| a.size).sum(),
        ..Status::default()
    };
    let mut downloading = Duration::ZERO;
    for a in &archives {
        if done(&opts.out, a) {
            s.done += 1;
            s.bytes_done += a.size;
        }
    }
    s.updated_at = now();
    write_status(&opts.out, &s).await;
    tracing::info!(
        archives = s.archives,
        done = s.done,
        "bulk OCR run starting"
    );
    let mut result = Ok(());
    for a in &archives {
        if done(&opts.out, a) {
            continue;
        }
        if *stop.borrow() {
            s.state = "stopped".into();
            break;
        }
        if Instant::now() >= deadline {
            s.state = "paused".into();
            tracing::info!(
                hours = opts.max_hours,
                "the run's time is up; the next run goes on"
            );
            break;
        }
        if opts.limit.is_some_and(|l| s.fetched_this_run >= l) {
            s.state = "limit".into();
            break;
        }
        // Downloads restart rather than resume: a partial from an earlier run
        // goes before the space is counted.
        let _ = tokio::fs::remove_file(opts.out.join(format!("{}.part", a.archive_name))).await;
        if free_bytes(&opts.out) < a.size.saturating_add(opts.min_free_bytes) {
            s.state = "disk".into();
            result = Err(anyhow::anyhow!(
                "{}: {} bytes needed, and {} must stay free; stopping",
                a.archive_name,
                a.size,
                opts.min_free_bytes
            ));
            break;
        }
        let t = Instant::now();
        match f.fetch(a, &opts.out, &stop, deadline).await {
            Ok(n) => {
                downloading += t.elapsed();
                s.done += 1;
                s.fetched_this_run += 1;
                s.bytes_done += n;
                s.bytes_this_run += n;
                tracing::info!(archive = %a.archive_name, bytes = n, secs = t.elapsed().as_secs(), "bulk OCR archive fetched");
            }
            Err(Fail::Stopped) => {
                s.state = if *stop.borrow() {
                    "stopped".into()
                } else {
                    "paused".into()
                };
                break;
            }
            Err(Fail::Blocked(m)) => {
                s.state = "blocked".into();
                s.failed.push(m.clone());
                result = Err(anyhow::anyhow!(
                    "LoC pushed back ({m}); the global pacer is at the slow rate; stopping"
                ));
                break;
            }
            Err(Fail::Transient(m)) => {
                tracing::warn!(archive = %a.archive_name, error = %m, "bulk OCR archive failed; the next run tries it again");
                if s.failed.len() < 100 {
                    s.failed.push(m);
                }
            }
        }
        s.elapsed_secs = start.elapsed().as_secs_f64();
        s.bytes_per_sec = if downloading.is_zero() {
            0.0
        } else {
            s.bytes_this_run as f64 / downloading.as_secs_f64()
        };
        s.updated_at = now();
        write_status(&opts.out, &s).await;
    }
    if s.state == "running" {
        s.state = if s.done == s.archives {
            "done".into()
        } else {
            "incomplete".into()
        };
    }
    s.elapsed_secs = start.elapsed().as_secs_f64();
    s.updated_at = now();
    write_status(&opts.out, &s).await;
    result.map(|()| s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_list_parses_and_names_are_checked() {
        let v = serde_json::json!({"datasets": [
            {"archive_name": "b_x_ver01.tar.bz2", "batch": "b_x_ver01", "url": "https://chroniclingamerica.loc.gov/data/ocr/b_x_ver01.tar.bz2",
             "size": 10, "sha256": "a".repeat(64), "page_count": 5},
            {"archive_name": "a_y_ver01.tar.bz2", "batch": "a_y_ver01", "url": "https://chroniclingamerica.loc.gov/data/ocr/a_y_ver01.tar.bz2",
             "size": 20, "sha256": "b".repeat(64)}
        ]});
        let l = parse_list(&v).unwrap();
        assert_eq!(l[0].archive_name, "a_y_ver01.tar.bz2");
        let bad = serde_json::json!({"datasets": [{"archive_name": "../x.tar.bz2", "batch": "x", "url": "https://loc.gov/x", "size": 1, "sha256": "a".repeat(64)}]});
        assert!(parse_list(&bad).is_err());
    }

    #[test]
    fn only_https_loc_gov() {
        let ok = |u: &str| allowed(&reqwest::Url::parse(u).unwrap());
        assert!(ok("https://chroniclingamerica.loc.gov/data/ocr/x.tar.bz2"));
        assert!(ok("https://tile.loc.gov/storage-services/x"));
        assert!(!ok("http://tile.loc.gov/x"));
        assert!(!ok("https://loc.gov.example.com/x"));
        assert!(!ok("https://example.com/loc.gov"));
    }
}
