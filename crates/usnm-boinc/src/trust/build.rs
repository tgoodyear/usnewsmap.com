use super::{immutable_image, Scope, Trust, MAX_REPORTS, TRUST_VERSION};
use anyhow::Context;
use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

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
            policies: Vec::new(),
            v: TRUST_VERSION,
        })
    }
}
