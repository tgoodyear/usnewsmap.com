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
//! ends the run with an error, so a block is reported, never pressed on. A
//! run doesn't start, and starts no further archive, while the global budget
//! is slowed, whichever process LoC pushed back on.
//!
//! The downloads themselves go through [`crate::fetcher`]; LoC's rules above
//! are its policy, `policies/loc.toml`. With a mirror
//! ([`Options::mirror`]), an archive's job lists the mirror first and LoC
//! second, so both are one code path. The mirror's manifest says what it holds,
//! and an archive it lacks or lists with another hash is recorded as held back
//! and comes from LoC alone, or waits if the run is mirror-only. The hash
//! checked is always LoC's. A slowed LoC budget then stops only what needs LoC.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;
use tokio::time::Instant;

use crate::fetcher::run::RunOptions;
use crate::fetcher::{
    Engine, EngineOptions, Fail, FileBudgets, FileJobs, HeldBack, Job, JobStore, Policies, LOC,
};

pub use crate::fetcher::Status;

pub const LIST_URL: &str = "https://www.loc.gov/collections/chronicling-america/datasets/?fo=json";

/// Verification failures on record that stop a run with a mirror, and keep it from restarting.
pub const MAX_CHECKSUM_FAILURES: u64 = 3;

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
    /// The hosts this run may ask.
    pub policies: Policies,
    /// A mirror to prefer.
    pub mirror: Option<Mirror>,
    /// Keeps what outlives a run beyond the archives: `.part` files that can
    /// resume, budgets other than LoC's, and the held-back and failure lists.
    /// Needed with a mirror.
    pub state: Option<PathBuf>,
    /// Transfers at once, within each host's own limit.
    pub workers: usize,
    /// Lowers the mirror's byte cap.
    pub rate_bytes_per_sec: Option<u64>,
}

/// A mirror of the archives.
#[derive(Debug, Clone)]
pub struct Mirror {
    /// An archive's URL is this plus its name.
    pub base_url: String,
    /// The mirror's manifest: JSON holding rows with `archive_name` and `sha256`.
    pub manifest: PathBuf,
    /// Never ask LoC. Archives the mirror can't supply wait.
    pub only: bool,
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

/// An archive already on disk with the listed size (its sha256 was checked
/// before it got that name).
fn done(out: &Path, a: &Archive) -> bool {
    std::fs::metadata(out.join(&a.archive_name)).is_ok_and(|m| m.len() == a.size)
}

/// The rows of a mirror's manifest, wherever in the JSON they are: name to sha256.
pub fn parse_manifest(v: &serde_json::Value) -> Option<HashMap<String, String>> {
    let found =
        |rows: &Vec<serde_json::Value>| rows.first().is_some_and(|r| r.get("sha256").is_some());
    let inner: Vec<&serde_json::Value> = match v {
        serde_json::Value::Array(rows) if found(rows) => {
            return Some(
                rows.iter()
                    .filter_map(|r| {
                        Some((
                            r.get("archive_name")?.as_str()?.to_owned(),
                            r.get("sha256")?.as_str()?.to_ascii_lowercase(),
                        ))
                    })
                    .collect(),
            );
        }
        serde_json::Value::Array(rows) => rows.iter().collect(),
        serde_json::Value::Object(m) => m.values().collect(),
        _ => Vec::new(),
    };
    inner.into_iter().find_map(parse_manifest)
}

/// The jobs for every archive not on disk, and those held back from the mirror.
fn plan(
    archives: &[Archive],
    out: &Path,
    mirror: Option<(&Mirror, &HashMap<String, String>)>,
) -> (Vec<Job>, Vec<HeldBack>) {
    let mut jobs = Vec::new();
    let mut held = Vec::new();
    for a in archives.iter().filter(|a| !done(out, a)) {
        let sha256 = a.sha256.to_ascii_lowercase();
        let mut sources = Vec::new();
        if let Some((m, listed)) = mirror {
            let reason = match listed.get(&a.archive_name) {
                None => Some("absent-from-mirror"),
                Some(s) if *s != sha256 => Some("sha256-differs"),
                Some(_) => None,
            };
            match reason {
                None => sources.push(format!("{}{}", m.base_url, a.archive_name)),
                Some(reason) => held.push(HeldBack {
                    reason,
                    name: a.archive_name.clone(),
                    size: a.size,
                    sha256: sha256.clone(),
                    page_count: a.page_count.unwrap_or(0),
                }),
            }
        }
        if !mirror.is_some_and(|(m, _)| m.only) {
            sources.push(a.url.clone());
        }
        if !sources.is_empty() {
            jobs.push(Job {
                name: a.archive_name.clone(),
                size: a.size,
                sha256,
                dest: out.join(&a.archive_name),
                sources,
            });
        }
    }
    (jobs, held)
}

/// The datasets table, once, into `path`.
async fn fetch_list(
    engine: &Engine,
    url: &str,
    path: &Path,
    stop: &watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let far = Instant::now() + Duration::from_secs(24 * 3600);
    let mut resp = engine
        .open(url, None, stop, far)
        .await
        .map_err(|e| match e {
            Fail::Blocked(m) | Fail::Transient(m) | Fail::Refused(m) | Fail::Checksum(m) => {
                anyhow::anyhow!("the datasets list: {m}")
            }
            Fail::Stopped | Fail::RangeRefused | Fail::Closed => anyhow::anyhow!("stopped"),
            Fail::Slowed(e) => e,
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
    tokio::fs::write(path, &body).await?;
    Ok(())
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
    let mut policies = opts.policies.clone();
    if let Some(loc) = policies.named_mut(LOC) {
        loc.min_gap_secs = opts.gap.as_secs();
    }
    let part_dir = match (&opts.mirror, &opts.state) {
        (Some(_), None) => bail!("a mirror needs a state directory"),
        (Some(_), Some(s)) => s.join("part"),
        (None, _) => opts.out.clone(),
    };
    let mirror_url = opts.mirror.as_ref().map(|m| m.base_url.clone());
    if let (Some(url), Some(rate)) = (&mirror_url, opts.rate_bytes_per_sec) {
        let u = reqwest::Url::parse(url)?;
        let name = policies
            .for_url(&u)
            .map_err(|e| anyhow::anyhow!(e))?
            .name
            .clone();
        if let Some(p) = policies.named_mut(&name) {
            p.max_bytes_per_sec = Some(p.max_bytes_per_sec.map_or(rate, |c| c.min(rate)));
        }
    }
    let budgets = FileBudgets::new(opts.state.as_ref().map(|s| s.join("budgets"))).with_file(
        LOC,
        opts.global_pacer.clone(),
        Some(opts.out.join(".last-request")),
    );
    let engine = Arc::new(Engine::new(
        policies,
        Box::new(budgets),
        EngineOptions {
            who: "bulk OCR".into(),
            rate_file: opts.state.as_ref().map(|s| s.join("rate-bytes-per-sec")),
        },
    )?);
    if opts.mirror.is_none() {
        // After a push-back the budget is at the slow rate for every process; the bulk
        // archives can wait for an operator's reset rather than add to it.
        engine.check_not_slowed(&opts.list_url).await?;
    }
    let list_path = opts.out.join("datasets.json");
    if !list_path.exists() {
        if opts.mirror.as_ref().is_some_and(|m| m.only) {
            bail!(
                "{} is missing, and a mirror-only run never asks LoC",
                list_path.display()
            );
        }
        fetch_list(&engine, &opts.list_url, &list_path, &stop).await?;
    }
    let v: serde_json::Value = serde_json::from_slice(&tokio::fs::read(&list_path).await?)?;
    let archives = parse_list(&v)?;
    let listed = match &opts.mirror {
        Some(m) => {
            let text = tokio::fs::read(&m.manifest)
                .await
                .with_context(|| m.manifest.display().to_string())?;
            let v: serde_json::Value =
                serde_json::from_slice(&text).context("the mirror's manifest isn't JSON")?;
            Some(parse_manifest(&v).context("the mirror's manifest has no archive rows")?)
        }
        None => None,
    };
    let (jobs, held) = plan(
        &archives,
        &opts.out,
        opts.mirror.as_ref().zip(listed.as_ref()),
    );
    if let Some(s) = &opts.state {
        tokio::fs::create_dir_all(s.join("part")).await?;
    }
    let mut s = Status {
        archives: archives.len(),
        bytes_total: archives.iter().map(|a| a.size).sum(),
        held_back: held.len() as u64,
        ..Status::default()
    };
    for a in archives.iter().filter(|a| done(&opts.out, a)) {
        s.done += 1;
        s.bytes_done += a.size;
    }
    tracing::info!(
        archives = s.archives,
        done = s.done,
        held_back = s.held_back,
        "bulk OCR run starting"
    );
    let state_for_jobs = opts.mirror.as_ref().and(opts.state.clone());
    let store = Arc::new(FileJobs::new(
        jobs,
        opts.out.join("bulk-status.json"),
        state_for_jobs,
    ));
    store.held_back(&held).await?;
    crate::fetcher::run::run(
        engine,
        store,
        RunOptions {
            part_dir,
            out: opts.out.clone(),
            workers: opts.workers,
            run_for: Duration::from_secs_f64(opts.max_hours * 3600.0),
            min_free_bytes: opts.min_free_bytes,
            limit: opts.limit,
            max_checksum_failures: opts.mirror.as_ref().map(|_| MAX_CHECKSUM_FAILURES),
        },
        s,
        stop,
    )
    .await
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
}
