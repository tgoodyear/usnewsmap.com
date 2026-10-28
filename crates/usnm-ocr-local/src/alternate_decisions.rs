//! Append-only, explicitly reviewed alternate decisions. This is a preview
//! reader: release and scheduling do not consult it.

use std::collections::BTreeSet;

use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use usnm_core::ids::PageKey;
use usnm_state::state::State;
use usnm_store::ObjectStore;

use crate::alternate::{self, digest, put_exact, read_retained, Rights};
use crate::{sync::Lease, LOCK, ROOT};

const MAX_DECISIONS: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Subject {
    pub doc_id: String,
    pub source_image_sha256: String,
    pub baseline_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "kebab-case", deny_unknown_fields)]
pub enum Action {
    Select {
        candidate: String,
        comparison_sha256: String,
    },
    RetainBaseline,
    Reject {
        candidate: String,
    },
    Revoke {
        candidate: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Decision {
    pub subject: Subject,
    /// Exact prior decision hash, or null for a new history. Stale writers fail.
    pub previous: Option<String>,
    pub action: Action,
    pub reason: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Record {
    pub id: String,
    pub decision: Decision,
    /// Preview only; no release or queue action follows.
    pub selected: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Counts {
    reference_chars: u64,
    baseline_edits: u64,
    candidate_edits: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Comparison {
    subject: Subject,
    candidate: String,
    reference_sha256: String,
    reference_status: String,
    reviewed_by: String,
    compatibility_accepted: bool,
    legacy_ws_v1: Counts,
    raw_v1: Counts,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Head {
    count: usize,
    id: String,
    previous: Option<String>,
}

fn expected_head(records: &[Record]) -> Option<Head> {
    records.last().map(|r| Head {
        count: records.len(),
        id: r.id.clone(),
        previous: r.decision.previous.clone(),
    })
}

async fn committed_head(
    curated: &dyn ObjectStore,
    subject: &Subject,
) -> anyhow::Result<Option<Head>> {
    let prefix = format!("{}/commit", prefix(subject)?);
    let objects = curated.list_info(&prefix).await?;
    ensure!(
        objects.len() <= MAX_DECISIONS,
        "decision commit bound exceeded"
    );
    let mut previous: Option<Head> = None;
    for (i, object) in objects.iter().enumerate() {
        ensure!(
            object.path == format!("{prefix}/{:06}.json", i + 1),
            "decision commit gap"
        );
        let head: Head = serde_json::from_slice(&read_retained(curated, object, 65536).await?)
            .map_err(|_| anyhow::anyhow!("invalid decision commit JSON"))?;
        ensure!(
            head.count == i + 1
                && hash_ok(&head.id)
                && head.previous.as_ref() == previous.as_ref().map(|p| &p.id),
            "decision commit chain mismatch"
        );
        previous = Some(head);
    }
    Ok(previous)
}

async fn commit(
    curated: &dyn ObjectStore,
    subject: &Subject,
    records: &[Record],
) -> anyhow::Result<()> {
    let head = expected_head(records).context("cannot commit empty decision history")?;
    put_exact(
        curated,
        &format!("{}/commit/{:06}.json", prefix(subject)?, head.count),
        &serde_json::to_vec(&head)?,
        "application/json",
    )
    .await?;
    Ok(())
}

fn rollback_of(pending: &Record, decision: &Decision) -> bool {
    let Action::Select { candidate, .. } = &pending.decision.action else {
        return false;
    };
    decision.previous.as_ref() == Some(&pending.id)
        && match &decision.action {
            Action::RetainBaseline => true,
            Action::Reject { candidate: target } | Action::Revoke { candidate: target } => {
                target == candidate
            }
            Action::Select { .. } => false,
        }
}

fn hash_ok(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}

fn prefix(subject: &Subject) -> anyhow::Result<String> {
    ensure!(
        PageKey::from_doc_id(&subject.doc_id)?.doc_id() == subject.doc_id,
        "noncanonical decision page"
    );
    ensure!(
        hash_ok(&subject.source_image_sha256) && hash_ok(&subject.baseline_sha256),
        "invalid decision scan/baseline hash"
    );
    Ok(format!(
        "{ROOT}/alternate-decisions/{}/{}/{}",
        subject.doc_id, subject.source_image_sha256, subject.baseline_sha256
    ))
}

#[derive(Default)]
struct Preview {
    selected: Option<String>,
    blocked: BTreeSet<String>,
}

impl Preview {
    fn apply(&mut self, action: &Action) -> anyhow::Result<()> {
        match action {
            Action::Select { candidate, .. } => {
                ensure!(
                    !self.blocked.contains(candidate),
                    "candidate rejected or revoked"
                );
                self.selected = Some(candidate.clone());
            }
            Action::RetainBaseline => self.selected = None,
            Action::Reject { candidate } | Action::Revoke { candidate } => {
                self.blocked.insert(candidate.clone());
                if self.selected.as_ref() == Some(candidate) {
                    self.selected = None;
                }
            }
        }
        Ok(())
    }
}

async fn history(
    curated: &dyn ObjectStore,
    subject: &Subject,
) -> anyhow::Result<(Vec<Record>, Preview)> {
    let prefix = format!("{}/history", prefix(subject)?);
    let objects = curated.list_info(&prefix).await?;
    ensure!(
        objects.len() <= MAX_DECISIONS,
        "alternate decision history bound exceeded"
    );
    let mut records: Vec<Record> = Vec::new();
    let mut preview = Preview::default();
    for (i, object) in objects.iter().enumerate() {
        ensure!(
            object.path == format!("{prefix}/{i:06}.json"),
            "decision history gap or foreign object"
        );
        let bytes = read_retained(curated, object, 65536).await?;
        let record: Record = serde_json::from_slice(&bytes)
            .map_err(|_| anyhow::anyhow!("invalid decision history JSON"))?;
        ensure!(
            record.decision.subject == *subject
                && record.decision.previous.as_ref() == records.last().map(|r| &r.id)
                && record.id == digest(&serde_json::to_vec(&record.decision)?),
            "decision history identity/chain mismatch"
        );
        preview.apply(&record.decision.action)?;
        ensure!(
            record.selected == preview.selected,
            "decision preview mismatch"
        );
        records.push(record);
    }
    Ok((records, preview))
}

async fn validate_action(
    curated: &dyn ObjectStore,
    decision: &Decision,
    comparison: Option<&[u8]>,
) -> anyhow::Result<()> {
    let candidate = match &decision.action {
        Action::Select { candidate, .. }
        | Action::Reject { candidate }
        | Action::Revoke { candidate } => candidate,
        Action::RetainBaseline => {
            ensure!(
                comparison.is_none(),
                "baseline decision must not carry a comparison"
            );
            return Ok(());
        }
    };
    let staged = if matches!(decision.action, Action::Select { .. }) {
        alternate::read(curated, &decision.subject.doc_id, candidate).await?
    } else {
        alternate::read_identity(curated, &decision.subject.doc_id, candidate).await?
    };
    ensure!(
        staged.binding.source_image_sha256 == decision.subject.source_image_sha256
            && staged.binding.baseline_sha256 == decision.subject.baseline_sha256,
        "candidate belongs to another scan or baseline"
    );
    if let Action::Select {
        comparison_sha256, ..
    } = &decision.action
    {
        ensure!(
            [
                staged.binding.text_rights,
                staged.binding.alto_rights,
                staged.binding.layout_rights
            ]
            .iter()
            .all(|r| *r == Rights::Cleared),
            "selection requires independently cleared component rights"
        );
        let bytes = comparison.context("selection requires a paired comparison")?;
        ensure!(
            !bytes.is_empty()
                && bytes.len() <= 65536
                && hash_ok(comparison_sha256)
                && digest(bytes) == *comparison_sha256,
            "comparison size/checksum mismatch"
        );
        let report: Comparison = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("invalid paired comparison JSON"))?;
        ensure!(
            report.subject == decision.subject
                && report.candidate == *candidate
                && hash_ok(&report.reference_sha256)
                && report.reference_status == "human-verified"
                && !report.reviewed_by.trim().is_empty()
                && report.reviewed_by.len() <= 512
                && report.compatibility_accepted,
            "comparison lacks same-source human-reference/compatibility acceptance"
        );
        for counts in [&report.legacy_ws_v1, &report.raw_v1] {
            ensure!(
                counts.reference_chars > 0 && counts.candidate_edits <= counts.baseline_edits,
                "comparison has an empty denominator or a measured regression"
            );
        }
    } else {
        ensure!(
            comparison.is_none(),
            "non-selection decision must not carry a comparison"
        );
    }
    Ok(())
}

/// Return the current preview only after revalidating selected artifacts and
/// comparison. Corruption/missing evidence errors rather than falling back.
pub async fn read(curated: &dyn ObjectStore, subject: &Subject) -> anyhow::Result<Option<Record>> {
    let head = committed_head(curated, subject).await?;
    let (records, _) = history(curated, subject).await?;
    ensure!(
        head == expected_head(&records),
        "decision history does not match committed head"
    );
    let latest = records.last().cloned();
    if let Some(selected) = latest.as_ref().and_then(|r| r.selected.as_ref()) {
        let selection = records.iter().rev().find(|r| {
            matches!(&r.decision.action, Action::Select {candidate, ..} if candidate == selected)
        }).context("selected candidate has no selection decision")?;
        if let Action::Select {
            comparison_sha256, ..
        } = &selection.decision.action
        {
            let evidence_prefix = format!("{}/evidence", prefix(subject)?);
            let objects = curated.list_info(&evidence_prefix).await?;
            let object = objects
                .iter()
                .find(|o| o.path == format!("{evidence_prefix}/{comparison_sha256}.json"))
                .context("selected comparison evidence missing")?;
            let bytes = read_retained(curated, object, 65536).await?;
            validate_action(curated, &selection.decision, Some(&bytes)).await?;
        }
    }
    ensure!(
        committed_head(curated, subject).await? == head,
        "decision head changed during preview; retry"
    );
    Ok(latest)
}

async fn validated_replay(
    curated: &dyn ObjectStore,
    subject: &Subject,
    record: &Record,
) -> anyhow::Result<Record> {
    read(curated, subject).await?;
    Ok(record.clone())
}

/// Append an explicit decision under the existing OCR lock. Its receipt is
/// written before separate immutable commit markers. Exact retry or an explicit
/// rollback can recover an interrupted selection without trusting its payloads.
pub async fn decide(
    state: &State,
    owner: &str,
    curated: &dyn ObjectStore,
    decision: &Decision,
    comparison: Option<&[u8]>,
) -> anyhow::Result<Record> {
    let path = prefix(&decision.subject)?;
    ensure!(
        !decision.reason.trim().is_empty()
            && decision.reason.len() <= 2048
            && !decision.evidence.trim().is_empty()
            && decision.evidence.len() <= 2048,
        "decision requires bounded reason and evidence"
    );
    validate_action(curated, decision, comparison).await?;
    let lease = Lease::take(state, owner).await?;
    let result: anyhow::Result<Record> = async {
        let (mut records, mut preview) = history(curated, &decision.subject).await?;
        let head = committed_head(curated, &decision.subject).await?;
        let committed = head.as_ref().map_or(0, |h| h.count);
        ensure!(
            committed <= records.len() && head == expected_head(&records[..committed]),
            "decision history does not match committed head"
        );
        let pending = records.len() - committed;
        if pending > 0 {
            ensure!(
                pending <= 2 && (records.len() < MAX_DECISIONS || preview.selected.is_none()),
                "invalid pending decision history"
            );
            let latest = records.last().context("pending decision missing")?;
            if latest.decision == *decision {
                let prior = latest.clone();
                ensure!(
                    pending == 1 || rollback_of(&records[records.len() - 2], decision),
                    "pending recovery is not a selection rollback"
                );
                for count in committed + 1..=records.len() {
                    lease.check()?;
                    commit(curated, &decision.subject, &records[..count]).await?;
                }
                return validated_replay(curated, &decision.subject, &prior).await;
            }
            ensure!(
                pending == 1 && rollback_of(latest, decision),
                "recover the exact pending decision or explicitly roll it back"
            );
        } else if let Some(prior) = records.last().filter(|r| r.decision == *decision) {
            return validated_replay(curated, &decision.subject, prior).await;
        }
        ensure!(
            records.len() < MAX_DECISIONS,
            "alternate decision history bound exceeded"
        );
        ensure!(
            decision.previous.as_ref() == records.last().map(|r| &r.id),
            "stale decision head; read current history before retrying"
        );
        preview.apply(&decision.action)?;
        ensure!(
            records.len() + 1 < MAX_DECISIONS || preview.selected.is_none(),
            "final decision slot is reserved for returning to baseline"
        );
        if let (
            Action::Select {
                comparison_sha256, ..
            },
            Some(bytes),
        ) = (&decision.action, comparison)
        {
            lease.check()?;
            put_exact(
                curated,
                &format!("{path}/evidence/{comparison_sha256}.json"),
                bytes,
                "application/json",
            )
            .await?;
        }
        let record = Record {
            id: digest(&serde_json::to_vec(decision)?),
            decision: decision.clone(),
            selected: preview.selected,
        };
        lease.check()?;
        put_exact(
            curated,
            &format!("{path}/history/{:06}.json", records.len()),
            &serde_json::to_vec_pretty(&record)?,
            "application/json",
        )
        .await?;
        records.push(record.clone());
        for count in committed + 1..=records.len() {
            lease.check()?;
            commit(curated, &decision.subject, &records[..count]).await?;
        }
        tracing::info!(
            doc_id = %decision.subject.doc_id,
            decision = %record.id,
            "recorded alternate preview decision"
        );
        Ok(record)
    }
    .await;
    drop(lease);
    let unlocked = state.unlock(LOCK, owner).await;
    let record = result?;
    unlocked?;
    Ok(record)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use usnm_state::docs::MemoryDocs;

    #[tokio::test]
    async fn stale_commit_cannot_regress_newer_head() {
        let temp = tempfile::tempdir().unwrap();
        let store = usnm_store::open(temp.path().to_str().unwrap()).unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        let mut decision = Decision {
            subject: Subject {
                doc_id: "sn83045462_1890-05-03_ed-1_seq-7".into(),
                source_image_sha256: "0".repeat(64),
                baseline_sha256: "1".repeat(64),
            },
            previous: None,
            action: Action::RetainBaseline,
            reason: "Synthetic baseline fixture".into(),
            evidence: "Synthetic evidence".into(),
        };
        let mut records = Vec::new();
        for _ in 0..3 {
            let record = decide(&state, "decide", store.as_ref(), &decision, None)
                .await
                .unwrap();
            decision.previous = Some(record.id.clone());
            records.push(record);
        }
        // A writer resumes its old commit after another owner advanced twice.
        commit(store.as_ref(), &decision.subject, &records[..1])
            .await
            .unwrap();
        assert_eq!(
            read(store.as_ref(), &decision.subject).await.unwrap(),
            records.last().cloned()
        );
        assert_eq!(
            validated_replay(store.as_ref(), &decision.subject, &records[0])
                .await
                .unwrap(),
            records[0]
        );
        let commits = store
            .list_info(&format!("{}/commit", prefix(&decision.subject).unwrap()))
            .await
            .unwrap();
        let latest = commits.last().unwrap();
        store
            .put(&latest.path, b"{}".to_vec(), "application/json")
            .await
            .unwrap();
        assert!(
            validated_replay(store.as_ref(), &decision.subject, &records[0])
                .await
                .is_err()
        );
    }
}
