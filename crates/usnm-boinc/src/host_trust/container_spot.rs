//! Container OCR spot checks (#1934, docs/boinc/server/batch-scheduler.md): a `usnm_ocr` page a
//! trusted host returned is read again, from the same inputs, on a second host with a gold
//! report for the same worker image, and the two texts are compared against limits that depend
//! on the pair's platforms. Container work is quorum 1 with `sample_trivial_validator`, so
//! nothing else ever compares a container result with a second reading.
//!
//! The controller (`boinc/scripts/batch_scheduler.py`) draws which published pages are replayed,
//! after submit, and bounds each call with a time budget. This module finds each page's host
//! (BOINC's canonical result), picks the peer, creates the replay unit and compares the
//! results when they are back. A result outside the limits holds both hosts' trust for the
//! image in `trust.json` ([`crate::trust::Trust::hold_host`]), as a native mismatch does: the
//! collect gate then keeps each host's new results out of publication until an operator
//! releases the hold. Each replay is journaled under `<root>/container-spots/<job>/<peer>/`,
//! and a compared one is never compared again.
//!
//! # Platform families and limits
//!
//! The family of a host is what its container actually ran, taken from the host row (`os_name`
//! and `p_vendor`), never from the platform of the app version it was sent: an Apple Silicon
//! client reports `x86_64-apple-darwin` as well as `arm64-apple-darwin`, and runs the arm64
//! image under either. Two hosts of one family are compared against [`STRICT`]; an arm64 host
//! and an amd64 host against [`CROSS`], since the same Tesseract build can differ by CPU
//! (#1835). A peer of the source's own family is preferred, and a peer of the other family is
//! used when there is none.
use super::native_spot::{keep_replay_files, Outcome, Paths, Report, Status};
use super::spot::{fetch, job_inputs, judge, replay_args, stage};
use crate::trust::{Hold, Trust};
use crate::{atomic, load, read, Lock};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use tokio::process::Command;
use usnm_ocr_contract::boinc;

/// The longest list of replays or checks one call takes.
pub const MAX_ITEMS: usize = 100;
/// The longest time budget a call takes, in seconds.
pub const MAX_BUDGET_SECS: u64 = 3600;

/// The CPU family a container image ran on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Family {
    Amd64,
    Arm64,
}

/// The family of a host from its `os_name` and `p_vendor`, or None when they don't say. An
/// unknown host is neither replayed nor used as a peer.
pub fn family(os_name: &str, p_vendor: &str) -> Option<Family> {
    if super::capability::X86_VENDORS.contains(&p_vendor) {
        Some(Family::Amd64)
    } else if (os_name == "Darwin" && p_vendor == "Apple")
        || p_vendor.to_ascii_lowercase().starts_with("arm")
    {
        Some(Family::Arm64)
    } else {
        None
    }
}

/// How two hosts' results are judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Pairing {
    /// Both hosts of one family.
    Strict,
    /// An arm64 host and an amd64 host.
    Cross,
}

impl Pairing {
    pub fn limits(self) -> Limits {
        match self {
            Self::Strict => STRICT,
            Self::Cross => CROSS,
        }
    }
}

/// What two readings of one page may differ by and still agree. Inclusive at each end.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Limits {
    /// The most the two texts' character edit rate (edits over the mean of their lengths) may be.
    pub max_cer: f64,
    /// The least the word-bag F1 (Dice) of the two texts may be.
    pub min_f1: f64,
    /// The range of the replay's length over the original's, in characters.
    pub min_ratio: f64,
    pub max_ratio: f64,
}

/// Two hosts of one family.
pub const STRICT: Limits = Limits {
    max_cer: 0.01,
    min_f1: 0.98,
    min_ratio: 0.98,
    max_ratio: 1.02,
};

/// An arm64 host against an amd64 host.
pub const CROSS: Limits = Limits {
    max_cer: 0.10,
    min_f1: 0.90,
    min_ratio: 0.90,
    max_ratio: 1.10,
};

/// Why `replay` is outside `limits` of `original`, if it is. Both texts are as `import` stores
/// them; they are normalised as the gold set's are before the three measures are taken.
pub fn outside(limits: &Limits, original: &str, replay: &str) -> Option<String> {
    let m = usnm_ocr_quality::align::pair(original, replay);
    let ratio = if m.chars_a == 0 {
        if m.chars_b == 0 {
            1.0
        } else {
            f64::INFINITY
        }
    } else {
        m.chars_b as f64 / m.chars_a as f64
    };
    let mut reasons = Vec::new();
    if m.cer_sym > limits.max_cer {
        reasons.push(format!("CER {:.4} over {}", m.cer_sym, limits.max_cer));
    }
    if m.dice < limits.min_f1 {
        reasons.push(format!("F1 {:.4} under {}", m.dice, limits.min_f1));
    }
    if !(limits.min_ratio..=limits.max_ratio).contains(&ratio) {
        reasons.push(format!(
            "length ratio {ratio:.4} outside {} to {}",
            limits.min_ratio, limits.max_ratio
        ));
    }
    (!reasons.is_empty()).then(|| reasons.join("; "))
}

/// The peer for `job`'s replay among `candidates` (host, family; each trusted for the image,
/// none the source), and how the pair is judged. A peer of the source's family is preferred,
/// spread over them by the job's digest so no host is every page's peer; failing one, a peer of
/// the other family.
pub fn pick_peer(
    job: &str,
    source: Family,
    candidates: &[(u64, Family)],
) -> Option<(u64, Pairing)> {
    let spread = job
        .get(..16)
        .and_then(|head| u64::from_str_radix(head, 16).ok())
        .unwrap_or(0) as usize;
    let mut sorted = candidates.to_vec();
    sorted.sort_unstable();
    let same: Vec<_> = sorted.iter().filter(|(_, f)| *f == source).collect();
    if !same.is_empty() {
        return Some((same[spread % same.len()].0, Pairing::Strict));
    }
    let other: Vec<_> = sorted.iter().filter(|(_, f)| *f != source).collect();
    (!other.is_empty()).then(|| (other[spread % other.len()].0, Pairing::Cross))
}

/// Where each host's `os_name` and `p_vendor` are read from.
pub enum Platforms {
    Db {
        mysql_defaults: PathBuf,
        mysql_ca: PathBuf,
        database: String,
    },
    #[cfg(test)]
    Fixed(BTreeMap<u64, (String, String)>),
}

impl Platforms {
    /// The family of each of `ids` the host row names one for.
    async fn families(&self, ids: &BTreeSet<u64>) -> anyhow::Result<BTreeMap<u64, Family>> {
        if ids.is_empty() {
            return Ok(BTreeMap::new());
        }
        match self {
            Self::Db {
                mysql_defaults,
                mysql_ca,
                database,
            } => {
                ensure!(ids.len() <= 5000, "too many hosts to look up");
                let list: Vec<String> = ids.iter().map(u64::to_string).collect();
                let rows = super::query_db(
                    mysql_defaults,
                    mysql_ca,
                    database,
                    &format!(
                        "SELECT id, LEFT(os_name, 20), LEFT(p_vendor, 12) FROM host WHERE id IN ({})",
                        list.join(",")
                    ),
                )
                .await?;
                let mut found = BTreeMap::new();
                for line in rows.lines() {
                    let f: Vec<&str> = line.split('\t').collect();
                    ensure!(f.len() == 3, "invalid host platform row");
                    let id: u64 = f[0].parse().context("invalid host platform row")?;
                    if let Some(family) = family(f[1], f[2]) {
                        found.insert(id, family);
                    }
                }
                Ok(found)
            }
            #[cfg(test)]
            Self::Fixed(hosts) => Ok(ids
                .iter()
                .filter_map(|id| {
                    let (os, vendor) = hosts.get(id)?;
                    Some((*id, family(os, vendor)?))
                })
                .collect()),
        }
    }
}

/// A replay to compare: `job`, which `source` returned and `target` read again, with the
/// assimilator results directory that holds the original.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Check {
    pub job: String,
    pub source: u64,
    pub target: u64,
    pub spool: PathBuf,
}

/// Parses the pages to replay: job ids, comma-separated.
pub fn parse_submit(text: &str) -> anyhow::Result<Vec<String>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let items: Vec<String> = text.split(',').map(str::to_owned).collect();
    ensure!(
        items.len() <= MAX_ITEMS,
        "at most {MAX_ITEMS} container spot items a call"
    );
    for id in &items {
        ensure!(boinc::is_sha256(id), "invalid job id");
    }
    Ok(items)
}

/// Parses the replays to compare: `<job>=<source>:<target>:<original spool>`, comma-separated.
pub fn parse_check(text: &str) -> anyhow::Result<Vec<Check>> {
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let items: Vec<Check> = text
        .split(',')
        .map(|item| {
            let (job, rest) = item
                .split_once('=')
                .context("a container spot check is <job>=<source>:<target>:<spool>")?;
            let parts: Vec<_> = rest.splitn(3, ':').collect();
            ensure!(parts.len() == 3, "invalid container spot item {item}");
            let check = Check {
                job: job.to_owned(),
                source: parts[0].parse().context("invalid source host")?,
                target: parts[1].parse().context("invalid target host")?,
                spool: PathBuf::from(parts[2]),
            };
            let spool = check.spool.to_string_lossy();
            ensure!(boinc::is_sha256(&check.job), "invalid job id");
            ensure!(
                check.source > 0 && check.target > 0 && check.source != check.target,
                "a container spot check needs two different hosts"
            );
            ensure!(
                check.spool.is_absolute() && spool.contains("/results/") && !spool.contains(".."),
                "invalid original spool"
            );
            Ok(check)
        })
        .collect::<anyhow::Result<_>>()?;
    ensure!(
        items.len() <= MAX_ITEMS,
        "at most {MAX_ITEMS} container spot items a call"
    );
    Ok(items)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    job: String,
    source_host: u64,
    target_host: u64,
    pairing: Pairing,
    workunit: String,
    stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<Outcome>,
    /// The image both hosts are held for, recorded with a mismatch so its holds can be
    /// completed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
}

fn job_dir(root: &Path, job: &str) -> PathBuf {
    root.join("container-spots").join(job)
}

fn journal_path(root: &Path, job: &str, target: u64) -> PathBuf {
    job_dir(root, job)
        .join(target.to_string())
        .join("journal.json")
}

fn workunit(job: &str, source: u64, target: u64) -> String {
    format!("usnm_spot_{job}_{source}_{target}")
}

fn outcome(
    job: &str,
    source: u64,
    target: u64,
    status: Status,
    detail: impl Into<String>,
) -> Outcome {
    Outcome {
        job: job.to_owned(),
        source_host: source,
        target_host: target,
        status,
        detail: detail.into(),
    }
}

/// Why the page may not be replayed: no original, no host, or a source or peer outside trust.
async fn plan(
    p: &Paths,
    trust: &Trust,
    plat: &Platforms,
    active: Option<&BTreeSet<u64>>,
    job: &boinc::Job,
) -> Result<(u64, u64, Pairing), String> {
    let source = match p.canonical.host(&job.workunit()).await {
        Ok(Some(host)) => host,
        Ok(None) => return Err("the original has no canonical result".into()),
        Err(error) => return Err(format!("canonical host: {error:#}")),
    };
    if let Some(reason) = trust.refuse_job(job, Some(source)) {
        return Err(format!("source: {reason}"));
    }
    let mut ids: BTreeSet<u64> = trust
        .trusted_hosts_for_job(job)
        .into_iter()
        .filter(|h| *h != source && active.is_none_or(|a| a.contains(h)))
        .collect();
    let candidates = ids.clone();
    ids.insert(source);
    let families = plat
        .families(&ids)
        .await
        .map_err(|error| format!("host platforms: {error:#}"))?;
    let Some(&mine) = families.get(&source) else {
        return Err(format!(
            "host {source}'s platform family is not known from its row"
        ));
    };
    let peers: Vec<(u64, Family)> = candidates
        .iter()
        .filter_map(|h| Some((*h, *families.get(h)?)))
        .collect();
    match pick_peer(&job.id, mine, &peers) {
        Some((target, pairing)) => Ok((source, target, pairing)),
        None => Err(format!(
            "no other active host is trusted for {} with a known platform",
            job.engine.image
        )),
    }
}

/// Creates the replay unit for `job_id`, or reports why not. Without `execute` nothing is written.
async fn submit_one(
    p: &Paths,
    trust: &Trust,
    plat: &Platforms,
    active: Option<&BTreeSet<u64>>,
    job_id: &str,
    execute: bool,
) -> anyhow::Result<Outcome> {
    let refused = |detail: String| Ok(outcome(job_id, 0, 0, Status::Refused, detail));
    // A page is replayed once, whichever peer was chosen the first time.
    if let Ok(entries) = std::fs::read_dir(job_dir(&p.root, job_id)) {
        for entry in entries {
            let path = entry?.path().join("journal.json");
            if !path.exists() {
                continue;
            }
            let old: Journal = serde_json::from_slice(&read(&path, 64 << 10)?)?;
            ensure!(
                old.stage != "submitting",
                "container spot submission uncertain; reconcile {} by hand",
                old.workunit
            );
            return Ok(outcome(
                job_id,
                old.source_host,
                old.target_host,
                Status::Submitted,
                "already submitted",
            ));
        }
    }
    let record = match load(&p.jobs_root, job_id) {
        Ok(record) => record,
        Err(error) => return refused(format!("original journal: {error:#}")),
    };
    if record.job.lines.is_some() || record.bundle.is_some() {
        return refused("a line job or a bundled page is not replayed".into());
    }
    let job = record.job;
    let (source, target, pairing) = match plan(p, trust, plat, active, &job).await {
        Ok(found) => found,
        Err(reason) => return refused(reason),
    };
    let (scan, request) = match job_inputs(&p.jobs_root, &job) {
        Ok(inputs) => inputs,
        Err(error) => return refused(format!("inputs: {error:#}")),
    };
    if !execute {
        return Ok(outcome(
            job_id,
            source,
            target,
            Status::Submitted,
            format!("would submit, {pairing:?} limits"),
        ));
    }
    ensure!(
        crate::gold::submit::assignment_enabled(&std::fs::read_to_string(
            p.project.join("config.xml")
        )?)?,
        "container spot checks need <enable_assignment>1</enable_assignment>"
    );
    let unit = workunit(job_id, source, target);
    let path = journal_path(&p.root, job_id, target);
    let mut journal = Journal {
        job: job_id.to_owned(),
        source_host: source,
        target_host: target,
        pairing,
        workunit: unit.clone(),
        stage: "submitting".into(),
        verdict: None,
        image: None,
    };
    stage(&p.project, &unit, scan, request).await?;
    atomic(&path, &serde_json::to_vec_pretty(&journal)?)?;
    let status = Command::new(p.project.join("bin/create_work"))
        .current_dir(&p.project)
        .args(replay_args(&unit, target))
        .status()
        .await?;
    ensure!(
        status.success(),
        "container spot submission of {unit} uncertain; reconcile by hand"
    );
    journal.stage = "submitted".into();
    atomic(&path, &serde_json::to_vec_pretty(&journal)?)?;
    Ok(outcome(
        job_id,
        source,
        target,
        Status::Submitted,
        format!("{unit} assigned to host {target}, {pairing:?} limits"),
    ))
}

/// Compares `c`'s replay with its original, once both are there, and holds both hosts on a
/// result outside the pair's limits. A recorded verdict is returned as it was, and a recorded
/// mismatch's holds are completed again, so a run stopped between the verdict and the holds
/// loses neither.
async fn check_one(p: &Paths, trust: &Trust, c: &Check, execute: bool) -> anyhow::Result<Outcome> {
    let path = journal_path(&p.root, &c.job, c.target);
    ensure!(
        path.exists(),
        "no container spot journal for {} on host {}",
        c.job,
        c.target
    );
    let mut journal: Journal = serde_json::from_slice(&read(&path, 64 << 10)?)?;
    ensure!(
        journal.job == c.job && journal.source_host == c.source,
        "container spot journal changed"
    );
    let done = |status: Status, detail: &str| outcome(&c.job, c.source, c.target, status, detail);
    if let Some(verdict) = journal.verdict.clone() {
        if journal.stage == "holding" && execute {
            hold_both(p, &journal)?;
            return finish(p, &path, &mut journal, verdict, execute);
        }
        if journal.stage == "keeping" && execute {
            return finish(p, &path, &mut journal, verdict, execute);
        }
        return Ok(verdict);
    }
    ensure!(
        journal.stage == "submitted",
        "container spot {} is not submitted",
        journal.workunit
    );
    let unit = journal.workunit.clone();
    if p.spool.join(format!("{unit}_error")).exists()
        || p.spool.join(format!("{unit}_no_output_files")).exists()
    {
        let verdict = done(Status::Failed, "the replay unit failed in BOINC");
        return finish(p, &path, &mut journal, verdict, execute);
    }
    if !p.spool.join(format!("{unit}__file_result.json")).exists() {
        return Ok(done(Status::Pending, "not back yet"));
    }
    let job = match load(&p.jobs_root, &c.job) {
        Ok(record) => record.job,
        Err(error) => {
            let verdict = done(Status::Unverified, &format!("original journal: {error:#}"));
            return finish(p, &path, &mut journal, verdict, execute);
        }
    };
    for host in [c.source, c.target] {
        if let Some(reason) = trust.refuse_job(&job, Some(host)) {
            let verdict = done(Status::Unverified, &format!("host {host}: {reason}"));
            return finish(p, &path, &mut journal, verdict, execute);
        }
    }
    // Each result is attributed to the host BOINC recorded for it, not the one the list names.
    for (name, host) in [(job.workunit(), c.source), (unit.clone(), c.target)] {
        match p.canonical.host(&name).await {
            Ok(Some(found)) if found == host => {}
            Ok(found) => {
                let detail =
                    format!("{name}'s canonical result is host {found:?}'s, not host {host}'s");
                let verdict = done(Status::Unverified, &detail);
                return finish(p, &path, &mut journal, verdict, execute);
            }
            Err(error) => {
                return Ok(done(Status::Pending, &format!("canonical host: {error:#}")));
            }
        }
    }
    // A file that can't be read is the filesystem's fault, not a host's.
    let original = match fetch(&c.spool, &job.workunit()) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            let verdict = done(Status::Unverified, &format!("original: {error}"));
            return finish(p, &path, &mut journal, verdict, execute);
        }
        Err(error) => return Ok(done(Status::Pending, &format!("original: {error}"))),
    };
    let original = match judge(&job, original) {
        Ok(original) => original,
        Err(error) => {
            let verdict = done(Status::Unverified, &format!("original: {error:#}"));
            return finish(p, &path, &mut journal, verdict, execute);
        }
    };
    let replay = match fetch(&p.spool, &unit) {
        Ok(files) => files,
        Err(error) => return Ok(done(Status::Pending, &format!("replay: {error}"))),
    };
    let replay = match judge(&job, replay) {
        Ok(replay) => replay,
        // Readable but not a valid result of its job: as much a fault as different text, since
        // the peer was trusted.
        Err(error) => {
            let detail = format!("the replay is not a valid result: {error:#}");
            return mismatch(p, &path, &mut journal, &job, detail, execute);
        }
    };
    match outside(&journal.pairing.limits(), &original.text, &replay.text) {
        None => {
            let detail = format!("within the {:?} limits", journal.pairing);
            finish(
                p,
                &path,
                &mut journal,
                done(Status::Match, &detail),
                execute,
            )
        }
        Some(reason) => {
            let detail = format!("{:?} limits: {reason}", journal.pairing);
            mismatch(p, &path, &mut journal, &job, detail, execute)
        }
    }
}

/// Records the mismatch, then holds both hosts: the verdict is journaled first, so a run that
/// stops before the holds completes them the next time it reads the verdict.
fn mismatch(
    p: &Paths,
    path: &Path,
    journal: &mut Journal,
    job: &boinc::Job,
    detail: String,
    execute: bool,
) -> anyhow::Result<Outcome> {
    let verdict = outcome(
        &journal.job,
        journal.source_host,
        journal.target_host,
        Status::Mismatch,
        detail,
    );
    if !execute {
        return Ok(verdict);
    }
    journal.image = Some(job.engine.image.clone());
    journal.stage = "holding".into();
    journal.verdict = Some(verdict.clone());
    atomic(path, &serde_json::to_vec_pretty(journal)?)?;
    hold_both(p, journal)?;
    finish(p, path, journal, verdict, execute)
}

/// Holds both hosts for the image; a hold already there stays.
fn hold_both(p: &Paths, journal: &Journal) -> anyhow::Result<()> {
    let image = journal
        .image
        .as_deref()
        .context("a recorded mismatch names the image")?;
    let at = chrono::Utc::now().to_rfc3339();
    for (host, peer) in [
        (journal.source_host, journal.target_host),
        (journal.target_host, journal.source_host),
    ] {
        Trust::hold_host(
            &p.trust,
            image,
            host,
            Hold {
                at: at.clone(),
                job: journal.job.clone(),
                peer,
                reason: format!(
                    "container spot check: results outside the {:?} limits",
                    journal.pairing
                ),
            },
        )?;
    }
    Ok(())
}

fn finish(
    p: &Paths,
    path: &Path,
    journal: &mut Journal,
    verdict: Outcome,
    execute: bool,
) -> anyhow::Result<Outcome> {
    if execute {
        journal.stage = "keeping".into();
        journal.verdict = Some(verdict.clone());
        atomic(path, &serde_json::to_vec_pretty(journal)?)?;
        let into = path.parent().context("a journal has a directory")?;
        keep_replay_files(&p.spool, &journal.workunit, into)?;
        journal.stage = "compared".into();
        atomic(path, &serde_json::to_vec_pretty(journal)?)?;
    }
    Ok(verdict)
}

/// Creates the replays of the `submit` pages and compares the `check` ones, under the journal
/// root's lock, and starts no item once `budget` has run out: the rest are reported `deferred`
/// and left as they were, so a call's time is bounded by `budget` and one item. Comparisons go
/// first, since they end replays already out. `active` limits the peers to hosts BOINC has
/// lately seen ask for work (None: not known). One item's problem is that item's outcome; only
/// a fault that leaves a unit uncertain fails the call. Without `execute` nothing is created or
/// written.
pub async fn container_spot(
    p: &Paths,
    plat: &Platforms,
    active: Option<&BTreeSet<u64>>,
    submit: &[String],
    check: &[Check],
    budget: Duration,
    execute: bool,
) -> anyhow::Result<Report> {
    ensure!(
        submit.len() <= MAX_ITEMS && check.len() <= MAX_ITEMS,
        "at most {MAX_ITEMS} container spot items a call"
    );
    ensure!(
        budget.as_secs() <= MAX_BUDGET_SECS,
        "the budget is at most {MAX_BUDGET_SECS} s"
    );
    for path in [&p.root, &p.project, &p.jobs_root, &p.trust, &p.spool] {
        ensure!(path.is_absolute(), "container spot paths must be absolute");
    }
    let started = Instant::now();
    let _lock = Lock::take(&p.root)?;
    let mut report = Report::default();
    for c in check {
        if started.elapsed() >= budget {
            report.checked.push(outcome(
                &c.job,
                c.source,
                c.target,
                Status::Deferred,
                "per-call time budget spent",
            ));
            continue;
        }
        // Read again for each: a mismatch just above may have held a host.
        let trust = Trust::load(&p.trust)?;
        report.checked.push(check_one(p, &trust, c, execute).await?);
    }
    for id in submit {
        if started.elapsed() >= budget {
            report.submitted.push(outcome(
                id,
                0,
                0,
                Status::Deferred,
                "per-call time budget spent",
            ));
            continue;
        }
        let trust = Trust::load(&p.trust)?;
        report
            .submitted
            .push(submit_one(p, &trust, plat, active, id, execute).await?);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::native_spot::Canonical;
    use super::super::spot::tests::{job, returned};
    use super::*;

    const IMAGE: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    struct Fixture {
        _tmp: tempfile::TempDir,
        paths: Paths,
        job: boinc::Job,
        plat: Platforms,
    }

    /// A project with one published page, a trust list naming the hosts below for its image,
    /// and each host's row: hosts 1 and 9 are Apple Silicon Macs, 2, 3 and 4 x86 Linux, 5 a
    /// Windows host, 6 a host whose row names no CPU vendor, 7 not on the trust list.
    fn fixture(trusted: &[u64]) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut job = job("eng", "eng");
        let jobs = root.join("jobs");
        let scan = vec![7u8; 100];
        job.scan.sha256 = boinc::sha256(&scan);
        job.id = job.identity().unwrap();
        let input = crate::dir(&jobs, &job.id).unwrap();
        std::fs::create_dir_all(&input).unwrap();
        std::fs::write(input.join("page"), &scan).unwrap();
        std::fs::write(
            input.join("request.json"),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        let record = crate::Record {
            job: job.clone(),
            plan: "main".into(),
            stage: crate::Stage::Published,
            stamp: "20261112T000000Z".into(),
            error: None,
            terminal: None,
            retry: None,
            ledger: None,
            bundle: None,
            unbundled: None,
            held: None,
        };
        crate::save(&jobs, &record).unwrap();
        let trust = serde_json::json!({
            "hosts": {IMAGE: trusted},
            "manifest_sha256": "b".repeat(64),
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": 0.1, "v": 5
        });
        std::fs::write(root.join("trust.json"), serde_json::to_vec(&trust).unwrap()).unwrap();
        for d in ["results/0", "results/7", "host-trust", "project"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let paths = Paths {
            root: root.join("host-trust"),
            project: root.join("project"),
            jobs_root: jobs,
            trust: root.join("trust.json"),
            spool: root.join("results/0"),
            canonical: Canonical::Fixed(Default::default()),
        };
        let rows = [
            (1, "Darwin", "Apple"),
            (9, "Darwin", "Apple"),
            (2, "Linux", "GenuineIntel"),
            (3, "Linux", "AuthenticAMD"),
            (4, "Linux", "GenuineIntel"),
            (5, "Microsoft Windows 11", "AuthenticAMD"),
            (6, "Linux", ""),
            (7, "Linux", "GenuineIntel"),
        ];
        let plat = Platforms::Fixed(
            rows.iter()
                .map(|(id, os, vendor)| (*id, (os.to_string(), vendor.to_string())))
                .collect(),
        );
        Fixture {
            _tmp: tmp,
            paths,
            job,
            plat,
        }
    }

    fn canonical(f: &Fixture, workunit: &str, host: u64) {
        let Canonical::Fixed(hosts) = &f.paths.canonical else {
            unreachable!()
        };
        hosts.lock().unwrap().insert(workunit.to_owned(), host);
    }

    fn trust(f: &Fixture) -> Trust {
        Trust::load(&f.paths.trust).unwrap()
    }

    fn set(hosts: &[u64]) -> BTreeSet<u64> {
        hosts.iter().copied().collect()
    }

    /// What `submit_one` plans for the page when `source` returned it.
    async fn planned(f: &Fixture, source: u64, active: Option<&BTreeSet<u64>>) -> Outcome {
        canonical(f, &f.job.workunit(), source);
        submit_one(&f.paths, &trust(f), &f.plat, active, &f.job.id, false)
            .await
            .unwrap()
    }

    /// Writes a submitted journal, as `submit_one` leaves it, and the original's result.
    fn submitted(f: &Fixture, source: u64, target: u64, pairing: Pairing) -> Check {
        let unit = workunit(&f.job.id, source, target);
        let journal = Journal {
            job: f.job.id.clone(),
            source_host: source,
            target_host: target,
            pairing,
            workunit: unit.clone(),
            stage: "submitted".into(),
            verdict: None,
            image: None,
        };
        atomic(
            &journal_path(&f.paths.root, &f.job.id, target),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
        canonical(f, &f.job.workunit(), source);
        canonical(f, &unit, target);
        Check {
            job: f.job.id.clone(),
            source,
            target,
            spool: f.paths.spool.parent().unwrap().join("7"),
        }
    }

    const PAGE: &str = "The quick brown fox jumps over the lazy dog while the committee \
        considered the petition of the citizens of the county concerning the new road";

    fn results(f: &Fixture, c: &Check, original: &str, replay: &str) {
        returned(&f.job, &c.spool, &f.job.workunit(), original);
        returned(
            &f.job,
            &f.paths.spool,
            &workunit(&c.job, c.source, c.target),
            replay,
        );
    }

    async fn compare(f: &Fixture, c: &Check) -> Outcome {
        check_one(&f.paths, &trust(f), c, true).await.unwrap()
    }

    #[test]
    fn a_hosts_family_is_what_its_row_says_it_runs() {
        assert_eq!(family("Darwin", "Apple"), Some(Family::Arm64));
        assert_eq!(family("Darwin", "GenuineIntel"), Some(Family::Amd64));
        assert_eq!(family("Linux", "AuthenticAMD"), Some(Family::Amd64));
        assert_eq!(
            family("Microsoft Windows 11", "GenuineIntel"),
            Some(Family::Amd64)
        );
        assert_eq!(family("Linux", "ARM"), Some(Family::Arm64));
        // Nothing says what an unknown host runs, so it is not guessed.
        assert_eq!(family("Linux", ""), None);
        assert_eq!(family("Linux", "Apple"), None);
    }

    #[test]
    fn a_peer_of_the_sources_family_is_preferred_and_the_other_family_is_the_fallback() {
        use Family::{Amd64, Arm64};
        let job = "0".repeat(64);
        let all = [(1, Arm64), (2, Amd64), (3, Amd64), (9, Arm64)];
        for (source, same) in [(Arm64, [1, 9]), (Amd64, [2, 3])] {
            let (peer, pairing) = pick_peer(&job, source, &all).unwrap();
            assert_eq!(pairing, Pairing::Strict);
            assert!(same.contains(&peer));
        }
        // No arm64 peer left for an arm64 page: an amd64 host, under the cross limits.
        let only_amd64 = [(2, Amd64), (3, Amd64)];
        let (peer, pairing) = pick_peer(&job, Arm64, &only_amd64).unwrap();
        assert_eq!((pairing, [2, 3].contains(&peer)), (Pairing::Cross, true));
        let only_arm64 = [(1, Arm64)];
        assert_eq!(
            pick_peer(&job, Amd64, &only_arm64),
            Some((1, Pairing::Cross))
        );
        assert_eq!(pick_peer(&job, Amd64, &[]), None);
        // Peers of one family share the pages out by job digest.
        let peers: BTreeSet<u64> = (0..16u64)
            .map(|n| {
                pick_peer(&format!("{n:016x}{}", "0".repeat(48)), Amd64, &only_amd64)
                    .unwrap()
                    .0
            })
            .collect();
        assert_eq!(peers, set(&[2, 3]));
    }

    #[test]
    fn the_limits_are_the_documented_ones() {
        assert_eq!(
            (
                STRICT.max_cer,
                STRICT.min_f1,
                STRICT.min_ratio,
                STRICT.max_ratio
            ),
            (0.01, 0.98, 0.98, 1.02)
        );
        assert_eq!(
            (
                CROSS.max_cer,
                CROSS.min_f1,
                CROSS.min_ratio,
                CROSS.max_ratio
            ),
            (0.10, 0.90, 0.90, 1.10)
        );
        assert_eq!(Pairing::Strict.limits(), STRICT);
        assert_eq!(Pairing::Cross.limits(), CROSS);
    }

    #[test]
    fn a_pair_that_differs_a_little_passes_cross_and_fails_strict() {
        // Two words in 28 misread by a letter: over 1% of the characters, 0.93 word F1.
        let near = PAGE
            .replace("committee", "commiftee")
            .replace("petition", "petifion");
        assert_eq!(outside(&STRICT, PAGE, PAGE), None);
        assert_eq!(outside(&CROSS, PAGE, PAGE), None);
        let strict = outside(&STRICT, PAGE, &near).unwrap();
        assert!(strict.contains("CER") && strict.contains("F1"), "{strict}");
        assert_eq!(outside(&CROSS, PAGE, &near), None);
        // Past the cross limits too: a quarter of the words are gone.
        let cut: String = PAGE.split(' ').step_by(2).collect::<Vec<_>>().join(" ");
        let reason = outside(&CROSS, PAGE, &cut).unwrap();
        assert!(reason.contains("length ratio"), "{reason}");
    }

    #[test]
    fn the_length_ratio_has_its_own_bounds() {
        // Same words, but padded: CER and F1 are the same measures, the ratio is what trips.
        let long = format!("{PAGE} {}", "x".repeat(PAGE.len() / 20));
        let reason = outside(&STRICT, PAGE, &long).unwrap();
        assert!(reason.contains("length ratio"), "{reason}");
        // Whitespace is normalised away, so reformatting alone is no difference.
        assert_eq!(outside(&STRICT, PAGE, &PAGE.replace(' ', "  ")), None);
        // An empty original agrees only with an empty replay.
        assert_eq!(outside(&CROSS, "", ""), None);
        assert!(outside(&CROSS, "", "word").is_some());
        assert!(outside(&CROSS, "word", "").is_some());
    }

    #[tokio::test]
    async fn same_platform_pages_are_planned_strict_and_arm64_to_amd64_cross() {
        // Hosts 1 and 9 (arm64) and 2 and 3 (amd64) are all trusted.
        let f = fixture(&[1, 2, 3, 9]);
        let o = planned(&f, 1, None).await;
        assert_eq!(
            (o.status.clone(), o.target_host),
            (Status::Submitted, 9),
            "{o:?}"
        );
        assert!(o.detail.contains("Strict"), "{o:?}");
        let o = planned(&f, 2, None).await;
        assert!(
            [3].contains(&o.target_host) && o.detail.contains("Strict"),
            "{o:?}"
        );
        // Host 9 alone on arm64: its pages go to amd64 under the cross limits.
        let f = fixture(&[1, 2, 3]);
        let o = planned(&f, 1, None).await;
        assert!(
            [2, 3].contains(&o.target_host) && o.detail.contains("Cross"),
            "{o:?}"
        );
        // The other way round, an amd64 page with only an arm64 peer.
        let f = fixture(&[1, 2]);
        let o = planned(&f, 2, None).await;
        assert!(o.target_host == 1 && o.detail.contains("Cross"), "{o:?}");
        // A Windows host is amd64 whatever platform row its version had.
        let f = fixture(&[5, 2, 9]);
        let o = planned(&f, 5, None).await;
        assert!(o.target_host == 2 && o.detail.contains("Strict"), "{o:?}");
    }

    #[tokio::test]
    async fn the_peer_must_be_trusted_active_not_held_and_of_a_known_family() {
        let f = fixture(&[1, 2, 6]);
        // Host 6's row names no vendor: not a peer. Host 7 isn't on the trust list.
        let o = planned(&f, 1, None).await;
        assert_eq!(o.target_host, 2, "{o:?}");
        // The only trusted peer is not asking for work.
        let o = planned(&f, 1, Some(&set(&[1, 7, 6]))).await;
        assert_eq!(o.status, Status::Refused, "{o:?}");
        assert!(o.detail.contains("no other active host"), "{o:?}");
        let o = planned(&f, 1, Some(&set(&[2]))).await;
        assert_eq!(o.target_host, 2, "{o:?}");
        // A held host is no peer, and its own pages are not replayed.
        Trust::hold_host(
            &f.paths.trust,
            IMAGE,
            2,
            Hold {
                at: "2026-11-14T00:00:00Z".into(),
                job: "c".repeat(64),
                peer: 9,
                reason: "test".into(),
            },
        )
        .unwrap();
        let o = planned(&f, 1, None).await;
        assert_eq!(o.status, Status::Refused, "{o:?}");
        let o = planned(&f, 2, None).await;
        assert!(
            o.detail.starts_with("source:") && o.detail.contains("held"),
            "{o:?}"
        );
        // A source whose row names no family, or that trust doesn't know, is refused.
        let o = planned(&f, 6, None).await;
        assert!(o.detail.contains("platform family"), "{o:?}");
        let o = planned(&f, 7, None).await;
        assert!(o.detail.starts_with("source:"), "{o:?}");
        // A page with no canonical result yet.
        let Canonical::Fixed(hosts) = &f.paths.canonical else {
            unreachable!()
        };
        hosts.lock().unwrap().clear();
        let o = submit_one(&f.paths, &trust(&f), &f.plat, None, &f.job.id, false)
            .await
            .unwrap();
        assert!(o.detail.contains("no canonical result"), "{o:?}");
    }

    #[tokio::test]
    async fn a_replay_is_made_once_and_only_for_an_ordinary_page() {
        let f = fixture(&[1, 9, 2]);
        let c = submitted(&f, 1, 9, Pairing::Strict);
        let o = submit_one(&f.paths, &trust(&f), &f.plat, None, &f.job.id, false)
            .await
            .unwrap();
        assert_eq!(
            (o.status, o.target_host, o.detail.as_str()),
            (Status::Submitted, c.target, "already submitted")
        );
        // A line job or a bundled page is passed over.
        let g = fixture(&[1, 9]);
        let mut record = load(&g.paths.jobs_root, &g.job.id).unwrap();
        record.bundle = Some("b".repeat(64));
        crate::save(&g.paths.jobs_root, &record).unwrap();
        let o = planned(&g, 1, None).await;
        assert_eq!(o.status, Status::Refused, "{o:?}");
    }

    #[tokio::test]
    async fn a_pair_within_its_limits_matches_and_holds_nobody() {
        let f = fixture(&[1, 9, 2]);
        let c = submitted(&f, 1, 9, Pairing::Strict);
        results(&f, &c, PAGE, PAGE);
        let o = compare(&f, &c).await;
        assert_eq!(o.status, Status::Match, "{o:?}");
        assert!(trust(&f).held_host(IMAGE, 1).is_none() && trust(&f).held_host(IMAGE, 9).is_none());
        // Judged: its files left the results directory, and it is never compared again.
        assert!(!f
            .paths
            .spool
            .join(format!("{}__file_text.txt", workunit(&c.job, 1, 9)))
            .exists());
        assert_eq!(compare(&f, &c).await.status, Status::Match);
    }

    #[tokio::test]
    async fn the_limits_follow_the_pairing_recorded_with_the_replay() {
        let near = PAGE
            .replace("committee", "commiftee")
            .replace("petition", "petifion");
        // The same two texts: outside the strict limits, inside the cross ones.
        let f = fixture(&[1, 2]);
        let c = submitted(&f, 2, 1, Pairing::Cross);
        results(&f, &c, PAGE, &near);
        let o = compare(&f, &c).await;
        assert_eq!(o.status, Status::Match, "{o:?}");
        let g = fixture(&[2, 3]);
        let c = submitted(&g, 2, 3, Pairing::Strict);
        results(&g, &c, PAGE, &near);
        let o = compare(&g, &c).await;
        assert_eq!(o.status, Status::Mismatch, "{o:?}");
        assert!(o.detail.contains("Strict"), "{o:?}");
    }

    #[tokio::test]
    async fn a_mismatch_holds_both_hosts_for_the_image_and_is_never_retried_away() {
        let f = fixture(&[1, 2, 9]);
        let c = submitted(&f, 2, 1, Pairing::Cross);
        let garbled = "zzzz qqqq xxxx";
        results(&f, &c, PAGE, garbled);
        let o = compare(&f, &c).await;
        assert_eq!(o.status, Status::Mismatch, "{o:?}");
        let t = trust(&f);
        for host in [1, 2] {
            let held = t.held_host(IMAGE, host).unwrap();
            assert!(held.contains("container spot check"), "{held}");
        }
        // The held hosts' results are refused by the collect gate; the uninvolved host is not.
        assert!(t.refuse_job(&f.job, Some(2)).unwrap().contains("held"));
        assert!(t.refuse_job(&f.job, Some(1)).unwrap().contains("held"));
        assert!(t.refuse_job(&f.job, Some(9)).is_none());
        // Compared again, the verdict stands, and a released hold stays released.
        Trust::release_hold(&f.paths.trust, IMAGE, 2, "looked").unwrap();
        assert_eq!(compare(&f, &c).await.status, Status::Mismatch);
        assert!(trust(&f).held_host(IMAGE, 2).is_none());
    }

    #[tokio::test]
    async fn a_run_stopped_between_the_verdict_and_the_holds_completes_them() {
        let f = fixture(&[1, 2]);
        let c = submitted(&f, 2, 1, Pairing::Cross);
        results(&f, &c, PAGE, "zzzz qqqq xxxx");
        let path = journal_path(&f.paths.root, &c.job, c.target);
        let mut journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        journal.image = Some(IMAGE.into());
        journal.stage = "holding".into();
        journal.verdict = Some(outcome(&c.job, 2, 1, Status::Mismatch, "cut off"));
        atomic(&path, &serde_json::to_vec(&journal).unwrap()).unwrap();
        assert_eq!(compare(&f, &c).await.status, Status::Mismatch);
        assert!(trust(&f).held_host(IMAGE, 1).is_some() && trust(&f).held_host(IMAGE, 2).is_some());
    }

    #[tokio::test]
    async fn a_replay_that_is_not_a_valid_result_holds_both_hosts() {
        let f = fixture(&[1, 2]);
        let c = submitted(&f, 2, 1, Pairing::Cross);
        returned(&f.job, &c.spool, &f.job.workunit(), PAGE);
        let unit = workunit(&c.job, 2, 1);
        returned(&f.job, &f.paths.spool, &unit, PAGE);
        std::fs::write(
            f.paths.spool.join(format!("{unit}__file_text.txt")),
            "changed",
        )
        .unwrap();
        let o = compare(&f, &c).await;
        assert_eq!(o.status, Status::Mismatch, "{o:?}");
        assert!(o.detail.contains("not a valid result"), "{o:?}");
    }

    #[tokio::test]
    async fn a_failed_pending_or_unverifiable_replay_holds_nobody() {
        let f = fixture(&[1, 2]);
        let c = submitted(&f, 2, 1, Pairing::Cross);
        assert_eq!(compare(&f, &c).await.status, Status::Pending);
        // The unit failed in BOINC: no fault of either host.
        let unit = workunit(&c.job, 2, 1);
        std::fs::write(f.paths.spool.join(format!("{unit}_error")), "").unwrap();
        assert_eq!(compare(&f, &c).await.status, Status::Failed);
        assert!(trust(&f).held_host(IMAGE, 1).is_none() && trust(&f).held_host(IMAGE, 2).is_none());
        // A result BOINC credits to a different host than the one named is no verdict.
        let g = fixture(&[1, 2]);
        let c = submitted(&g, 2, 1, Pairing::Cross);
        results(&g, &c, PAGE, "zzzz qqqq xxxx");
        canonical(&g, &workunit(&c.job, 2, 1), 2);
        let o = compare(&g, &c).await;
        assert_eq!(o.status, Status::Unverified, "{o:?}");
        assert!(trust(&g).held_host(IMAGE, 1).is_none());
        // A host no longer trusted when the replay is back: unverified.
        let h = fixture(&[2]);
        let c = submitted(&h, 2, 1, Pairing::Cross);
        results(&h, &c, PAGE, PAGE);
        assert_eq!(compare(&h, &c).await.status, Status::Unverified);
    }

    #[tokio::test]
    async fn the_budget_defers_what_it_does_not_reach_and_writes_nothing_for_it() {
        let f = fixture(&[1, 9, 2]);
        let c = submitted(&f, 1, 9, Pairing::Strict);
        results(&f, &c, PAGE, "zzzz qqqq xxxx");
        let other = "e".repeat(64);
        let pages = vec![other.clone()];
        let run = |budget: Duration| {
            container_spot(
                &f.paths,
                &f.plat,
                None,
                &pages,
                std::slice::from_ref(&c),
                budget,
                true,
            )
        };
        // No time at all: neither the comparison nor the replay is started.
        let report = run(Duration::ZERO).await.unwrap();
        assert_eq!(report.checked[0].status, Status::Deferred);
        assert_eq!(report.submitted[0].status, Status::Deferred);
        assert!(
            trust(&f).held_host(IMAGE, 1).is_none(),
            "a deferred check holds nobody"
        );
        assert!(!job_dir(&f.paths.root, &other).exists());
        // With time, the comparison runs first and holds the pair; the unknown page is refused.
        let report = run(Duration::from_secs(60)).await.unwrap();
        assert_eq!(report.checked[0].status, Status::Mismatch);
        assert_eq!(report.submitted[0].status, Status::Refused);
        assert!(trust(&f).held_host(IMAGE, 1).is_some());
        assert!(container_spot(
            &f.paths,
            &f.plat,
            None,
            &[],
            &[],
            Duration::from_secs(MAX_BUDGET_SECS + 1),
            false
        )
        .await
        .is_err());
    }

    #[test]
    fn items_parse_from_the_bridge_variables() {
        let id = "a".repeat(64);
        assert_eq!(parse_submit(&format!("{id},{id}")).unwrap().len(), 2);
        assert!(parse_submit("").unwrap().is_empty());
        assert!(parse_submit("xyz").is_err());
        let checks = parse_check(&format!("{id}=1:9:/p/results/7")).unwrap();
        assert_eq!((checks[0].source, checks[0].target), (1, 9));
        assert_eq!(checks[0].spool, Path::new("/p/results/7"));
        for bad in [
            format!("{id}=1:1:/p/results/7"),
            format!("{id}=1:9:/p/../results/7"),
            format!("{id}=1:9:results/7"),
            format!("{id}=1:9"),
            "x=1:2:/p/results/7".into(),
        ] {
            assert!(parse_check(&bad).is_err(), "{bad}");
        }
        let many = vec![id.clone(); MAX_ITEMS + 1].join(",");
        assert!(parse_submit(&many).is_err());
    }
}
