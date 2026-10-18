//! Host trust from the gold check (#468): a `usnm_ocr` result is published only when the host
//! that returned it has a valid gold report for the same engine image with overall CER at or
//! below a threshold on one pinned challenge, without skips or complete crop loss, and the
//! page's source language, model profile and scoring configuration match that calibration.
//!
//! - `usnm-boinc gold trust` requires validated canonical `reports.jsonl` and bounded native
//!   `native-outcomes.jsonl` evidence; it writes scoped `trust.json` host eligibility.
//! - `usnm-boinc collect --trust trust.json --host-id N` checks the result's host before it
//!   publishes. An untrusted or unknown host's result is held back: its spool files and a reason
//!   go to `<root>/quarantine/<job>/`, nothing is published, and the job stays as it was, so a
//!   later collect (after the host passes a gold unit) can publish it.
//!
//! In native mode, BOINC's result-table host ID is authoritative and is checked against the
//! host claimed by the uploaded report. It remains a BOINC quality signal, not an identity check.
//! Native outcome snapshots are accepted only when every result of an assimilated workunit is
//! present.
use crate::{atomic, load, Stage};
use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use usnm_store::ObjectStore;

pub const TRUST_VERSION: u32 = 5;
const MAX_REPORTS: u64 = 64 << 20;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Scope {
    pub language: String,
    pub profile: String,
    pub psm: u8,
    pub dpi: u16,
}

impl Scope {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.language.len() == 3 && self.language.bytes().all(|b| b.is_ascii_lowercase()),
            "calibrated source language must be a three-letter lower-case code"
        );
        anyhow::ensure!(
            !self.profile.is_empty()
                && self.profile.len() <= 64
                && self
                    .profile
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-')),
            "invalid calibrated model profile"
        );
        anyhow::ensure!(
            self.psm == 3 && self.dpi == 300,
            "only the Gold PSM 3 / 300 DPI configuration is calibrated"
        );
        Ok(())
    }
}

/// `trust.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Trust {
    /// Engine image digest to the host ids with a passing gold report for it.
    pub hosts: BTreeMap<String, BTreeSet<u64>>,
    pub manifest_sha256: Option<String>,
    pub scope: Option<Scope>,
    pub max_cer: f64,
    pub v: u32,
}

#[derive(Deserialize)]
struct Row {
    completed_at: chrono::DateTime<chrono::Utc>,
    engine_image: String,
    #[serde(default)]
    bundle_version: u32,
    host_id: Option<u64>,
    manifest_sha256: String,
    #[serde(default)]
    workunit: Option<String>,
    #[serde(default)]
    native_result_id: Option<u64>,
    #[serde(default)]
    server_received_at: Option<i64>,
    #[serde(default)]
    language: Option<String>,
    #[serde(default)]
    profile: Option<String>,
    crops: usize,
    skipped: usize,
    complete_loss_crops: usize,
    profiles: BTreeSet<String>,
    psm: u8,
    dpi: u32,
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
    #[cfg(test)]
    pub(crate) fn build_without_native(
        reports: &[PathBuf],
        max_cer: f64,
        manifest_sha256: &str,
        scope: Scope,
    ) -> anyhow::Result<Self> {
        Self::build_inner(reports, &[], max_cer, manifest_sha256, scope, false)
    }

    /// Builds trust from canonical quality reports and native BOINC attempt metadata. Unlike
    /// [`Self::build`], this requires server-bound result IDs and timestamps for every report.
    pub fn build_with_outcomes(
        reports: &[PathBuf],
        outcomes: &[PathBuf],
        max_cer: f64,
        manifest_sha256: &str,
        scope: Scope,
    ) -> anyhow::Result<Self> {
        anyhow::ensure!(
            !outcomes.is_empty(),
            "native Gold outcome files are required"
        );
        Self::build_inner(reports, outcomes, max_cer, manifest_sha256, scope, true)
    }

    fn build_inner(
        reports: &[PathBuf],
        outcome_paths: &[PathBuf],
        max_cer: f64,
        manifest_sha256: &str,
        scope: Scope,
        require_native: bool,
    ) -> anyhow::Result<Self> {
        scope.validate()?;
        anyhow::ensure!(
            (0.0..=1.0).contains(&max_cer),
            "max CER must be between 0 and 1"
        );
        anyhow::ensure!(
            usnm_ocr_contract::boinc::is_sha256(manifest_sha256),
            "challenge manifest must be a lower-case SHA-256"
        );
        let mut latest = BTreeMap::new();
        let mut native_results = BTreeMap::new();
        let mut native_count = 0usize;
        let mut native_bytes = 0usize;
        for path in outcome_paths {
            let bytes = crate::read(path, MAX_REPORTS as usize)
                .with_context(|| format!("reading {}", path.display()))?;
            native_bytes = native_bytes.saturating_add(bytes.len());
            anyhow::ensure!(
                native_bytes <= 16 << 20,
                "native Gold outcome evidence exceeds its size limit"
            );
            for (n, line) in std::str::from_utf8(&bytes)?.lines().enumerate() {
                if line.trim().is_empty() {
                    continue;
                }
                native_count += 1;
                anyhow::ensure!(native_count <= 10_000, "too many native Gold result rows");
                let row: crate::gold::collect::NativeResult = serde_json::from_str(line)
                    .with_context(|| format!("{}:{}", path.display(), n + 1))?;
                anyhow::ensure!(
                    row.schema_version == 3
                        && row.workunit_assimilate_state
                            == crate::gold::collect::BOINC_ASSIMILATE_STATE_DONE
                        && row.workunit_result_count > 0
                        && row.app == "usnm_gold"
                        && row.workunit.strip_prefix("usnm_gold_").is_some_and(
                            |id| id.len() == 64 && usnm_ocr_contract::boinc::is_sha256(id)
                        )
                        && row.result_id > 0
                        && row.app_version_id > 0
                        && immutable_image(&row.engine_image)
                        && usnm_ocr_contract::boinc::is_sha256(&row.manifest_sha256)
                        && row.host_id.is_none_or(|host| host > 0)
                        && (0..=16).contains(&row.server_state)
                        && (0..=16).contains(&row.outcome)
                        && (-4096..=4096).contains(&row.exit_status)
                        && row.sent_at.is_none_or(|time| time > 0)
                        && row.received_at.is_none_or(|time| time > 0)
                        && row.bundle_version > 0
                        && row.psm == 3
                        && row.dpi == 300,
                    "invalid native Gold result metadata"
                );
                let key = (row.workunit.clone(), row.result_id);
                anyhow::ensure!(
                    native_results.insert(key, row).is_none(),
                    "duplicate native Gold result"
                );
            }
        }
        if require_native {
            crate::gold::collect::validate_complete_result_sets(
                &native_results.values().cloned().collect::<Vec<_>>(),
            )?;
        }
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
                anyhow::ensure!(
                    row.manifest_sha256 == manifest_sha256,
                    "gold report challenge does not match the calibrated manifest"
                );
                let native_binding = if require_native {
                    let language = row
                        .language
                        .as_deref()
                        .context("canonical report lacks native language scope")?;
                    let profile = row
                        .profile
                        .as_deref()
                        .context("canonical report lacks native profile scope")?;
                    anyhow::ensure!(
                        language.len() == 3
                            && language.bytes().all(|b| b.is_ascii_lowercase())
                            && !profile.is_empty()
                            && profile.len() <= 64
                            && profile.bytes().all(|b| {
                                b.is_ascii_alphanumeric() || matches!(b, b'_' | b'+' | b'-')
                            }),
                        "canonical report has invalid native calibration scope"
                    );
                    let workunit = row
                        .workunit
                        .as_deref()
                        .context("canonical report lacks its workunit")?;
                    let result_id = row
                        .native_result_id
                        .context("canonical report lacks native result id")?;
                    let received_at = row
                        .server_received_at
                        .context("canonical report lacks server receive timestamp")?;
                    let native = native_results
                        .get(&(workunit.to_owned(), result_id))
                        .context("canonical report has no matching native BOINC result")?;
                    anyhow::ensure!(
                        native.canonical
                            && native.app == "usnm_gold"
                            && native.host_id.is_some_and(|host| host > 0)
                            && native.host_id == row.host_id
                            && native.engine_image == row.engine_image
                            && native.bundle_version == row.bundle_version
                            && native.manifest_sha256 == row.manifest_sha256
                            && native.language == language
                            && native.profile == profile
                            && native.psm == row.psm
                            && u32::from(native.dpi) == row.dpi
                            && native.received_at == Some(received_at)
                            && native.server_state == crate::gold::collect::BOINC_SERVER_STATE_OVER
                            && native.outcome == crate::gold::collect::BOINC_OUTCOME_SUCCESS,
                        "canonical report disagrees with its native BOINC result"
                    );
                    Some((language, profile, received_at))
                } else {
                    None
                };
                if let Some((language, profile, _)) = native_binding {
                    if language != scope.language || profile != scope.profile {
                        continue;
                    }
                }
                anyhow::ensure!(
                    row.complete_loss_crops <= row.crops,
                    "gold report complete-loss count exceeds scored crops"
                );
                anyhow::ensure!(
                    row.psm == scope.psm && row.dpi == u32::from(scope.dpi),
                    "gold report does not cover the calibrated scoring configuration"
                );
                let event_at = if let Some((_, _, received_at)) = native_binding {
                    chrono::DateTime::from_timestamp(received_at, 0)
                        .context("invalid server receive timestamp")?
                } else {
                    row.completed_at
                };
                let overall = row.summary.iter().find(|s| s.source == "all");
                if let Some(host) = row.host_id.filter(|h| *h > 0) {
                    let passed = row.crops > 0
                        && row.skipped == 0
                        && row.complete_loss_crops == 0
                        && row.profiles.len() == 1
                        && row.profiles.contains(&scope.profile)
                        && overall
                            .and_then(|s| s.cer)
                            .is_some_and(|cer| cer.is_finite() && cer >= 0.0 && cer <= max_cer);
                    let value = latest
                        .entry((row.engine_image, host))
                        .or_insert((event_at, passed));
                    if event_at > value.0 {
                        *value = (event_at, passed);
                    } else if event_at == value.0 {
                        value.1 &= passed;
                    }
                }
            }
        }
        if require_native {
            for event in native_results.values() {
                if event.manifest_sha256 != manifest_sha256
                    || event.language != scope.language
                    || event.profile != scope.profile
                    || event.psm != scope.psm
                    || u32::from(event.dpi) != u32::from(scope.dpi)
                    || event.server_state != crate::gold::collect::BOINC_SERVER_STATE_OVER
                    || event.outcome != crate::gold::collect::BOINC_OUTCOME_CLIENT_ERROR
                    || matches!(
                        event.exit_status,
                        crate::gold::collect::BOINC_EXIT_ABORTED_BY_PROJECT
                            | crate::gold::collect::BOINC_EXIT_ABORTED_VIA_GUI
                    )
                {
                    continue;
                }
                let (Some(host), Some(received_at)) = (event.host_id, event.received_at) else {
                    continue;
                };
                let event_at = chrono::DateTime::from_timestamp(received_at, 0)
                    .context("invalid terminal result receive timestamp")?;
                let value = latest
                    .entry((event.engine_image.clone(), host))
                    .or_insert((event_at, false));
                if event_at > value.0 {
                    *value = (event_at, false);
                } else if event_at == value.0 {
                    value.1 = false;
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
            manifest_sha256: Some(manifest_sha256.to_owned()),
            scope: Some(scope),
            max_cer,
            v: TRUST_VERSION,
        })
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let t: Self = serde_json::from_slice(&crate::read(path, MAX_REPORTS as usize)?)
            .with_context(|| format!("reading {}", path.display()))?;
        anyhow::ensure!(t.v == TRUST_VERSION, "unsupported trust list version");
        t.scope
            .as_ref()
            .context("missing calibrated job scope")?
            .validate()?;
        anyhow::ensure!(
            t.manifest_sha256
                .as_deref()
                .is_some_and(usnm_ocr_contract::boinc::is_sha256),
            "invalid calibrated challenge manifest"
        );
        anyhow::ensure!((0.0..=1.0).contains(&t.max_cer), "invalid trust threshold");
        anyhow::ensure!(
            t.hosts
                .iter()
                .all(|(image, hosts)| immutable_image(image) && !hosts.contains(&0)),
            "invalid trust image or host"
        );
        Ok(t)
    }

    /// For issuing gold only: a missing list or legacy v1-v4 list trusts no host, so every
    /// host is offered gold (which is how a threshold gets
    /// calibrated in the first place). Anything else must load as [`Trust::load`] would.
    pub fn load_or_none_trusted(path: &Path) -> anyhow::Result<Self> {
        let none = || Self {
            hosts: BTreeMap::new(),
            manifest_sha256: None,
            scope: None,
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
        if matches!(
            raw.get("v").and_then(serde_json::Value::as_u64),
            Some(1..=4)
        ) {
            eprintln!(
                "{} uses an uncalibrated legacy trust policy; no host is trusted",
                path.display()
            );
            return Ok(none());
        }
        Self::load(path)
    }

    /// Image-level eligibility for Gold onboarding only, not permission to publish a page.
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

    pub fn refuse_job(
        &self,
        job: &usnm_ocr_contract::boinc::Job,
        host_id: Option<u64>,
    ) -> Option<String> {
        if let Some(reason) = self.refuse(&job.engine.image, host_id) {
            return Some(reason);
        }
        let Some(scope) = &self.scope else {
            return Some("no calibrated job scope".into());
        };
        if job.message.language != scope.language
            || job.engine.profile != scope.profile
            || job.engine.psm != scope.psm
            || job.engine.dpi != scope.dpi
            || !job.engine.kind.is_tesseract()
        {
            return Some(
                "source language, model profile or scoring configuration is outside calibrated trust"
                    .into(),
            );
        }
        None
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
        if let Some(reason) = trust.refuse_job(&record.job, host_id) {
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

    const MANIFEST: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn scope() -> Scope {
        Scope {
            language: "eng".into(),
            profile: "eng".into(),
            psm: 3,
            dpi: 300,
        }
    }

    fn row(image: &str, host: Option<u64>, cer: Option<f64>) -> String {
        serde_json::json!({
            "completed_at": "2026-10-15T00:00:00Z",
            "bundle_version": 1,
            "engine_image": image,
            "host_id": host,
            "manifest_sha256": MANIFEST,
            "crops": 2,
            "skipped": 0,
            "complete_loss_crops": 0,
            "profiles": ["eng"],
            "psm": 3,
            "dpi": 300,
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
        let t = Trust::build_without_native(&[a, b], 0.10, MANIFEST, scope()).unwrap();
        assert_eq!(t.manifest_sha256.as_deref(), Some(MANIFEST));
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
        assert!(Trust::build_without_native(&[], 2.0, MANIFEST, scope()).is_err());
        assert!(Trust::build_without_native(&[], 0.1, "not-a-manifest", scope()).is_err());
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
            assert!(Trust::build_without_native(&paths, 0.1, MANIFEST, scope())
                .unwrap()
                .refuse(&image, Some(42))
                .is_some());
        }
        std::fs::write(&b, row(&image, Some(42), None)).unwrap();
        assert!(
            Trust::build_without_native(&[a, b.clone()], 0.1, MANIFEST, scope())
                .unwrap()
                .hosts
                .is_empty()
        );
        std::fs::write(&b, row("mutable:latest", Some(42), Some(0.0))).unwrap();
        assert!(Trust::build_without_native(&[b], 0.1, MANIFEST, scope()).is_err());
    }

    #[test]
    fn gold_issuance_treats_missing_or_legacy_lists_as_trusting_no_host() {
        let tmp = tempfile::tempdir().unwrap();
        let p = tmp.path().join("trust.json");
        let image = format!("sha256:{}", "a".repeat(64));
        assert!(Trust::load_or_none_trusted(&p).unwrap().hosts.is_empty());
        for version in [1, 2, 3, 4] {
            let legacy =
                serde_json::json!({"v": version, "max_cer": 0.1, "hosts": {image.clone(): [42]}});
            std::fs::write(&p, legacy.to_string()).unwrap();
            assert!(Trust::load(&p).is_err());
            let t = Trust::load_or_none_trusted(&p).unwrap();
            assert!(t.refuse(&image, Some(42)).is_some());
        }
        std::fs::write(&p, r#"{"v": 99, "max_cer": 0.1, "hosts": {}}"#).unwrap();
        assert!(Trust::load_or_none_trusted(&p).is_err());
        std::fs::write(&p, "not json").unwrap();
        assert!(Trust::load_or_none_trusted(&p).is_err());
        let current = Trust::build_without_native(&[], 0.1, MANIFEST, scope()).unwrap();
        std::fs::write(&p, serde_json::to_vec(&current).unwrap()).unwrap();
        assert_eq!(Trust::load_or_none_trusted(&p).unwrap(), current);
    }

    #[test]
    fn complete_loss_and_incomplete_coverage_revoke_even_below_aggregate_threshold() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("reports.jsonl");
        let image = format!("sha256:{}", "a".repeat(64));
        let good = row(&image, Some(42), Some(0.0551));
        for (field, value) in [("complete_loss_crops", 1), ("skipped", 1), ("crops", 0)] {
            let mut failed: serde_json::Value =
                serde_json::from_str(&row(&image, Some(42), Some(0.0827))).unwrap();
            failed[field] = serde_json::json!(value);
            failed["completed_at"] = serde_json::json!("2026-10-16T00:00:00Z");
            std::fs::write(&path, format!("{good}\n{failed}\n")).unwrap();
            assert!(
                Trust::build_without_native(
                    std::slice::from_ref(&path),
                    0.08734328574534386,
                    MANIFEST,
                    scope(),
                )
                .unwrap()
                .refuse(&image, Some(42))
                .is_some(),
                "{field}"
            );
        }
    }

    #[test]
    fn rejects_wrong_challenges_and_missing_or_impossible_crop_quality() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("reports.jsonl");
        let image = format!("sha256:{}", "a".repeat(64));
        let good: serde_json::Value =
            serde_json::from_str(&row(&image, Some(42), Some(0.01))).unwrap();
        let mut wrong = good.clone();
        wrong["manifest_sha256"] = serde_json::json!("c".repeat(64));
        std::fs::write(&path, wrong.to_string()).unwrap();
        assert!(
            Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                .is_err()
        );
        let mut absent = good.clone();
        absent
            .as_object_mut()
            .unwrap()
            .remove("complete_loss_crops");
        std::fs::write(&path, absent.to_string()).unwrap();
        assert!(
            Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                .is_err()
        );
        let mut impossible = good;
        impossible["complete_loss_crops"] = serde_json::json!(3);
        std::fs::write(&path, impossible.to_string()).unwrap();
        assert!(Trust::build_without_native(&[path], 0.1, MANIFEST, scope()).is_err());
    }

    #[test]
    fn rejects_unscoped_or_mismatched_evidence() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("reports.jsonl");
        let image = format!("sha256:{}", "a".repeat(64));
        let good: serde_json::Value =
            serde_json::from_str(&row(&image, Some(42), Some(0.01))).unwrap();
        for (field, value) in [
            ("psm", serde_json::json!(6)),
            ("dpi", serde_json::json!(150)),
        ] {
            let mut wrong = good.clone();
            wrong[field] = value;
            std::fs::write(&path, wrong.to_string()).unwrap();
            assert!(
                Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                    .is_err(),
                "{field}"
            );
            wrong.as_object_mut().unwrap().remove(field);
            std::fs::write(&path, wrong.to_string()).unwrap();
            assert!(
                Trust::build_without_native(std::slice::from_ref(&path), 0.1, MANIFEST, scope())
                    .is_err(),
                "missing {field}"
            );
        }
        let mut missing = good;
        missing.as_object_mut().unwrap().remove("profiles");
        std::fs::write(&path, missing.to_string()).unwrap();
        assert!(Trust::build_without_native(&[path], 0.1, MANIFEST, scope()).is_err());
    }

    #[test]
    fn newest_wrong_or_mixed_profile_report_revokes() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("reports.jsonl");
        let image = format!("sha256:{}", "a".repeat(64));
        let good = row(&image, Some(42), Some(0.01));
        for kind in ["wrong-profile", "mixed-profiles"] {
            let mut failed: serde_json::Value = serde_json::from_str(&good).unwrap();
            failed["completed_at"] = serde_json::json!("2026-10-16T00:00:00Z");
            failed["profiles"] = serde_json::json!(["fra"]);
            if kind == "mixed-profiles" {
                failed["profiles"] = serde_json::json!(["eng", "fra"]);
            }
            std::fs::write(&path, format!("{good}\n{failed}\n")).unwrap();
            assert!(Trust::build_without_native(
                std::slice::from_ref(&path),
                0.1,
                MANIFEST,
                scope()
            )
            .unwrap()
            .hosts
            .is_empty());
        }
    }

    #[test]
    fn rejects_missing_or_invalid_policy_scope() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trust.json");
        let valid = Trust::build_without_native(&[], 0.1, MANIFEST, scope()).unwrap();
        let mut missing = valid.clone();
        missing.scope = None;
        std::fs::write(&path, serde_json::to_vec(&missing).unwrap()).unwrap();
        assert!(Trust::load(&path).is_err());
        for (field, value) in [
            ("language", serde_json::json!("english")),
            ("profile", serde_json::json!("")),
            ("psm", serde_json::json!(6)),
            ("dpi", serde_json::json!(150)),
        ] {
            let mut invalid = serde_json::to_value(&valid).unwrap();
            invalid["scope"][field] = value;
            std::fs::write(&path, invalid.to_string()).unwrap();
            assert!(Trust::load(&path).is_err(), "{field}");
            let invalid: Trust = serde_json::from_value(invalid).unwrap();
            assert!(
                Trust::build_without_native(&[], 0.1, MANIFEST, invalid.scope.unwrap()).is_err(),
                "{field}"
            );
        }
    }
}
