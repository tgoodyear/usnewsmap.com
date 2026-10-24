//! Explicitly invoked, bounded host onboarding and independent OCR spot checks.
//! These workflows never change project settings or publication state. Only `promote` changes
//! trust, and only by adding hosts whose complete native gold evidence passes a scope's
//! existing calibration (#901).
use crate::{gold, load, read};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use tokio::process::Command;
use usnm_ocr_contract::boinc;

pub mod cli;
mod onboard;
mod promote;
mod spot;

pub use onboard::onboard;
pub use promote::{promote, HostFailure, Promotion};
pub use spot::{spot_compare, spot_submit};

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

const MAX_ATTEMPTS: u32 = 10_000;

/// Attempt 0 keeps the original per-host root; later attempts are fresh campaign roots
/// because a targeted gold campaign root is bound to one assignment.
fn attempt_root(host_root: &Path, attempt: u32) -> PathBuf {
    if attempt == 0 {
        host_root.to_path_buf()
    } else {
        host_root.join("retry").join(attempt.to_string())
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
    use super::spot::sample;
    use super::*;

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
}
