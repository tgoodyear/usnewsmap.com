//! Hand staged units to stock `create_work`, two results per unit, with the
//! assessment's write-ahead markers (`submitting`, then `submitted`).
use super::{units, APP, JOB_FILE, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::path::PathBuf;
use tokio::process::Command;
use usnm_ocr_contract::surya::{Job, MAX_JSON, MAX_SCAN};

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub project: PathBuf,
    pub max_units: usize,
    /// Single-replica, exact-host probe only; ordinary campaigns remain quorum two.
    pub target_host: Option<u64>,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub submitted: usize,
    pub uncertain: usize,
}

/// Round 1 (#370) took a median 18,753 CPU-seconds per tiled page. At a few
/// GFLOPS per core that is about 1e14 operations; the bound leaves room for
/// slow hosts, and each mode has its own deadline inside the unit anyway.
pub const FPOPS_EST: &str = "1e14";
pub const FPOPS_BOUND: &str = "2e16";
/// 12 GiB, rounded up from round 1's 11.8 GB peak with 8 threads. A workunit
/// can be sent to either version, so this must admit the CPU version's
/// measured peak even though the GPU container itself is capped at 8 GiB.
pub const MEMORY_BOUND: &str = "12884901888";

pub fn create_work_args(job: &Job) -> Vec<String> {
    work_args(job, None)
}

fn work_args(job: &Job, target_host: Option<u64>) -> Vec<String> {
    let mut args: Vec<String> = [
        "--appname",
        APP,
        "--wu_name",
        &job.workunit(),
        "--wu_template",
        "templates/usnm_surya_in",
        "--result_template",
        "templates/usnm_surya_out",
        "--min_quorum",
        "2",
        "--target_nresults",
        "2",
        "--max_error_results",
        "3",
        "--max_total_results",
        "4",
        "--max_success_results",
        "3",
        "--rsc_fpops_est",
        FPOPS_EST,
        "--rsc_fpops_bound",
        FPOPS_BOUND,
        "--rsc_memory_bound",
        MEMORY_BOUND,
        "--rsc_disk_bound",
        "2147483648",
        "--delay_bound",
        "259200",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    if let Some(host) = target_host {
        for (flag, value) in [
            ("--min_quorum", "1"),
            ("--target_nresults", "1"),
            ("--max_error_results", "1"),
            ("--max_total_results", "1"),
            ("--max_success_results", "1"),
        ] {
            let index = args.iter().position(|arg| arg == flag).unwrap();
            args[index + 1] = value.into();
        }
        let index = args.iter().position(|arg| arg == "--wu_name").unwrap();
        args[index + 1] = format!("{}_host{host}", job.workunit());
        args.extend(["--target_host".into(), host.to_string()]);
    }
    args.extend(physical_names(job).into_iter().map(|(p, _)| p));
    args
}

/// Physical names in the download tree, in input template order, with the
/// unit file each comes from.
pub fn physical_names(job: &Job) -> Vec<(String, &'static str)> {
    vec![
        (format!("{}_page_jpg", job.id), PAGE_FILE),
        (format!("{}_surya_json", job.id), JOB_FILE),
    ]
}

pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    let project = opts.project.canonicalize()?;
    if let Some(host) = opts.target_host {
        ensure!(host > 0, "target host must be positive");
        ensure!(
            (1..=30).contains(&opts.max_units),
            "targeted probe is limited to 30 units"
        );
        let config = std::fs::read_to_string(project.join("config.xml"))?;
        ensure!(
            crate::gold::submit::assignment_enabled(&config)?,
            "exact-host probe requires enable_assignment=1"
        );
    }
    super::stage::verify_benchmark(&opts.root)?;
    let dir = units(&opts.root);
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .with_context(|| format!("reading {dir:?}"))?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().join(JOB_FILE).exists())
        .map(|e| e.path())
        .collect();
    names.sort();
    // This lock serializes the whole campaign, not just this invocation's result count.
    let lock = opts.root.join(".submit-lock");
    std::fs::create_dir(&lock).context("campaign is locked; inspect before recovery")?;
    let outcome = submit_units(opts, &project, names).await;
    std::fs::remove_dir(lock)?;
    outcome
}

async fn submit_units(
    opts: &Options,
    project: &std::path::Path,
    names: Vec<PathBuf>,
) -> anyhow::Result<Summary> {
    if opts.target_host.is_some() {
        ensure!(
            names.len() <= 30,
            "targeted probe campaign exceeds 30 units"
        );
        let policy = serde_json::to_vec(&opts.target_host)?;
        let path = opts.root.join("target-host.json");
        if path.exists() {
            ensure!(std::fs::read(&path)? == policy, "campaign target changed");
        } else {
            ensure!(
                !names
                    .iter()
                    .any(|u| u.join("submitted").exists() || u.join("submitting").exists()),
                "cannot retarget an existing campaign"
            );
            atomic(&path, &policy)?;
        }
    } else {
        ensure!(
            !opts.root.join("target-host.json").exists(),
            "targeted campaign cannot become untargeted"
        );
    }
    let mut summary = Summary::default();
    for unit in names {
        if summary.submitted >= opts.max_units {
            break;
        }
        if unit.join("submitted").exists() {
            continue;
        }
        if unit.join("submitting").exists() {
            summary.uncertain += 1;
            continue;
        }
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JSON)?)?;
        job.validate()?;
        let page = read(&unit.join(PAGE_FILE), MAX_SCAN)?;
        ensure!(
            page.len() as u64 == job.input.bytes
                && usnm_ocr_contract::boinc::sha256(&page) == job.input.sha256,
            "staged page changed for {}",
            job.doc_id
        );
        let work_name = opts
            .target_host
            .map_or_else(|| job.workunit(), |h| format!("{}_host{h}", job.workunit()));
        atomic(&unit.join("submitting"), work_name.as_bytes())?;
        for (physical, file) in physical_names(&job) {
            let limit = if file == JOB_FILE { MAX_JSON } else { MAX_SCAN };
            crate::assess::submit::stage_file(&project, &physical, &read(&unit.join(file), limit)?)
                .await?;
        }
        let status = Command::new(project.join("bin/create_work"))
            .current_dir(&project)
            .args(work_args(&job, opts.target_host))
            .status()
            .await?;
        ensure!(
            status.success(),
            "create_work failed for {}; {} is uncertain until checked",
            job.workunit(),
            unit.display()
        );
        std::fs::rename(unit.join("submitting"), unit.join("submitted"))?;
        summary.submitted += 1;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn exact_host_probe_has_one_result_and_no_fallback() {
        let job = super::super::work::tests::job();
        let args = work_args(&job, Some(10));
        for flag in ["--min_quorum", "--target_nresults", "--max_total_results"] {
            let index = args.iter().position(|a| a == flag).unwrap();
            assert_eq!(args[index + 1], "1");
        }
        let index = args.iter().position(|a| a == "--target_host").unwrap();
        assert_eq!(args[index + 1], "10");
        assert!(!create_work_args(&job).contains(&"--target_host".into()));
    }

    #[test]
    fn two_inputs_in_template_order() {
        let j = super::super::work::tests::job();
        let names = physical_names(&j);
        assert_eq!(names[0].1, PAGE_FILE);
        assert_eq!(names[1].1, JOB_FILE);
        let args = create_work_args(&j);
        assert!(args.contains(&"templates/usnm_surya_in".to_string()));
        assert_eq!(
            args[args.iter().position(|a| a == "--rsc_memory_bound").unwrap() + 1],
            MEMORY_BOUND
        );
        assert!(args.ends_with(&[names[0].0.clone(), names[1].0.clone()]));
    }
}
