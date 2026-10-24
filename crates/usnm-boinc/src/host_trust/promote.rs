//! Promotion on evidence: the only workflow that changes trust.
use super::{campaign_roots, onboarded_hosts, submitted_attempts, Config};
use crate::{atomic, gold, read, trust::Trust, Lock};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::boinc;

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

#[cfg(test)]
mod tests {
    use super::*;

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
