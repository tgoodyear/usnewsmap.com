//! Server side: one `usnm_ocr` workunit for several reserved pages, with the same write-ahead
//! states as a single page. Every member goes to `submitting` together, with the bundle's id
//! on its server record, before stock `create_work` runs; an uncertain submission is reconciled
//! for the whole bundle (`crate::submit::reconcile`).
use crate::{atomic, atomic_download, dir, load, read, save, Lock, Stage};
use anyhow::{ensure, Context};
use std::path::{Path, PathBuf};
use tokio::process::Command;
use usnm_ocr_contract::boinc::{
    self,
    bundle::{Bundle, JOB_FILE, PAGES_FILE},
    Job,
};

/// Where the server keeps a bundle's inputs, beside the page journals.
pub fn bundle_dir(root: &Path, id: &str) -> anyhow::Result<PathBuf> {
    ensure!(boinc::is_sha256(id), "invalid bundle id");
    Ok(root.join("bundles").join(id))
}

/// What one call submitted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Submitted {
    /// The bundle's id, when one was made.
    pub bundle: Option<String>,
    /// Pages submitted as single-page units: those taken out of an earlier bundle, or a lone
    /// page left over.
    pub alone: Vec<String>,
}

/// Stock `create_work`'s arguments for `bundle`; `app_version` pins the worker version that
/// reads bundles (`--app_version_num`), so an older one never gets one.
pub fn create_work_args(bundle: &Bundle, app_version: u32) -> Vec<String> {
    let n = bundle.pages.len() as f64;
    let (delay_bound, priority) = crate::submit::schedule(bundle.pages[0].job.message.reason);
    // The scans, each page's saved outputs and the assembled outputs, with room to spare.
    let disk = (2u64 << 30) + 2 * bundle.bytes();
    let mut args: Vec<String> = [
        "--appname",
        "usnm_ocr",
        "--wu_name",
        &bundle.workunit(),
        "--wu_template",
        "templates/usnm_ocr_bundle_in",
        "--result_template",
        "templates/usnm_ocr_bundle_out",
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
        &format!("{:e}", 1e12 * n),
        "--rsc_fpops_bound",
        &format!("{:e}", 1e14 * n),
        "--rsc_memory_bound",
        "1610612736",
        "--rsc_disk_bound",
        &disk.to_string(),
        "--delay_bound",
        delay_bound,
        "--priority",
        priority,
    ]
    .into_iter()
    .map(str::to_owned)
    .collect();
    args.extend(["--app_version_num".into(), app_version.to_string()]);
    // Template file 0 opens as `pages`, file 1 as `ocr_bundle.json`.
    args.push(super::physical(&bundle.id, PAGES_FILE));
    args.push(super::physical(&bundle.id, JOB_FILE));
    args
}

/// Submit reserved pages `ids` as one unit (2 to 32 pages, one image and one queue). A page
/// taken out of an earlier bundle, and a page left alone, go as single-page units.
pub async fn submit(
    root: &Path,
    project: &Path,
    ids: &[String],
    app_version: Option<u32>,
) -> anyhow::Result<Submitted> {
    ensure!(
        !ids.is_empty() && ids.len() <= boinc::bundle::MAX_PAGES,
        "a unit holds 1 to {} pages",
        boinc::bundle::MAX_PAGES
    );
    let mut out = Submitted::default();
    let (alone, together): (Vec<String>, Vec<String>) = {
        let _lock = Lock::take(root)?;
        let mut alone = vec![];
        let mut together = vec![];
        for id in ids {
            if load(root, id)?.unbundled.is_some() {
                alone.push(id.clone());
            } else {
                together.push(id.clone());
            }
        }
        (alone, together)
    };
    let (alone, together) = if together.len() == 1 {
        ([alone, together].concat(), vec![])
    } else {
        (alone, together)
    };
    for id in &alone {
        crate::submit::submit(root, project, id).await?;
        out.alone.push(id.clone());
    }
    if together.is_empty() {
        return Ok(out);
    }
    // An unpinned bundle could go to an older worker that can't read it.
    let app_version = app_version.filter(|v| *v > 0).context(
        "a bundle needs --app-version-num, the usnm_ocr version whose worker reads bundles",
    )?;
    out.bundle = Some(submit_bundle(root, project, &together, app_version).await?);
    Ok(out)
}

async fn submit_bundle(
    root: &Path,
    project: &Path,
    ids: &[String],
    app_version: u32,
) -> anyhow::Result<String> {
    let _lock = Lock::take(root)?;
    let project = project.canonicalize()?;
    let mut records = Vec::with_capacity(ids.len());
    let mut jobs: Vec<Job> = Vec::with_capacity(ids.len());
    let mut scans = Vec::new();
    for id in ids {
        let record = load(root, id)?;
        ensure!(
            record.stage == Stage::Reserved && record.bundle.is_none(),
            "job {id} is not reserved; uncertain submissions require reconciliation"
        );
        ensure!(
            records
                .first()
                .is_none_or(|r: &crate::Record| r.plan == record.plan),
            "a bundle's pages belong to one plan"
        );
        let input = dir(root, id)?;
        let job: Job =
            serde_json::from_slice(&read(&input.join("request.json"), boinc::MAX_JSON)?)?;
        ensure!(job == record.job, "staged request changed");
        let scan = read(&input.join("page"), boinc::MAX_SCAN)?;
        ensure!(
            scan.len() as u64 == job.scan.bytes && boinc::sha256(&scan) == job.scan.sha256,
            "staged scan changed"
        );
        scans.extend(scan);
        jobs.push(job);
        records.push(record);
    }
    let bundle = Bundle::new(jobs)?;
    let bundle_json = serde_json::to_vec_pretty(&bundle)?;
    let held = bundle_dir(root, &bundle.id)?;
    atomic(&held.join(PAGES_FILE), &scans)?;
    atomic(&held.join(JOB_FILE), &bundle_json)?;
    for record in &mut records {
        record.stage = Stage::Submitting;
        record.bundle = Some(bundle.id.clone());
        save(root, record)?;
    }
    let result = async {
        for (logical, bytes) in [(PAGES_FILE, &scans), (JOB_FILE, &bundle_json)] {
            let physical = super::physical(&bundle.id, logical);
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
            atomic_download(&path, bytes)?;
        }
        let status = Command::new(project.join("bin/create_work"))
            .current_dir(&project)
            .args(create_work_args(&bundle, app_version))
            .status()
            .await?;
        ensure!(status.success(), "create_work failed");
        Ok::<_, anyhow::Error>(())
    }
    .await;
    match result {
        Ok(()) => {
            for record in &mut records {
                record.stage = Stage::Submitted;
                record.error = None;
                save(root, record)?;
            }
            Ok(bundle.id)
        }
        Err(e) => {
            for record in &mut records {
                record.error = Some(format!(
                    "bundle submission uncertain; inspect BOINC for workunit {} before reconciling",
                    bundle.workunit()
                ));
                save(root, record)?;
            }
            Err(e.context("bundle submission uncertain; automatic resubmission disabled"))
        }
    }
}

/// The BOINC workunit job `id` runs in, for the canonical result's host lookup: its bundle's,
/// or its own (also for a job with no server journal in `root`).
pub fn workunit(root: &Path, id: &str) -> anyhow::Result<String> {
    if !dir(root, id)?.join("record.json").exists() {
        return Ok(format!("usnm_{id}"));
    }
    Ok(load(root, id)?.workunit())
}

#[cfg(all(test, unix))]
pub(crate) mod tests {
    use super::*;
    use crate::bundle::work::tests::{job, scan};
    use crate::Record;
    use std::os::unix::fs::PermissionsExt;

    pub(crate) fn executable(path: &Path, body: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, body).unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// A server hand-off root with reserved pages `ns`, and a project whose stock tools log
    /// their arguments (`create_work` fails when `fail`).
    pub(crate) struct Server {
        pub _dir: tempfile::TempDir,
        pub root: PathBuf,
        pub project: PathBuf,
    }
    impl Server {
        pub(crate) fn new(ns: &[u8]) -> (Self, Vec<String>) {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().canonicalize().unwrap();
            let (root, project) = (path.join("handoff"), path.join("project"));
            std::fs::create_dir_all(project.join("download")).unwrap();
            executable(
                &project.join("bin/dir_hier_path"),
                "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n",
            );
            let mut ids = vec![];
            for n in ns {
                let job = job(*n);
                let d = root.join("jobs").join(&job.id);
                atomic(&d.join("page"), &scan(*n)).unwrap();
                atomic(&d.join("request.json"), &serde_json::to_vec(&job).unwrap()).unwrap();
                ids.push(job.id.clone());
                save(
                    &root,
                    &Record {
                        job,
                        plan: "test".into(),
                        stage: Stage::Reserved,
                        stamp: "test".into(),
                        error: None,
                        ledger: None,
                        bundle: None,
                        unbundled: None,
                    },
                )
                .unwrap();
            }
            let server = Self {
                _dir: dir,
                root,
                project,
            };
            server.tool(false);
            (server, ids)
        }
        pub(crate) fn tool(&self, fail: bool) {
            executable(
                &self.project.join("bin/create_work"),
                &format!(
                    "#!/bin/sh\nprintf '%s\\n' \"$*\" >> calls\n{}",
                    if fail { "exit 1\n" } else { "" }
                ),
            );
        }
        pub(crate) fn calls(&self) -> Vec<String> {
            std::fs::read_to_string(self.project.join("calls"))
                .unwrap_or_default()
                .lines()
                .map(str::to_owned)
                .collect()
        }
    }

    #[tokio::test]
    async fn one_workunit_carries_the_pages_and_each_record_names_it() {
        let (s, ids) = Server::new(&[1, 2, 3]);
        let out = submit(&s.root, &s.project, &ids, Some(106)).await.unwrap();
        let bundle_id = out.bundle.clone().unwrap();
        assert!(out.alone.is_empty());
        let calls = s.calls();
        assert_eq!(calls.len(), 1);
        assert!(calls[0].contains(&format!("--wu_name usnm_ocrb_{bundle_id}")));
        assert!(calls[0].contains("templates/usnm_ocr_bundle_in"));
        assert!(calls[0].contains("--app_version_num 106"));
        assert!(calls[0].contains("--delay_bound 259200 --priority 0"));
        let staged = std::fs::read(
            s.project
                .join("download")
                .join(format!("{bundle_id}_pages")),
        )
        .unwrap();
        assert_eq!(staged, [scan(1), scan(2), scan(3)].concat());
        let bundle: Bundle = serde_json::from_slice(
            &std::fs::read(
                s.project
                    .join("download")
                    .join(format!("{bundle_id}_ocr_bundle_json")),
            )
            .unwrap(),
        )
        .unwrap();
        bundle.validate().unwrap();
        assert_eq!(bundle.pages.len(), 3);
        for id in &ids {
            let r = load(&s.root, id).unwrap();
            assert_eq!(
                (r.stage, r.bundle.as_deref()),
                (Stage::Submitted, Some(bundle_id.as_str()))
            );
            assert_eq!(
                workunit(&s.root, id).unwrap(),
                format!("usnm_ocrb_{bundle_id}")
            );
        }
        let other = "e".repeat(64);
        assert_eq!(workunit(&s.root, &other).unwrap(), format!("usnm_{other}"));
        // A second call submits nothing: the pages are past reserved.
        assert!(submit(&s.root, &s.project, &ids, Some(106)).await.is_err());
        assert_eq!(s.calls().len(), 1);
    }

    #[tokio::test]
    async fn requeued_and_lone_pages_go_alone() {
        let (s, ids) = Server::new(&[1, 2, 3]);
        let mut r = load(&s.root, &ids[0]).unwrap();
        r.unbundled = Some("b".repeat(64));
        save(&s.root, &r).unwrap();
        // Without a pinned version nothing is bundled, and nothing is submitted.
        assert!(submit(&s.root, &s.project, &ids[1..], None).await.is_err());
        assert!(s.calls().is_empty());
        let out = submit(&s.root, &s.project, &ids, Some(106)).await.unwrap();
        assert_eq!(out.alone, vec![ids[0].clone()]);
        assert!(out.bundle.is_some());
        let calls = s.calls();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains(&format!("--wu_name usnm_{}", ids[0])));
        assert!(!calls[0].contains("--app_version_num"));
        assert!(calls[1].contains("--app_version_num 106"));
        // One page alone is a single-page unit.
        let (s, ids) = Server::new(&[4]);
        let out = submit(&s.root, &s.project, &ids, Some(106)).await.unwrap();
        assert_eq!((out.bundle, out.alone), (None, ids.clone()));
        assert!(s.calls()[0].contains("templates/usnm_ocr_in"));
    }

    #[tokio::test]
    async fn an_uncertain_bundle_is_reconciled_as_a_whole() {
        let (s, ids) = Server::new(&[1, 2]);
        s.tool(true);
        assert!(submit(&s.root, &s.project, &ids, Some(106)).await.is_err());
        for id in &ids {
            let r = load(&s.root, id).unwrap();
            assert_eq!(r.stage, Stage::Submitting);
            assert!(r.error.unwrap().contains("usnm_ocrb_"));
        }
        // Absent on the server: every member is reserved again, out of the bundle.
        crate::submit::reconcile(&s.root, &ids[0], false).unwrap();
        for id in &ids {
            let r = load(&s.root, id).unwrap();
            assert_eq!((r.stage, r.bundle), (Stage::Reserved, None));
        }
        s.tool(false);
        let out = submit(&s.root, &s.project, &ids, Some(106)).await.unwrap();
        // Present: every member is submitted.
        let (s2, ids2) = Server::new(&[3, 4]);
        s2.tool(true);
        assert!(submit(&s2.root, &s2.project, &ids2, Some(106))
            .await
            .is_err());
        // Interrupted half way through marking them submitted: present finishes the rest.
        let mut first = load(&s2.root, &ids2[0]).unwrap();
        first.stage = Stage::Submitted;
        save(&s2.root, &first).unwrap();
        crate::submit::reconcile(&s2.root, &ids2[1], true).unwrap();
        for id in &ids2 {
            assert_eq!(load(&s2.root, id).unwrap().stage, Stage::Submitted);
        }
        // Once all are submitted there is nothing left to reconcile.
        assert!(crate::submit::reconcile(&s2.root, &ids2[0], true).is_err());
        assert!(out.bundle.is_some());
    }

    #[test]
    fn work_is_scaled_to_the_bundle() {
        let bundle = Bundle::new(vec![job(1), job(2)]).unwrap();
        let args = create_work_args(&bundle, 106);
        let at = |flag: &str| args[args.iter().position(|a| a == flag).unwrap() + 1].clone();
        assert_eq!(at("--rsc_fpops_est"), "2e12");
        assert_eq!(at("--rsc_fpops_bound"), "2e14");
        assert_eq!(
            at("--rsc_disk_bound"),
            ((2u64 << 30) + 2 * bundle.bytes()).to_string()
        );
        assert_eq!(
            args.last().unwrap(),
            &format!("{}_ocr_bundle_json", bundle.id)
        );
    }
}
