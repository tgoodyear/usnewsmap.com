//! Stock create_work integration with a write-ahead submission state.
use crate::{atomic_download, dir, load, read, save, Lock, Stage};
use anyhow::ensure;
use std::path::Path;
use tokio::process::Command;
use usnm_ocr_contract::message::Reason;

mod remote;
pub use remote::{prepare_download, Download, PreparedDownload};

/// A page someone was shown (`ocr-priority`, #866) gets a one-day deadline, so a host that
/// doesn't return it hands it on within a day rather than three. Its workunit priority
/// takes effect once the feeder orders by priority. The claim's lease must outlast the
/// deadline plus a collect.
pub(crate) fn schedule(reason: Reason) -> (&'static str, &'static str) {
    match reason {
        Reason::User => ("86400", "10"),
        Reason::Bulk => ("259200", "0"),
    }
}

pub async fn submit(root: &Path, project: &Path, id: &str) -> anyhow::Result<()> {
    submit_with_download(root, project, id, None).await
}

/// Native OCR pilot only; all other submitters retain their own staging policy.
pub async fn submit_with_download(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
) -> anyhow::Result<()> {
    let _lock = Lock::take(root)?;
    let project = project.canonicalize()?;
    let mut record = load(root, id)?;
    ensure!(
        record.stage == Stage::Reserved,
        "job is not reserved; uncertain submissions require reconciliation"
    );
    let input = dir(root, id)?;
    let job: usnm_ocr_contract::boinc::Job = serde_json::from_slice(&read(
        &input.join("request.json"),
        usnm_ocr_contract::boinc::MAX_JSON,
    )?)?;
    ensure!(job == record.job, "staged request changed");
    let scan = read(&input.join("page"), usnm_ocr_contract::boinc::MAX_SCAN)?;
    ensure!(
        scan.len() as u64 == job.scan.bytes
            && usnm_ocr_contract::boinc::sha256(&scan) == job.scan.sha256,
        "staged scan changed"
    );
    let remote = if let Some(download) = download {
        Some(download.prepare(&scan, &job.scan.sha256).await?)
    } else {
        None
    };
    record.stage = Stage::Submitting;
    save(root, &record)?;
    let result = async {
        for (logical, limit) in [
            ("page", usnm_ocr_contract::boinc::MAX_SCAN),
            ("request.json", usnm_ocr_contract::boinc::MAX_JSON),
        ] {
            if logical == "page" && remote.is_some() {
                continue;
            }
            let physical = format!("{}_{}", id, logical.replace('.', "_"));
            let output = Command::new(project.join("bin/dir_hier_path"))
                .arg(&physical)
                .current_dir(&project)
                .output()
                .await?;
            ensure!(output.status.success(), "dir_hier_path failed");
            let path = String::from_utf8(output.stdout)?;
            let path = Path::new(path.trim());
            ensure!(!path.as_os_str().is_empty(), "empty staging path");
            let path = if path.is_absolute() {
                path.to_owned()
            } else {
                project.join(path)
            };
            atomic_download(&path, &read(&dir(root, id)?.join(logical), limit)?)?;
        }
        let (delay_bound, priority) = schedule(job.message.reason);
        let mut command = Command::new(project.join("bin/create_work"));
        command.current_dir(&project).args([
            "--appname",
            "usnm_ocr",
            "--wu_name",
            &record.job.workunit(),
            "--wu_template",
            "templates/usnm_ocr_in",
            "--result_template",
            "templates/usnm_ocr_out",
            "--min_quorum",
            "1",
            "--target_nresults",
            "1",
            "--max_error_results",
            "3",
            "--max_total_results",
            "5",
            "--max_success_results",
            "3",
            "--rsc_fpops_est",
            "1e12",
            "--rsc_fpops_bound",
            "1e14",
            "--rsc_memory_bound",
            "1610612736",
            "--rsc_disk_bound",
            "1073741824",
            "--delay_bound",
            delay_bound,
            "--priority",
            priority,
        ]);
        if let Some(retry) = &record.retry {
            command.args(["--app_version_num", &retry.app_version_num.to_string()]);
        }
        // Stock create_work inserts remote_file at this exact input position.
        // Template file 0 opens as page, file 1 as request.json.
        if let Some(remote) = &remote {
            command.args(["--remote_file", &remote.url, &remote.bytes, &remote.md5]);
        } else {
            command.arg(format!("{id}_page"));
        }
        let status = command.arg(format!("{id}_request_json")).status().await?;
        ensure!(status.success(), "create_work failed");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => {
            record.stage = Stage::Submitted;
            record.error = None;
            save(root, &record)
        }
        Err(e) => {
            record.error=Some("submission uncertain; inspect BOINC by deterministic workunit name before reconciling".into());
            save(root, &record)?;
            Err(e.context("submission uncertain; automatic resubmission disabled"))
        }
    }
}
/// Explicit operator reconciliation AFTER checking the BOINC server. A page submitted in an
/// OCR bundle (#1059) is reconciled with every other page of its bundle: check the bundle's
/// workunit (`usnm-boinc status` names it); absent puts them all back to reserved, out of the
/// bundle.
pub fn reconcile(root: &Path, id: &str, present: bool) -> anyhow::Result<()> {
    let _lock = Lock::take(root)?;
    let record = load(root, id)?;
    let mut members = match &record.bundle {
        Some(bundle) => crate::records(root)?
            .into_iter()
            .filter(|r| r.bundle.as_ref() == Some(bundle))
            .collect(),
        None => vec![record],
    };
    let bundled = members.len() > 1 || members.first().is_some_and(|r| r.bundle.is_some());
    // A bundle interrupted while its members were being marked submitted has some of each:
    // confirming it present finishes the job, so it may be retried safely.
    members.retain(|r| !(bundled && present && r.stage == Stage::Submitted));
    ensure!(
        !members.is_empty(),
        "only uncertain submissions or server-recovered terminal jobs can be reconciled"
    );
    for record in &members {
        ensure!(
            record.stage == Stage::Submitting || (record.stage == Stage::Failed && present),
            "only uncertain submissions or server-recovered terminal jobs can be reconciled"
        );
    }
    for record in &mut members {
        record.stage = if present {
            Stage::Submitted
        } else {
            Stage::Reserved
        };
        if !present {
            record.bundle = None;
        }
        record.error = None;
        save(root, record)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_pages_get_a_shorter_deadline_and_a_higher_priority() {
        assert_eq!(schedule(Reason::User), ("86400", "10"));
        assert_eq!(schedule(Reason::Bulk), ("259200", "0"));
    }
}
