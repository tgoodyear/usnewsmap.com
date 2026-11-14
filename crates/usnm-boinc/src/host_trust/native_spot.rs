//! Native OCR spot checks (#1863, docs/boinc/apps/native-ocr.md): a native page a trusted host
//! returned is read again by a second host trusted for the same build, or for a build
//! `native-build-trust.json` lists as identical, and the two results are compared byte for byte,
//! except that ALTO confidence attributes (`WC`, `CC`, `PC`) are left out (#1835).
//!
//! The controller (`boinc/scripts/batch_scheduler.py`) picks which pages, after their results
//! came back and were published, so a host can't tell at dispatch which of its units will be
//! checked. This module creates the replay units and compares their results with the
//! originals. A mismatch holds both hosts' trust for their builds in `trust.json`
//! ([`crate::trust::Trust::hold_host`]): the comparison can't say which of the two is wrong.
//! Each replay is journaled under `<root>/native-spots/<job>/<target host>/`, and a compared
//! one is never compared again, so a mismatch stays on record and is never retried away.
use super::spot::{agree, fetch, job_inputs, judge, stage};
use crate::trust::{BuildTrust, Hold, Trust};
use crate::{atomic, load, read, Lock};
use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use usnm_ocr_contract::boinc;

/// The longest list of replays or checks one call takes.
pub const MAX_ITEMS: usize = 100;

/// A replay to create: `job`, which `source_host` returned, read again by `target_host` on
/// `target_image` (the job's own build, or one listed as identical to it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Replay {
    pub job: String,
    pub source_host: u64,
    pub target_host: u64,
    pub target_image: String,
    /// For a check: the assimilator results directory that holds the original result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_spool: Option<PathBuf>,
}

impl Replay {
    /// Parses `<job>=<source>:<target>:<target image hex>[:<original spool>]`, comma-separated.
    pub fn parse_list(text: &str, spool: bool) -> anyhow::Result<Vec<Self>> {
        if text.trim().is_empty() {
            return Ok(Vec::new());
        }
        let items: Vec<_> = text
            .split(',')
            .map(|item| Self::parse(item, spool))
            .collect::<anyhow::Result<_>>()?;
        ensure!(
            items.len() <= MAX_ITEMS,
            "at most {MAX_ITEMS} native spot items a call"
        );
        Ok(items)
    }

    fn parse(item: &str, spool: bool) -> anyhow::Result<Self> {
        let (job, rest) = item
            .split_once('=')
            .context("a native spot item is <job>=<source>:<target>:<image>")?;
        let parts: Vec<_> = rest.splitn(4, ':').collect();
        ensure!(
            parts.len() == if spool { 4 } else { 3 },
            "invalid native spot item {item}"
        );
        let replay = Self {
            job: job.to_owned(),
            source_host: parts[0].parse().context("invalid source host")?,
            target_host: parts[1].parse().context("invalid target host")?,
            target_image: format!("sha256:{}", parts[2]),
            original_spool: spool.then(|| PathBuf::from(parts[3])),
        };
        replay.validate()?;
        Ok(replay)
    }

    fn validate(&self) -> anyhow::Result<()> {
        ensure!(boinc::is_sha256(&self.job), "invalid job id");
        ensure!(
            self.source_host > 0 && self.target_host > 0 && self.source_host != self.target_host,
            "a native spot check needs two different hosts"
        );
        ensure!(
            self.target_image
                .strip_prefix("sha256:")
                .is_some_and(boinc::is_sha256),
            "invalid target image"
        );
        if let Some(spool) = &self.original_spool {
            let text = spool.to_string_lossy();
            ensure!(
                spool.is_absolute() && text.contains("/results/") && !text.contains(".."),
                "invalid original spool"
            );
        }
        Ok(())
    }

    /// The replay's BOINC workunit, named for the job and both hosts, as container spot units are.
    pub fn workunit(&self) -> String {
        format!(
            "usnm_spot_{}_{}_{}",
            self.job, self.source_host, self.target_host
        )
    }
}

/// What happened to one replay or check.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// The replay unit exists (made now, or before).
    Submitted,
    /// Nothing was made: no peer can take it (trust, build, inputs). `detail` says why.
    Refused,
    /// The replay hasn't come back yet.
    Pending,
    /// The replay unit failed in BOINC: no comparison, and no fault of the original.
    Failed,
    /// The results couldn't be compared (an original or a host's trust gone). Not a verdict.
    Unverified,
    Match,
    /// Different bytes within one build or a listed pair: both hosts are held.
    Mismatch,
    /// Left for the next call: the call's time budget was spent before this item (a container
    /// spot check, #1934). Not a verdict, and nothing was made or written for it.
    Deferred,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Outcome {
    pub job: String,
    pub source_host: u64,
    pub target_host: u64,
    pub status: Status,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Report {
    pub submitted: Vec<Outcome>,
    pub checked: Vec<Outcome>,
}

/// Where the replays run and are read from.
pub struct Paths {
    /// This step's journal root (`$SHARE_DIR/host-trust`).
    pub root: PathBuf,
    pub project: PathBuf,
    /// The bridge's job journals (`$HANDOFF_DIR`): each original's request and scan.
    pub jobs_root: PathBuf,
    pub trust: PathBuf,
    /// The replays' assimilator results directory (batch 0).
    pub spool: PathBuf,
    /// Where each workunit's canonical result's host is read from.
    pub canonical: Canonical,
}

/// The host BOINC recorded for a workunit's canonical result. The two hosts of a check are
/// taken from the controller's list, so each is checked against BOINC before a result is
/// attributed to it.
pub enum Canonical {
    Db {
        mysql_defaults: PathBuf,
        mysql_ca: PathBuf,
        database: String,
    },
    #[cfg(test)]
    Fixed(std::sync::Mutex<std::collections::BTreeMap<String, u64>>),
}

impl Canonical {
    pub(super) async fn host(&self, workunit: &str) -> anyhow::Result<Option<u64>> {
        ensure!(
            workunit
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid workunit"
        );
        match self {
            Self::Db {
                mysql_defaults,
                mysql_ca,
                database,
            } => {
                let rows = super::query_db(
                    mysql_defaults,
                    mysql_ca,
                    database,
                    &format!("SELECT r.hostid FROM workunit w JOIN result r ON r.id=w.canonical_resultid WHERE w.name='{workunit}'"),
                )
                .await?;
                let hosts = super::hosts(&rows)?;
                ensure!(
                    hosts.len() <= 1 && rows.lines().count() <= 1,
                    "ambiguous canonical result for {workunit}"
                );
                Ok(hosts.first().copied())
            }
            #[cfg(test)]
            Self::Fixed(hosts) => Ok(hosts.lock().unwrap().get(workunit).copied()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Journal {
    replay: Replay,
    workunit: String,
    stage: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verdict: Option<Outcome>,
    /// The original's build, recorded with a mismatch so its holds can be completed again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    source_image: Option<String>,
}

fn journal_path(root: &Path, r: &Replay) -> PathBuf {
    root.join("native-spots")
        .join(&r.job)
        .join(r.target_host.to_string())
        .join("journal.json")
}

/// The job as the target reads it: the original, or for a build listed as identical, the same
/// page and settings under that build's digest (a job of its own id).
fn target_job(job: &boinc::Job, image: &str) -> anyhow::Result<boinc::Job> {
    if job.engine.image == image {
        return Ok(job.clone());
    }
    let mut derived = job.clone();
    derived.engine.image = image.to_owned();
    derived.id = derived.identity()?;
    Ok(derived)
}

fn outcome(r: &Replay, status: Status, detail: impl Into<String>) -> Outcome {
    Outcome {
        job: r.job.clone(),
        source_host: r.source_host,
        target_host: r.target_host,
        status,
        detail: detail.into(),
    }
}

/// Why `r` may not be replayed or compared now, if it may not: both hosts must be trusted for
/// their builds (not held), on a build with a passing challenge, and the two builds comparable.
fn refuse(
    trust: &Trust,
    builds: &BuildTrust,
    job: &boinc::Job,
    target: &boinc::Job,
    r: &Replay,
) -> Option<String> {
    if !builds.comparable(&job.engine.image, &target.engine.image) {
        return Some(format!(
            "builds {} and {} are not listed as identical in native-build-trust.json",
            job.engine.image, target.engine.image
        ));
    }
    if let Some(reason) = trust.refuse_native_job(job, Some(r.source_host), builds) {
        return Some(format!("source: {reason}"));
    }
    trust
        .refuse_native_job(target, Some(r.target_host), builds)
        .map(|reason| format!("peer: {reason}"))
}

fn create_args(workunit: &str, target_host: u64, version: u32) -> Vec<String> {
    [
        "--appname",
        crate::submit::NATIVE_APP,
        "--wu_name",
        workunit,
        "--wu_template",
        "templates/usnm_ocr_in",
        "--result_template",
        "templates/usnm_ocr_out",
        "--min_quorum",
        "1",
        "--target_nresults",
        "1",
        "--max_error_results",
        "1",
        "--max_total_results",
        "1",
        "--max_success_results",
        "1",
        "--target_host",
        &target_host.to_string(),
        "--app_version_num",
        &version.to_string(),
        "--rsc_fpops_est",
        "1e12",
        "--rsc_fpops_bound",
        "1e14",
        "--rsc_memory_bound",
        "1610612736",
        "--rsc_disk_bound",
        "1073741824",
        "--delay_bound",
        "259200",
        &format!("{workunit}_page"),
        &format!("{workunit}_request_json"),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}

/// Creates the replay unit for `r`, or reports why not. Without `execute` nothing is written.
async fn submit_one(
    p: &Paths,
    trust: &Trust,
    builds: &BuildTrust,
    versions: &[u32],
    r: &Replay,
    execute: bool,
) -> anyhow::Result<Outcome> {
    let path = journal_path(&p.root, r);
    if path.exists() {
        let old: Journal = serde_json::from_slice(&read(&path, 64 << 10)?)?;
        ensure!(
            old.replay.job == r.job
                && old.replay.source_host == r.source_host
                && old.replay.target_image == r.target_image,
            "native spot journal changed"
        );
        ensure!(
            old.stage != "submitting",
            "native spot submission uncertain; reconcile {} by hand",
            old.workunit
        );
        return Ok(outcome(r, Status::Submitted, "already submitted"));
    }
    // The original's journal goes when its history is archived: that replay can't be made.
    let job = match load(&p.jobs_root, &r.job) {
        Ok(record) => record.job,
        Err(error) => {
            return Ok(outcome(
                r,
                Status::Refused,
                format!("original journal: {error:#}"),
            ))
        }
    };
    if job.lines.is_some() {
        return Ok(outcome(r, Status::Refused, "a line job is not native work"));
    }
    match p.canonical.host(&job.workunit()).await {
        Ok(Some(host)) if host == r.source_host => {}
        Ok(host) => {
            return Ok(outcome(
                r,
                Status::Refused,
                format!(
                    "the original's canonical result is host {host:?}'s, not host {}'s",
                    r.source_host
                ),
            ))
        }
        Err(error) => {
            return Ok(outcome(
                r,
                Status::Refused,
                format!("canonical host: {error:#}"),
            ))
        }
    }
    let target = target_job(&job, &r.target_image)?;
    if let Some(reason) = refuse(trust, builds, &job, &target, r) {
        return Ok(outcome(r, Status::Refused, reason));
    }
    let version = match crate::submit::native_version(&p.project, &target.engine.image, versions) {
        Ok(version) => version,
        Err(error) => return Ok(outcome(r, Status::Refused, format!("{error:#}"))),
    };
    let (scan, original_request) = match job_inputs(&p.jobs_root, &job) {
        Ok(inputs) => inputs,
        Err(error) => return Ok(outcome(r, Status::Refused, format!("inputs: {error:#}"))),
    };
    if !execute {
        return Ok(outcome(r, Status::Submitted, "would submit"));
    }
    ensure!(
        crate::gold::submit::assignment_enabled(&std::fs::read_to_string(
            p.project.join("config.xml")
        )?)?,
        "native spot checks need <enable_assignment>1</enable_assignment>"
    );
    let request = if target == job {
        original_request
    } else {
        serde_json::to_vec(&target)?
    };
    let workunit = r.workunit();
    let mut journal = Journal {
        replay: r.clone(),
        workunit: workunit.clone(),
        stage: "submitting".into(),
        verdict: None,
        source_image: None,
    };
    stage(&p.project, &workunit, scan, request).await?;
    atomic(&path, &serde_json::to_vec_pretty(&journal)?)?;
    let status = Command::new(p.project.join("bin/create_work"))
        .current_dir(&p.project)
        .args(create_args(&workunit, r.target_host, version))
        .status()
        .await?;
    ensure!(
        status.success(),
        "native spot submission of {workunit} uncertain; reconcile by hand"
    );
    journal.stage = "submitted".into();
    atomic(&path, &serde_json::to_vec_pretty(&journal)?)?;
    Ok(outcome(
        r,
        Status::Submitted,
        format!("{workunit} pinned to version {version}"),
    ))
}

/// Compares `r`'s replay with its original, once both are there, and holds both hosts on a
/// mismatch. A recorded verdict is returned as it was, and a recorded mismatch's holds are
/// completed again, so a run stopped between the verdict and the holds loses neither.
async fn check_one(
    p: &Paths,
    trust: &Trust,
    builds: &BuildTrust,
    r: &Replay,
    execute: bool,
) -> anyhow::Result<Outcome> {
    let path = journal_path(&p.root, r);
    ensure!(
        path.exists(),
        "no native spot journal for {} on host {}",
        r.job,
        r.target_host
    );
    let mut journal: Journal = serde_json::from_slice(&read(&path, 64 << 10)?)?;
    ensure!(
        journal.replay.job == r.job
            && journal.replay.source_host == r.source_host
            && journal.replay.target_image == r.target_image,
        "native spot journal changed"
    );
    if let Some(verdict) = journal.verdict.clone() {
        // Stage `holding`: a run stopped between the verdict and its holds. Once the holds are
        // in (`compared`), they are never written again, so a released hold stays released.
        if journal.stage == "holding" && execute {
            let source = journal
                .source_image
                .clone()
                .context("a recorded mismatch names the original's build")?;
            hold_both(p, r, &source)?;
            return finish(p, &path, &mut journal, verdict, execute);
        }
        // Stage `keeping`: the verdict is in, its files not all moved yet.
        if journal.stage == "keeping" && execute {
            return finish(p, &path, &mut journal, verdict, execute);
        }
        return Ok(verdict);
    }
    ensure!(
        journal.stage == "submitted",
        "native spot {} is not submitted",
        journal.workunit
    );
    let workunit = journal.workunit.clone();
    if p.spool.join(format!("{workunit}_error")).exists()
        || p.spool.join(format!("{workunit}_no_output_files")).exists()
    {
        return finish(
            p,
            &path,
            &mut journal,
            outcome(r, Status::Failed, "the replay unit failed in BOINC"),
            execute,
        );
    }
    if !p
        .spool
        .join(format!("{workunit}__file_result.json"))
        .exists()
    {
        return Ok(outcome(r, Status::Pending, "not back yet"));
    }
    let job = match load(&p.jobs_root, &r.job) {
        Ok(record) => record.job,
        Err(error) => {
            return finish(
                p,
                &path,
                &mut journal,
                outcome(
                    r,
                    Status::Unverified,
                    format!("original journal: {error:#}"),
                ),
                execute,
            )
        }
    };
    let target = target_job(&job, &r.target_image)?;
    if let Some(reason) = refuse(trust, builds, &job, &target, r) {
        return finish(
            p,
            &path,
            &mut journal,
            outcome(r, Status::Unverified, reason),
            execute,
        );
    }
    // Each result is attributed to the host BOINC recorded for it, not the one the list names.
    for (unit, host) in [
        (job.workunit(), r.source_host),
        (workunit.clone(), r.target_host),
    ] {
        match p.canonical.host(&unit).await {
            Ok(Some(found)) if found == host => {}
            Ok(found) => {
                let detail =
                    format!("{unit}'s canonical result is host {found:?}'s, not host {host}'s");
                return finish(
                    p,
                    &path,
                    &mut journal,
                    outcome(r, Status::Unverified, detail),
                    execute,
                );
            }
            Err(error) => {
                return Ok(outcome(
                    r,
                    Status::Pending,
                    format!("canonical host: {error:#}"),
                ))
            }
        }
    }
    let spool = r
        .original_spool
        .as_deref()
        .context("a check needs the original's spool")?;
    // A file that can't be read is the filesystem's fault, not a host's: the original gone is
    // no verdict, anything else is tried again on the next call.
    let original = match fetch(spool, &job.workunit()) {
        Ok(files) => files,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return finish(
                p,
                &path,
                &mut journal,
                outcome(r, Status::Unverified, format!("original: {error}")),
                execute,
            )
        }
        Err(error) => return Ok(outcome(r, Status::Pending, format!("original: {error}"))),
    };
    let original = match judge(&job, original) {
        Ok(original) => original,
        Err(error) => {
            return finish(
                p,
                &path,
                &mut journal,
                outcome(r, Status::Unverified, format!("original: {error:#}")),
                execute,
            )
        }
    };
    let replay = match fetch(&p.spool, &workunit) {
        Ok(files) => files,
        Err(error) => return Ok(outcome(r, Status::Pending, format!("replay: {error}"))),
    };
    let replay = match judge(&target, replay) {
        Ok(replay) => replay,
        // The replay came back, readable, but isn't a valid result for its job: as much a fault
        // as different bytes, since the peer was trusted.
        Err(error) => {
            return mismatch(
                p,
                &path,
                &mut journal,
                r,
                &job,
                format!("the replay is not a valid result: {error:#}"),
                execute,
            )
        }
    };
    if agree(builds, &original, &replay, true)? {
        return finish(
            p,
            &path,
            &mut journal,
            outcome(
                r,
                Status::Match,
                "text bytes and ALTO (without confidences) identical",
            ),
            execute,
        );
    }
    let detail = format!(
        "text {} against {}, ALTO without confidences {} against {}",
        original.text_sha256,
        replay.text_sha256,
        original.alto_exact_sha256,
        replay.alto_exact_sha256
    );
    mismatch(p, &path, &mut journal, r, &job, detail, execute)
}

/// Records the mismatch, then holds both hosts: the verdict is journaled first, so a run that
/// stops before the holds completes them the next time it reads the verdict.
fn mismatch(
    p: &Paths,
    path: &Path,
    journal: &mut Journal,
    r: &Replay,
    job: &boinc::Job,
    detail: String,
    execute: bool,
) -> anyhow::Result<Outcome> {
    let verdict = outcome(r, Status::Mismatch, detail);
    if !execute {
        return Ok(verdict);
    }
    journal.source_image = Some(job.engine.image.clone());
    journal.stage = "holding".into();
    journal.verdict = Some(verdict.clone());
    atomic(path, &serde_json::to_vec_pretty(journal)?)?;
    hold_both(p, r, &job.engine.image)?;
    finish(p, path, journal, verdict, execute)
}

/// Holds the source host for its build and the peer for its own; a hold already there stays.
fn hold_both(p: &Paths, r: &Replay, source_image: &str) -> anyhow::Result<()> {
    let at = chrono::Utc::now().to_rfc3339();
    for (image, host, peer) in [
        (source_image, r.source_host, r.target_host),
        (r.target_image.as_str(), r.target_host, r.source_host),
    ] {
        Trust::hold_host(
            &p.trust,
            image,
            host,
            Hold {
                at: at.clone(),
                job: r.job.clone(),
                peer,
                reason: "native spot check: results differ within one build".into(),
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
        // The verdict is journaled before the files move, so a run stopped in between keeps it
        // and finishes the move the next time it reads it.
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

/// Moves a judged replay's files out of the results directory into its journal's: they stay as
/// evidence, and don't pile up where the batch inventory counts entries.
pub(super) fn keep_replay_files(spool: &Path, workunit: &str, into: &Path) -> anyhow::Result<()> {
    let kept = into.join("replay");
    for suffix in [
        "__file_result.json",
        "__file_text.txt",
        "__file_layout.xml",
        "_error",
        "_no_output_files",
    ] {
        let from = spool.join(format!("{workunit}{suffix}"));
        match std::fs::symlink_metadata(&from) {
            Ok(meta) if meta.is_file() => {
                std::fs::create_dir_all(&kept)?;
                let to = kept.join(format!("{workunit}{suffix}"));
                if std::fs::rename(&from, &to).is_err() {
                    atomic(&to, &std::fs::read(&from)?)?;
                    std::fs::remove_file(&from)?;
                }
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

/// Creates the `submit` replays and compares the `check` ones, under the journal root's lock.
/// One item's problem is that item's outcome; only a fault that leaves a unit uncertain fails
/// the call. Without `execute` nothing is created or written.
pub async fn native_spot(
    p: &Paths,
    builds: &BuildTrust,
    versions: &[u32],
    submit: &[Replay],
    check: &[Replay],
    execute: bool,
) -> anyhow::Result<Report> {
    ensure!(
        submit.len() <= MAX_ITEMS && check.len() <= MAX_ITEMS,
        "at most {MAX_ITEMS} native spot items a call"
    );
    for path in [&p.root, &p.project, &p.jobs_root, &p.trust, &p.spool] {
        ensure!(path.is_absolute(), "native spot paths must be absolute");
    }
    let _lock = Lock::take(&p.root)?;
    let mut report = Report::default();
    for r in check {
        ensure!(
            r.original_spool.is_some(),
            "a check needs the original's spool"
        );
        // Read again for each: a mismatch just above may have held a host.
        let trust = Trust::load(&p.trust)?;
        report
            .checked
            .push(check_one(p, &trust, builds, r, execute).await?);
    }
    let trust = Trust::load(&p.trust)?;
    for r in submit {
        if r.original_spool.is_some() {
            bail!("a replay to submit takes no spool");
        }
        report
            .submitted
            .push(submit_one(p, &trust, builds, versions, r, execute).await?);
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::super::spot::tests::{job, returned};
    use super::*;
    use crate::trust::builds::with_build;

    const LINUX: &str = "sha256:4d6e2a3be41b3b9c9cf50e6404c6f056177cef547e9d9675298ee1fec94cc5d1";

    struct Fixture {
        _tmp: tempfile::TempDir,
        paths: Paths,
        job: boinc::Job,
        builds: BuildTrust,
        other: String,
    }

    /// A project with a native version of the job's build (and of `other`, listed as identical
    /// to it), the job's journal and inputs, and a trust list naming hosts 13 and 240.
    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let mut job = job("eng", "eng");
        job.engine.image = LINUX.into();
        job.id = job.identity().unwrap();
        let other = format!("sha256:{}", "7".repeat(64));
        let mut builds = with_build(
            BuildTrust::embedded().unwrap().clone(),
            &other,
            "x86_64-pc-linux-gnu",
            &"b".repeat(64),
            0.1,
            Some(100),
        );
        builds.builds.get_mut(LINUX).unwrap().challenges[0].manifest_sha256 = "b".repeat(64);
        builds.builds.get_mut(LINUX).unwrap().challenges[0].max_cer = 0.1;
        let (lo, hi) = if LINUX < other.as_str() {
            (LINUX.to_owned(), other.clone())
        } else {
            (other.clone(), LINUX.to_owned())
        };
        builds.equivalent.push(crate::trust::builds::Equivalence {
            digests: [lo, hi],
            evidence: "a test pair".into(),
        });
        let trust = serde_json::json!({
            "hosts": {LINUX: [13, 240], other.clone(): [300]},
            "manifest_sha256": "b".repeat(64),
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": 0.1, "v": 5
        });
        std::fs::write(root.join("trust.json"), serde_json::to_vec(&trust).unwrap()).unwrap();
        let project = root.join("project");
        for (version, image) in [("1.0", LINUX), ("1.1", other.as_str())] {
            let dir = project
                .join("apps")
                .join(crate::submit::NATIVE_APP)
                .join(version)
                .join("x86_64-pc-linux-gnu");
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(
                dir.join(format!("usnm_native_build_{}.json", &image[7..])),
                "{}",
            )
            .unwrap();
        }
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
            plan: "native".into(),
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
        for d in ["results/0", "results/7", "host-trust"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let paths = Paths {
            root: root.join("host-trust"),
            project,
            jobs_root: jobs,
            trust: root.join("trust.json"),
            spool: root.join("results/0"),
            canonical: Canonical::Fixed(Default::default()),
        };
        if let Canonical::Fixed(hosts) = &paths.canonical {
            hosts.lock().unwrap().insert(job.workunit(), 13);
        }
        Fixture {
            _tmp: tmp,
            paths,
            job,
            builds,
            other,
        }
    }

    fn replay(f: &Fixture, target: u64, image: &str, spool: bool) -> Replay {
        Replay {
            job: f.job.id.clone(),
            source_host: 13,
            target_host: target,
            target_image: image.into(),
            original_spool: spool.then(|| f.paths.spool.parent().unwrap().join("7")),
        }
    }

    /// Writes a submitted journal for `r`, as `submit_one` leaves it.
    fn submitted(f: &Fixture, r: &Replay) {
        let mut plain = r.clone();
        plain.original_spool = None;
        let journal = Journal {
            replay: plain,
            workunit: r.workunit(),
            stage: "submitted".into(),
            verdict: None,
            source_image: None,
        };
        canonical(f, &r.workunit(), r.target_host);
        atomic(
            &journal_path(&f.paths.root, r),
            &serde_json::to_vec(&journal).unwrap(),
        )
        .unwrap();
    }

    fn block<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future)
    }

    /// Records host `host` as the canonical host of `workunit` in BOINC.
    fn canonical(f: &Fixture, workunit: &str, host: u64) {
        let Canonical::Fixed(hosts) = &f.paths.canonical else {
            unreachable!()
        };
        hosts.lock().unwrap().insert(workunit.to_owned(), host);
    }

    fn trust(f: &Fixture) -> Trust {
        Trust::load(&f.paths.trust).unwrap()
    }

    #[test]
    fn items_parse_from_the_bridge_variables() {
        let id = "a".repeat(64);
        let list = Replay::parse_list(&format!("{id}=13:240:{}", "b".repeat(64)), false).unwrap();
        assert_eq!(list[0].target_image, format!("sha256:{}", "b".repeat(64)));
        let checks = Replay::parse_list(
            &format!("{id}=13:240:{}:/p/results/7", "b".repeat(64)),
            true,
        )
        .unwrap();
        assert_eq!(
            checks[0].original_spool.as_deref(),
            Some(Path::new("/p/results/7"))
        );
        for bad in [
            format!("{id}=13:13:{}", "b".repeat(64)),
            format!("{id}=13:240:xyz"),
            format!("{id}=13:240:{}:/p/../results/7", "b".repeat(64)),
            "x=1:2:3".into(),
        ] {
            assert!(
                Replay::parse_list(&bad, bad.matches(':').count() == 3).is_err(),
                "{bad}"
            );
        }
    }

    #[tokio::test]
    async fn a_peer_on_the_same_or_a_listed_build_is_offered_and_an_unlisted_one_refused() {
        let f = fixture();
        let t = trust(&f);
        // Without --execute: the peer on the same build, and the one on the listed build.
        for (target, image) in [(240, LINUX.to_owned()), (300, f.other.clone())] {
            let o = submit_one(
                &f.paths,
                &t,
                &f.builds,
                &[100, 101],
                &replay(&f, target, &image, false),
                false,
            )
            .await
            .unwrap();
            assert_eq!(o.status, Status::Submitted, "{o:?}");
        }
        // A build not listed as identical: refused, and nothing staged.
        let unlisted = format!("sha256:{}", "9".repeat(64));
        let o = submit_one(
            &f.paths,
            &t,
            &f.builds,
            &[100, 101],
            &replay(&f, 300, &unlisted, false),
            false,
        )
        .await
        .unwrap();
        assert_eq!(o.status, Status::Refused);
        assert!(o.detail.contains("not listed as identical"), "{o:?}");
        // The listed build, once the pair is gone from the file: refused.
        let mut unpaired = f.builds.clone();
        unpaired.equivalent.clear();
        let o = submit_one(
            &f.paths,
            &t,
            &unpaired,
            &[100, 101],
            &replay(&f, 300, &f.other, false),
            false,
        )
        .await
        .unwrap();
        assert_eq!(o.status, Status::Refused);
        // A peer the list doesn't trust for its build: refused.
        let o = submit_one(
            &f.paths,
            &t,
            &f.builds,
            &[100, 101],
            &replay(&f, 241, LINUX, false),
            false,
        )
        .await
        .unwrap();
        assert_eq!(o.status, Status::Refused);
        assert!(o.detail.starts_with("peer:"), "{o:?}");
        // No registered version packages the peer's build: refused.
        let o = submit_one(
            &f.paths,
            &t,
            &f.builds,
            &[100],
            &replay(&f, 300, &f.other, false),
            false,
        )
        .await
        .unwrap();
        assert_eq!(o.status, Status::Refused);
    }

    #[test]
    fn a_match_is_recorded_and_settles_nothing_else() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        assert_eq!(
            block(check_one(&f.paths, &trust(&f), &f.builds, &r, true))
                .unwrap()
                .status,
            Status::Pending
        );
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Match, "{o:?}");
        assert!(trust(&f).holds.is_empty());
        // The verdict is kept: comparing again returns it, whatever the files say now.
        returned(&f.job, &f.paths.spool, &r.workunit(), "OTHER\n");
        assert_eq!(
            block(check_one(&f.paths, &trust(&f), &f.builds, &r, true))
                .unwrap()
                .status,
            Status::Match
        );
    }

    #[test]
    fn a_mismatch_holds_both_hosts_and_is_never_compared_again() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELL0\n");
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Mismatch, "{o:?}");
        let t = trust(&f);
        assert_eq!(
            t.holds[LINUX].keys().copied().collect::<Vec<_>>(),
            vec![13, 240]
        );
        assert_eq!(t.holds[LINUX][&13].peer, 240);
        // Both hosts are refused for the build, in submission and in collection.
        for host in [13, 240] {
            assert!(t.refuse_job(&f.job, Some(host)).unwrap().contains("held"));
        }
        assert!(t.trusted_hosts_for_job(&f.job).is_empty());
        // Restoring identical files doesn't undo it: the verdict stands.
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        assert_eq!(
            block(check_one(&f.paths, &t, &f.builds, &r, true))
                .unwrap()
                .status,
            Status::Mismatch
        );
        // A later replay for either host is refused while the hold stands.
        let next = replay(&f, 240, LINUX, false);
        let mut fresh = next.clone();
        fresh.target_host = 241;
        let rt = tokio::runtime::Runtime::new().unwrap();
        let o = rt
            .block_on(submit_one(&f.paths, &t, &f.builds, &[100], &fresh, false))
            .unwrap();
        assert_eq!(o.status, Status::Refused);
        assert!(o.detail.starts_with("source:"), "{o:?}");
    }

    #[test]
    fn a_release_after_review_lifts_one_hold_and_is_kept_on_record() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELL0\n");
        block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert!(Trust::release_hold(&f.paths.trust, LINUX, 240, "").is_err());
        assert!(Trust::release_hold(&f.paths.trust, LINUX, 999, "looked").is_err());
        let release = Trust::release_hold(
            &f.paths.trust,
            LINUX,
            240,
            "replica disk fault, scan re-read matches host 13",
        )
        .unwrap();
        assert_eq!(release.hold.peer, 13);
        let t = trust(&f);
        assert!(t.held_host(LINUX, 240).is_none());
        assert!(t.held_host(LINUX, 13).is_some());
        let ledger =
            std::fs::read_to_string(f.paths.trust.with_file_name("holds-released.jsonl")).unwrap();
        assert!(ledger.contains("replica disk fault"));
        Trust::release_hold(&f.paths.trust, LINUX, 13, "reviewed").unwrap();
        assert!(trust(&f).holds.is_empty());
        assert!(trust(&f).refuse_job(&f.job, Some(13)).is_none());
    }

    #[test]
    fn a_failed_or_unverifiable_replay_is_no_verdict_and_holds_nobody() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        std::fs::write(f.paths.spool.join(format!("{}_error", r.workunit())), "").unwrap();
        assert_eq!(
            block(check_one(&f.paths, &trust(&f), &f.builds, &r, true))
                .unwrap()
                .status,
            Status::Failed
        );
        let r2 = replay(&f, 241, LINUX, true);
        submitted(&f, &r2);
        returned(&f.job, &f.paths.spool, &r2.workunit(), "HELLO\n");
        // Host 241 isn't trusted, and the original is gone: unverified either way.
        assert_eq!(
            block(check_one(&f.paths, &trust(&f), &f.builds, &r2, true))
                .unwrap()
                .status,
            Status::Unverified
        );
        assert!(trust(&f).holds.is_empty());
    }

    #[test]
    fn holds_survive_a_merge() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELL0\n");
        block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        let held = trust(&f);
        let mut fresh = held.clone();
        fresh.holds.clear();
        assert_eq!(fresh.merge(&held).unwrap().holds, held.holds);
        assert_eq!(held.merge(&fresh).unwrap().holds, held.holds);
    }

    #[test]
    fn each_result_is_attributed_to_the_host_boinc_recorded() {
        let f = fixture();
        // The list says host 13 returned the original, BOINC says host 14: no replay.
        canonical(&f, &f.job.workunit(), 14);
        let o = block(submit_one(
            &f.paths,
            &trust(&f),
            &f.builds,
            &[100],
            &replay(&f, 240, LINUX, false),
            false,
        ))
        .unwrap();
        assert_eq!(o.status, Status::Refused);
        assert!(o.detail.contains("canonical"), "{o:?}");
        // Different bytes from a replay BOINC credits to another host: no verdict, nobody held.
        canonical(&f, &f.job.workunit(), 13);
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        canonical(&f, &r.workunit(), 999);
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELL0\n");
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Unverified, "{o:?}");
        assert!(trust(&f).holds.is_empty());
    }

    #[test]
    fn an_unreadable_replay_file_is_tried_again_not_judged() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        let text = f
            .paths
            .spool
            .join(format!("{}__file_text.txt", r.workunit()));
        std::fs::remove_file(&text).unwrap();
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Pending, "{o:?}");
        assert!(trust(&f).holds.is_empty());
        std::fs::write(&text, "HELLO\n").unwrap();
        assert_eq!(
            block(check_one(&f.paths, &trust(&f), &f.builds, &r, true))
                .unwrap()
                .status,
            Status::Match
        );
    }

    #[test]
    fn a_mismatch_recorded_before_its_holds_completes_them() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        // As a run that stopped after journaling the verdict and before writing the holds.
        let path = journal_path(&f.paths.root, &r);
        let mut journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        journal.source_image = Some(LINUX.into());
        journal.stage = "holding".into();
        journal.verdict = Some(outcome(&r, Status::Mismatch, "recorded"));
        atomic(&path, &serde_json::to_vec(&journal).unwrap()).unwrap();
        assert!(trust(&f).holds.is_empty());
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Mismatch);
        assert_eq!(
            trust(&f).holds[LINUX].keys().copied().collect::<Vec<_>>(),
            vec![13, 240]
        );
        // Once complete, a released hold isn't written again by reading the verdict.
        Trust::release_hold(&f.paths.trust, LINUX, 240, "reviewed").unwrap();
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Mismatch);
        assert!(trust(&f).held_host(LINUX, 240).is_none());
    }

    #[test]
    fn a_judged_replays_files_leave_the_results_directory() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(
            &f.job,
            r.original_spool.as_deref().unwrap(),
            &f.job.workunit(),
            "HELLO\n",
        );
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(std::fs::read_dir(&f.paths.spool).unwrap().count(), 0);
        let kept = journal_path(&f.paths.root, &r)
            .parent()
            .unwrap()
            .join("replay");
        assert!(kept
            .join(format!("{}__file_text.txt", r.workunit()))
            .is_file());
        // The original's files are the plan's, and stay.
        let original = r.original_spool.as_deref().unwrap();
        assert!(original
            .join(format!("{}__file_text.txt", f.job.workunit()))
            .is_file());
    }

    #[test]
    fn an_oversized_replay_is_judged_without_being_read_whole() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        let text = f
            .paths
            .spool
            .join(format!("{}__file_text.txt", r.workunit()));
        std::fs::write(&text, vec![b'a'; boinc::MAX_TEXT + 10]).unwrap();
        let files = super::super::spot::fetch(&f.paths.spool, &r.workunit()).unwrap();
        assert!(super::super::spot::judge(&f.job, files).is_err());
    }

    #[test]
    fn an_archived_original_is_no_replay_and_no_verdict() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        std::fs::remove_dir_all(crate::dir(&f.paths.jobs_root, &f.job.id).unwrap()).unwrap();
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Unverified, "{o:?}");
        let o = block(submit_one(
            &f.paths,
            &trust(&f),
            &f.builds,
            &[100],
            &replay(&f, 241, LINUX, false),
            false,
        ))
        .unwrap();
        assert_eq!(o.status, Status::Refused);
        assert!(o.detail.starts_with("original journal"), "{o:?}");
    }

    #[test]
    fn a_verdict_whose_files_were_not_all_moved_finishes_the_move() {
        let f = fixture();
        let r = replay(&f, 240, LINUX, true);
        submitted(&f, &r);
        returned(&f.job, &f.paths.spool, &r.workunit(), "HELLO\n");
        // As a run that stopped after journaling a match and before moving its files.
        let path = journal_path(&f.paths.root, &r);
        let mut journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        journal.stage = "keeping".into();
        journal.verdict = Some(outcome(&r, Status::Match, "recorded"));
        atomic(&path, &serde_json::to_vec(&journal).unwrap()).unwrap();
        let o = block(check_one(&f.paths, &trust(&f), &f.builds, &r, true)).unwrap();
        assert_eq!(o.status, Status::Match);
        assert_eq!(std::fs::read_dir(&f.paths.spool).unwrap().count(), 0);
        let journal: Journal = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(journal.stage, "compared");
    }
}
