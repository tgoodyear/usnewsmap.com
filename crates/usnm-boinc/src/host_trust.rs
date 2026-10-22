//! Explicitly invoked, bounded host onboarding and independent OCR spot checks.
//! These workflows never change project settings or publication state. Only `promote` changes
//! trust, and only by adding hosts whose complete native gold evidence passes a scope's
//! existing calibration (#901).
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
    #[serde(default)]
    pub scope: Option<crate::trust::Scope>,
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
        if let Some(scope) = &c.scope {
            scope.validate()?;
            ensure!(
                scope.psm == c.engine.psm && scope.dpi == c.engine.dpi,
                "campaign scope scoring configuration must match engine"
            );
        }
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

async fn legacy_blocks_attempt(
    c: &Config,
    legacy_host_root: &Path,
    active_host_root: &Path,
    execute: bool,
) -> anyhow::Result<bool> {
    if legacy_host_root == active_host_root || std::fs::symlink_metadata(legacy_host_root).is_err()
    {
        return Ok(false);
    }
    let mut attempt = 0;
    while attempt_root(legacy_host_root, attempt + 1)
        .join("units/0000")
        .is_dir()
    {
        attempt += 1;
        ensure!(attempt < MAX_ATTEMPTS, "too many gold attempts");
    }
    let root = attempt_root(legacy_host_root, attempt);
    let unit = gold::submit::units_dir(&root).join("0000");
    if !unit.join("submitted").is_file() {
        ensure!(
            std::fs::symlink_metadata(unit.join("submitting")).is_err(),
            "uncertain gold submission in {}; reconcile native campaign markers",
            unit.display()
        );
        return Ok(false);
    }
    if std::fs::symlink_metadata(active_host_root).is_ok() {
        return Ok(false);
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
                    "legacy gold attempt {attempt} ({workunit}) failed; diagnostics in {}; issuing another",
                    root.join("failure.json").display()
                );
            }
            Ok(false)
        }
        GoldStatus::Unknown => {
            eprintln!("{workunit} is not in the database; not reissuing");
            Ok(true)
        }
        GoldStatus::Pending | GoldStatus::Succeeded => Ok(true),
    }
}

/// This prepared campaign's onboarding root, and the root it had before the scope became part
/// of its identity (whose attempts still count for their hosts).
fn campaign_roots(
    c: &Config,
    prepared: &gold::prepare::Prepared,
    scope: &crate::trust::Scope,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let campaign = boinc::sha256(&serde_json::to_vec(&(prepared, &c.engine, scope))?);
    let legacy = boinc::sha256(&serde_json::to_vec(&(prepared, &c.engine))?);
    let onboard = c.root.join("onboard");
    Ok((onboard.join(campaign), onboard.join(legacy)))
}

/// Discover hosts centrally, skip hosts already trusted for this prepared campaign's exact
/// `(language, profile, psm, dpi)`, `manifest_sha256`, and immutable worker image, and keep one
/// gold unit outstanding per untrusted host and campaign: a host whose attempt failed gets a new
/// one. Native gold submission journals handle uncertain dispatch.
pub async fn onboard(c: &Config, execute: bool) -> anyhow::Result<Vec<u64>> {
    c.validate()?;
    let scope = c
        .scope
        .as_ref()
        .context("prepared campaign scope is required for onboarding")?;
    let trust = Trust::load_or_none_trusted(&c.trust)?;
    let prepared = gold::prepare::read(&c.prepared)?;
    gold::submit::job(&prepared, &c.engine, 0)?;
    let inventory =
        hosts(&query(c, "SELECT id FROM host WHERE id>0 ORDER BY id LIMIT 100001").await?)?;
    ensure!(
        inventory.len() <= 100000,
        "host inventory exceeds safety bound"
    );
    let trusted_hosts =
        trust.campaign_trusted_hosts(&c.engine.image, &prepared.manifest_sha256, scope);
    let pending: Vec<_> = inventory
        .into_iter()
        .filter(|h| !trusted_hosts.is_some_and(|hosts| hosts.contains(h)))
        .collect();
    let (campaign_root, legacy_root) = campaign_roots(c, &prepared, scope)?;
    // Hosts are visited in id order and the pass stops at max_hosts, so hosts with an
    // outstanding unit never starve newly discovered ones.
    let mut plans = Vec::new();
    for host in pending {
        let host_str = host.to_string();
        let active_host_root = campaign_root.join(&host_str);
        if legacy_blocks_attempt(c, &legacy_root.join(&host_str), &active_host_root, execute)
            .await?
        {
            continue;
        }
        if let Some(root) = next_attempt(c, &active_host_root, execute).await? {
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

/// The submitted gold attempts of one host under one onboarding root, oldest first.
fn submitted_attempts(host_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut roots = Vec::new();
    let mut attempt = 0;
    loop {
        let root = attempt_root(host_root, attempt);
        let unit = gold::submit::units_dir(&root).join("0000");
        if attempt > 0 && !unit.is_dir() {
            break;
        }
        if unit.join("submitted").is_file() {
            roots.push(root);
        }
        attempt += 1;
        ensure!(attempt < MAX_ATTEMPTS, "too many gold attempts");
    }
    Ok(roots)
}

/// The hosts with an onboarding directory under `root`, in id order.
fn onboarded_hosts(root: &Path) -> anyhow::Result<BTreeSet<u64>> {
    let mut hosts = BTreeSet::new();
    if std::fs::symlink_metadata(root).is_err() {
        return Ok(hosts);
    }
    for entry in std::fs::read_dir(root)? {
        let entry = entry?;
        let Some(host) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
            .filter(|host| *host > 0)
        else {
            continue;
        };
        ensure!(
            entry.file_type()?.is_dir(),
            "onboarding host entry must be a regular directory"
        );
        hosts.insert(host);
        ensure!(
            hosts.len() <= 100_000,
            "onboarding hosts exceed safety bound"
        );
    }
    Ok(hosts)
}

/// The newest passing canonical report of `host` in a set of `reports.jsonl` files: the evidence
/// a promotion records.
#[derive(Debug, Clone, Deserialize)]
struct Evidence {
    host_id: Option<u64>,
    workunit: Option<String>,
    native_result_id: Option<u64>,
    server_received_at: Option<i64>,
}

fn newest_evidence(reports: &[PathBuf], host: u64) -> anyhow::Result<Evidence> {
    let mut newest: Option<Evidence> = None;
    for path in reports {
        for line in std::str::from_utf8(&read(path, 64 << 20)?)?.lines() {
            if line.trim().is_empty() {
                continue;
            }
            let row: Evidence = serde_json::from_str(line)?;
            if row.host_id != Some(host) {
                continue;
            }
            if newest
                .as_ref()
                .is_none_or(|n| row.server_received_at > n.server_received_at)
            {
                newest = Some(row);
            }
        }
    }
    let newest = newest.context("no canonical report for the eligible host")?;
    ensure!(
        newest.workunit.is_some() && newest.native_result_id.is_some(),
        "eligible host's report lacks its native identity"
    );
    Ok(newest)
}

/// One host's promotion, kept in `<root>/promotions.jsonl`. A recorded workunit is never used
/// again: if trust is later rebuilt without the host, the same evidence does not bring it back.
///
/// The record is written ahead of the trust change, uncommitted, with the list's digest before
/// it. The next pass settles an uncommitted record: if the list is still exactly that digest,
/// the change never happened and is made now; if the list changed, the record is committed as
/// it stands (the host added only if the list has it), so a later revocation is never undone.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PromotionRecord {
    at: String,
    language: String,
    profile: String,
    engine_image: String,
    manifest_sha256: String,
    max_cer: f64,
    host: u64,
    workunit: String,
    native_result_id: u64,
    added: bool,
    committed: bool,
    trust_before_sha256: String,
    trust_after_sha256: Option<String>,
}

impl PromotionRecord {
    fn same_evidence(&self, other: &Self) -> bool {
        (
            &self.language,
            &self.profile,
            &self.engine_image,
            self.host,
            &self.workunit,
        ) == (
            &other.language,
            &other.profile,
            &other.engine_image,
            other.host,
            &other.workunit,
        )
    }
}

fn write_records(root: &Path, records: &[PromotionRecord]) -> anyhow::Result<()> {
    let mut ledger = Vec::new();
    for record in records {
        ledger.extend(serde_json::to_vec(record)?);
        ledger.push(b'\n');
    }
    ensure!(
        ledger.len() <= 16 << 20,
        "promotion ledger exceeds its size limit"
    );
    atomic(&root.join(PROMOTIONS), &ledger)
}

/// Whether the list at `path` trusts `host` for `scope` and `image`, and the list's digest.
fn trusts(
    path: &Path,
    scope: &crate::trust::Scope,
    image: &str,
    host: u64,
) -> anyhow::Result<(bool, String)> {
    let bytes = read(path, 64 << 20)?;
    let trust = Trust::load(path)?;
    let has = trust
        .policy_for(&scope.language, &scope.profile)
        .and_then(|p| p.hosts.get(image).map(|h| h.contains(&host)))
        .unwrap_or(false);
    Ok((has, boinc::sha256(&bytes)))
}

/// Under the controller lock: settle earlier uncommitted records of this scope and image, drop
/// candidates whose evidence the ledger (read again now) already holds, then write the new
/// records ahead, change trust, and commit them.
fn commit_promotions(
    c: &Config,
    scope: &crate::trust::Scope,
    policy: &crate::trust::ScopedPolicy,
    candidates: Vec<PromotionRecord>,
    summary: &mut Promotion,
) -> anyhow::Result<()> {
    let _lock = Lock::take(&c.root)?;
    let image = &c.engine.image;
    // Admits only into the exact list version `expected`, compared under the trust lock.
    let admit = |expected: &str, hosts: BTreeSet<u64>| {
        Trust::admit_hosts(
            &c.trust,
            Some(expected),
            scope,
            &policy.manifest_sha256,
            policy.max_cer,
            image,
            &hosts,
        )
    };
    let mut records = promotion_records(&c.root)?;
    let ours = |r: &PromotionRecord| {
        !r.committed
            && r.language == scope.language
            && r.profile == scope.profile
            && &r.engine_image == image
    };
    if records.iter().any(ours) {
        let current = boinc::sha256(&read(&c.trust, 64 << 20)?);
        // Replayed only while the list is the version the record was written against and the
        // scope still has the calibration the evidence was judged by.
        let replay: BTreeSet<u64> = records
            .iter()
            .filter(|r| {
                ours(r)
                    && r.trust_before_sha256 == current
                    && r.manifest_sha256 == policy.manifest_sha256
                    && r.max_cer == policy.max_cer
            })
            .map(|r| r.host)
            .collect();
        if !replay.is_empty() {
            // None: the list changed after it was read here; settle without admitting.
            if let Some(admission) = admit(&current, replay)? {
                summary.promoted.extend(admission.added.iter().copied());
            }
        }
        for record in records.iter_mut().filter(|r| ours(r)) {
            let (has, digest) = trusts(&c.trust, scope, image, record.host)?;
            record.added = has;
            record.committed = true;
            record.trust_after_sha256 = Some(digest);
        }
        write_records(&c.root, &records)?;
    }
    let mut fresh = Vec::new();
    for candidate in candidates {
        if records.iter().any(|r| r.same_evidence(&candidate)) {
            summary.evidence_used.push(candidate.host);
        } else {
            fresh.push(candidate);
        }
    }
    if fresh.is_empty() {
        return Ok(());
    }
    let before = boinc::sha256(&read(&c.trust, 64 << 20)?);
    let start = records.len();
    for mut record in fresh {
        record.trust_before_sha256 = before.clone();
        records.push(record);
    }
    write_records(&c.root, &records)?;
    let hosts: BTreeSet<u64> = records[start..].iter().map(|r| r.host).collect();
    let Some(admission) = admit(&before, hosts.clone())? else {
        // The list changed since `before` and nothing was written: drop the records so the
        // next pass judges these hosts again against the list as it is then.
        records.truncate(start);
        write_records(&c.root, &records)?;
        for host in hosts {
            summary.failed.push(HostFailure {
                host,
                error: "trust.json changed during promotion; nothing admitted".into(),
            });
        }
        return Ok(());
    };
    for record in &mut records[start..] {
        record.added = admission.added.contains(&record.host);
        record.committed = true;
        record.trust_after_sha256 = Some(admission.after_sha256.clone());
    }
    write_records(&c.root, &records)?;
    summary.promoted.extend(admission.added.iter().copied());
    summary.trust_before_sha256 = Some(admission.before_sha256);
    summary.trust_after_sha256 = Some(admission.after_sha256);
    Ok(())
}

const PROMOTIONS: &str = "promotions.jsonl";

fn promotion_records(root: &Path) -> anyhow::Result<Vec<PromotionRecord>> {
    let path = root.join(PROMOTIONS);
    if !path.exists() {
        return Ok(Vec::new());
    }
    std::str::from_utf8(&read(&path, 16 << 20)?)?
        .lines()
        .filter(|line| !line.trim().is_empty())
        .map(|line| Ok(serde_json::from_str(line)?))
        .collect()
}

#[derive(Debug, Serialize)]
pub struct HostFailure {
    pub host: u64,
    pub error: String,
}

/// What one promotion pass found, host by host.
#[derive(Debug, Default, Serialize)]
pub struct Promotion {
    /// Added to the scope's trusted hosts by this pass.
    pub promoted: Vec<u64>,
    /// Passing evidence that would promote them (dry run).
    pub would_promote: Vec<u64>,
    /// Passing evidence already used once; a later rebuild without the host stands.
    pub evidence_used: Vec<u64>,
    /// Complete evidence that does not pass the frozen calibration.
    pub not_eligible: Vec<u64>,
    /// No assimilated gold result yet.
    pub pending: Vec<u64>,
    /// Collection or evaluation failed; the host is left as it was.
    pub failed: Vec<HostFailure>,
    pub trust_before_sha256: Option<String>,
    pub trust_after_sha256: Option<String>,
}

/// Collect the onboarding gold units of untrusted hosts and admit the hosts whose complete
/// native evidence passes the scope's frozen calibration (#901). The threshold and challenge
/// come from the calibrated trust list, never from this pass, and must match the prepared
/// campaign; other scopes and hosts are kept, and nothing is revoked here. Without `execute`
/// nothing is queried or written: hosts are judged from evidence already collected.
pub async fn promote(c: &Config, spool: &Path, execute: bool) -> anyhow::Result<Promotion> {
    promote_with(c, spool, execute, execute).await
}

/// `promote`, with collection (BOINC queries and `out/` writes) and the trust/ledger write
/// switched separately, so the judgment and the write can be tested on collected evidence.
async fn promote_with(
    c: &Config,
    spool: &Path,
    collect: bool,
    write: bool,
) -> anyhow::Result<Promotion> {
    c.validate()?;
    let scope = c
        .scope
        .as_ref()
        .context("prepared campaign scope is required for promotion")?;
    let prepared = gold::prepare::read(&c.prepared)?;
    let trust = Trust::load(&c.trust)?;
    let policy = trust
        .policy_for(&scope.language, &scope.profile)
        .context("no calibrated trust policy for the onboarding scope")?;
    ensure!(
        &policy.scope == scope && policy.manifest_sha256 == prepared.manifest_sha256,
        "the onboarding campaign is not the scope's calibrated challenge; nothing promoted"
    );
    let image = &c.engine.image;
    let trusted = policy.hosts.get(image).cloned().unwrap_or_default();
    let (campaign_root, legacy_root) = campaign_roots(c, &prepared, scope)?;
    let records = promotion_records(&c.root)?;
    let native = gold::collect::NativeConfig {
        mysql_defaults_file: c.mysql_defaults.clone(),
        mysql_ca_file: c.mysql_ca.clone(),
        database: c.database.clone(),
        language: scope.language.clone(),
        profile: scope.profile.clone(),
    };
    let mut hosts = onboarded_hosts(&campaign_root)?;
    hosts.extend(onboarded_hosts(&legacy_root)?);
    let mut summary = Promotion::default();
    let mut admit = Vec::new();
    for host in hosts.into_iter().filter(|h| !trusted.contains(h)) {
        let mut attempts = submitted_attempts(&legacy_root.join(host.to_string()))?;
        attempts.extend(submitted_attempts(&campaign_root.join(host.to_string()))?);
        if attempts.is_empty() {
            continue;
        }
        let judged = async {
            let mut pending = false;
            for root in &attempts {
                if collect {
                    let rows = gold::collect::native_results(root, &native).await?;
                    let s = gold::collect::run_with_native(
                        &gold::collect::Options {
                            root: root.clone(),
                            project: c.project.clone(),
                            spool: spool.to_path_buf(),
                            out: root.join("out"),
                        },
                        &rows,
                        &native,
                    )?;
                    pending |= s.pending > 0;
                } else {
                    pending |= !root.join("out/reports.jsonl").is_file();
                }
            }
            let out: Vec<_> = attempts
                .iter()
                .map(|root| root.join("out"))
                .filter(|out| out.join("reports.jsonl").is_file())
                .collect();
            if out.is_empty() {
                return Ok::<_, anyhow::Error>((pending, None));
            }
            let reports: Vec<_> = out.iter().map(|o| o.join("reports.jsonl")).collect();
            let outcomes: Vec<_> = out
                .iter()
                .map(|o| o.join("native-outcomes.jsonl"))
                .collect();
            let built = Trust::build_with_outcomes(
                &reports,
                &outcomes,
                policy.max_cer,
                &policy.manifest_sha256,
                scope.clone(),
            )?;
            let eligible = built.hosts.get(image).cloned().unwrap_or_default();
            ensure!(
                eligible.iter().all(|h| *h == host),
                "host {host}'s gold evidence names another host"
            );
            if !eligible.contains(&host) {
                return Ok((pending, None));
            }
            Ok((pending, Some(newest_evidence(&reports, host)?)))
        }
        .await;
        match judged {
            Err(error) => summary.failed.push(HostFailure {
                host,
                error: format!("{error:#}"),
            }),
            Ok((_, Some(evidence))) => {
                let workunit = evidence.workunit.clone().unwrap_or_default();
                if records.iter().any(|r| {
                    r.host == host
                        && r.workunit == workunit
                        && r.language == scope.language
                        && r.profile == scope.profile
                        && &r.engine_image == image
                }) {
                    summary.evidence_used.push(host);
                } else {
                    admit.push((host, evidence));
                }
            }
            Ok((true, None)) => summary.pending.push(host),
            Ok((false, None)) => summary.not_eligible.push(host),
        }
    }
    if !write {
        summary.would_promote = admit.iter().map(|(host, _)| *host).collect();
        return Ok(summary);
    }
    let at = chrono::Utc::now().to_rfc3339();
    let candidates = admit
        .into_iter()
        .map(|(host, evidence)| PromotionRecord {
            at: at.clone(),
            language: scope.language.clone(),
            profile: scope.profile.clone(),
            engine_image: image.clone(),
            manifest_sha256: policy.manifest_sha256.clone(),
            max_cer: policy.max_cer,
            host,
            workunit: evidence.workunit.unwrap_or_default(),
            native_result_id: evidence.native_result_id.unwrap_or_default(),
            added: false,
            committed: false,
            trust_before_sha256: String::new(),
            trust_after_sha256: None,
        })
        .collect();
    // Even with no candidates: an earlier pass may have left a record to settle.
    commit_promotions(c, scope, &policy, candidates, &mut summary)?;
    summary.promoted.sort_unstable();
    summary.promoted.dedup();
    Ok(summary)
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

    /// A controller whose prepared campaign is the calibrated eng/eng challenge, with host 1
    /// trusted, and the root of its onboarding campaign.
    fn promotion_fixture(dir: &Path) -> (Config, PathBuf) {
        use std::os::unix::fs::PermissionsExt;
        let prepared = gold::submit::tests::prepared();
        let engine = gold::submit::tests::engine();
        atomic(&dir.join("mysql.cnf"), b"[client]\n").unwrap();
        std::fs::set_permissions(
            dir.join("mysql.cnf"),
            std::fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        atomic(&dir.join("ca.pem"), b"ca").unwrap();
        atomic(
            &dir.join("prepared").join(gold::prepare::PREPARED),
            &serde_json::to_vec(&prepared).unwrap(),
        )
        .unwrap();
        atomic(
            &dir.join("gold/trust.json"),
            &serde_json::to_vec(&serde_json::json!({
                "hosts": {engine.image.clone(): [1]}, "manifest_sha256": prepared.manifest_sha256,
                "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
                "max_cer": 0.1, "v": crate::trust::TRUST_VERSION,
            }))
            .unwrap(),
        )
        .unwrap();
        let c: Config = serde_json::from_value(serde_json::json!({
            "root": dir.join("host-trust"), "project": dir.join("project"),
            "jobs_root": dir.join("handoff"), "mysql_defaults": dir.join("mysql.cnf"),
            "mysql_ca": dir.join("ca.pem"), "database": "fixture",
            "trust": dir.join("gold/trust.json"), "prepared": dir.join("prepared"),
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "engine": engine, "max_hosts": 10, "max_spots": 1, "sample_every": 1,
        }))
        .unwrap();
        let scope = c.scope.clone().unwrap();
        let (campaign, _) = campaign_roots(&c, &prepared, &scope).unwrap();
        (c, campaign)
    }

    /// One submitted onboarding attempt for `host`, with collected evidence when `cer` is given.
    fn attempt(root: &Path, host: u64, cer: Option<f64>, result_id: u64) {
        let unit = gold::submit::units_dir(root).join("0000");
        atomic(&unit.join("submitted"), b"").unwrap();
        let Some(cer) = cer else { return };
        let image = gold::submit::tests::engine().image;
        let manifest = gold::submit::tests::prepared().manifest_sha256;
        let workunit = format!(
            "usnm_gold_{}",
            format!("{result_id:x}").repeat(64)[..64].to_owned()
        );
        let received_at = 1_792_000_000 + result_id as i64;
        let report = serde_json::json!({
            "completed_at": "2026-10-21T00:00:00Z", "bundle_version": 1, "engine_image": image,
            "host_id": host, "manifest_sha256": manifest, "workunit": workunit,
            "native_result_id": result_id, "server_received_at": received_at,
            "language": "eng", "profile": "eng", "crops": 30, "skipped": 0,
            "complete_loss_crops": 0, "profiles": ["eng"], "psm": 3, "dpi": 300,
            "summary": [{"cer": cer, "crops": 30, "ref_chars": 7786, "source": "all", "wer": cer}],
        });
        let outcome = serde_json::json!({
            "schema_version": 3, "app": "usnm_gold", "app_version_id": 40, "bundle_version": 1,
            "canonical": true, "dpi": 300, "engine_image": image, "host_id": host,
            "language": "eng", "manifest_sha256": manifest,
            "outcome": gold::collect::BOINC_OUTCOME_SUCCESS, "exit_status": 0, "profile": "eng",
            "psm": 3, "received_at": received_at, "result_id": result_id,
            "sent_at": received_at - 30, "server_state": gold::collect::BOINC_SERVER_STATE_OVER,
            "workunit": workunit,
            "workunit_assimilate_state": gold::collect::BOINC_ASSIMILATE_STATE_DONE,
            "workunit_result_count": 1,
        });
        atomic(
            &root.join("out/reports.jsonl"),
            format!("{report}\n").as_bytes(),
        )
        .unwrap();
        atomic(
            &root.join("out/native-outcomes.jsonl"),
            format!("{outcome}\n").as_bytes(),
        )
        .unwrap();
    }

    fn trusted(c: &Config) -> BTreeSet<u64> {
        let t = Trust::load(&c.trust).unwrap();
        t.policy_for("eng", "eng").unwrap().hosts[&c.engine.image].clone()
    }

    #[tokio::test]
    async fn promotion_admits_only_passing_untrusted_hosts_once_and_keeps_revocations() {
        let dir = tempfile::tempdir().unwrap();
        let (c, campaign) = promotion_fixture(dir.path());
        let spool = dir.path().join("spool");
        attempt(&campaign.join("1"), 1, Some(0.05), 11); // already trusted: left alone
        attempt(&campaign.join("2"), 2, Some(0.07), 12); // passes
        attempt(&campaign.join("3"), 3, Some(0.30), 13); // a verdict over the threshold
        attempt(&campaign.join("4"), 4, None, 14); // not yet assimilated
                                                   // A failed first attempt and a passing retry: the retry's evidence counts.
        attempt(&campaign.join("5"), 5, Some(0.50), 15);
        attempt(&campaign.join("5/retry/1"), 5, Some(0.06), 16);

        let dry = promote_with(&c, &spool, false, false).await.unwrap();
        assert_eq!(dry.would_promote, [2, 5]);
        assert_eq!(
            (dry.not_eligible.clone(), dry.pending.clone()),
            (vec![3], vec![4])
        );
        assert!(dry.promoted.is_empty() && dry.failed.is_empty());
        assert!(!c.root.join(PROMOTIONS).exists());
        assert_eq!(trusted(&c), BTreeSet::from([1]));

        let done = promote_with(&c, &spool, false, true).await.unwrap();
        assert_eq!(done.promoted, [2, 5]);
        assert_eq!(trusted(&c), BTreeSet::from([1, 2, 5]));
        let records = promotion_records(&c.root).unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!((records[1].host, records[1].native_result_id), (5, 16));
        assert!(records
            .iter()
            .all(|r| r.added && r.committed && r.max_cer == 0.1));
        assert_eq!(
            done.trust_after_sha256.as_deref(),
            Some(boinc::sha256(&std::fs::read(&c.trust).unwrap()).as_str())
        );

        // A rebuild drops host 2 again: the same evidence never brings it back.
        let mut t = Trust::load(&c.trust).unwrap();
        t.hosts
            .insert(c.engine.image.clone(), BTreeSet::from([1, 5]));
        atomic(&c.trust, &serde_json::to_vec(&t).unwrap()).unwrap();
        let again = promote_with(&c, &spool, false, true).await.unwrap();
        assert!(again.promoted.is_empty());
        assert_eq!(again.evidence_used, [2]);
        assert_eq!(trusted(&c), BTreeSet::from([1, 5]));
        assert_eq!(promotion_records(&c.root).unwrap().len(), 2);
    }

    /// The ledger as an interrupted pass leaves it: host `host`'s record written ahead, with
    /// the list's digest at that moment, and trust not yet changed.
    fn interrupted(c: &Config, host: u64, workunit: &str) {
        let before = boinc::sha256(&std::fs::read(&c.trust).unwrap());
        write_records(
            &c.root,
            &[PromotionRecord {
                at: "2026-10-22T00:00:00Z".into(),
                language: "eng".into(),
                profile: "eng".into(),
                engine_image: c.engine.image.clone(),
                manifest_sha256: gold::submit::tests::prepared().manifest_sha256,
                max_cer: 0.1,
                host,
                workunit: workunit.into(),
                native_result_id: 12,
                added: false,
                committed: false,
                trust_before_sha256: before,
                trust_after_sha256: None,
            }],
        )
        .unwrap();
    }

    #[tokio::test]
    async fn an_interrupted_promotion_is_finished_only_while_trust_is_unchanged() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _) = promotion_fixture(dir.path());
        let spool = dir.path().join("spool");
        std::fs::create_dir_all(&c.root).unwrap();
        // Crashed between the record and the trust write: the next pass makes the change.
        interrupted(&c, 2, "usnm_gold_x");
        let done = promote_with(&c, &spool, false, true).await.unwrap();
        assert_eq!(done.promoted, [2]);
        assert_eq!(trusted(&c), BTreeSet::from([1, 2]));
        let records = promotion_records(&c.root).unwrap();
        assert!(records[0].committed && records[0].added);
        assert_eq!(
            records[0].trust_after_sha256.as_deref(),
            Some(boinc::sha256(&std::fs::read(&c.trust).unwrap()).as_str())
        );
        // Crashed, then trust was rebuilt before the next pass: the record is settled as it
        // stands and the host is not added.
        interrupted(&c, 3, "usnm_gold_y");
        let mut t = Trust::load(&c.trust).unwrap();
        t.hosts.insert(c.engine.image.clone(), BTreeSet::from([1]));
        atomic(&c.trust, &serde_json::to_vec(&t).unwrap()).unwrap();
        let done = promote_with(&c, &spool, false, true).await.unwrap();
        assert!(done.promoted.is_empty());
        assert_eq!(trusted(&c), BTreeSet::from([1]));
        let records = promotion_records(&c.root).unwrap();
        assert!(records[0].committed && !records[0].added);
    }

    #[tokio::test]
    async fn an_interrupted_promotion_judged_by_another_calibration_is_not_replayed() {
        let dir = tempfile::tempdir().unwrap();
        let (c, _) = promotion_fixture(dir.path());
        std::fs::create_dir_all(&c.root).unwrap();
        // Judged under max_cer 0.2, then the threshold became 0.1 before the trust write; the
        // record points at the current list version, so only the calibration tells them apart.
        interrupted(&c, 2, "usnm_gold_x");
        let mut records = promotion_records(&c.root).unwrap();
        records[0].max_cer = 0.2;
        write_records(&c.root, &records).unwrap();
        let done = promote_with(&c, &dir.path().join("spool"), false, true)
            .await
            .unwrap();
        assert!(done.promoted.is_empty());
        assert_eq!(trusted(&c), BTreeSet::from([1]));
        let records = promotion_records(&c.root).unwrap();
        assert!(records[0].committed && !records[0].added);
    }

    #[tokio::test]
    async fn evidence_recorded_after_the_judgment_is_not_applied_again() {
        let dir = tempfile::tempdir().unwrap();
        let (c, campaign) = promotion_fixture(dir.path());
        attempt(&campaign.join("2"), 2, Some(0.07), 12);
        // Another pass used the same workunit after this one judged it, and a rebuild has
        // since dropped the host: the ledger read under the lock wins.
        let candidate = PromotionRecord {
            at: "2026-10-22T00:00:00Z".into(),
            language: "eng".into(),
            profile: "eng".into(),
            engine_image: c.engine.image.clone(),
            manifest_sha256: gold::submit::tests::prepared().manifest_sha256,
            max_cer: 0.1,
            host: 2,
            workunit: "usnm_gold_x".into(),
            native_result_id: 12,
            added: false,
            committed: false,
            trust_before_sha256: String::new(),
            trust_after_sha256: None,
        };
        write_records(
            &c.root,
            &[PromotionRecord {
                committed: true,
                added: true,
                trust_after_sha256: Some("0".repeat(64)),
                ..candidate.clone()
            }],
        )
        .unwrap();
        let policy = Trust::load(&c.trust)
            .unwrap()
            .policy_for("eng", "eng")
            .unwrap();
        let mut summary = Promotion::default();
        commit_promotions(
            &c,
            c.scope.as_ref().unwrap(),
            &policy,
            vec![candidate],
            &mut summary,
        )
        .unwrap();
        assert_eq!(summary.evidence_used, [2]);
        assert!(summary.promoted.is_empty());
        assert_eq!(trusted(&c), BTreeSet::from([1]));
        assert_eq!(promotion_records(&c.root).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn promotion_refuses_another_challenge_and_isolates_a_bad_host() {
        let dir = tempfile::tempdir().unwrap();
        let (c, campaign) = promotion_fixture(dir.path());
        let spool = dir.path().join("spool");
        attempt(&campaign.join("2"), 2, Some(0.07), 12);
        // Host 3's report names host 2: fail closed for host 3 only.
        attempt(&campaign.join("3"), 2, Some(0.07), 13);
        let done = promote_with(&c, &spool, false, true).await.unwrap();
        assert_eq!(done.promoted, [2]);
        assert_eq!(done.failed.len(), 1);
        assert_eq!(done.failed[0].host, 3);
        assert_eq!(trusted(&c), BTreeSet::from([1, 2]));

        // The calibrated challenge is another one: nothing is judged or written.
        let mut t = Trust::load(&c.trust).unwrap();
        t.manifest_sha256 = Some("9".repeat(64));
        atomic(&c.trust, &serde_json::to_vec(&t).unwrap()).unwrap();
        let before = std::fs::read(&c.trust).unwrap();
        assert!(promote_with(&c, &spool, false, true).await.is_err());
        assert_eq!(std::fs::read(&c.trust).unwrap(), before);
        // No calibrated list at all: refused, never treated as "trust no host".
        std::fs::remove_file(&c.trust).unwrap();
        assert!(promote_with(&c, &spool, false, true).await.is_err());
    }
}
