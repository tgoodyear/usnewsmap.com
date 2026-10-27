//! Explicit operator disposition for a contract-invalid canonical output.
use crate::{atomic, dir, load, read, save, Lock, Stage};
use anyhow::ensure;
use std::path::Path;
use usnm_ocr_contract::boinc::{self, ResultManifest};

/// Preserve the pinned bytes before marking a submitted job failed. This never publishes,
/// releases a native reservation, deletes a scientific row, or resubmits a workunit.
pub fn reject(root: &Path, spool: &Path, id: &str, expected: [&str; 3]) -> anyhow::Result<()> {
    let _lock = Lock::take(root)?;
    let mut record = load(root, id)?;
    ensure!(
        record.stage == Stage::Submitted || record.stage == Stage::Failed,
        "only submitted jobs may be rejected"
    );
    ensure!(
        record.bundle.is_none(),
        "reconcile bundled outputs separately"
    );
    ensure!(
        !dir(root, id)?.join("accepted/result.json").exists(),
        "accepted output must be reconciled, not rejected"
    );
    let names = ["result.json", "text.txt", "layout.xml"];
    let limits = [boinc::MAX_JSON, boinc::MAX_TEXT, boinc::MAX_ALTO];
    let mut files = Vec::new();
    for ((name, limit), expected) in names.iter().zip(limits).zip(expected) {
        ensure!(boinc::is_sha256(expected), "invalid expected checksum");
        let bytes = read(
            &spool.join(format!("{}__file_{name}", record.job.workunit())),
            limit,
        )?;
        ensure!(
            boinc::sha256(&bytes) == expected,
            "output changed; reconcile again"
        );
        files.push(bytes);
    }
    record.job.validate()?;
    let reason = match serde_json::from_slice::<ResultManifest>(&files[0]) {
        Err(_) => "invalid_result_json",
        Ok(result) if result.matches(&record.job, &files[1], &files[2]).is_err() => {
            "invalid_output_contract"
        }
        Ok(_) => {
            anyhow::bail!("output contract is valid; refusal is not an invalid-output disposition")
        }
    };
    let quarantine = root.join("quarantine").join(id).join("rejected");
    let receipt_path = quarantine.join("receipt.json");
    let receipt = serde_json::to_vec_pretty(&serde_json::json!({
        "job": id,
        "workunit": record.job.workunit(),
        "reason": reason,
        "sha256": expected,
    }))?;
    if receipt_path.exists() {
        ensure!(
            read(&receipt_path, 16384)? == receipt,
            "rejection receipt conflicts"
        );
        for (name, bytes) in names.iter().zip(&files) {
            ensure!(
                read(&quarantine.join(name), boinc::MAX_ALTO)? == *bytes,
                "quarantined bytes conflict"
            );
        }
    } else {
        ensure!(
            record.stage == Stage::Submitted,
            "failed job has no rejection receipt"
        );
        for (name, bytes) in names.iter().zip(&files) {
            atomic(&quarantine.join(name), bytes)?;
        }
        atomic(&receipt_path, &receipt)?;
    }
    record.stage = Stage::Failed;
    record.error = Some(format!(
        "canonical output rejected: {reason}; originals quarantined"
    ));
    save(root, &record)
}
