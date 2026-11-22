//! Hand staged units to stock `create_work`, two results per unit, with the
//! assessment's write-ahead markers (`submitting`, then `submitted`).
//!
//! `places.txt` is the same file for every unit of a campaign: its physical
//! name is its checksum, so the download tree holds it once and a host
//! downloads it once (the input template marks it sticky). The quality
//! measure's resource file (`qe.bin`), in units whose job names one, is
//! shared the same way by every unit of its language
//! (`usnm_quality_qe_in`).
use super::{units, APP, JOB_FILE, PLACES_FILE, QE_FILE};
use crate::{atomic, read};
use anyhow::ensure;
use std::path::PathBuf;
use tokio::process::Command;
use usnm_ocr_contract::quality::{
    Job, MAX_ALTO, MAX_BUNDLE_ALTO, MAX_BUNDLE_SCAN, MAX_BUNDLE_TEXTS, MAX_JOB_JSON, MAX_PLACES,
    MAX_QE, MAX_SCAN, MAX_TEXTS,
};

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub project: PathBuf,
    pub max_units: usize,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub submitted: usize,
    pub uncertain: usize,
}

/// One page: a few alignments of some thousand words and one pass over a
/// downscaled image, well under a minute on one core. UNVERIFIED on
/// volunteers' hosts until the first units return.
pub const FPOPS_EST: &str = "1e12";
pub const FPOPS_BOUND: &str = "1e14";
/// 1.5 GiB, the `docker` plan class's container (`boinc/apps/job.toml`).
pub const MEMORY_BOUND: &str = "1610612736";

pub fn create_work_args(job: &Job) -> Vec<String> {
    let n_pages = job.pages.len().max(1);
    let fpops_est = if n_pages == 1 {
        FPOPS_EST.to_owned()
    } else {
        format!("{n_pages}e12")
    };
    let fpops_bound = if n_pages == 1 {
        FPOPS_BOUND.to_owned()
    } else {
        format!("{n_pages}e14")
    };
    let disk_bound = if job.pages.is_empty() {
        "1073741824"
    } else {
        "2147483648"
    };
    let mut args: Vec<String> = [
        "--appname",
        APP,
        "--wu_name",
        &job.workunit(),
        "--wu_template",
        if job.qe.is_some() {
            "templates/usnm_quality_qe_in"
        } else {
            "templates/usnm_quality_in"
        },
        "--result_template",
        "templates/usnm_quality_out",
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
        &fpops_est,
        "--rsc_fpops_bound",
        &fpops_bound,
        "--rsc_memory_bound",
        MEMORY_BOUND,
        "--rsc_disk_bound",
        disk_bound,
        "--delay_bound",
        "259200",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    args.extend(physical_names(job).into_iter().map(|(p, _)| p));
    args
}

/// Physical names in the download tree, in input template order, with the
/// unit file each comes from. The places file is named by its checksum,
/// shared across the campaign.
pub fn physical_names(job: &Job) -> Vec<(String, &'static str)> {
    let mut names = vec![
        (format!("{}_page_jpg", job.id), super::PAGE_FILE),
        (format!("{}_loc_xml", job.id), super::ALTO_FILE),
        (format!("{}_texts_json", job.id), super::TEXTS_FILE),
        (
            format!("usnm_quality_places_v2_{}", job.inputs.places.sha256),
            PLACES_FILE,
        ),
    ];
    if let Some(qe) = &job.qe {
        names.push((format!("usnm_quality_qe_v1_{}", qe.input.sha256), QE_FILE));
    }
    names.push((format!("{}_quality_json", job.id), JOB_FILE));
    names
}

/// The largest a unit's file of this name may be.
pub(crate) fn limit(file: &str) -> usize {
    match file {
        super::PAGE_FILE => MAX_BUNDLE_SCAN.max(MAX_SCAN),
        super::ALTO_FILE => MAX_BUNDLE_ALTO.max(MAX_ALTO),
        super::TEXTS_FILE => MAX_BUNDLE_TEXTS.max(MAX_TEXTS),
        PLACES_FILE => MAX_PLACES,
        QE_FILE => MAX_QE,
        _ => MAX_JOB_JSON,
    }
}

/// Size, SHA-256 and MD5 (lowercase hex) of a regular file in one chunked pass, never holding
/// more than one 1 MiB buffer.
/// The file must be at most `max` bytes: its size is checked first, and the reader is capped at
/// one byte over so a file that grows while it is read still fails.
pub(crate) fn hash_file(
    path: &std::path::Path,
    max: usize,
) -> anyhow::Result<(u64, String, String)> {
    use std::io::Read;
    let file = crate::open_regular(path)?;
    ensure!(file.metadata()?.len() <= max as u64, "file over size limit");
    let hashed = hash_reader(&mut file.take(max as u64 + 1))?;
    ensure!(hashed.0 <= max as u64, "file over size limit");
    Ok(hashed)
}

pub(crate) fn hash_reader(r: &mut impl std::io::Read) -> anyhow::Result<(u64, String, String)> {
    use md5::Md5;
    use sha2::{Digest, Sha256};
    let (mut sha, mut md5) = (Sha256::new(), Md5::new());
    let mut buf = vec![0u8; 1 << 20];
    let mut len = 0u64;
    loop {
        let n = r.read(&mut buf)?;
        if n == 0 {
            break;
        }
        sha.update(&buf[..n]);
        md5.update(&buf[..n]);
        len += n as u64;
    }
    let hex = |d: &[u8]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
    Ok((len, hex(&sha.finalize()), hex(&md5.finalize())))
}

/// The unit's job record, validated, with every staged input equal to what the record names.
/// What a submission (on-prem or cloud) checks before it sends anything.
pub(crate) fn load_checked(unit: &std::path::Path) -> anyhow::Result<Job> {
    Ok(check_unit(unit, usize::MAX)?.0)
}

/// [`load_checked`], but a file over `stream_over` bytes is checked in one chunked pass instead
/// of being read into memory; its MD5 comes back from that same pass, by unit file name.
pub(crate) fn check_unit(
    unit: &std::path::Path,
    stream_over: usize,
) -> anyhow::Result<(Job, std::collections::BTreeMap<&'static str, String>)> {
    let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)?;
    job.validate()?;
    super::page_dims::require_image(unit, "quality submit")?;
    let mut streamed = std::collections::BTreeMap::new();
    let mut inputs = vec![
        (super::PAGE_FILE, &job.inputs.page),
        (super::ALTO_FILE, &job.inputs.alto),
        (super::TEXTS_FILE, &job.inputs.texts),
        (PLACES_FILE, &job.inputs.places),
    ];
    if let Some(qe) = &job.qe {
        inputs.push((QE_FILE, &qe.input));
    }
    for (file, input) in inputs {
        let path = unit.join(file);
        let big = std::fs::metadata(&path).is_ok_and(|m| m.len() > stream_over as u64);
        let matches = if big {
            let (len, sha, md5) = hash_file(&path, limit(file))?;
            streamed.insert(file, md5);
            len == input.bytes && sha == input.sha256
        } else {
            input.matches(&read(&path, limit(file))?)
        };
        ensure!(matches, "staged {file} changed for {}", job.doc_id);
    }
    Ok((job, streamed))
}

pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    run_selected(opts, None).await
}

pub async fn run_selected(
    opts: &Options,
    members: Option<&std::path::Path>,
) -> anyhow::Result<Summary> {
    let project = opts.project.canonicalize()?;
    let dir = units(&opts.root);
    let names = crate::selection::units(&dir, JOB_FILE, members)?;
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
        let job = load_checked(&unit)?;
        atomic(&unit.join("submitting"), job.workunit().as_bytes())?;
        for (physical, file) in physical_names(&job) {
            crate::assess::submit::stage_file(
                &project,
                &physical,
                &read(&unit.join(file), limit(file))?,
            )
            .await?;
        }
        let status = Command::new(project.join("bin/create_work"))
            .current_dir(&project)
            .args(create_work_args(&job))
            .status()
            .await?;
        ensure!(
            status.success(),
            "create_work failed for {}; {} is uncertain until checked",
            job.workunit(),
            unit.display()
        );
        std::fs::rename(unit.join("submitting"), unit.join("submitted"))?;
        std::fs::File::open(&unit)?.sync_all()?;
        summary.submitted += 1;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[tokio::test]
    async fn bounded_submission_keeps_unrelated_quality_jobs_pending() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("quality");
        let a = "sn1_1900-01-01_ed-1_seq-1";
        let b = "sn1_1900-01-01_ed-1_seq-2";
        for doc in [a, b] {
            let unit = units(&root).join(doc);
            let mut job = crate::quality::work::tests::unit(&unit);
            job.doc_id = doc.into();
            job.id = job.identity().unwrap();
            atomic(&unit.join(JOB_FILE), &serde_json::to_vec(&job).unwrap()).unwrap();
        }
        let project = tmp.path().join("project");
        crate::bundle::submit::tests::executable(
            &project.join("bin/dir_hier_path"),
            "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n",
        );
        crate::bundle::submit::tests::executable(
            &project.join("bin/create_work"),
            "#!/bin/sh\nexit 0\n",
        );
        let members = tmp.path().join("members.json");
        atomic(&members, &serde_json::to_vec(&[b]).unwrap()).unwrap();
        let opts = Options {
            root: root.clone(),
            project,
            max_units: 1,
        };
        assert_eq!(
            run_selected(&opts, Some(&members)).await.unwrap().submitted,
            1
        );
        assert_eq!(
            run_selected(&opts, Some(&members)).await.unwrap().submitted,
            0
        );
        assert!(units(&root).join(b).join("submitted").is_file());
        assert!(!units(&root).join(a).join("submitted").exists());
    }

    #[test]
    fn five_inputs_in_template_order_with_a_shared_places_file() {
        let tmp = tempfile::tempdir().unwrap();
        let j = crate::quality::work::tests::unit(tmp.path());
        let names = physical_names(&j);
        let files: Vec<&str> = names.iter().map(|(_, f)| *f).collect();
        assert_eq!(files, super::super::inputs());
        assert!(names[3].0.starts_with("usnm_quality_places_"));
        assert!(!names[3].0.contains(&j.id));
        // Old workunits can still delete the original shared name.
        assert!(names[3].0.starts_with("usnm_quality_places_v2_"));
        let args = create_work_args(&j);
        assert!(args.contains(&"templates/usnm_quality_in".to_string()));
        assert!(args.ends_with(&[names[3].0.clone(), names[4].0.clone()]));
    }

    #[test]
    fn a_job_with_the_quality_measure_ships_its_shared_resource_file() {
        let tmp = tempfile::tempdir().unwrap();
        let mut j = crate::quality::work::tests::unit(tmp.path());
        j.qe = Some(usnm_ocr_contract::quality::QeInput {
            input: usnm_ocr_contract::quality::Input::of(b"qe", "lake:qe-resources/qe-v1/eng.qe"),
            language: "eng".into(),
        });
        j.id = j.identity().unwrap();
        let names = physical_names(&j);
        let files: Vec<&str> = names.iter().map(|(_, f)| *f).collect();
        assert_eq!(files, super::super::inputs_with_qe());
        assert_eq!(
            names[4].0,
            format!("usnm_quality_qe_v1_{}", j.qe.as_ref().unwrap().input.sha256)
        );
        assert!(!names[4].0.contains(&j.id));
        let args = create_work_args(&j);
        assert!(args.contains(&"templates/usnm_quality_qe_in".to_string()));
        assert!(args.ends_with(&[names[4].0.clone(), names[5].0.clone()]));
    }
}
