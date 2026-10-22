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
    pub fn validate(&self) -> anyhow::Result<()> {
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

/// One calibrated `(language, profile, psm, dpi)` policy entry within [`Trust`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScopedPolicy {
    /// Engine image digest to the host ids with a passing gold report for this scope.
    pub hosts: BTreeMap<String, BTreeSet<u64>>,
    pub manifest_sha256: String,
    pub scope: Scope,
    pub max_cer: f64,
}

impl ScopedPolicy {
    fn validate(&self) -> anyhow::Result<()> {
        Trust::validate_policy_fields(
            &self.hosts,
            &self.manifest_sha256,
            &self.scope,
            self.max_cer,
        )
    }
}

/// Borrowed view of a calibrated `(language, profile)` policy within [`Trust`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PolicyView<'a> {
    pub hosts: &'a BTreeMap<String, BTreeSet<u64>>,
    pub manifest_sha256: &'a str,
    pub scope: &'a Scope,
    pub max_cer: f64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LegacyTrust {
    #[allow(dead_code)]
    hosts: BTreeMap<String, BTreeSet<u64>>,
    #[allow(dead_code)]
    #[serde(default)]
    manifest_sha256: Option<String>,
    #[allow(dead_code)]
    #[serde(default)]
    scope: Option<serde_json::Value>,
    #[allow(dead_code)]
    max_cer: f64,
    #[allow(dead_code)]
    v: u32,
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
    /// Coexisting per-(language, profile) calibrated policies when multiple scopes are active.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub policies: Vec<ScopedPolicy>,
    pub v: u32,
}

/// What [`Trust::admit_hosts`] changed: the list's digest before and after, and the hosts it
/// added (empty when every host was already eligible, and the file was not rewritten).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Admission {
    pub before_sha256: String,
    pub after_sha256: String,
    pub added: BTreeSet<u64>,
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
            policies: Vec::new(),
            v: TRUST_VERSION,
        })
    }

    fn validate_policy_fields(
        hosts: &BTreeMap<String, BTreeSet<u64>>,
        manifest_sha256: &str,
        scope: &Scope,
        max_cer: f64,
    ) -> anyhow::Result<()> {
        scope.validate()?;
        anyhow::ensure!(
            usnm_ocr_contract::boinc::is_sha256(manifest_sha256),
            "invalid calibrated challenge manifest"
        );
        anyhow::ensure!(
            max_cer.is_finite() && (0.0..=1.0).contains(&max_cer),
            "invalid trust threshold"
        );
        anyhow::ensure!(
            hosts
                .iter()
                .all(|(image, hosts)| immutable_image(image) && !hosts.contains(&0)),
            "invalid trust image or host"
        );
        Ok(())
    }

    fn primary_policy(&self) -> anyhow::Result<ScopedPolicy> {
        let scope = self.scope.clone().context("missing calibrated job scope")?;
        let manifest_sha256 = self
            .manifest_sha256
            .clone()
            .context("invalid calibrated challenge manifest")?;
        Ok(ScopedPolicy {
            hosts: self.hosts.clone(),
            manifest_sha256,
            scope,
            max_cer: self.max_cer,
        })
    }

    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.v == 4 || self.v == TRUST_VERSION,
            "unsupported trust list version"
        );
        anyhow::ensure!(
            self.v == TRUST_VERSION || self.policies.is_empty(),
            "v4 trust list cannot contain multiple scoped policies"
        );
        let scope = self
            .scope
            .as_ref()
            .context("missing calibrated job scope")?;
        let manifest_sha256 = self
            .manifest_sha256
            .as_deref()
            .context("invalid calibrated challenge manifest")?;
        Self::validate_policy_fields(&self.hosts, manifest_sha256, scope, self.max_cer)?;
        if !self.policies.is_empty() {
            anyhow::ensure!(
                self.policies.len() <= 64,
                "too many scoped policies in trust list"
            );
            let mut seen = BTreeSet::new();
            let mut primary_matched = false;
            for policy in &self.policies {
                policy.validate()?;
                let key = (
                    policy.scope.language.as_str(),
                    policy.scope.profile.as_str(),
                );
                anyhow::ensure!(seen.insert(key), "duplicate calibrated scope in trust list");
                if policy.scope.language == scope.language && policy.scope.profile == scope.profile
                {
                    anyhow::ensure!(
                        policy.hosts == self.hosts
                            && policy.manifest_sha256 == manifest_sha256
                            && &policy.scope == scope
                            && policy.max_cer == self.max_cer,
                        "top-level trust scope disagrees with its scoped policy entry"
                    );
                    primary_matched = true;
                }
            }
            anyhow::ensure!(
                primary_matched,
                "scoped policies must include the primary trust scope"
            );
        }
        Ok(())
    }

    /// Returns all active calibrated `(language, profile)` policies in this trust artifact.
    pub fn effective_policies(&self) -> anyhow::Result<Vec<ScopedPolicy>> {
        self.validate()?;
        if self.policies.is_empty() {
            Ok(vec![self.primary_policy()?])
        } else {
            Ok(self.policies.clone())
        }
    }

    /// Borrows the calibrated policy for `(language, profile)` without cloning host maps.
    pub fn policy_view(&self, language: &str, profile: &str) -> Option<PolicyView<'_>> {
        if !self.policies.is_empty() {
            self.policies
                .iter()
                .find(|p| p.scope.language == language && p.scope.profile == profile)
                .map(|p| PolicyView {
                    hosts: &p.hosts,
                    manifest_sha256: &p.manifest_sha256,
                    scope: &p.scope,
                    max_cer: p.max_cer,
                })
        } else {
            let scope = self.scope.as_ref()?;
            let manifest_sha256 = self.manifest_sha256.as_deref()?;
            (scope.language == language && scope.profile == profile).then_some(PolicyView {
                hosts: &self.hosts,
                manifest_sha256,
                scope,
                max_cer: self.max_cer,
            })
        }
    }

    /// Looks up the calibrated policy for `(language, profile)`, if any.
    pub fn policy_for(&self, language: &str, profile: &str) -> Option<ScopedPolicy> {
        let view = self.policy_view(language, profile)?;
        Some(ScopedPolicy {
            hosts: view.hosts.clone(),
            manifest_sha256: view.manifest_sha256.to_owned(),
            scope: view.scope.clone(),
            max_cer: view.max_cer,
        })
    }

    /// Merges `incoming` calibrated scope(s) into `self` without clobbering unrelated scopes.
    /// Re-promoting an existing `(language, profile)` scope replaces only that scope.
    pub fn merge(&self, incoming: &Self) -> anyhow::Result<Self> {
        let existing_policies = self.effective_policies()?;
        let incoming_policies = incoming.effective_policies()?;
        let primary_scope = self.primary_policy()?.scope;
        let primary_key = (primary_scope.language, primary_scope.profile);
        let mut by_scope = BTreeMap::new();
        for policy in existing_policies {
            by_scope.insert(
                (policy.scope.language.clone(), policy.scope.profile.clone()),
                policy,
            );
        }
        for policy in incoming_policies {
            by_scope.insert(
                (policy.scope.language.clone(), policy.scope.profile.clone()),
                policy,
            );
        }
        let primary = by_scope
            .get(&primary_key)
            .cloned()
            .context("primary trust scope missing after merge")?;
        let policies = if by_scope.len() > 1 {
            by_scope.into_values().collect()
        } else {
            Vec::new()
        };
        let merged = Self {
            hosts: primary.hosts,
            manifest_sha256: Some(primary.manifest_sha256),
            scope: Some(primary.scope),
            max_cer: primary.max_cer,
            policies,
            v: TRUST_VERSION,
        };
        merged.validate()?;
        Ok(merged)
    }

    /// Atomically writes `self` to `out`, holding a destination-directory lock across the entire
    /// read–merge–write sequence and merging existing `out` + `merge_from` + `self` (with explicit
    /// conflict precedence `out < merge_from < self`) so promoting a new `(language, profile)`
    /// scope preserves existing calibrated scopes while discarding any uncalibrated legacy
    /// v1–v3 or unscoped v4 eligibility at `out` or `merge_from`.
    pub fn promote_to(&self, out: &Path, merge_from: Option<&Path>) -> anyhow::Result<Self> {
        self.promote_to_with_limit(out, merge_from, MAX_REPORTS as usize)
    }

    fn promote_to_with_limit(
        &self,
        out: &Path,
        merge_from: Option<&Path>,
        max_bytes: usize,
    ) -> anyhow::Result<Self> {
        self.validate()?;
        let out = if out.parent().is_some_and(|p| p.as_os_str().is_empty()) {
            Path::new(".").join(out)
        } else {
            out.to_path_buf()
        };
        let parent = out.parent().context("no parent directory")?;
        let _lock = crate::Lock::take(parent)?;
        let existing_out = if out.exists() {
            Self::load_for_promotion(&out)?
        } else {
            None
        };
        let existing_merge = match merge_from {
            Some(path) => Self::load_for_promotion(path)?,
            None => None,
        };
        let merged = match (existing_out, existing_merge) {
            (Some(out_policy), Some(merge_policy)) => {
                out_policy.merge(&merge_policy)?.merge(self)?
            }
            (Some(base), None) | (None, Some(base)) => base.merge(self)?,
            (None, None) => self.clone(),
        };
        let encoded = serde_json::to_vec_pretty(&merged)?;
        anyhow::ensure!(
            encoded.len() <= max_bytes,
            "promoted trust policy exceeds {max_bytes}-byte reader limit"
        );
        atomic(&out, &encoded)?;
        Ok(merged)
    }

    /// Adds `hosts` to one calibrated scope's eligibility for `engine_image` in the list at
    /// `out`, holding the directory lock across the read and the write (#901). The scope must
    /// still be calibrated with exactly `manifest_sha256` and `max_cer`, the values the evidence
    /// was judged against: this never sets or changes a calibration. Other scopes, images and
    /// hosts are kept, and nothing is removed; revocation stays with a full `gold trust`
    /// rebuild. With nothing new to add the file is left as it was.
    ///
    /// With `expected_sha256`, the list must still be exactly that version, checked under the
    /// same lock as the write: otherwise nothing is written and `None` is returned, so a
    /// rebuild that happened in between is never undone.
    #[allow(clippy::too_many_arguments)]
    pub fn admit_hosts(
        out: &Path,
        expected_sha256: Option<&str>,
        scope: &Scope,
        manifest_sha256: &str,
        max_cer: f64,
        engine_image: &str,
        hosts: &BTreeSet<u64>,
    ) -> anyhow::Result<Option<Admission>> {
        anyhow::ensure!(
            immutable_image(engine_image),
            "gold engine image must be an immutable sha256 digest"
        );
        anyhow::ensure!(
            !hosts.is_empty() && !hosts.contains(&0),
            "hosts to admit must be positive ids"
        );
        let parent = out
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let _lock = crate::Lock::take(parent)?;
        let bytes = crate::read(out, MAX_REPORTS as usize)
            .with_context(|| format!("reading {}", out.display()))?;
        anyhow::ensure!(
            !Self::is_legacy_uncalibrated(&bytes, out)?,
            "{} is an uncalibrated legacy list; rebuild trust before admitting hosts",
            out.display()
        );
        let before_sha256 = usnm_ocr_contract::boinc::sha256(&bytes);
        if expected_sha256.is_some_and(|expected| expected != before_sha256) {
            return Ok(None);
        }
        let mut trust = Self::from_bytes(&bytes, out)?;
        let view = trust
            .policy_view(&scope.language, &scope.profile)
            .context("no calibrated policy for this scope")?;
        anyhow::ensure!(
            view.scope == scope
                && view.manifest_sha256 == manifest_sha256
                && view.max_cer == max_cer,
            "the scope's calibration changed since the evidence was judged; not admitting"
        );
        let primary = trust
            .scope
            .as_ref()
            .is_some_and(|s| s.language == scope.language && s.profile == scope.profile);
        let mut added = BTreeSet::new();
        for host in hosts {
            let insert = |map: &mut BTreeMap<String, BTreeSet<u64>>| {
                map.entry(engine_image.to_owned())
                    .or_default()
                    .insert(*host)
            };
            let mut new = false;
            if let Some(policy) = trust
                .policies
                .iter_mut()
                .find(|p| p.scope.language == scope.language && p.scope.profile == scope.profile)
            {
                new |= insert(&mut policy.hosts);
            }
            // The top-level fields mirror the primary scope's policy entry.
            if primary {
                new |= insert(&mut trust.hosts);
            }
            if new {
                added.insert(*host);
            }
        }
        if added.is_empty() {
            return Ok(Some(Admission {
                after_sha256: before_sha256.clone(),
                before_sha256,
                added,
            }));
        }
        trust.validate()?;
        let encoded = serde_json::to_vec_pretty(&trust)?;
        anyhow::ensure!(
            encoded.len() <= MAX_REPORTS as usize,
            "trust policy exceeds its reader limit"
        );
        atomic(out, &encoded)?;
        Ok(Some(Admission {
            before_sha256,
            after_sha256: usnm_ocr_contract::boinc::sha256(&encoded),
            added,
        }))
    }

    fn from_bytes(bytes: &[u8], path: &Path) -> anyhow::Result<Self> {
        let t: Self =
            serde_json::from_slice(bytes).with_context(|| format!("reading {}", path.display()))?;
        t.validate()?;
        Ok(t)
    }

    fn is_legacy_uncalibrated(bytes: &[u8], path: &Path) -> anyhow::Result<bool> {
        let raw: serde_json::Value =
            serde_json::from_slice(bytes).with_context(|| format!("reading {}", path.display()))?;
        let version = raw.get("v").and_then(serde_json::Value::as_u64);
        if matches!(version, Some(1..=3))
            || (version == Some(4) && raw.get("scope").is_none_or(serde_json::Value::is_null))
        {
            let _: LegacyTrust = serde_json::from_value(raw)
                .with_context(|| format!("reading legacy trust policy {}", path.display()))?;
            return Ok(true);
        }
        Ok(false)
    }

    fn load_for_promotion(path: &Path) -> anyhow::Result<Option<Self>> {
        let bytes = crate::read(path, MAX_REPORTS as usize)
            .with_context(|| format!("reading {}", path.display()))?;
        if Self::is_legacy_uncalibrated(&bytes, path)? {
            eprintln!(
                "{} uses an uncalibrated legacy trust policy; discarding legacy eligibility during promotion",
                path.display()
            );
            return Ok(None);
        }
        Self::from_bytes(&bytes, path).map(Some)
    }

    pub fn load(path: &Path) -> anyhow::Result<Self> {
        let bytes = crate::read(path, MAX_REPORTS as usize)
            .with_context(|| format!("reading {}", path.display()))?;
        Self::from_bytes(&bytes, path)
    }

    /// For issuing gold only: a missing list or legacy uncalibrated list trusts no host, so every
    /// host is offered gold (which is how a threshold gets calibrated in the first place).
    /// Anything else must load as [`Trust::load`] would.
    pub fn load_or_none_trusted(path: &Path) -> anyhow::Result<Self> {
        let none = || Self {
            hosts: BTreeMap::new(),
            manifest_sha256: None,
            scope: None,
            max_cer: 0.0,
            policies: Vec::new(),
            v: TRUST_VERSION,
        };
        if !path.exists() {
            eprintln!("no trust list at {}; no host is trusted", path.display());
            return Ok(none());
        }
        let bytes = crate::read(path, MAX_REPORTS as usize)
            .with_context(|| format!("reading {}", path.display()))?;
        if Self::is_legacy_uncalibrated(&bytes, path)? {
            eprintln!(
                "{} uses an uncalibrated legacy trust policy; no host is trusted",
                path.display()
            );
            return Ok(none());
        }
        Self::from_bytes(&bytes, path)
    }

    /// Borrows the set of hosts already trusted for a prepared Gold campaign's exact
    /// `(language, profile, psm, dpi)`, calibrated `manifest_sha256`, and immutable worker
    /// `engine_image` without cloning host sets.
    pub fn campaign_trusted_hosts<'a>(
        &'a self,
        engine_image: &str,
        manifest_sha256: &str,
        scope: &Scope,
    ) -> Option<&'a BTreeSet<u64>> {
        if !immutable_image(engine_image)
            || !usnm_ocr_contract::boinc::is_sha256(manifest_sha256)
            || scope.validate().is_err()
        {
            return None;
        }
        let policy = self.policy_view(&scope.language, &scope.profile)?;
        if policy.scope != scope || policy.manifest_sha256 != manifest_sha256 {
            return None;
        }
        policy.hosts.get(engine_image)
    }

    /// Campaign-scoped eligibility for Gold onboarding: a host is skipped only when it is already
    /// trusted for this prepared campaign's exact `(language, profile, psm, dpi)`, calibrated
    /// `manifest_sha256`, and immutable worker `engine_image`.
    pub fn refuse_campaign(
        &self,
        engine_image: &str,
        manifest_sha256: &str,
        scope: &Scope,
        host_id: Option<u64>,
    ) -> Option<String> {
        if !immutable_image(engine_image)
            || !usnm_ocr_contract::boinc::is_sha256(manifest_sha256)
            || scope.validate().is_err()
            || host_id == Some(0)
        {
            return Some("invalid immutable engine image, manifest, scope or host id".into());
        }
        let Some(host) = host_id else {
            return Some("no host id for this result".into());
        };
        if self.scope.is_none() && self.policies.is_empty() {
            return Some("no calibrated job scope".into());
        }
        if let Err(err) = self.validate() {
            return Some(format!("invalid calibrated trust policy: {err}"));
        }
        let Some(policy) = self.policy_view(&scope.language, &scope.profile) else {
            return Some(
                "campaign language, model profile or scoring configuration is outside calibrated trust"
                    .into(),
            );
        };
        if policy.scope != scope || policy.manifest_sha256 != manifest_sha256 {
            return Some(
                "campaign manifest, model profile or scoring configuration is outside calibrated trust"
                    .into(),
            );
        }
        if policy
            .hosts
            .get(engine_image)
            .is_some_and(|h| h.contains(&host))
        {
            None
        } else {
            Some(format!(
                "host {host} has no gold report for {engine_image} with CER <= {}",
                policy.max_cer
            ))
        }
    }

    /// Single-scope eligibility helper for Gold onboarding; multi-scope policies must use
    /// [`Trust::refuse_campaign`].
    pub fn refuse(&self, engine_image: &str, host_id: Option<u64>) -> Option<String> {
        if !self.policies.is_empty() {
            return Some("multi-scope trust policy requires explicit campaign scope".into());
        }
        let (Some(manifest), Some(scope)) = (self.manifest_sha256.as_deref(), self.scope.as_ref())
        else {
            return Some("no calibrated job scope".into());
        };
        self.refuse_campaign(engine_image, manifest, scope, host_id)
    }

    /// Returns the hosts trusted for `job`'s exact `(language, profile, psm, dpi)` and image.
    pub fn trusted_hosts_for_job(&self, job: &usnm_ocr_contract::boinc::Job) -> BTreeSet<u64> {
        if !immutable_image(&job.engine.image)
            || !job.engine.kind.is_tesseract()
            || self.validate().is_err()
        {
            return BTreeSet::new();
        }
        let Some(policy) = self.policy_view(&job.message.language, &job.engine.profile) else {
            return BTreeSet::new();
        };
        if policy.scope.psm != job.engine.psm || policy.scope.dpi != job.engine.dpi {
            return BTreeSet::new();
        }
        policy
            .hosts
            .get(&job.engine.image)
            .cloned()
            .unwrap_or_default()
    }

    pub fn refuse_job(
        &self,
        job: &usnm_ocr_contract::boinc::Job,
        host_id: Option<u64>,
    ) -> Option<String> {
        if !immutable_image(&job.engine.image) || host_id == Some(0) {
            return Some("invalid immutable engine image or host id".into());
        }
        let Some(host) = host_id else {
            return Some("no host id for this result".into());
        };
        if self.scope.is_none() && self.policies.is_empty() {
            return Some("no calibrated job scope".into());
        }
        if let Err(err) = self.validate() {
            return Some(format!("invalid calibrated trust policy: {err}"));
        }
        if !job.engine.kind.is_tesseract() {
            return Some(
                "source language, model profile or scoring configuration is outside calibrated trust"
                    .into(),
            );
        }
        let Some(policy) = self.policy_view(&job.message.language, &job.engine.profile) else {
            return Some(
                "source language, model profile or scoring configuration is outside calibrated trust"
                    .into(),
            );
        };
        if job.engine.psm != policy.scope.psm || job.engine.dpi != policy.scope.dpi {
            return Some(
                "source language, model profile or scoring configuration is outside calibrated trust"
                    .into(),
            );
        }
        if policy
            .hosts
            .get(&job.engine.image)
            .is_some_and(|h| h.contains(&host))
        {
            None
        } else {
            Some(format!(
                "host {host} has no gold report for {} with CER <= {} in scope {}/{}",
                job.engine.image, policy.max_cer, policy.scope.language, policy.scope.profile
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

    fn job(image: &str, language: &str, profile: &str) -> usnm_ocr_contract::boinc::Job {
        usnm_ocr_contract::boinc::Job {
            v: 1,
            id: "a".repeat(64),
            message: usnm_ocr_contract::message::OcrMessage {
                v: 1,
                page: "sn83030214/1900-01-01/ed-1/seq-1".into(),
                batch: "batch_test_ver01".into(),
                language: language.into(),
                reason: usnm_ocr_contract::message::Reason::Bulk,
                score: Some(0.5),
                queued_at: "2026-10-17T00:00:00Z".into(),
            },
            batch_version: 1,
            scan: usnm_ocr_contract::boinc::Scan {
                url: "https://tile.loc.gov/image-services/iiif/service:ndnp:test/full/full/0/default.jpg".into(),
                iiif_base: "https://tile.loc.gov/image-services/iiif/service:ndnp:test".into(),
                bytes: 1234,
                sha256: "c".repeat(64),
                md5: None,
            },
            engine: usnm_ocr_contract::boinc::Engine {
                kind: usnm_ocr_contract::routing::EngineKind::Tesseract,
                image: image.into(),
                version: "tesseract 5.5.0".into(),
                profile: profile.into(),
                models: "sha256:001122334455".into(),
                psm: 3,
                dpi: 300,
                timeout_secs: 600,
            },
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn native_report_and_outcome(
        workunit_byte: char,
        result_id: u64,
        image: &str,
        host: u64,
        manifest: &str,
        language: &str,
        profile: &str,
        cer: f64,
        received_at: i64,
    ) -> (String, String) {
        let workunit = format!("usnm_gold_{}", workunit_byte.to_string().repeat(64));
        let report = serde_json::json!({
            "completed_at": "2026-10-18T00:00:00Z",
            "bundle_version": 1,
            "engine_image": image,
            "host_id": host,
            "manifest_sha256": manifest,
            "workunit": workunit,
            "native_result_id": result_id,
            "server_received_at": received_at,
            "language": language,
            "profile": profile,
            "crops": 12,
            "skipped": 0,
            "complete_loss_crops": 0,
            "profiles": [profile],
            "psm": 3,
            "dpi": 300,
            "summary": [
                {"cer": cer, "crops": 12, "ref_chars": 7786, "source": "all", "wer": cer},
            ],
        })
        .to_string();
        let outcome = serde_json::json!({
            "schema_version": 3,
            "app": "usnm_gold",
            "app_version_id": 40,
            "bundle_version": 1,
            "canonical": true,
            "dpi": 300,
            "engine_image": image,
            "host_id": host,
            "language": language,
            "manifest_sha256": manifest,
            "outcome": crate::gold::collect::BOINC_OUTCOME_SUCCESS,
            "exit_status": 0,
            "profile": profile,
            "psm": 3,
            "received_at": received_at,
            "result_id": result_id,
            "sent_at": received_at - 30,
            "server_state": crate::gold::collect::BOINC_SERVER_STATE_OVER,
            "workunit": workunit,
            "workunit_assimilate_state": crate::gold::collect::BOINC_ASSIMILATE_STATE_DONE,
            "workunit_result_count": 1,
        })
        .to_string();
        (report, outcome)
    }

    #[test]
    fn v4_single_scope_english_policy_remains_backward_compatible() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("host-trust.json");
        let image = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
        let eng_manifest = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
        let v4_json = serde_json::json!({
            "hosts": {
                image: [1, 8]
            },
            "manifest_sha256": eng_manifest,
            "scope": {
                "language": "eng",
                "profile": "eng",
                "psm": 3,
                "dpi": 300
            },
            "max_cer": 0.08734328574534386,
            "v": 4
        });
        std::fs::write(&path, serde_json::to_vec_pretty(&v4_json).unwrap()).unwrap();

        let loaded = Trust::load(&path).unwrap();
        assert_eq!(loaded.v, 4);
        assert!(loaded.policies.is_empty());
        assert_eq!(Trust::load_or_none_trusted(&path).unwrap(), loaded);
        assert_eq!(
            loaded.policy_for("eng", "eng").unwrap().hosts[image],
            BTreeSet::from([1, 8])
        );
        assert!(loaded.policy_for("deu", "frak2021").is_none());

        let eng_job = job(image, "eng", "eng");
        let deu_job = job(image, "deu", "frak2021");
        let deu_scope = Scope {
            language: "deu".into(),
            profile: "frak2021".into(),
            psm: 3,
            dpi: 300,
        };
        let deu_manifest = "e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd";
        assert!(loaded.refuse_job(&eng_job, Some(1)).is_none());
        assert!(loaded.refuse_job(&eng_job, Some(8)).is_none());
        assert!(loaded.refuse_job(&eng_job, Some(2)).is_some());
        assert!(loaded
            .refuse_job(&deu_job, Some(1))
            .is_some_and(|r| r.contains("outside calibrated trust")));
        assert!(loaded
            .refuse_campaign(image, eng_manifest, &scope(), Some(1))
            .is_none());
        assert!(loaded
            .refuse_campaign(image, eng_manifest, &scope(), Some(8))
            .is_none());
        assert!(loaded
            .refuse_campaign(image, eng_manifest, &scope(), Some(9))
            .is_some());
        // Loading an English-only v4 policy must NOT skip hosts 1 and 8 for a German campaign or
        // an uncalibrated manifest.
        assert!(loaded
            .refuse_campaign(image, deu_manifest, &deu_scope, Some(1))
            .is_some());
        assert!(loaded
            .refuse_campaign(image, deu_manifest, &deu_scope, Some(8))
            .is_some());
        assert!(loaded
            .refuse_campaign(image, deu_manifest, &scope(), Some(1))
            .is_some());

        // A v4 artifact with multi-scope `policies` is rejected.
        let mut invalid_v4 = v4_json;
        invalid_v4["policies"] = serde_json::json!([{
            "hosts": { image: [1, 8] },
            "manifest_sha256": eng_manifest,
            "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
            "max_cer": 0.08734328574534386
        }]);
        std::fs::write(&path, invalid_v4.to_string()).unwrap();
        assert!(Trust::load(&path).is_err());
    }

    #[test]
    fn promoting_deu_frak2021_coexists_without_clobbering_eng_and_enforces_per_scope_trust() {
        let tmp = tempfile::tempdir().unwrap();
        let trust_path = tmp.path().join("trust.json");
        let image = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
        let other_image = format!("sha256:{}", "f".repeat(64));
        let eng_manifest = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
        let deu_manifest = "e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd";

        // Start with the existing v4 English policy on disk (hosts 1 and 8).
        let v4_eng = serde_json::json!({
            "hosts": { image: [1, 8] },
            "manifest_sha256": eng_manifest,
            "scope": { "language": "eng", "profile": "eng", "psm": 3, "dpi": 300 },
            "max_cer": 0.08734328574534386,
            "v": 4
        });
        std::fs::write(&trust_path, serde_json::to_vec_pretty(&v4_eng).unwrap()).unwrap();

        // Build and promote a calibrated German Fraktur policy (hosts 1 and 9 pass; host 8 fails CER).
        let deu_reports = tmp.path().join("deu-reports.jsonl");
        let deu_outcomes = tmp.path().join("deu-outcomes.jsonl");
        let (r1, o1) = native_report_and_outcome(
            '1',
            101,
            image,
            1,
            deu_manifest,
            "deu",
            "frak2021",
            0.0539,
            1_700_000_100,
        );
        let (r8, o8) = native_report_and_outcome(
            '2',
            102,
            image,
            8,
            deu_manifest,
            "deu",
            "frak2021",
            0.0993,
            1_700_000_200,
        );
        let (r9, o9) = native_report_and_outcome(
            '3',
            103,
            image,
            9,
            deu_manifest,
            "deu",
            "frak2021",
            0.0610,
            1_700_000_300,
        );
        std::fs::write(&deu_reports, [r1, r8, r9].join("\n")).unwrap();
        std::fs::write(&deu_outcomes, [o1, o8, o9].join("\n")).unwrap();

        let deu_scope = Scope {
            language: "deu".into(),
            profile: "frak2021".into(),
            psm: 3,
            dpi: 300,
        };
        let deu_built = Trust::build_with_outcomes(
            std::slice::from_ref(&deu_reports),
            std::slice::from_ref(&deu_outcomes),
            0.06755715386591318,
            deu_manifest,
            deu_scope.clone(),
        )
        .unwrap();
        assert_eq!(deu_built.hosts[image], BTreeSet::from([1, 9]));

        // Promote into the existing trust_path; must merge alongside eng/eng rather than clobbering it.
        let merged = deu_built.promote_to(&trust_path, None).unwrap();
        let reloaded = Trust::load(&trust_path).unwrap();
        assert_eq!(merged, reloaded);
        assert_eq!(reloaded.v, TRUST_VERSION);
        // Primary top-level eng/eng scope and hosts remain intact.
        assert_eq!(reloaded.scope, Some(scope()));
        assert_eq!(reloaded.manifest_sha256.as_deref(), Some(eng_manifest));
        assert_eq!(reloaded.hosts[image], BTreeSet::from([1, 8]));
        assert_eq!(reloaded.policies.len(), 2);

        let eng_policy = reloaded.policy_for("eng", "eng").unwrap();
        assert_eq!(eng_policy.hosts[image], BTreeSet::from([1, 8]));
        assert_eq!(eng_policy.manifest_sha256, eng_manifest);

        let deu_policy = reloaded.policy_for("deu", "frak2021").unwrap();
        assert_eq!(deu_policy.hosts[image], BTreeSet::from([1, 9]));
        assert_eq!(deu_policy.manifest_sha256, deu_manifest);

        let eng_job = job(image, "eng", "eng");
        let deu_job = job(image, "deu", "frak2021");

        // Host 1 is trusted in both scopes.
        assert!(reloaded.refuse_job(&eng_job, Some(1)).is_none());
        assert!(reloaded.refuse_job(&deu_job, Some(1)).is_none());

        // Host 8 is trusted ONLY for eng/eng -> allowed on eng/eng, strictly refused on deu/frak2021.
        assert!(reloaded.refuse_job(&eng_job, Some(8)).is_none());
        assert!(reloaded
            .refuse_job(&deu_job, Some(8))
            .is_some_and(|r| r.contains("in scope deu/frak2021")));

        // Host 9 is trusted ONLY for deu/frak2021 -> allowed on deu/frak2021, strictly refused on eng/eng.
        assert!(reloaded.refuse_job(&deu_job, Some(9)).is_none());
        assert!(reloaded
            .refuse_job(&eng_job, Some(9))
            .is_some_and(|r| r.contains("in scope eng/eng")));

        // Cross-scope profile/language combinations and wrong images are refused even for Host 1.
        assert!(reloaded
            .refuse_job(&job(image, "deu", "eng"), Some(1))
            .is_some_and(|r| r.contains("outside calibrated trust")));
        assert!(reloaded
            .refuse_job(&job(image, "eng", "frak2021"), Some(1))
            .is_some_and(|r| r.contains("outside calibrated trust")));
        assert!(reloaded
            .refuse_job(&job(&other_image, "deu", "frak2021"), Some(1))
            .is_some());

        // Campaign-scoped onboarding eligibility: English skips {1, 8} and offers Gold to {9};
        // German skips {1, 9} and offers Gold to {8}; unscoped `refuse` fails closed on multi-scope.
        assert!(reloaded
            .refuse_campaign(image, eng_manifest, &scope(), Some(1))
            .is_none());
        assert!(reloaded
            .refuse_campaign(image, eng_manifest, &scope(), Some(8))
            .is_none());
        assert!(reloaded
            .refuse_campaign(image, eng_manifest, &scope(), Some(9))
            .is_some());
        assert!(reloaded
            .refuse_campaign(image, deu_manifest, &deu_scope, Some(1))
            .is_none());
        assert!(reloaded
            .refuse_campaign(image, deu_manifest, &deu_scope, Some(9))
            .is_none());
        assert!(reloaded
            .refuse_campaign(image, deu_manifest, &deu_scope, Some(8))
            .is_some());
        assert!(reloaded.refuse(image, Some(1)).is_some());

        // A later terminal client error in deu/frak2021 revokes Host 9 in deu/frak2021 without touching eng/eng.
        let revoke_outcome = serde_json::json!({
            "schema_version": 3,
            "app": "usnm_gold",
            "app_version_id": 40,
            "bundle_version": 1,
            "canonical": false,
            "dpi": 300,
            "engine_image": image,
            "host_id": 9,
            "language": "deu",
            "manifest_sha256": deu_manifest,
            "outcome": crate::gold::collect::BOINC_OUTCOME_CLIENT_ERROR,
            "exit_status": 1,
            "profile": "frak2021",
            "psm": 3,
            "received_at": 1_700_000_400,
            "result_id": 104,
            "sent_at": 1_700_000_350,
            "server_state": crate::gold::collect::BOINC_SERVER_STATE_OVER,
            "workunit": format!("usnm_gold_{}", "4".repeat(64)),
            "workunit_assimilate_state": crate::gold::collect::BOINC_ASSIMILATE_STATE_DONE,
            "workunit_result_count": 1,
        })
        .to_string();
        let existing_outcomes = std::fs::read_to_string(&deu_outcomes).unwrap();
        std::fs::write(
            &deu_outcomes,
            format!("{existing_outcomes}\n{revoke_outcome}"),
        )
        .unwrap();
        let deu_rebuilt = Trust::build_with_outcomes(
            std::slice::from_ref(&deu_reports),
            std::slice::from_ref(&deu_outcomes),
            0.06755715386591318,
            deu_manifest,
            deu_scope,
        )
        .unwrap();
        let updated = deu_rebuilt.promote_to(&trust_path, None).unwrap();
        assert!(updated.refuse_job(&deu_job, Some(9)).is_some());
        assert!(updated.refuse_job(&deu_job, Some(1)).is_none());
        assert!(updated.refuse_job(&eng_job, Some(1)).is_none());
        assert!(updated.refuse_job(&eng_job, Some(8)).is_none());

        // Corrupting `policies` to disagree with top-level primary scope or duplicating a scope fails closed.
        let mut mismatch = serde_json::to_value(&updated).unwrap();
        mismatch["policies"][1]["max_cer"] = serde_json::json!(0.5);
        std::fs::write(&trust_path, mismatch.to_string()).unwrap();
        assert!(Trust::load(&trust_path).is_err());

        let mut duplicate = serde_json::to_value(&updated).unwrap();
        let dup_entry = duplicate["policies"][0].clone();
        duplicate["policies"]
            .as_array_mut()
            .unwrap()
            .push(dup_entry);
        std::fs::write(&trust_path, duplicate.to_string()).unwrap();
        assert!(Trust::load(&trust_path).is_err());
    }

    #[test]
    fn promote_to_preserves_distinct_destination_policies_and_locks_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let out_dir = tmp.path().join("shared");
        let stage_dir = tmp.path().join("stage");
        std::fs::create_dir_all(&out_dir).unwrap();
        std::fs::create_dir_all(&stage_dir).unwrap();
        let out_path = out_dir.join("trust.json");
        let merge_path = stage_dir.join("merge-source.json");
        let image = "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
        let eng_manifest = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
        let deu_manifest = "e24325e6ad77df7f5345859b9dfb524b590cb6db728876aefabecdeea35c81cd";
        let fra_manifest = "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9";

        // Existing destination `--out` holds `eng/eng` (hosts {1, 8}) and an older `deu/frak2021` (host {9}).
        let (eng_r1, eng_o1) = native_report_and_outcome(
            '1',
            1,
            image,
            1,
            eng_manifest,
            "eng",
            "eng",
            0.04,
            1_700_000_100,
        );
        let (eng_r8, eng_o8) = native_report_and_outcome(
            '2',
            2,
            image,
            8,
            eng_manifest,
            "eng",
            "eng",
            0.05,
            1_700_000_200,
        );
        let eng_reports = stage_dir.join("eng-reports.jsonl");
        let eng_outcomes = stage_dir.join("eng-outcomes.jsonl");
        std::fs::write(&eng_reports, [eng_r1, eng_r8].join("\n")).unwrap();
        std::fs::write(&eng_outcomes, [eng_o1, eng_o8].join("\n")).unwrap();
        let eng_trust = Trust::build_with_outcomes(
            std::slice::from_ref(&eng_reports),
            std::slice::from_ref(&eng_outcomes),
            0.08734328574534386,
            eng_manifest,
            scope(),
        )
        .unwrap();

        let deu_scope = Scope {
            language: "deu".into(),
            profile: "frak2021".into(),
            psm: 3,
            dpi: 300,
        };
        let (deu_r9, deu_o9) = native_report_and_outcome(
            '3',
            3,
            image,
            9,
            deu_manifest,
            "deu",
            "frak2021",
            0.05,
            1_700_000_300,
        );
        let deu_old_reports = stage_dir.join("deu-old-reports.jsonl");
        let deu_old_outcomes = stage_dir.join("deu-old-outcomes.jsonl");
        std::fs::write(&deu_old_reports, &deu_r9).unwrap();
        std::fs::write(&deu_old_outcomes, &deu_o9).unwrap();
        let deu_old = Trust::build_with_outcomes(
            std::slice::from_ref(&deu_old_reports),
            std::slice::from_ref(&deu_old_outcomes),
            0.06755715386591318,
            deu_manifest,
            deu_scope.clone(),
        )
        .unwrap();
        eng_trust.promote_to(&out_path, None).unwrap();
        deu_old.promote_to(&out_path, None).unwrap();

        // Distinct `--merge` file has `fra/fra` (host {7}) and `deu/frak2021` (hosts {1, 9}),
        // but does NOT contain `eng/eng`.
        let fra_scope = Scope {
            language: "fra".into(),
            profile: "fra".into(),
            psm: 3,
            dpi: 300,
        };
        let (fra_r7, fra_o7) = native_report_and_outcome(
            '4',
            4,
            image,
            7,
            fra_manifest,
            "fra",
            "fra",
            0.03,
            1_700_000_400,
        );
        let fra_reports = stage_dir.join("fra-reports.jsonl");
        let fra_outcomes = stage_dir.join("fra-outcomes.jsonl");
        std::fs::write(&fra_reports, &fra_r7).unwrap();
        std::fs::write(&fra_outcomes, &fra_o7).unwrap();
        let fra_trust = Trust::build_with_outcomes(
            std::slice::from_ref(&fra_reports),
            std::slice::from_ref(&fra_outcomes),
            0.07,
            fra_manifest,
            fra_scope,
        )
        .unwrap();
        let (deu_r1, deu_o1) = native_report_and_outcome(
            '5',
            5,
            image,
            1,
            deu_manifest,
            "deu",
            "frak2021",
            0.05,
            1_700_000_500,
        );
        let deu_mid_reports = stage_dir.join("deu-mid-reports.jsonl");
        let deu_mid_outcomes = stage_dir.join("deu-mid-outcomes.jsonl");
        std::fs::write(&deu_mid_reports, [deu_r1.clone(), deu_r9].join("\n")).unwrap();
        std::fs::write(&deu_mid_outcomes, [deu_o1.clone(), deu_o9].join("\n")).unwrap();
        let deu_mid = Trust::build_with_outcomes(
            std::slice::from_ref(&deu_mid_reports),
            std::slice::from_ref(&deu_mid_outcomes),
            0.06755715386591318,
            deu_manifest,
            deu_scope.clone(),
        )
        .unwrap();
        fra_trust.promote_to(&merge_path, None).unwrap();
        deu_mid.promote_to(&merge_path, None).unwrap();

        // Newly promoted `self` updates `deu/frak2021` to host {1} only (host 9 revoked).
        let deu_new_reports = stage_dir.join("deu-new-reports.jsonl");
        let deu_new_outcomes = stage_dir.join("deu-new-outcomes.jsonl");
        std::fs::write(&deu_new_reports, &deu_r1).unwrap();
        std::fs::write(&deu_new_outcomes, &deu_o1).unwrap();
        let deu_new = Trust::build_with_outcomes(
            std::slice::from_ref(&deu_new_reports),
            std::slice::from_ref(&deu_new_outcomes),
            0.06755715386591318,
            deu_manifest,
            deu_scope,
        )
        .unwrap();

        // Promoting `deu_new` with `--merge merge_path --out out_path` must preserve destination-only
        // `eng/eng` ({1, 8}), merge `fra/fra` ({7}) from `merge_path`, and apply `deu_new` ({1}) with
        // precedence `out < merge_from < self`.
        let promoted = deu_new.promote_to(&out_path, Some(&merge_path)).unwrap();
        assert_eq!(promoted.scope, Some(scope()));
        assert_eq!(
            promoted.policy_for("eng", "eng").unwrap().hosts[image],
            BTreeSet::from([1, 8])
        );
        assert_eq!(
            promoted.policy_for("fra", "fra").unwrap().hosts[image],
            BTreeSet::from([7])
        );
        assert_eq!(
            promoted.policy_for("deu", "frak2021").unwrap().hosts[image],
            BTreeSet::from([1])
        );

        // Holding the destination directory lock blocks/refuses a competing `promote_to` across the
        // entire read–merge–write operation without overwriting `out_path`.
        let held_lock = crate::Lock::take(&out_dir).unwrap();
        assert!(deu_old.promote_to(&out_path, None).is_err());
        drop(held_lock);
        assert_eq!(
            Trust::load(&out_path)
                .unwrap()
                .policy_for("deu", "frak2021")
                .unwrap()
                .hosts[image],
            BTreeSet::from([1])
        );

        // `promote_to` cleanly upgrades a legacy v1–v3 or unscoped v4 `--out` file by discarding
        // uncalibrated legacy eligibility while keeping `Trust::load` strict for collection.
        for version in [1, 2, 3, 4] {
            let legacy = serde_json::json!({
                "v": version,
                "max_cer": 0.1,
                "hosts": { image: [42, 99] }
            });
            std::fs::write(&out_path, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();
            assert!(Trust::load(&out_path).is_err());
            let upgraded = eng_trust.promote_to(&out_path, None).unwrap();
            assert_eq!(upgraded, eng_trust);
            assert_eq!(Trust::load(&out_path).unwrap(), eng_trust);
            assert_eq!(
                upgraded.campaign_trusted_hosts(image, eng_manifest, &scope()),
                Some(&BTreeSet::from([1, 8]))
            );
        }

        // Corrupt or unsupported-version `--out` files still fail closed during promotion.
        std::fs::write(&out_path, r#"{"v": 99, "max_cer": 0.1, "hosts": {}}"#).unwrap();
        assert!(eng_trust.promote_to(&out_path, None).is_err());

        // An oversized merged policy is rejected before replacing the destination file, leaving
        // the existing readable trust artifact unchanged.
        std::fs::remove_file(&out_path).unwrap();
        eng_trust.promote_to(&out_path, None).unwrap();
        let before_oversized = std::fs::read(&out_path).unwrap();
        assert!(deu_old
            .promote_to_with_limit(&out_path, None, before_oversized.len())
            .is_err());
        assert_eq!(std::fs::read(&out_path).unwrap(), before_oversized);
        assert_eq!(Trust::load(&out_path).unwrap(), eng_trust);
    }

    const ADMIT_IMAGE: &str =
        "sha256:247c131a599403645c4c96782c0556dc27f27a5d991c764b4b9565182a8c71aa";
    const ADMIT_MANIFEST: &str = "d8cd7279090a6427c0cc6f0e82573e3771c27f90df6500bf5a44b268f6e0f05f";
    const ADMIT_CER: f64 = 0.08734328574534386;

    fn two_scope_list() -> serde_json::Value {
        let eng = serde_json::json!({
            "hosts": {ADMIT_IMAGE: [1, 8]}, "manifest_sha256": ADMIT_MANIFEST,
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": ADMIT_CER,
        });
        let deu = serde_json::json!({
            "hosts": {ADMIT_IMAGE: [1, 2, 3]}, "manifest_sha256": "7".repeat(64),
            "scope": {"language": "deu", "profile": "frak2021", "psm": 3, "dpi": 300},
            "max_cer": 0.2,
        });
        serde_json::json!({
            "hosts": {ADMIT_IMAGE: [1, 8]}, "manifest_sha256": ADMIT_MANIFEST,
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": ADMIT_CER, "policies": [deu, eng], "v": TRUST_VERSION,
        })
    }

    #[test]
    fn admitting_hosts_adds_to_one_scope_and_keeps_every_other_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trust.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&two_scope_list()).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let admitted = Trust::admit_hosts(
            &path,
            None,
            &scope(),
            ADMIT_MANIFEST,
            ADMIT_CER,
            ADMIT_IMAGE,
            &BTreeSet::from([2, 8]),
        )
        .unwrap()
        .unwrap();
        assert_eq!(admitted.added, BTreeSet::from([2]));
        assert_eq!(
            admitted.before_sha256,
            usnm_ocr_contract::boinc::sha256(&before)
        );
        let after = std::fs::read(&path).unwrap();
        assert_eq!(
            admitted.after_sha256,
            usnm_ocr_contract::boinc::sha256(&after)
        );
        let t = Trust::load(&path).unwrap();
        let eng = t.policy_for("eng", "eng").unwrap();
        assert_eq!(eng.hosts[ADMIT_IMAGE], BTreeSet::from([1, 2, 8]));
        assert_eq!(
            (eng.max_cer, eng.manifest_sha256.as_str()),
            (ADMIT_CER, ADMIT_MANIFEST)
        );
        assert_eq!(t.hosts[ADMIT_IMAGE], BTreeSet::from([1, 2, 8]));
        let deu = t.policy_for("deu", "frak2021").unwrap();
        assert_eq!(deu.hosts[ADMIT_IMAGE], BTreeSet::from([1, 2, 3]));
        assert_eq!(deu.max_cer, 0.2);
        // Again: nothing new, so the file is not rewritten.
        let again = Trust::admit_hosts(
            &path,
            None,
            &scope(),
            ADMIT_MANIFEST,
            ADMIT_CER,
            ADMIT_IMAGE,
            &BTreeSet::from([2]),
        )
        .unwrap()
        .unwrap();
        assert!(again.added.is_empty());
        assert_eq!(again.before_sha256, again.after_sha256);
        assert_eq!(std::fs::read(&path).unwrap(), after);
    }

    #[test]
    fn admitting_into_a_single_scope_list_or_a_new_image_keeps_the_calibration() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trust.json");
        let list = serde_json::json!({
            "hosts": {ADMIT_IMAGE: [1]}, "manifest_sha256": ADMIT_MANIFEST,
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": ADMIT_CER, "v": 4,
        });
        std::fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
        let other = format!("sha256:{}", "e".repeat(64));
        Trust::admit_hosts(
            &path,
            None,
            &scope(),
            ADMIT_MANIFEST,
            ADMIT_CER,
            &other,
            &BTreeSet::from([5]),
        )
        .unwrap();
        let t = Trust::load(&path).unwrap();
        assert_eq!(t.v, 4);
        assert!(t.policies.is_empty());
        assert_eq!(t.hosts[ADMIT_IMAGE], BTreeSet::from([1]));
        assert_eq!(t.hosts[&other], BTreeSet::from([5]));
        assert_eq!(t.max_cer, ADMIT_CER);
    }

    #[test]
    fn admitting_refuses_a_changed_calibration_an_unknown_scope_or_a_legacy_list() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trust.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&two_scope_list()).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let hosts = BTreeSet::from([2]);
        let fra = Scope {
            language: "fra".into(),
            profile: "fra".into(),
            psm: 3,
            dpi: 300,
        };
        for (scope, manifest, cer, image, hosts) in [
            (
                scope(),
                "c".repeat(64),
                ADMIT_CER,
                ADMIT_IMAGE.to_owned(),
                hosts.clone(),
            ),
            (
                scope(),
                ADMIT_MANIFEST.to_owned(),
                0.09,
                ADMIT_IMAGE.to_owned(),
                hosts.clone(),
            ),
            (
                fra,
                ADMIT_MANIFEST.to_owned(),
                ADMIT_CER,
                ADMIT_IMAGE.to_owned(),
                hosts.clone(),
            ),
            (
                scope(),
                ADMIT_MANIFEST.to_owned(),
                ADMIT_CER,
                "worker:latest".to_owned(),
                hosts.clone(),
            ),
            (
                scope(),
                ADMIT_MANIFEST.to_owned(),
                ADMIT_CER,
                ADMIT_IMAGE.to_owned(),
                BTreeSet::from([0]),
            ),
            (
                scope(),
                ADMIT_MANIFEST.to_owned(),
                ADMIT_CER,
                ADMIT_IMAGE.to_owned(),
                BTreeSet::new(),
            ),
        ] {
            assert!(
                Trust::admit_hosts(&path, None, &scope, &manifest, cer, &image, &hosts).is_err()
            );
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
        let legacy = serde_json::json!({"hosts": {ADMIT_IMAGE: [1]}, "max_cer": 0.1, "v": 3});
        std::fs::write(&path, serde_json::to_vec(&legacy).unwrap()).unwrap();
        assert!(Trust::admit_hosts(
            &path,
            None,
            &scope(),
            ADMIT_MANIFEST,
            ADMIT_CER,
            ADMIT_IMAGE,
            &BTreeSet::from([2])
        )
        .is_err());
        assert!(Trust::admit_hosts(
            &tmp.path().join("missing.json"),
            None,
            &scope(),
            ADMIT_MANIFEST,
            ADMIT_CER,
            ADMIT_IMAGE,
            &BTreeSet::from([2])
        )
        .is_err());
    }

    #[test]
    fn admitting_against_an_expected_version_writes_nothing_once_the_list_changed() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("trust.json");
        std::fs::write(&path, serde_json::to_vec_pretty(&two_scope_list()).unwrap()).unwrap();
        let before = std::fs::read(&path).unwrap();
        let hosts = BTreeSet::from([2]);
        let stale = "0".repeat(64);
        let admit = |expected: &str| {
            Trust::admit_hosts(
                &path,
                Some(expected),
                &scope(),
                ADMIT_MANIFEST,
                ADMIT_CER,
                ADMIT_IMAGE,
                &hosts,
            )
            .unwrap()
        };
        assert!(admit(&stale).is_none());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let current = usnm_ocr_contract::boinc::sha256(&before);
        assert_eq!(admit(&current).unwrap().added, hosts);
    }
}
