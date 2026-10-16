//! Host trust from the gold check (#468): a `usnm_ocr` result is published only when the host
//! that returned it has a valid gold report for the same engine image with overall CER at or
//! below a threshold.
//!
//! - `usnm-boinc gold trust` reads one or more `reports.jsonl` files (from `gold collect`) and
//!   writes `trust.json`: per engine image, the host ids that passed.
//! - `usnm-boinc collect --trust trust.json --host-id N` checks the result's host before it
//!   publishes. An untrusted or unknown host's result is held back: its spool files and a reason
//!   go to `<root>/quarantine/<job>/`, nothing is published, and the job stays as it was, so a
//!   later collect (after the host passes a gold unit) can publish it.
//!
//! The host id is the client's own claim, so this is a quality signal for trusted volunteers,
//! not an identity check.
use crate::{atomic, load, Stage};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use usnm_store::ObjectStore;

pub const TRUST_VERSION: u32 = 2;
const MAX_REPORTS: u64 = 64 << 20;

/// `trust.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    /// Engine image digest to the host ids with a passing gold report for it.
    pub hosts: BTreeMap<String, BTreeSet<u64>>,
    pub max_cer: f64,
    pub v: u32,
}

#[derive(Deserialize)]
struct Row {
    completed_at: chrono::DateTime<chrono::Utc>,
    engine_image: String,
    host_id: Option<u64>,
    summary: Vec<SummaryRow>,
}
#[derive(Deserialize)]
struct SummaryRow {
    cer: Option<f64>,
    source: String,
}

impl Trust {
    /// Builds the list from `gold collect`'s `reports.jsonl` files. Every row there is a valid
    /// canonical result (collect checked it again); rows with no host id or no overall CER are
    /// skipped.
    pub fn build(reports: &[PathBuf], max_cer: f64) -> anyhow::Result<Self> {
        anyhow::ensure!(
            (0.0..=1.0).contains(&max_cer),
            "max CER must be between 0 and 1"
        );
        let mut latest = BTreeMap::new();
        for path in reports {
            let bytes = crate::read(path, MAX_REPORTS as usize)
                .with_context(|| format!("reading {}", path.display()))?;
            for (n, line) in std::str::from_utf8(&bytes)?.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                let row: Row = serde_json::from_str(line)
                    .with_context(|| format!("{}:{}", path.display(), n + 1))?;
                anyhow::ensure!(
                    immutable_image(&row.engine_image),
                    "gold engine image must be an immutable sha256 digest"
                );
                let overall = row.summary.iter().find(|s| s.source == "all");
                if let Some(host) = row.host_id.filter(|h| *h > 0) {
                    let passed = overall
                        .and_then(|s| s.cer)
                        .is_some_and(|cer| cer.is_finite() && cer >= 0.0 && cer <= max_cer);
                    let value = latest
                        .entry((row.engine_image, host))
                        .or_insert((row.completed_at, passed));
                    if row.completed_at > value.0 {
                        *value = (row.completed_at, passed);
                    } else if row.completed_at == value.0 {
                        value.1 &= passed;
                    }
                }
            }
        }
        let mut hosts: BTreeMap<String, BTreeSet<u64>> = BTreeMap::new();
        for ((image, host), (_, passed)) in latest {
            if passed {
                hosts.entry(image).or_default().insert(host);
            }
        }
        Ok(Self {
            hosts,
            max_cer,
            v: TRUST_VERSION,
        })
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let t: Self = serde_json::from_slice(&crate::read(path, MAX_REPORTS as usize)?)
            .with_context(|| format!("reading {}", path.display()))?;
        anyhow::ensure!(t.v == TRUST_VERSION, "unsupported trust list version");
        anyhow::ensure!((0.0..=1.0).contains(&t.max_cer), "invalid trust threshold");
        anyhow::ensure!(
            t.hosts
                .iter()
                .all(|(image, hosts)| immutable_image(image) && !hosts.contains(&0)),
            "invalid trust image or host"
        );
        Ok(t)
    }

    /// For issuing gold only: a missing list, or the uncalibrated v1 list that v2 deliberately
    /// rejects, trusts no host, so every host is offered gold (which is how a threshold gets
    /// calibrated in the first place). Anything else must load as [`Trust::load`] would.
    pub fn load_or_none_trusted(path: &Path) -> anyhow::Result<Self> {
        let none = || Self {
            hosts: BTreeMap::new(),
            max_cer: 0.0,
            v: TRUST_VERSION,
        };
        if !path.exists() {
            eprintln!("no trust list at {}; no host is trusted", path.display());
            return Ok(none());
        }
        let raw: serde_json::Value =
            serde_json::from_slice(&crate::read(path, MAX_REPORTS as usize)?)
                .with_context(|| format!("reading {}", path.display()))?;
        if raw.get("v").and_then(serde_json::Value::as_u64) == Some(1) {
            eprintln!(
                "{} is the uncalibrated v1 trust list; no host is trusted",
                path.display()
            );
            return Ok(none());
        }
        Self::load(path)
    }

    /// `None` when the host is trusted for this image; otherwise why not.
    pub fn refuse(&self, engine_image: &str, host_id: Option<u64>) -> Option<String> {
        if !immutable_image(engine_image) || host_id == Some(0) {
            return Some("invalid immutable engine image or host id".into());
        }
        let Some(host) = host_id else {
            return Some("no host id for this result".into());
        };
        if self
            .hosts
            .get(engine_image)
            .is_some_and(|h| h.contains(&host))
        {
            None
        } else {
            Some(format!(
                "host {host} has no gold report for {engine_image} with CER <= {}",
                self.max_cer
            ))
        }
    }
}

fn immutable_image(image: &str) -> bool {
    image
        .strip_prefix("sha256:")
        .is_some_and(usnm_ocr_contract::boinc::is_sha256)
}

pub fn quarantine_dir(root: &Path, id: &str) -> PathBuf {
    root.join("quarantine").join(id)
}

/// `usnm-boinc collect` with the trust check in front of [`crate::collect`].
pub async fn collect(
    root: &Path,
    spool: &Path,
    id: &str,
    trust: &Trust,
    host_id: Option<u64>,
    curated: &dyn ObjectStore,
    out: &dyn ObjectStore,
) -> anyhow::Result<()> {
    let record = load(root, id)?;
    let wu = record.job.workunit();
    // A terminal failure has no canonical result (so no host id): let collect mark it failed.
    let terminal = spool.join(format!("{wu}_error")).exists()
        || spool.join(format!("{wu}_no_output_files")).exists();
    if record.stage != Stage::Published && !terminal {
        if let Some(reason) = trust.refuse(&record.job.engine.image, host_id) {
            let q = quarantine_dir(root, id);
            std::fs::create_dir_all(&q)?;
            for logical in ["result.json", "text.txt", "layout.xml"] {
                let f = spool.join(format!("{wu}__file_{logical}"));
                if f.is_file() {
                    std::fs::copy(&f, q.join(logical))?;
                }
            }
            atomic(
                &q.join("reason.json"),
                &serde_json::to_vec_pretty(&serde_json::json!({
                    "at": chrono::Utc::now().to_rfc3339(),
                    "engine_image": record.job.engine.image,
                    "host_id": host_id,
                    "job": id,
                    "reason": reason,
                    "workunit": wu,
                }))?,
            )?;
            bail!("held back, not published: {reason}");
        }
    }
    crate::collect(root, spool, id, curated, out).await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(image: &str, host: Option<u64>, cer: Option<f64>) -> String {
        serde_json::json!({
            "completed_at": "2026-10-15T00:00:00Z",
            "bundle_version": 1,
            "engine_image": image,
            "host_id": host,
            "summary": [
                {"cer": cer, "crops": 2, "ref_chars": 10, "source": "all", "wer": cer},
                {"cer": 0.0, "crops": 1, "ref_chars": 5, "source": "bln600", "wer": 0.0},
            ],
        })
        .to_string()
    }

    #[test]
    fn builds_from_reports_and_refuses_the_rest() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("a.jsonl");
        let b = tmp.path().join("b.jsonl");
        std::fs::write(
            &a,
            [
                row(&format!("sha256:{}", "a".repeat(64)), Some(1), Some(0.05)),
                row(&format!("sha256:{}", "a".repeat(64)), Some(2), Some(0.30)),
                row(&format!("sha256:{}", "a".repeat(64)), None, Some(0.01)),
                row(&format!("sha256:{}", "a".repeat(64)), Some(3), None),
            ]
            .join("\n"),
        )
        .unwrap();
        let image_a = format!("sha256:{}", "a".repeat(64));
        let image_b = format!("sha256:{}", "b".repeat(64));
        std::fs::write(&b, row(&image_b, Some(2), Some(0.10))).unwrap();
        let t = Trust::build(&[a, b], 0.10).unwrap();
        assert_eq!(t.hosts[&image_a], BTreeSet::from([1]));
        assert_eq!(t.hosts[&image_b], BTreeSet::from([2]));
        assert!(t.refuse(&image_a, Some(1)).is_none());
        assert!(t.refuse(&image_a, Some(2)).is_some());
        assert!(t.refuse(&image_b, Some(1)).is_some());
        assert!(t.refuse("sha256:cc", Some(1)).is_some());
        assert!(t.refuse("sha256:aa", None).is_some());
        let p = tmp.path().join("trust.json");
        std::fs::write(&p, serde_json::to_vec(&t).unwrap()).unwrap();
        assert_eq!(Trust::load(&p).unwrap(), t);
        assert!(Trust::build(&[], 2.0).is_err());
    }

    #[test]
    fn newest_failure_revokes_in_any_file_order_and_ties_fail_closed() {
        let tmp = tempfile::tempdir().unwrap();
        let a = tmp.path().join("pass");
        let b = tmp.path().join("fail");
        let image = format!("sha256:{}", "a".repeat(64));
        std::fs::write(&a, row(&image, Some(42), Some(0.01))).unwrap();
        std::fs::write(
            &b,
            row(&image, Some(42), Some(0.8)).replace("2026-10-15", "2026-10-16"),
        )
        .unwrap();
        for paths in [vec![a.clone(), b.clone()], vec![b.clone(), a.clone()]] {
            assert!(Trust::build(&paths, 0.1)
                .unwrap()
                .refuse(&image, Some(42))
                .is_some());
        }
        std::fs::write(&b, row(&image, Some(42), None)).unwrap();
        assert!(Trust::build(&[a, b.clone()], 0.1).unwrap().hosts.is_empty());
        std::fs::write(&b, row("mutable:latest", Some(42), Some(0.0))).unwrap();
        assert!(Trust::build(&[b], 0.1).is_err());
    }

    #[test]
    fn gold_issuance_treats_a_missing_or_v1_list_as_trusting_no_host() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("trust.json");
        let image = format!("sha256:{}", "a".repeat(64));
        assert!(Trust::load_or_none_trusted(&p).unwrap().hosts.is_empty());
        let v1 = serde_json::json!({"v": 1, "max_cer": 0.1, "hosts": {image.clone(): [42]}});
        std::fs::write(&p, v1.to_string()).unwrap();
        assert!(Trust::load(&p).is_err());
        let t = Trust::load_or_none_trusted(&p).unwrap();
        assert!(t.refuse(&image, Some(42)).is_some());
        std::fs::write(&p, r#"{"v": 3, "max_cer": 0.1, "hosts": {}}"#).unwrap();
        assert!(Trust::load_or_none_trusted(&p).is_err());
        std::fs::write(&p, "not json").unwrap();
        assert!(Trust::load_or_none_trusted(&p).is_err());
        let v2 = Trust::build(&[], 0.1).unwrap();
        std::fs::write(&p, serde_json::to_vec(&v2).unwrap()).unwrap();
        assert_eq!(Trust::load_or_none_trusted(&p).unwrap(), v2);
    }
}
