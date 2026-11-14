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
    submit_pinned(root, project, id, download, native, None).await
}

/// The workunit batch of every line job (#1621), retries included. The `docker_lines` plan
/// class (boinc/project/plan_class_spec.xml) admits only this batch, so its app version loads
/// beside the live `docker` one without serving unpinned work, and a line job's pin can only
/// be met there. Its results land in `results/1621`.
pub const LINE_BATCH: u32 = 1621;

/// The plan class a retry's pinned version must be in: a line job's worker is a
/// `docker_lines` version, every other page's a `docker` one.
fn retry_plan_class(job: &usnm_ocr_contract::boinc::Job) -> &'static str {
    if job.lines.is_some() {
        "docker_lines"
    } else {
        "docker"
    }
}

/// [`submit_checked`], pinned to the `usnm_ocr` version `pin` (`--app_version_num`). A line job
/// (#1621) that isn't a retry needs one, so a worker that predates line jobs never gets it; a
/// retry is pinned by its own record.
pub async fn submit_pinned(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
    native: Option<&RetryNative<'_>>,
    pin: Option<u32>,
) -> anyhow::Result<()> {
    submit_placed(root, project, id, download, native, pin, None).await
}

/// The BOINC app of the native OCR build, which needs no container runtime (#1418,
/// docs/boinc/apps/native-ocr.md). Its units are only ever assigned to one host.
pub const NATIVE_APP: &str = "usnm_ocr_native";

/// Which app a single-page unit goes to.
#[derive(Clone, Copy)]
enum Build<'a> {
    /// `usnm_ocr`, the worker image. `trust` is given for a controller-side retry's child
    /// (#1938) that goes to one host.
    Container {
        trust: Option<&'a crate::trust::Trust>,
    },
    /// [`NATIVE_APP`]: always for one host (`--target_host`), and only one `trust` already
    /// admits for the job's exact scope and native digest, on a build `builds` records a
    /// passing challenge for.
    Native {
        host: Option<u64>,
        trust: &'a crate::trust::Trust,
        builds: &'a crate::trust::BuildTrust,
        registered: &'a [u32],
    },
}

/// [`submit_pinned`], and for a line job optionally targeted at one host (`--target_host`,
/// which needs `<enable_assignment>`): a targeted job never enters the feeder's array, so only
/// that host, calibrated on the line worker, can get it.
pub async fn submit_placed(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
    native: Option<&RetryNative<'_>>,
    pin: Option<u32>,
    target_host: Option<u64>,
) -> anyhow::Result<()> {
    submit_unit(
        root,
        project,
        id,
        download,
        native,
        pin,
        target_host,
        Build::Container { trust: None },
    )
    .await
}

/// [`submit_placed`] for a controller-side retry's child (#1938), which may be for the hosts its
/// journal names (`retry.provenance.target_hosts`, each golded for its image when it was
/// prepared). `trust` is checked again here, for the host and the child's image, before anything
/// is staged: a host that is not golded for the image is never sent the page, and a child whose
/// journal names hosts is never submitted without one of them.
pub async fn submit_retry_targeted(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
    native: Option<&RetryNative<'_>>,
    target_host: Option<u64>,
    trust: &crate::trust::Trust,
) -> anyhow::Result<()> {
    submit_unit(
        root,
        project,
        id,
        download,
        native,
        None,
        target_host,
        Build::Container { trust: Some(trust) },
    )
    .await
}

/// Why a retry's child may not go to `target_host` (#1938): its journal names the hosts it may
/// go to, and the trust list must still admit the host for the child's image. None when the
/// child has no note, or names no host and is not targeted.
fn refuse_retry_target(
    record: &crate::Record,
    target_host: Option<u64>,
    trust: Option<&crate::trust::Trust>,
) -> Option<String> {
    let provenance = record.retry.as_ref()?.provenance.as_ref()?;
    let Some(host) = target_host else {
        return (!provenance.target_hosts.is_empty()).then(|| {
            format!(
                "the retry is for hosts {:?} and needs --target-host: it is never submitted to the pool",
                provenance.target_hosts
            )
        });
    };
    if !provenance.target_hosts.contains(&host) {
        return Some(format!(
            "host {host} is not one the retry was prepared for ({:?})",
            provenance.target_hosts
        ));
    }
    let Some(trust) = trust else {
        return Some("a targeted retry needs --trust, the host trust list".into());
    };
    trust
        .refuse_job(&record.job, Some(host))
        .map(|reason| format!("trust refuses host {host} for this retry: {reason}"))
}

/// Submits job `id` as a [`NATIVE_APP`] unit for `target_host` (#1780). A native unit is always
/// targeted: with no host it is refused. Before anything is staged or created, `trust` must
/// admit the host for the job's exact scope and native digest ([`crate::trust::Trust::refuse_job`]),
/// so a host that isn't trusted for the native build is never sent a page. The build must also
/// have a passing run of the scope's calibrated challenge in `builds`
/// ([`crate::trust::Trust::refuse_native_job`]). Line jobs, retries
/// and bundles stay container work. `registered` are the live (registered, not deprecated)
/// [`NATIVE_APP`] version numbers, from the database: the unit is pinned to the newest of them
/// that packages the job's build.
#[allow(clippy::too_many_arguments)]
pub async fn submit_native(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
    target_host: Option<u64>,
    trust: &crate::trust::Trust,
    builds: &crate::trust::BuildTrust,
    registered: &[u32],
) -> anyhow::Result<()> {
    submit_unit(
        root,
        project,
        id,
        download,
        None,
        None,
        target_host,
        Build::Native {
            host: target_host,
            trust,
            builds,
            registered,
        },
    )
    .await
}

/// The `version_num` of the newest of the `registered` [`NATIVE_APP`] versions in `project` that
/// packages the build `image` names: one whose directory (`apps/usnm_ocr_native/<major.minor>/<platform>/`) holds
/// its manifest under the physical name that carries the digest (`usnm_native_build_<sha256>.json`,
/// boinc/scripts/install.py). A unit is pinned to it, so the scheduler never sends it a newer
/// build whose worker would refuse the job. None packages it: an error.
/// A directory a failed publication left without its `app_version` row isn't in `registered`, so it
/// is never chosen.
pub(crate) fn native_version(
    project: &Path,
    image: &str,
    registered: &[u32],
) -> anyhow::Result<u32> {
    let digest = image
        .strip_prefix("sha256:")
        .filter(|d| usnm_ocr_contract::boinc::is_sha256(d))
        .context("a native job's engine.image is sha256:<64 hex>")?;
    let manifest = format!("usnm_native_build_{digest}.json");
    let mut found = None;
    let apps = project.join("apps").join(NATIVE_APP);
    for version in std::fs::read_dir(&apps).into_iter().flatten() {
        let version = version?;
        let name = version.file_name();
        let Some((major, minor)) = name.to_str().and_then(|n| n.split_once('.')) else {
            continue;
        };
        let (Ok(major), Ok(minor)) = (major.parse::<u32>(), minor.parse::<u32>()) else {
            continue;
        };
        if minor >= 100 || !version.file_type()?.is_dir() {
            continue;
        }
        for platform in std::fs::read_dir(version.path())? {
            let dir = platform?.path();
            let listed = std::fs::read_to_string(dir.join("version.xml"))
                .is_ok_and(|xml| xml.contains(&manifest));
            let number = 100 * major + minor;
            if registered.contains(&number) && (dir.join(&manifest).is_file() || listed) {
                found = Some(found.map_or(number, |n: u32| n.max(number)));
            }
        }
    }
    found.with_context(|| {
        format!(
            "no registered {NATIVE_APP} version in {} packages the build {image}",
            apps.display()
        )
    })
}

#[allow(clippy::too_many_arguments)]
async fn submit_unit(
    root: &Path,
    project: &Path,
    id: &str,
    download: Option<&Download>,
    native: Option<&RetryNative<'_>>,
    pin: Option<u32>,
    target_host: Option<u64>,
    build: Build<'_>,
) -> anyhow::Result<()> {
    let _lock = Lock::take(root)?;
    let project = project.canonicalize()?;
    let mut record = load(root, id)?;
    ensure!(
        record.stage == Stage::Reserved,
        "job is not reserved; uncertain submissions require reconciliation"
    );
    let is_retry = record.retry.is_some();
    let pin = if is_retry {
        None
    } else {
        pin.filter(|v| *v > 0)
    };
    let app = match build {
        Build::Container { .. } => "usnm_ocr",
        Build::Native {
            host,
            trust,
            builds,
            ..
        } => {
            let Some(host) = host else {
                anyhow::bail!(
                    "a {NATIVE_APP} unit needs --target-host: native units are only ever assigned to one host"
                );
            };
            ensure!(host > 0, "--target-host must be positive");
            ensure!(
                record.job.lines.is_none() && !is_retry && record.bundle.is_none(),
                "line jobs, retries and bundles are container work, never {NATIVE_APP} units"
            );
            if let Some(reason) = trust.refuse_native_job(&record.job, Some(host), builds) {
                anyhow::bail!("trust refuses host {host} for this {NATIVE_APP} job: {reason}");
            }
            NATIVE_APP
        }
    };
    let native_unit = matches!(build, Build::Native { .. });
    // A native unit is pinned to the version that packages its build (#1780): with more than one
    // published, the scheduler would otherwise pick the newest.
    let pin = match build {
        Build::Native { registered, .. } => Some(native_version(
            &project,
            &record.job.engine.image,
            registered,
        )?),
        Build::Container { .. } => pin,
    };
    ensure!(
        record.job.lines.is_none() || is_retry || pin.is_some(),
        "a line job needs --app-version-num, the usnm_ocr version whose worker reads line jobs"
    );
    let retry_trust = match build {
        Build::Container { trust } => trust,
        Build::Native { .. } => None,
    };
    if let Some(reason) = refuse_retry_target(&record, target_host, retry_trust) {
        anyhow::bail!("{reason}");
    }
    if let Some(host) = target_host {
        ensure!(host > 0, "--target-host must be positive");
        ensure!(
            native_unit
                || record.job.lines.is_some()
                || record
                    .retry
                    .as_ref()
                    .is_some_and(|retry| retry.provenance.is_some()),
            "only a line job, a controller-side retry or a {NATIVE_APP} unit can be targeted at a host"
        );
        let config = std::fs::read_to_string(project.join("config.xml"))?;
        ensure!(
            crate::gold::submit::assignment_enabled(&config)?,
            "targeted work requires an explicitly enabled <enable_assignment>1</enable_assignment>"
        );
    }
    let expected = record.queued()?;
    ensure!(
        !is_retry || native.is_some(),
        "retry submission requires native state and curated store for lease-bound revalidation"
    );
    let input = dir(root, id)?;
    let job: usnm_ocr_contract::boinc::Job = serde_json::from_slice(&read(
        &input.join("request.json"),
        usnm_ocr_contract::boinc::MAX_REQUEST,
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
                .args(["-c", "import sys,types; m=types.ModuleType('retry_release_check'); exec(sys.argv[1],m.__dict__); m.verify_retry_worker(m.Path(sys.argv[2]),int(sys.argv[3]),sys.argv[4],sys.argv[5])"])
                .arg(include_str!("../../../boinc/scripts/acceptance_server.py"))
                .arg(&project)
                .arg(retry.app_version_num.to_string())
                .arg(&job.engine.image)
                .arg(retry_plan_class(&job))
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
                ("request.json", usnm_ocr_contract::boinc::MAX_REQUEST),
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
                app,
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
            } else if let Some(pin) = pin {
                command.args(["--app_version_num", &pin.to_string()]);
            }
            if record.job.lines.is_some() {
                command.args(["--batch", &LINE_BATCH.to_string()]);
            }
            if let Some(host) = target_host {
                command.args(["--target_host", &host.to_string()]);
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

/// Native OCR units (#1780): always for one host, and only one trust admits.
#[cfg(all(test, unix))]
mod native_tests {
    use super::*;
    use crate::bundle::submit::tests::Server;
    use crate::bundle::work::tests::job;

    const ASSIGNMENT: &str =
        "<boinc><config><enable_assignment>1</enable_assignment></config></boinc>";

    /// A list that trusts `hosts` for `image` in the test jobs' eng/eng scope.
    fn trust_for(dir: &Path, image: &str, hosts: &[u64]) -> crate::trust::Trust {
        let path = dir.join("trust.json");
        let list = serde_json::json!({
            "hosts": {image: hosts},
            "manifest_sha256": "b".repeat(64),
            "scope": {"language": "eng", "profile": "eng", "psm": 3, "dpi": 300},
            "max_cer": 0.08734328574534386,
            "v": 4
        });
        std::fs::write(&path, serde_json::to_vec(&list).unwrap()).unwrap();
        crate::trust::Trust::load(&path).unwrap()
    }

    /// The reviewed builds, with the test jobs' own digest as a Linux build that passed the
    /// test lists' challenge.
    fn builds() -> crate::trust::BuildTrust {
        crate::trust::builds::with_build(
            crate::trust::BuildTrust::embedded().unwrap().clone(),
            &job(1).engine.image,
            "x86_64-pc-linux-gnu",
            &"b".repeat(64),
            0.08734328574534386,
            Some(5929),
        )
    }

    /// Trusts `hosts` for the test jobs' own digest.
    fn trust(dir: &Path, hosts: &[u64]) -> crate::trust::Trust {
        trust_for(dir, &job(1).engine.image, hosts)
    }

    /// Publishes native version `version` of the build `image` names into the project.
    fn publish(s: &Server, version: &str, image: &str) {
        let dir = s
            .project
            .join("apps")
            .join(NATIVE_APP)
            .join(version)
            .join("x86_64-pc-linux-gnu");
        std::fs::create_dir_all(&dir).unwrap();
        let digest = image.strip_prefix("sha256:").unwrap();
        std::fs::write(dir.join(format!("usnm_native_build_{digest}.json")), "{}").unwrap();
    }

    fn stage(s: &Server, id: &str) -> Stage {
        load(&s.root, id).unwrap().stage
    }

    /// A retry's child (#1938) for `hosts`, as its journal records it.
    fn retry_child(hosts: &[u64]) -> crate::Record {
        let job = job(1);
        let parent = "c".repeat(64);
        let retry = serde_json::json!({
            "previous_job": parent, "previous_attempt": 0,
            "previous_workunit": format!("usnm_{parent}"),
            "previous_ledger": {"batch_version": 1, "reason": "bulk", "language": "eng",
                "plan": "p", "attempt": 1, "request_day": null,
                "queued_at": "2026-10-24T12:00:00.000000Z"},
            "terminal": {"error_mask": 2, "no_output_files": false, "marker_sha256": "d".repeat(64)},
            "previous_error": "failed", "app_version_num": 106, "reviewed_reason": "r",
            "provenance": {"retry_of": parent, "parent_image": format!("sha256:{}", "e".repeat(64)),
                "reason": "slow-class", "target_hosts": hosts},
        });
        crate::Record {
            job,
            plan: "p".into(),
            stage: Stage::Reserved,
            stamp: "s".into(),
            error: None,
            terminal: None,
            retry: Some(serde_json::from_value(retry).unwrap()),
            ledger: None,
            bundle: None,
            unbundled: None,
            held: None,
        }
    }

    #[test]
    fn a_retrys_child_goes_only_to_its_golded_hosts_and_never_to_the_pool() {
        let dir = tempfile::tempdir().unwrap();
        let image = job(1).engine.image;
        let golded = trust_for(dir.path(), &image, &[1, 2]);
        let one_host = trust_for(dir.path(), &image, &[2]);
        let child = retry_child(&[1, 2]);
        // For hosts, so never the pool.
        let why = refuse_retry_target(&child, None, None).unwrap();
        assert!(why.contains("never submitted to the pool"), "{why}");
        // A host the child was not prepared for, even one the trust list admits.
        let why = refuse_retry_target(&child, Some(9), Some(&trust_for(dir.path(), &image, &[9])));
        assert!(why.unwrap().contains("not one the retry was prepared for"));
        // A prepared host needs the trust list, which must still admit it for the child's image.
        let why = refuse_retry_target(&child, Some(1), None).unwrap();
        assert!(why.contains("needs --trust"), "{why}");
        let why = refuse_retry_target(&child, Some(1), Some(&one_host)).unwrap();
        assert!(why.contains("trust refuses host 1"), "{why}");
        let other = trust_for(dir.path(), &format!("sha256:{}", "f".repeat(64)), &[1, 2]);
        assert!(refuse_retry_target(&child, Some(1), Some(&other)).is_some());
        assert_eq!(refuse_retry_target(&child, Some(1), Some(&golded)), None);
        // A child with no hosts is for the pool and is not targeted.
        let pooled = retry_child(&[]);
        assert_eq!(refuse_retry_target(&pooled, None, None), None);
        assert!(refuse_retry_target(&pooled, Some(1), Some(&golded)).is_some());
        // A page with no note is not this check's.
        let mut plain = retry_child(&[]);
        plain.retry.as_mut().unwrap().provenance = None;
        assert_eq!(refuse_retry_target(&plain, Some(1), None), None);
    }

    #[tokio::test]
    async fn a_page_that_is_not_a_retry_is_never_targeted_by_the_retry_route() {
        let (s, ids) = Server::new(&[1]);
        std::fs::write(s.project.join("config.xml"), ASSIGNMENT).unwrap();
        let trust = trust(&s.root, &[13]);
        let error =
            submit_retry_targeted(&s.root, &s.project, &ids[0], None, None, Some(13), &trust)
                .await
                .unwrap_err();
        assert!(
            error.to_string().contains("can be targeted at a host"),
            "{error:#}"
        );
        assert!(s.calls().is_empty());
    }

    #[tokio::test]
    async fn a_native_unit_without_a_target_host_is_refused() {
        let (s, ids) = Server::new(&[1]);
        std::fs::write(s.project.join("config.xml"), ASSIGNMENT).unwrap();
        let trust = trust(&s.root, &[13]);
        let error = submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            None,
            &trust,
            &builds(),
            &[100],
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("needs --target-host"),
            "{error:#}"
        );
        assert!(submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            Some(0),
            &trust,
            &builds(),
            &[100]
        )
        .await
        .is_err());
        assert!(s.calls().is_empty());
        assert_eq!(stage(&s, &ids[0]), Stage::Reserved);
    }

    #[tokio::test]
    async fn a_host_trust_refuses_gets_nothing_created() {
        let (s, ids) = Server::new(&[1]);
        std::fs::write(s.project.join("config.xml"), ASSIGNMENT).unwrap();
        // Host 5 isn't on the list, and host 13 trusted for another build (the worker image)
        // isn't trusted for this one.
        let other = format!("sha256:{}", "c".repeat(64));
        for (image, host) in [(job(1).engine.image, 5), (other, 13)] {
            let trust = trust_for(&s.root, &image, &[13]);
            let error = submit_native(
                &s.root,
                &s.project,
                &ids[0],
                None,
                Some(host),
                &trust,
                &builds(),
                &[100],
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("trust refuses host"),
                "{error:#}"
            );
        }
        assert!(s.calls().is_empty());
        assert!(std::fs::read_dir(s.project.join("download"))
            .unwrap()
            .next()
            .is_none());
        assert_eq!(stage(&s, &ids[0]), Stage::Reserved);
    }

    #[tokio::test]
    async fn a_trusted_host_gets_one_assigned_native_unit() {
        let (s, ids) = Server::new(&[1]);
        let trust = trust(&s.root, &[13]);
        // Without assignment the unit could go to any host.
        std::fs::write(s.project.join("config.xml"), "<boinc><config/></boinc>").unwrap();
        assert!(submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            Some(13),
            &trust,
            &builds(),
            &[100]
        )
        .await
        .is_err());
        assert!(s.calls().is_empty());
        std::fs::write(s.project.join("config.xml"), ASSIGNMENT).unwrap();
        // No published version packages the job's build: nothing is created.
        publish(&s, "1.1", &format!("sha256:{}", "c".repeat(64)));
        let error = submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            Some(13),
            &trust,
            &builds(),
            &[100],
        )
        .await
        .unwrap_err();
        assert!(
            error.to_string().contains("packages the build"),
            "{error:#}"
        );
        assert!(s.calls().is_empty());
        assert_eq!(stage(&s, &ids[0]), Stage::Reserved);
        // 1.0 packages it, beside the newer 1.1 of another build and a 1.2 of this build that a
        // failed publication left unregistered: the unit is pinned to 1.0.
        publish(&s, "1.0", &job(1).engine.image);
        publish(&s, "1.2", &job(1).engine.image);
        assert!(submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            Some(13),
            &trust,
            &builds(),
            &[]
        )
        .await
        .is_err());
        submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            Some(13),
            &trust,
            &builds(),
            &[100, 101],
        )
        .await
        .unwrap();
        let calls = s.calls();
        assert_eq!(calls.len(), 1);
        assert!(
            calls[0].starts_with(&format!("--appname {NATIVE_APP} ")),
            "{}",
            calls[0]
        );
        assert!(calls[0].contains(&format!("--wu_name usnm_{}", ids[0])));
        assert!(calls[0].contains("--target_host 13"));
        assert!(calls[0].contains("--app_version_num 100 "), "{}", calls[0]);
        assert_eq!(stage(&s, &ids[0]), Stage::Submitted);
    }

    #[tokio::test]
    async fn a_build_without_a_passing_challenge_gets_nothing_created() {
        let (s, ids) = Server::new(&[1]);
        std::fs::write(s.project.join("config.xml"), ASSIGNMENT).unwrap();
        publish(&s, "1.0", &job(1).engine.image);
        // Host 13 is on the list for the job's digest, but the digest isn't a recorded build,
        // or its recorded run is over the threshold.
        let trust = trust(&s.root, &[13]);
        let unlisted = crate::trust::BuildTrust::embedded().unwrap().clone();
        let failed = crate::trust::builds::with_build(
            unlisted.clone(),
            &job(1).engine.image,
            "x86_64-pc-linux-gnu",
            &"b".repeat(64),
            0.08734328574534386,
            Some(7300),
        );
        for builds in [unlisted, failed] {
            let error = submit_native(
                &s.root,
                &s.project,
                &ids[0],
                None,
                Some(13),
                &trust,
                &builds,
                &[100],
            )
            .await
            .unwrap_err();
            assert!(
                error.to_string().contains("native-build-trust.json"),
                "{error:#}"
            );
        }
        assert!(s.calls().is_empty());
        assert_eq!(stage(&s, &ids[0]), Stage::Reserved);
        // The same digest as a Windows build with a passing run takes the same path as Linux.
        let windows = crate::trust::builds::with_build(
            crate::trust::BuildTrust::embedded().unwrap().clone(),
            &job(1).engine.image,
            "windows_x86_64",
            &"b".repeat(64),
            0.08734328574534386,
            Some(6000),
        );
        submit_native(
            &s.root,
            &s.project,
            &ids[0],
            None,
            Some(13),
            &trust,
            &windows,
            &[100],
        )
        .await
        .unwrap();
        assert_eq!(s.calls().len(), 1);
        assert_eq!(stage(&s, &ids[0]), Stage::Submitted);
    }

    #[tokio::test]
    async fn container_units_are_unchanged() {
        let (s, ids) = Server::new(&[1]);
        submit(&s.root, &s.project, &ids[0]).await.unwrap();
        let calls = s.calls();
        assert!(calls[0].starts_with("--appname usnm_ocr "), "{}", calls[0]);
        assert!(!calls[0].contains("--target_host"));
        // A container page job still can't be targeted.
        let (s, ids) = Server::new(&[2]);
        std::fs::write(s.project.join("config.xml"), ASSIGNMENT).unwrap();
        assert!(
            submit_placed(&s.root, &s.project, &ids[0], None, None, None, Some(13))
                .await
                .is_err()
        );
        assert!(s.calls().is_empty());
    }
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
