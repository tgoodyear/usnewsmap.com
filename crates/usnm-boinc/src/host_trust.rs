//! Explicitly invoked, bounded host onboarding and independent OCR spot checks.
//! These workflows never change trust, project settings or publication state.
use crate::{atomic, gold, load, read, trust::Trust, Lock};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::process::Command;
use usnm_ocr_contract::boinc::{self, ResultManifest};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub root: PathBuf,
    pub project: PathBuf,
    pub jobs_root: PathBuf,
    pub mysql_defaults: PathBuf,
    pub mysql_ca: PathBuf,
    pub database: String,
    pub trust: PathBuf,
    pub prepared: PathBuf,
    pub engine: gold::job::Engine,
    pub max_hosts: usize,
    pub max_spots: usize,
    /// One job in every N, selected deterministically from its digest.
    pub sample_every: u64,
}

impl Config {
    pub fn parse(path: &Path) -> anyhow::Result<Self> {
        let c: Self = serde_json::from_slice(&read(path, 16 << 10)?)?;
        ensure!(c.root.is_absolute(), "controller root must be absolute");
        Ok(c)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let c = Self::parse(path)?;
        c.validate()?;
        Ok(c)
    }

    fn validate(&self) -> anyhow::Result<()> {
        let c = self;
        ensure!(
            !c.database.is_empty()
                && c.database.len() <= 64
                && c.database
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
            "invalid database"
        );
        ensure!(
            (1..=100).contains(&c.max_hosts),
            "max_hosts must be 1 to 100"
        );
        ensure!(
            (1..=100).contains(&c.max_spots),
            "max_spots must be 1 to 100"
        );
        ensure!(c.sample_every > 0, "sample_every must be positive");
        ensure!(
            c.engine
                .image
                .strip_prefix("sha256:")
                .is_some_and(boinc::is_sha256),
            "immutable worker image required"
        );
        let defaults = std::fs::symlink_metadata(&c.mysql_defaults)?;
        ensure!(defaults.is_file(), "mysql defaults must be a regular file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            ensure!(
                defaults.permissions().mode() & 0o077 == 0,
                "mysql defaults must be private"
            );
        }
        ensure!(c.mysql_ca.is_file(), "mysql CA file missing");
        ensure!(
            [
                &c.root,
                &c.project,
                &c.jobs_root,
                &c.mysql_defaults,
                &c.mysql_ca,
                &c.trust,
                &c.prepared
            ]
            .iter()
            .all(|p| p.is_absolute()),
            "configuration paths must be absolute"
        );
        Ok(())
    }
}

async fn query(c: &Config, sql: &str) -> anyhow::Result<String> {
    let mut command = Command::new("mysql");
    let command = command
        .arg(format!(
            "--defaults-extra-file={}",
            c.mysql_defaults.display()
        ))
        .args(["--ssl", "--ssl-verify-server-cert"])
        .arg(format!("--ssl-ca={}", c.mysql_ca.display()))
        .args(["--batch", "--skip-column-names", &c.database, "-e", sql])
        .kill_on_drop(true);
    let out = tokio::time::timeout(std::time::Duration::from_secs(60), command.output())
        .await
        .context("host inventory query timed out")??;
    // Connector diagnostics can contain credentials. Do not copy them into reports/logs.
    ensure!(
        out.status.success(),
        "read-only host inventory query failed"
    );
    ensure!(
        out.stdout.len() <= 8 << 20,
        "host inventory over size limit"
    );
    String::from_utf8(out.stdout).context("host inventory is not UTF-8")
}

fn hosts(text: &str) -> anyhow::Result<BTreeSet<u64>> {
    text.lines()
        .map(|line| {
            let id: u64 = line.trim().parse().context("invalid host inventory")?;
            ensure!(id > 0, "host must be positive");
            Ok(id)
        })
        .collect()
}

async fn canonical_host(c: &Config, workunit: &str) -> anyhow::Result<u64> {
    ensure!(
        workunit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid workunit"
    );
    let output = query(c, &format!(
        "SELECT r.hostid FROM workunit w JOIN result r ON r.id=w.canonical_resultid WHERE w.name='{workunit}'"
    )).await?;
    ensure!(
        output.lines().count() == 1,
        "missing or ambiguous canonical result"
    );
    let ids = hosts(&output)?;
    ensure!(ids.len() == 1, "missing or ambiguous canonical host");
    Ok(*ids.first().unwrap())
}

/// One BOINC result of a failed gold attempt, kept with the attempt as its diagnostics.
#[derive(Debug, PartialEq, Serialize)]
struct FailedResult {
    result: u64,
    outcome: i64,
    client_state: i64,
    exit_status: i64,
    /// The client's uploaded stderr (BOINC `result.stderr_out`), first 64 KiB.
    stderr: String,
}

#[derive(Debug, PartialEq)]
enum GoldStatus {
    /// The workunit row is gone (purged) or never reached the database; leave it alone.
    Unknown,
    Pending,
    Succeeded,
    Failed(Vec<FailedResult>),
}

const SERVER_STATE_OVER: i64 = 5;
const OUTCOME_SUCCESS: i64 = 1;
const MAX_ATTEMPTS: u32 = 10_000;

fn decode_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    ensure!(
        text.is_ascii() && text.len().is_multiple_of(2),
        "invalid hex"
    );
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).context("invalid hex"))
        .collect()
}

/// Rows of `id, server_state, outcome, client_state, exit_status, HEX(stderr)` from a
/// `workunit LEFT JOIN result` query: no rows means no workunit; a NULL id means no result yet.
fn gold_status(text: &str) -> anyhow::Result<GoldStatus> {
    let mut failed = Vec::new();
    let mut any = false;
    let mut pending = false;
    let mut succeeded = false;
    for line in text.lines() {
        any = true;
        let f: Vec<&str> = line.split('\t').collect();
        ensure!(f.len() == 6, "invalid gold status row");
        if f[0] == "NULL" {
            pending = true;
            continue;
        }
        let n = |s: &str| s.parse::<i64>().context("invalid gold status field");
        let (server_state, outcome) = (n(f[1])?, n(f[2])?);
        if server_state != SERVER_STATE_OVER {
            pending = true;
        } else if outcome == OUTCOME_SUCCESS {
            succeeded = true;
        } else {
            let stderr = if f[5] == "NULL" {
                Vec::new()
            } else {
                decode_hex(f[5])?
            };
            failed.push(FailedResult {
                result: f[0].parse().context("invalid result id")?,
                outcome,
                client_state: n(f[3])?,
                exit_status: n(f[4])?,
                stderr: String::from_utf8_lossy(&stderr).into_owned(),
            });
        }
    }
    Ok(match (any, pending, succeeded) {
        (false, ..) => GoldStatus::Unknown,
        (_, true, _) => GoldStatus::Pending,
        (_, _, true) => GoldStatus::Succeeded,
        _ => GoldStatus::Failed(failed),
    })
}

async fn query_gold_status(c: &Config, workunit: &str) -> anyhow::Result<GoldStatus> {
    ensure!(
        workunit
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_'),
        "invalid workunit"
    );
    gold_status(&query(c, &format!(
        "SELECT r.id, r.server_state, r.outcome, r.client_state, r.exit_status, HEX(LEFT(r.stderr_out, 65536)) \
         FROM workunit w LEFT JOIN result r ON r.workunitid=w.id WHERE w.name='{workunit}' ORDER BY r.id"
    )).await?)
}

/// Attempt 0 keeps the original per-host root; later attempts are fresh campaign roots
/// because a targeted gold campaign root is bound to one assignment.
fn attempt_root(host_root: &Path, attempt: u32) -> PathBuf {
    if attempt == 0 {
        host_root.to_path_buf()
    } else {
        host_root.join("retry").join(attempt.to_string())
    }
}

/// Where this host's next gold unit goes, or None while its latest attempt is pending,
/// succeeded or unknown. A failed attempt never parks the host: its BOINC diagnostics are
/// written to `failure.json` (on `--execute`) and the following attempt is planned.
async fn next_attempt(
    c: &Config,
    host_root: &Path,
    execute: bool,
) -> anyhow::Result<Option<PathBuf>> {
    let mut attempt = 0;
    while attempt_root(host_root, attempt + 1)
        .join("units/0000")
        .is_dir()
    {
        attempt += 1;
        ensure!(attempt < MAX_ATTEMPTS, "too many gold attempts");
    }
    let root = attempt_root(host_root, attempt);
    let unit = gold::submit::units_dir(&root).join("0000");
    // Not yet submitted, or uncertain: gold submission's own journal decides (and refuses).
    if !unit.join("submitted").is_file() {
        return Ok(Some(root));
    }
    let job: gold::job::Job =
        serde_json::from_slice(&read(&unit.join(gold::job::JOB_FILE), gold::job::MAX_JSON)?)?;
    let workunit = job.workunit();
    match query_gold_status(c, &workunit).await? {
        GoldStatus::Failed(results) => {
            if execute {
                let failure = serde_json::json!({ "workunit": workunit, "results": results });
                atomic(
                    &root.join("failure.json"),
                    &serde_json::to_vec_pretty(&failure)?,
                )?;
                eprintln!(
                    "gold attempt {attempt} ({workunit}) failed; diagnostics in {}; issuing another",
                    root.join("failure.json").display()
                );
            }
            Ok(Some(attempt_root(host_root, attempt + 1)))
        }
        GoldStatus::Unknown => {
            eprintln!("{workunit} is not in the database; not reissuing");
            Ok(None)
        }
        GoldStatus::Pending | GoldStatus::Succeeded => Ok(None),
    }
}

/// Discover hosts centrally, skip same-image trusted hosts, and keep one gold unit
/// outstanding per untrusted host and campaign: a host whose attempt failed gets a new one.
/// Native gold submission journals handle uncertain dispatch.
pub async fn onboard(c: &Config, execute: bool) -> anyhow::Result<Vec<u64>> {
    c.validate()?;
    let trust = Trust::load_or_none_trusted(&c.trust)?;
    let prepared = gold::prepare::read(&c.prepared)?;
    gold::submit::job(&prepared, &c.engine, 0)?;
    let inventory =
        hosts(&query(c, "SELECT id FROM host WHERE id>0 ORDER BY id LIMIT 100001").await?)?;
    ensure!(
        inventory.len() <= 100000,
        "host inventory exceeds safety bound"
    );
    let pending: Vec<_> = inventory
        .into_iter()
        .filter(|h| trust.refuse(&c.engine.image, Some(*h)).is_some())
        .collect();
    let campaign = boinc::sha256(&serde_json::to_vec(&(&prepared, &c.engine))?);
    let campaign_root = c.root.join("onboard").join(&campaign);
    // Hosts are visited in id order and the pass stops at max_hosts, so hosts with an
    // outstanding unit never starve newly discovered ones.
    let mut plans = Vec::new();
    for host in pending {
        if let Some(root) = next_attempt(c, &campaign_root.join(host.to_string()), execute).await? {
            plans.push((host, root));
            if plans.len() == c.max_hosts {
                break;
            }
        }
    }
    if !execute {
        return Ok(plans.into_iter().map(|(host, _)| host).collect());
    }
    let _lock = Lock::take(&c.root)?;
    let shared = campaign_root.join("shared");
    if !plans.is_empty() {
        for name in [gold::prepare::PREPARED, gold::prepare::BUNDLE_COPY] {
            let limit = if name == gold::prepare::PREPARED {
                16 << 20
            } else {
                gold::job::MAX_BUNDLE as usize
            };
            let bytes = read(&c.prepared.join(name), limit)?;
            let dest = shared.join(name);
            if dest.exists() {
                ensure!(
                    read(&dest, limit)? == bytes,
                    "shared campaign input changed"
                );
            } else {
                atomic(&dest, &bytes)?;
            }
        }
        for original in &prepared.remotes {
            ensure!(
                gold::job::remote_name_ok(&original.name),
                "invalid prepared original name"
            );
            let source = c.prepared.join("cache").join(&original.name);
            let bytes = read(&source, gold::job::MAX_REMOTE as usize)?;
            use md5::Digest as _;
            let md5: String = md5::Md5::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect();
            ensure!(
                bytes.len() as u64 == original.bytes
                    && boinc::sha256(&bytes) == original.sha256
                    && md5 == original.md5,
                "prepared original differs from its checksums"
            );
            let dest = shared.join("cache").join(&original.name);
            if dest.exists() {
                ensure!(
                    read(&dest, gold::job::MAX_REMOTE as usize)? == bytes,
                    "shared campaign original changed"
                );
            } else {
                atomic(&dest, &bytes)?;
            }
        }
    }
    let mut issued = Vec::new();
    for (host, root) in plans {
        for name in [gold::prepare::PREPARED, gold::prepare::BUNDLE_COPY] {
            let source = shared.join(name);
            let limit = if name == gold::prepare::PREPARED {
                16 << 20
            } else {
                gold::job::MAX_BUNDLE as usize
            };
            let dest = root.join(name);
            link_input(&source, &dest, limit)?;
        }
        for original in &prepared.remotes {
            let name = Path::new("cache").join(&original.name);
            link_input(
                &shared.join(&name),
                &root.join(&name),
                gold::job::MAX_REMOTE as usize,
            )?;
        }
        let s = gold::submit::run(&gold::submit::Options {
            root,
            project: c.project.clone(),
            units: 1,
            target_host: Some(host),
            engine: c.engine.clone(),
        })
        .await?;
        ensure!(
            s.uncertain == 0,
            "uncertain gold submission; reconcile native campaign markers"
        );
        issued.push(host);
    }
    Ok(issued)
}

fn link_input(source: &Path, dest: &Path, limit: usize) -> anyhow::Result<()> {
    link_input_with(source, dest, limit, |s, d| std::fs::hard_link(s, d))
}

/// Hard-links where the filesystem can; Azure Files SMB can't (EOPNOTSUPP), so the
/// already verified bytes are written atomically instead.
fn link_input_with(
    source: &Path,
    dest: &Path,
    limit: usize,
    link: impl Fn(&Path, &Path) -> std::io::Result<()>,
) -> anyhow::Result<()> {
    let bytes = read(source, limit)?;
    if dest.exists() {
        ensure!(
            read(dest, limit)? == bytes,
            "onboarding campaign input changed"
        );
    } else {
        std::fs::create_dir_all(dest.parent().context("input has no parent")?)?;
        match link(source, dest) {
            Err(e) if e.kind() == std::io::ErrorKind::Unsupported => atomic(dest, &bytes)?,
            r => r?,
        }
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spot {
    job: String,
    image: String,
    source_host: u64,
    target_host: u64,
    workunit: String,
    stage: String,
}

fn sample(id: &str, every: u64) -> anyhow::Result<bool> {
    ensure!(boinc::is_sha256(id) && every > 0, "invalid sampling inputs");
    Ok(u64::from_str_radix(&id[..16], 16)? % every == 0)
}

#[cfg(test)]
fn plan(id: &str, image: &str, host: u64, trust: &Trust) -> anyhow::Result<Spot> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    ensure!(
        trust.refuse(image, Some(host)).is_none(),
        "source host is not trusted"
    );
    let target = trust
        .hosts
        .get(image)
        .into_iter()
        .flatten()
        .copied()
        .find(|h| *h != host && *h > 0)
        .context("a second trusted host for this exact image is required")?;
    Ok(Spot {
        job: id.into(),
        image: image.into(),
        source_host: host,
        target_host: target,
        workunit: format!("usnm_spot_{id}_{host}_{target}"),
        stage: "planned".into(),
    })
}

fn plan_job(id: &str, job: &boinc::Job, host: u64, trust: &Trust) -> anyhow::Result<Spot> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    ensure!(
        trust.refuse_job(job, Some(host)).is_none(),
        "source job is outside calibrated host trust"
    );
    let target = trust
        .trusted_hosts_for_job(job)
        .into_iter()
        .find(|h| *h != host && *h > 0)
        .context("a second trusted host for this exact image is required")?;
    Ok(Spot {
        job: id.into(),
        image: job.engine.image.clone(),
        source_host: host,
        target_host: target,
        workunit: format!("usnm_spot_{id}_{host}_{target}"),
        stage: "planned".into(),
    })
}

fn spot_args(s: &Spot) -> Vec<String> {
    [
        "--appname".into(),
        "usnm_ocr".into(),
        "--wu_name".into(),
        s.workunit.clone(),
        "--wu_template".into(),
        "templates/usnm_ocr_in".into(),
        "--result_template".into(),
        "templates/usnm_ocr_out".into(),
        "--min_quorum".into(),
        "1".into(),
        "--target_nresults".into(),
        "1".into(),
        "--max_error_results".into(),
        "1".into(),
        "--max_total_results".into(),
        "1".into(),
        "--max_success_results".into(),
        "1".into(),
        "--target_host".into(),
        s.target_host.to_string(),
        "--rsc_fpops_est".into(),
        "1e12".into(),
        "--rsc_fpops_bound".into(),
        "1e14".into(),
        "--rsc_memory_bound".into(),
        "1610612736".into(),
        "--rsc_disk_bound".into(),
        "1073741824".into(),
        "--delay_bound".into(),
        "259200".into(),
        format!("{}_page", s.workunit),
        format!("{}_request_json", s.workunit),
    ]
    .into()
}

/// Sample existing OCR jobs, replay their unchanged inputs on a different trusted host.
/// The independent workunit name cannot replace the original canonical result.
pub async fn spot_submit(c: &Config, jobs: &Path, execute: bool) -> anyhow::Result<Vec<String>> {
    c.validate()?;
    let ids: Vec<String> = serde_json::from_slice(&read(jobs, 1 << 20)?)?;
    ensure!(ids.len() <= 10000, "too many candidate jobs");
    let ids: BTreeSet<_> = ids.into_iter().collect();
    let trust = Trust::load(&c.trust)?;
    let _lock = if execute {
        Some(Lock::take(&c.root)?)
    } else {
        None
    };
    let mut made = Vec::new();
    for id in ids {
        if !sample(&id, c.sample_every)? {
            continue;
        }
        let record = load(&c.jobs_root, &id)?;
        ensure!(
            record.job.engine.image == c.engine.image,
            "candidate worker image differs"
        );
        let host = canonical_host(c, &record.job.workunit()).await?;
        ensure!(
            trust.refuse_job(&record.job, Some(host)).is_none(),
            "source job is outside calibrated host trust"
        );
        let mut spot = plan_job(&id, &record.job, host, &trust)?;
        let path = c.root.join("spots").join(&id).join("submission.json");
        if path.exists() {
            let old: Spot = serde_json::from_slice(&read(&path, 4096)?)?;
            spot.stage.clone_from(&old.stage);
            ensure!(old == spot, "spot-check identity or host changed");
            ensure!(
                old.stage == "submitted",
                "spot submission uncertain; reconcile server before any retry"
            );
            continue;
        }
        if execute {
            // Gold uses the same exact active-XML assignment opt-in check.
            ensure!(
                gold::submit::assignment_enabled(&std::fs::read_to_string(
                    c.project.join("config.xml")
                )?)?,
                "spot-check assignment must be explicitly enabled by the operator"
            );
            stage_inputs(c, &record.job, &spot.workunit).await?;
            spot.stage = "submitting".into();
            atomic(&path, &serde_json::to_vec_pretty(&spot)?)?;
            let status = Command::new(c.project.join("bin/create_work"))
                .current_dir(&c.project)
                .args(spot_args(&spot))
                .status()
                .await?;
            ensure!(
                status.success(),
                "spot submission uncertain; inspect deterministic workunit name"
            );
            spot.stage = "submitted".into();
            atomic(&path, &serde_json::to_vec_pretty(&spot)?)?;
        }
        made.push(spot.workunit);
        if made.len() == c.max_spots {
            break;
        }
    }
    Ok(made)
}

async fn stage_inputs(c: &Config, job: &boinc::Job, workunit: &str) -> anyhow::Result<()> {
    let input = crate::dir(&c.jobs_root, &job.id)?;
    let request = read(&input.join("request.json"), boinc::MAX_JSON)?;
    ensure!(
        serde_json::from_slice::<boinc::Job>(&request)? == *job,
        "staged job changed"
    );
    let scan = read(&input.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        scan.len() as u64 == job.scan.bytes && boinc::sha256(&scan) == job.scan.sha256,
        "staged scan changed"
    );
    for (name, bytes) in [("page", scan), ("request_json", request)] {
        let physical = format!("{workunit}_{name}");
        let output = Command::new(c.project.join("bin/dir_hier_path"))
            .arg(&physical)
            .current_dir(&c.project)
            .output()
            .await?;
        ensure!(output.status.success(), "dir_hier_path failed");
        let path = String::from_utf8(output.stdout)?;
        let path = Path::new(path.trim());
        ensure!(!path.as_os_str().is_empty(), "empty staging path");
        let path = if path.is_absolute() {
            path.to_owned()
        } else {
            c.project.join(path)
        };
        crate::atomic_download(&path, &bytes)?;
    }
    Ok(())
}

fn result(job: &boinc::Job, spool: &Path, workunit: &str) -> anyhow::Result<String> {
    let file = |name| spool.join(format!("{workunit}__file_{name}"));
    ensure!(
        !spool.join(format!("{workunit}_error")).exists()
            && !spool.join(format!("{workunit}_no_output_files")).exists(),
        "spot-check member failed"
    );
    let manifest: ResultManifest =
        serde_json::from_slice(&read(&file("result.json"), boinc::MAX_JSON)?)?;
    let text = read(&file("text.txt"), boinc::MAX_TEXT)?;
    let alto = read(&file("layout.xml"), boinc::MAX_ALTO)?;
    manifest.matches(job, &text, &alto)?;
    usnm_ingest::ocr_local::checks::check_alto(&alto)
        .map_err(|r| anyhow::anyhow!("{}", r.code()))?;
    Ok(usnm_core::text::normalize_ocr(std::str::from_utf8(&text)?))
}

/// A conservative two-host quorum: both canonical hosts must still be trusted and produce
/// valid results for the identical job with exactly equal normalized text. Never publish.
pub async fn spot_compare(
    c: &Config,
    id: &str,
    original_spool: &Path,
    spot_spool: &Path,
) -> anyhow::Result<bool> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    ensure!(c.root.is_absolute(), "controller root must be absolute");
    let _lock = Lock::take(&c.root)?;
    let path = c.root.join("spots").join(id);
    atomic(
        &path.join("comparison.json"),
        br#"{"status":"unverified","agrees":false}"#,
    )?;
    c.validate()?;
    let spot: Spot = serde_json::from_slice(&read(&path.join("submission.json"), 4096)?)?;
    let record = load(&c.jobs_root, id)?;
    ensure!(
        spot.job == id
            && spot.image == record.job.engine.image
            && spot.image == c.engine.image
            && spot.stage == "submitted",
        "invalid spot journal"
    );
    let trust = Trust::load(&c.trust)?;
    let source = canonical_host(c, &record.job.workunit()).await?;
    let target = canonical_host(c, &spot.workunit).await?;
    ensure!(
        source == spot.source_host && target == spot.target_host && source != target,
        "canonical hosts do not match independent assignment"
    );
    ensure!(
        trust.refuse_job(&record.job, Some(source)).is_none()
            && trust.refuse_job(&record.job, Some(target)).is_none(),
        "spot-check job scope or host trust revoked"
    );
    let a = result(&record.job, original_spool, &record.job.workunit())?;
    let b = result(&record.job, spot_spool, &spot.workunit)?;
    let agrees = a == b;
    atomic(
        &path.join("comparison.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "job": id, "image": spot.image, "hosts": [source, target], "quorum": 2,
            "agrees": agrees, "comparison": "exact-normalized-text",
            "status": "verified",
            "at": chrono::Utc::now().to_rfc3339(),
            "reason": if agrees { "independent-host-agreement" } else { "text-disagreement" },
        }))?,
    )?;
    Ok(agrees)
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quarantine {
    pub at: String,
    pub engine_image: String,
    pub host_id: Option<u64>,
    pub job: String,
    pub reason: String,
    pub workunit: String,
}

/// Read-only, bounded view of the existing collector's held results and reasons.
pub fn quarantine(root: &Path) -> anyhow::Result<Vec<Quarantine>> {
    let path = root.join("quarantine");
    if !path.exists() {
        return Ok(Vec::new());
    }
    ensure!(
        std::fs::symlink_metadata(&path)?.file_type().is_dir(),
        "quarantine must be a regular directory"
    );
    let mut rows = Vec::new();
    for (n, entry) in std::fs::read_dir(path)?.enumerate() {
        ensure!(n < 10000, "quarantine view exceeds safety bound");
        let entry = entry?;
        let name = entry.file_name();
        let Some(id) = name.to_str().filter(|id| boinc::is_sha256(id)) else {
            continue;
        };
        ensure!(
            entry.file_type()?.is_dir(),
            "quarantine entry must be a regular directory"
        );
        if matches!(
            load(root, id)?.stage,
            crate::Stage::Published | crate::Stage::Failed
        ) {
            continue;
        }
        let row: Quarantine =
            serde_json::from_slice(&read(&entry.path().join("reason.json"), 16 << 10)?)?;
        ensure!(
            row.job == id && row.workunit == format!("usnm_{id}"),
            "quarantine identity mismatch"
        );
        rows.push(row);
    }
    rows.sort_by(|a, b| a.job.cmp(&b.job));
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gold_status_retries_only_terminal_failures_and_keeps_diagnostics() {
        assert_eq!(gold_status("").unwrap(), GoldStatus::Unknown);
        assert_eq!(
            gold_status("NULL\tNULL\tNULL\tNULL\tNULL\tNULL\n").unwrap(),
            GoldStatus::Pending
        );
        assert_eq!(
            gold_status("7\t4\t0\t1\t0\tNULL\n").unwrap(),
            GoldStatus::Pending
        );
        assert_eq!(
            gold_status("7\t5\t1\t5\t0\t\n").unwrap(),
            GoldStatus::Succeeded
        );
        // "Error -1" hex-encoded, as HEX(stderr_out) returns it.
        let GoldStatus::Failed(r) = gold_status("7\t5\t3\t5\t255\t4572726F72202D31\n").unwrap()
        else {
            panic!("expected a failure")
        };
        assert_eq!(
            r,
            vec![FailedResult {
                result: 7,
                outcome: 3,
                client_state: 5,
                exit_status: 255,
                stderr: "Error -1".into()
            }]
        );
        assert!(gold_status("7\t5\t3\n").is_err());
        assert!(gold_status("7\t5\t3\t5\t255\t4G\n").is_err());
    }

    #[test]
    fn onboarding_inputs_are_copied_where_hard_links_are_unsupported() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("shared/bundle");
        atomic(&source, b"bundle").unwrap();
        let unsupported =
            |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::Unsupported));
        let dest = dir.path().join("host/7/bundle");
        link_input_with(&source, &dest, 64, unsupported).unwrap();
        assert_eq!(std::fs::read(&dest).unwrap(), b"bundle");
        // A retry accepts the identical copy and refuses a changed one.
        link_input_with(&source, &dest, 64, unsupported).unwrap();
        atomic(&dest, b"other").unwrap();
        assert!(link_input_with(&source, &dest, 64, unsupported).is_err());
        // Other link errors still fail.
        let denied =
            |_: &Path, _: &Path| Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        assert!(link_input_with(&source, &dir.path().join("host/8/bundle"), 64, denied).is_err());
        let linked = dir.path().join("host/9/bundle");
        link_input(&source, &linked, 64).unwrap();
        use std::os::unix::fs::MetadataExt;
        assert_eq!(
            std::fs::metadata(&linked).unwrap().ino(),
            std::fs::metadata(&source).unwrap().ino()
        );
    }

    fn trust() -> Trust {
        Trust {
            hosts: [(format!("sha256:{}", "a".repeat(64)), [1, 2].into())].into(),
            manifest_sha256: Some("b".repeat(64)),
            scope: Some(crate::trust::Scope {
                language: "eng".into(),
                profile: "eng".into(),
                psm: 3,
                dpi: 300,
            }),
            max_cer: 0.1,
            policies: Vec::new(),
            v: crate::trust::TRUST_VERSION,
        }
    }

    #[test]
    fn host_inventory_and_sampling_fail_closed() {
        assert_eq!(hosts("2\n1\n2\n").unwrap(), [1, 2].into());
        for bad in ["0", "-1", "1\t2", "not-host", "18446744073709551616"] {
            assert!(hosts(bad).is_err(), "{bad}");
        }
        assert!(sample(&"0".repeat(64), 100).unwrap());
        assert!(!sample(&format!("{}{}", "0".repeat(15), "1".repeat(49)), 100).unwrap());
        assert!(sample("../job", 1).is_err());
        assert!(sample(&"a".repeat(64), 0).is_err());
    }

    #[test]
    fn spot_requires_distinct_trusted_hosts_and_preserves_original_input_identity() {
        let t = trust();
        let image = format!("sha256:{}", "a".repeat(64));
        let id = "b".repeat(64);
        let s = plan(&id, &image, 1, &t).unwrap();
        assert_eq!(s.target_host, 2);
        assert_ne!(s.workunit, format!("usnm_{id}"));
        let args = spot_args(&s);
        assert!(args.windows(2).any(|a| a == ["--target_host", "2"]));
        assert_eq!(
            &args[args.len() - 2..],
            [
                format!("{}_page", s.workunit),
                format!("{}_request_json", s.workunit)
            ]
        );
        assert!(plan(&id, &image, 3, &t).is_err());
        assert!(plan(&id, &format!("sha256:{}", "c".repeat(64)), 1, &t).is_err());
        let mut t = t;
        t.hosts.get_mut(&image).unwrap().remove(&2);
        assert!(plan(&id, &image, 1, &t).is_err());

        // Scoped coexistence: spot-check planning for deu/frak2021 selects only a host trusted in
        // that scope, never a host trusted only for eng/eng.
        let eng = trust();
        let deu = Trust {
            hosts: [(image.clone(), [2, 5].into())].into(),
            manifest_sha256: Some("c".repeat(64)),
            scope: Some(crate::trust::Scope {
                language: "deu".into(),
                profile: "frak2021".into(),
                psm: 3,
                dpi: 300,
            }),
            max_cer: 0.08,
            policies: Vec::new(),
            v: crate::trust::TRUST_VERSION,
        };
        let coexisting = eng.merge(&deu).unwrap();
        let deu_job = boinc::Job {
            v: 1,
            id: id.clone(),
            message: usnm_ocr_contract::message::OcrMessage {
                v: 1,
                page: "sn83030214/1900-01-01/ed-1/seq-1".into(),
                batch: "batch_test_ver01".into(),
                language: "deu".into(),
                reason: usnm_ocr_contract::message::Reason::Bulk,
                score: Some(0.5),
                queued_at: "2026-10-18T00:00:00Z".into(),
            },
            batch_version: 1,
            scan: boinc::Scan {
                url: "https://tile.loc.gov/image-services/iiif/service:ndnp:test/full/full/0/default.jpg".into(),
                iiif_base: "https://tile.loc.gov/image-services/iiif/service:ndnp:test".into(),
                bytes: 100,
                sha256: "d".repeat(64),
                md5: None,
            },
            engine: boinc::Engine {
                kind: usnm_ocr_contract::routing::EngineKind::Tesseract,
                image: image.clone(),
                version: "tesseract 5.5.0".into(),
                profile: "frak2021".into(),
                models: "sha256:001122334455".into(),
                psm: 3,
                dpi: 300,
                timeout_secs: 600,
            },
        };
        assert!(plan_job(&id, &deu_job, 1, &coexisting).is_err());
        let deu_spot = plan_job(&id, &deu_job, 2, &coexisting).unwrap();
        assert_eq!(deu_spot.target_host, 5);
    }
}
