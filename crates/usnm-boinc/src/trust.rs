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
use anyhow::Context;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

mod build;
mod gate;
mod promote;
#[cfg(test)]
mod tests;

use gate::immutable_image;
pub use gate::{collect, quarantine_dir};
pub use promote::Admission;

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

impl Trust {
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
}
