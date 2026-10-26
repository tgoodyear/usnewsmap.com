use super::{immutable_image, Scope, Trust, MAX_REPORTS, TRUST_VERSION};
use crate::atomic;
use anyhow::Context;
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

/// What [`Trust::admit_hosts`] changed: the list's digest before and after, and the hosts it
/// added (empty when every host was already eligible, and the file was not rewritten).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Admission {
    pub before_sha256: String,
    pub after_sha256: String,
    pub added: BTreeSet<u64>,
}

impl Trust {
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

    pub(super) fn promote_to_with_limit(
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
}
