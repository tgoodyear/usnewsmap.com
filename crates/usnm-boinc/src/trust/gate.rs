use super::{Scope, Trust};
use crate::{atomic, load, Stage};
use anyhow::bail;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use usnm_store::ObjectStore;

impl Trust {
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

pub(super) fn immutable_image(image: &str) -> bool {
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
