//! Stock create_work integration with a write-ahead submission state.
use crate::{atomic_download, dir, load, read, save, Lock, Stage};
use anyhow::{ensure, Context};
use std::path::Path;
use tokio::process::Command;
use usnm_ocr_contract::message::Reason;

mod remote;
pub use remote::{prepare_download, Download, PreparedDownload};

async fn bounded_create_work(
    command: &mut Command,
    check: impl Fn() -> anyhow::Result<()>,
    timeout: std::time::Duration,
) -> anyhow::Result<()> {
    check()?;
    #[cfg(unix)]
    command.process_group(0);
    let mut child = command.kill_on_drop(true).spawn()?;
    #[cfg(unix)]
    let group = crate::ProcessGroup::new(child.id())?;
    let result = tokio::time::timeout(timeout, async {
        loop {
            check()?;
            tokio::select! {
                status = child.wait() => {
                    check()?;
                    let status = status.context("could not wait for create_work")?;
                    ensure!(status.success(), "create_work failed");
                    return Ok::<_, anyhow::Error>(());
                },
                _ = tokio::time::sleep(std::time::Duration::from_millis(100)) => {},
            }
        }
    })
    .await
    .context("create_work exceeded its submission deadline")
    .and_then(|result| result);
    if let Err(error) = result {
        #[cfg(unix)]
        group
            .kill()
            .with_context(|| format!("{error:#}; stopping uncertain create_work"))?;
        #[cfg(not(unix))]
        child
            .start_kill()
            .with_context(|| format!("{error:#}; stopping uncertain create_work"))?;
        child
            .wait()
            .await
            .with_context(|| format!("{error:#}; reaping uncertain create_work"))?;
        return Err(error);
    }
    Ok(())
}

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
    submit_checked(root, project, id, download, None).await
}

pub struct RetryNative<'a> {
    pub state: &'a usnm_state::state::State,
    pub curated: &'a dyn usnm_store::ObjectStore,
}

pub async fn submit_checked(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
    native: Option<&RetryNative<'_>>,
) -> anyhow::Result<()> {
    let _lock = Lock::take(root)?;
    let project = project.canonicalize()?;
    let mut record = load(root, id)?;
    ensure!(
        record.stage == Stage::Reserved,
        "job is not reserved; uncertain submissions require reconciliation"
    );
    let is_retry = record.retry.is_some();
    let expected = record.queued()?;
    ensure!(
        !is_retry || native.is_some(),
        "retry submission requires native state and curated store for lease-bound revalidation"
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
    let submission = |lease: Option<usnm_ocr_local::boinc::SubmissionLease>| async move {
        if let Some(retry) = &record.retry {
            let output = tokio::time::timeout(
            std::time::Duration::from_secs(90),
            Command::new("python3")
                .args(["-c", "import sys,types; m=types.ModuleType('retry_release_check'); exec(sys.argv[1],m.__dict__); m.verify_retry_worker(m.Path(sys.argv[2]),int(sys.argv[3]),sys.argv[4])"])
                .arg(include_str!("../../../scripts/boinc/acceptance_server.py"))
                .arg(&project)
                .arg(retry.app_version_num.to_string())
                .arg(&job.engine.image)
                .kill_on_drop(true)
                .output(),
        ).await??;
            ensure!(
                output.status.success(),
                "retry signed worker/app-version verification failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
        let remote = if let Some(download) = download {
            Some(download.prepare(&scan, &job.scan.sha256).await?)
        } else {
            None
        };
        if let Some(lease) = &lease {
            lease.check()?;
        }
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
            command.arg(format!("{id}_request_json"));
            if let Some(lease) = &lease {
                bounded_create_work(
                    &mut command,
                    || lease.check(),
                    std::time::Duration::from_secs(120),
                )
                .await?;
                lease.check()?;
            } else {
                ensure!(command.status().await?.success(), "create_work failed");
            }
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
    };
    if is_retry {
        let native = native.context("retry submission requires native stores")?;
        usnm_ocr_local::boinc::with_retry_reservation(
            native.state,
            native.curated,
            &expected,
            |lease| submission(Some(lease)),
        )
        .await
    } else {
        submission(None).await
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

    #[cfg(unix)]
    #[tokio::test]
    async fn uncertain_create_work_is_stopped_and_reaped_on_timeout_or_lease_loss() {
        for lose_lease in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let mutation = dir.path().join("created");
            let mut command = Command::new("sh");
            command
                .args(["-c", "sleep 1; printf created > \"$1\"", "create_work"])
                .arg(&mutation);
            let checks = std::cell::Cell::new(0);
            let error = bounded_create_work(
                &mut command,
                || {
                    checks.set(checks.get() + 1);
                    ensure!(!lose_lease || checks.get() < 3, "lease lost");
                    Ok(())
                },
                std::time::Duration::from_millis(300),
            )
            .await
            .unwrap_err();
            assert!(error
                .to_string()
                .contains(if lose_lease { "lease lost" } else { "deadline" }));
            tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
            assert!(
                !mutation.exists(),
                "stopped create_work mutated the database later"
            );
        }
    }
}
