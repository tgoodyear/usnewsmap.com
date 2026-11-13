//! Independent OCR spot checks: replay a sampled job on a second trusted host and compare.
use super::{canonical_host, Config};
use crate::{atomic, gold, load, read, trust::Trust, Lock};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::Path};
use tokio::process::Command;
use usnm_ocr_contract::boinc::{self, ResultManifest};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Spot {
    job: String,
    image: String,
    source_host: u64,
    target_host: u64,
    workunit: String,
    stage: String,
}

pub(super) fn sample(id: &str, every: u64) -> anyhow::Result<bool> {
    ensure!(boinc::is_sha256(id) && every > 0, "invalid sampling inputs");
    Ok(u64::from_str_radix(&id[..16], 16)? % every == 0)
}

#[cfg(test)]
fn plan(id: &str, image: &str, host: u64, trust: &Trust) -> anyhow::Result<Spot> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    ensure!(
        trust.refuse(image, Some(host)).is_none(),
        "source host is not trusted"
    );
    let target = trust
        .hosts
        .get(image)
        .into_iter()
        .flatten()
        .copied()
        .find(|h| *h != host && *h > 0)
        .context("a second trusted host for this exact image is required")?;
    Ok(Spot {
        job: id.into(),
        image: image.into(),
        source_host: host,
        target_host: target,
        workunit: format!("usnm_spot_{id}_{host}_{target}"),
        stage: "planned".into(),
    })
}

fn plan_job(id: &str, job: &boinc::Job, host: u64, trust: &Trust) -> anyhow::Result<Spot> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    ensure!(
        trust.refuse_job(job, Some(host)).is_none(),
        "source job is outside calibrated host trust"
    );
    let target = trust
        .trusted_hosts_for_job(job)
        .into_iter()
        .find(|h| *h != host && *h > 0)
        .context("a second trusted host for this exact image is required")?;
    Ok(Spot {
        job: id.into(),
        image: job.engine.image.clone(),
        source_host: host,
        target_host: target,
        workunit: format!("usnm_spot_{id}_{host}_{target}"),
        stage: "planned".into(),
    })
}

fn spot_args(s: &Spot) -> Vec<String> {
    [
        "--appname".into(),
        "usnm_ocr".into(),
        "--wu_name".into(),
        s.workunit.clone(),
        "--wu_template".into(),
        "templates/usnm_ocr_in".into(),
        "--result_template".into(),
        "templates/usnm_ocr_out".into(),
        "--min_quorum".into(),
        "1".into(),
        "--target_nresults".into(),
        "1".into(),
        "--max_error_results".into(),
        "1".into(),
        "--max_total_results".into(),
        "1".into(),
        "--max_success_results".into(),
        "1".into(),
        "--target_host".into(),
        s.target_host.to_string(),
        "--rsc_fpops_est".into(),
        "1e12".into(),
        "--rsc_fpops_bound".into(),
        "1e14".into(),
        "--rsc_memory_bound".into(),
        "1610612736".into(),
        "--rsc_disk_bound".into(),
        "1073741824".into(),
        "--delay_bound".into(),
        "259200".into(),
        format!("{}_page", s.workunit),
        format!("{}_request_json", s.workunit),
    ]
    .into()
}

/// Sample existing OCR jobs, replay their unchanged inputs on a different trusted host.
/// The independent workunit name cannot replace the original canonical result.
pub async fn spot_submit(c: &Config, jobs: &Path, execute: bool) -> anyhow::Result<Vec<String>> {
    c.validate()?;
    let ids: Vec<String> = serde_json::from_slice(&read(jobs, 1 << 20)?)?;
    ensure!(ids.len() <= 10000, "too many candidate jobs");
    let ids: BTreeSet<_> = ids.into_iter().collect();
    let trust = Trust::load(&c.trust)?;
    let _lock = if execute {
        Some(Lock::take(&c.root)?)
    } else {
        None
    };
    let mut made = Vec::new();
    for id in ids {
        if !sample(&id, c.sample_every)? {
            continue;
        }
        let record = load(&c.jobs_root, &id)?;
        ensure!(
            record.job.engine.image == c.engine.image,
            "candidate worker image differs"
        );
        let host = canonical_host(c, &record.job.workunit()).await?;
        ensure!(
            trust.refuse_job(&record.job, Some(host)).is_none(),
            "source job is outside calibrated host trust"
        );
        let mut spot = plan_job(&id, &record.job, host, &trust)?;
        let path = c.root.join("spots").join(&id).join("submission.json");
        if path.exists() {
            let old: Spot = serde_json::from_slice(&read(&path, 4096)?)?;
            spot.stage.clone_from(&old.stage);
            ensure!(old == spot, "spot-check identity or host changed");
            ensure!(
                old.stage == "submitted",
                "spot submission uncertain; reconcile server before any retry"
            );
            continue;
        }
        if execute {
            // Gold uses the same exact active-XML assignment opt-in check.
            ensure!(
                gold::submit::assignment_enabled(&std::fs::read_to_string(
                    c.project.join("config.xml")
                )?)?,
                "spot-check assignment must be explicitly enabled by the operator"
            );
            stage_inputs(c, &record.job, &spot.workunit).await?;
            spot.stage = "submitting".into();
            atomic(&path, &serde_json::to_vec_pretty(&spot)?)?;
            let status = Command::new(c.project.join("bin/create_work"))
                .current_dir(&c.project)
                .args(spot_args(&spot))
                .status()
                .await?;
            ensure!(
                status.success(),
                "spot submission uncertain; inspect deterministic workunit name"
            );
            spot.stage = "submitted".into();
            atomic(&path, &serde_json::to_vec_pretty(&spot)?)?;
        }
        made.push(spot.workunit);
        if made.len() == c.max_spots {
            break;
        }
    }
    Ok(made)
}

async fn stage_inputs(c: &Config, job: &boinc::Job, workunit: &str) -> anyhow::Result<()> {
    let (scan, request) = job_inputs(&c.jobs_root, job)?;
    stage(&c.project, workunit, scan, request).await
}

/// The scan and the request.json bytes `jobs_root` holds for `job`, checked against it.
pub(super) fn job_inputs(jobs_root: &Path, job: &boinc::Job) -> anyhow::Result<(Vec<u8>, Vec<u8>)> {
    let input = crate::dir(jobs_root, &job.id)?;
    let request = read(&input.join("request.json"), boinc::MAX_REQUEST)?;
    ensure!(
        serde_json::from_slice::<boinc::Job>(&request)? == *job,
        "staged job changed"
    );
    let scan = read(&input.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        scan.len() as u64 == job.scan.bytes && boinc::sha256(&scan) == job.scan.sha256,
        "staged scan changed"
    );
    Ok((scan, request))
}

/// Puts a spot unit's two inputs in the project's download tree under `<workunit>_page` and
/// `<workunit>_request_json`.
pub(super) async fn stage(
    project: &Path,
    workunit: &str,
    scan: Vec<u8>,
    request: Vec<u8>,
) -> anyhow::Result<()> {
    for (name, bytes) in [("page", scan), ("request_json", request)] {
        let physical = format!("{workunit}_{name}");
        let output = Command::new(project.join("bin/dir_hier_path"))
            .arg(&physical)
            .current_dir(project)
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
        crate::atomic_download(&path, &bytes)?;
    }
    Ok(())
}

/// A spot-check member's result: its build (the `engine.image` its manifest names), its text as
/// `import` stores it, and the sha256 of the text and ALTO bytes it returned.
#[derive(Debug, Clone, PartialEq)]
pub(super) struct Returned {
    pub image: String,
    pub text: String,
    pub text_sha256: String,
    pub alto_sha256: String,
}

pub(super) fn result(job: &boinc::Job, spool: &Path, workunit: &str) -> anyhow::Result<Returned> {
    ensure!(
        !spool.join(format!("{workunit}_error")).exists()
            && !spool.join(format!("{workunit}_no_output_files")).exists(),
        "spot-check member failed"
    );
    judge(job, fetch(spool, workunit)?)
}

/// A result's three returned files, as read.
pub(super) struct Files {
    result: Vec<u8>,
    text: Vec<u8>,
    alto: Vec<u8>,
}

/// Reads a result's files. An error here is the filesystem's (missing, unreadable), not the
/// host's: the bytes themselves are judged by [`judge`].
pub(super) fn fetch(spool: &Path, workunit: &str) -> std::io::Result<Files> {
    let file = |name: &str| -> std::io::Result<Vec<u8>> {
        let path = spool.join(format!("{workunit}__file_{name}"));
        if std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                format!("{} is a symlink", path.display()),
            ));
        }
        std::fs::read(path)
    };
    Ok(Files {
        result: file("result.json")?,
        text: file("text.txt")?,
        alto: file("layout.xml")?,
    })
}

/// Whether `files` are a valid result of `job`, and what it returned.
pub(super) fn judge(job: &boinc::Job, files: Files) -> anyhow::Result<Returned> {
    let Files { result, text, alto } = files;
    ensure!(
        result.len() <= boinc::MAX_JSON
            && text.len() <= boinc::MAX_TEXT
            && alto.len() <= boinc::MAX_ALTO,
        "result over size limit"
    );
    let manifest: ResultManifest = serde_json::from_slice(&result)?;
    manifest.matches(job, &text, &alto)?;
    usnm_ocr_local::checks::check_alto(&alto).map_err(|r| anyhow::anyhow!("{}", r.code()))?;
    Ok(Returned {
        image: manifest.engine.image,
        text: crate::normalized_text(job, &text)?,
        text_sha256: boinc::sha256(&text),
        alto_sha256: boinc::sha256(&alto),
    })
}

/// Whether two results agree. They are compared only between results of one build, or of two
/// builds `builds` lists as equivalent (#1418): output from builds not shown to be identical is
/// never compared byte for byte, so this fails. A container spot check compares the text as
/// `import` stores it; a native one (`exact`, #1863) the returned text and ALTO bytes.
pub(super) fn agree(
    builds: &crate::trust::BuildTrust,
    a: &Returned,
    b: &Returned,
    exact: bool,
) -> anyhow::Result<bool> {
    ensure!(
        builds.comparable(&a.image, &b.image),
        "results from builds {} and {} are not comparable byte for byte",
        a.image,
        b.image
    );
    Ok(if exact {
        a.text_sha256 == b.text_sha256 && a.alto_sha256 == b.alto_sha256
    } else {
        a.text == b.text
    })
}

/// A conservative two-host quorum: both canonical hosts must still be trusted and produce
/// valid results for the identical job with exactly equal normalized text. Never publish.
pub async fn spot_compare(
    c: &Config,
    id: &str,
    original_spool: &Path,
    spot_spool: &Path,
) -> anyhow::Result<bool> {
    ensure!(boinc::is_sha256(id), "invalid job id");
    ensure!(c.root.is_absolute(), "controller root must be absolute");
    let _lock = Lock::take(&c.root)?;
    let path = c.root.join("spots").join(id);
    atomic(
        &path.join("comparison.json"),
        br#"{"status":"unverified","agrees":false}"#,
    )?;
    c.validate()?;
    let spot: Spot = serde_json::from_slice(&read(&path.join("submission.json"), 4096)?)?;
    let record = load(&c.jobs_root, id)?;
    ensure!(
        spot.job == id
            && spot.image == record.job.engine.image
            && spot.image == c.engine.image
            && spot.stage == "submitted",
        "invalid spot journal"
    );
    let trust = Trust::load(&c.trust)?;
    let source = canonical_host(c, &record.job.workunit()).await?;
    let target = canonical_host(c, &spot.workunit).await?;
    ensure!(
        source == spot.source_host && target == spot.target_host && source != target,
        "canonical hosts do not match independent assignment"
    );
    ensure!(
        trust.refuse_job(&record.job, Some(source)).is_none()
            && trust.refuse_job(&record.job, Some(target)).is_none(),
        "spot-check job scope or host trust revoked"
    );
    let a = result(&record.job, original_spool, &record.job.workunit())?;
    let b = result(&record.job, spot_spool, &spot.workunit)?;
    let agrees = agree(crate::trust::BuildTrust::embedded()?, &a, &b, false)?;
    atomic(
        &path.join("comparison.json"),
        &serde_json::to_vec_pretty(&serde_json::json!({
            "job": id, "image": spot.image, "hosts": [source, target], "quorum": 2,
            "agrees": agrees, "comparison": "exact-normalized-text",
            "status": "verified",
            "at": chrono::Utc::now().to_rfc3339(),
            "reason": if agrees { "independent-host-agreement" } else { "text-disagreement" },
        }))?,
    )?;
    Ok(agrees)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    fn trust() -> Trust {
        Trust {
            hosts: [(format!("sha256:{}", "a".repeat(64)), [1, 2].into())].into(),
            manifest_sha256: Some("b".repeat(64)),
            scope: Some(crate::trust::Scope {
                language: "eng".into(),
                profile: "eng".into(),
                psm: 3,
                dpi: 300,
            }),
            max_cer: 0.1,
            policies: Vec::new(),
            holds: Default::default(),
            v: crate::trust::TRUST_VERSION,
        }
    }

    #[test]
    fn results_compare_only_within_a_digest_or_a_listed_pair() {
        let builds = crate::trust::BuildTrust::embedded().unwrap();
        let linux = "sha256:4d6e2a3be41b3b9c9cf50e6404c6f056177cef547e9d9675298ee1fec94cc5d1";
        let worker = "sha256:dac9c123a52d7b10281d3204a815233da1fe9819afdadd4341a034a95441eb34";
        let macos = "sha256:78065a0a43d8f8bf67c00c014ae2b0540d51d0038e4050dbbd509e14b63fa836";
        let windows = format!("sha256:{}", "7".repeat(64));
        let r = |image: &str, text: &str| Returned {
            image: image.to_owned(),
            text: text.to_owned(),
            text_sha256: boinc::sha256(text.as_bytes()),
            alto_sha256: "a".repeat(64),
        };
        assert!(agree(builds, &r(macos, "a"), &r(macos, "a"), false).unwrap());
        assert!(!agree(builds, &r(macos, "a"), &r(macos, "b"), false).unwrap());
        // Linux and the worker image it was shown identical to, as today.
        assert!(agree(builds, &r(linux, "a"), &r(worker, "a"), false).unwrap());
        assert!(!agree(builds, &r(worker, "a"), &r(linux, "b"), false).unwrap());
        // No pair across platforms is listed, whatever the text.
        for (a, b) in [(macos, linux), (macos, worker), (windows.as_str(), linux)] {
            assert!(
                agree(builds, &r(a, "a"), &r(b, "a"), true).is_err(),
                "{a} {b}"
            );
        }
        // Exact: the same normalized text with other bytes, or other ALTO, differs.
        let mut other = r(macos, "a");
        other.text_sha256 = "b".repeat(64);
        assert!(agree(builds, &r(macos, "a"), &other, false).unwrap());
        assert!(!agree(builds, &r(macos, "a"), &other, true).unwrap());
        let mut alto = r(macos, "a");
        alto.alto_sha256 = "c".repeat(64);
        assert!(!agree(builds, &r(macos, "a"), &alto, true).unwrap());
    }

    #[test]
    fn spot_requires_distinct_trusted_hosts_and_preserves_original_input_identity() {
        let t = trust();
        let image = format!("sha256:{}", "a".repeat(64));
        let id = "b".repeat(64);
        let s = plan(&id, &image, 1, &t).unwrap();
        assert_eq!(s.target_host, 2);
        assert_ne!(s.workunit, format!("usnm_{id}"));
        let args = spot_args(&s);
        assert!(args.windows(2).any(|a| a == ["--target_host", "2"]));
        assert_eq!(
            &args[args.len() - 2..],
            [
                format!("{}_page", s.workunit),
                format!("{}_request_json", s.workunit)
            ]
        );
        assert!(plan(&id, &image, 3, &t).is_err());
        assert!(plan(&id, &format!("sha256:{}", "c".repeat(64)), 1, &t).is_err());
        let mut t = t;
        t.hosts.get_mut(&image).unwrap().remove(&2);
        assert!(plan(&id, &image, 1, &t).is_err());

        // Scoped coexistence: spot-check planning for deu/frak2021 selects only a host trusted in
        // that scope, never a host trusted only for eng/eng.
        let eng = trust();
        let deu = Trust {
            hosts: [(image.clone(), [2, 5].into())].into(),
            manifest_sha256: Some("c".repeat(64)),
            scope: Some(crate::trust::Scope {
                language: "deu".into(),
                profile: "frak2021".into(),
                psm: 3,
                dpi: 300,
            }),
            max_cer: 0.08,
            policies: Vec::new(),
            holds: Default::default(),
            v: crate::trust::TRUST_VERSION,
        };
        let coexisting = eng.merge(&deu).unwrap();
        let deu_job = boinc::Job {
            retry_of: None,
            lines: None,
            attempt: 0,
            v: 1,
            id: id.clone(),
            message: usnm_ocr_contract::message::OcrMessage {
                v: 1,
                page: "sn83030214/1900-01-01/ed-1/seq-1".into(),
                batch: "batch_test_ver01".into(),
                language: "deu".into(),
                reason: usnm_ocr_contract::message::Reason::Bulk,
                score: Some(0.5),
                queued_at: "2026-10-18T00:00:00Z".into(),
            },
            batch_version: 1,
            scan: boinc::Scan {
                url: "https://tile.loc.gov/image-services/iiif/service:ndnp:test/full/full/0/default.jpg".into(),
                iiif_base: "https://tile.loc.gov/image-services/iiif/service:ndnp:test".into(),
                bytes: 100,
                sha256: "d".repeat(64),
                md5: None,
            },
            engine: boinc::Engine {
                kind: usnm_ocr_contract::routing::EngineKind::Tesseract,
                image: image.clone(),
                version: "tesseract 5.5.0".into(),
                profile: "frak2021".into(),
                models: "sha256:001122334455".into(),
                psm: 3,
                dpi: 300,
                timeout_secs: 600,
            },
        };
        assert!(plan_job(&id, &deu_job, 1, &coexisting).is_err());
        let deu_spot = plan_job(&id, &deu_job, 2, &coexisting).unwrap();
        assert_eq!(deu_spot.target_host, 5);
    }

    /// A job as the bridge makes it for a page in `language` read with `profile`.
    pub(in crate::host_trust) fn job(language: &str, profile: &str) -> boinc::Job {
        let mut job = boinc::Job {
            retry_of: None,
            lines: None,
            attempt: 0,
            v: 1,
            id: String::new(),
            message: usnm_ocr_contract::message::OcrMessage {
                v: 1,
                page: "sn83030214/1900-01-01/ed-1/seq-1".into(),
                batch: "nbu_gage".into(),
                language: language.into(),
                reason: usnm_ocr_contract::message::Reason::Bulk,
                score: Some(0.5),
                queued_at: "2026-10-18T00:00:00Z".into(),
            },
            batch_version: 1,
            scan: boinc::Scan {
                url: "https://tile.loc.gov/image-services/iiif/service:ndnp:test/full/full/0/default.jpg".into(),
                iiif_base: "https://tile.loc.gov/image-services/iiif/service:ndnp:test".into(),
                bytes: 100,
                sha256: "d".repeat(64),
                md5: None,
            },
            engine: boinc::Engine {
                kind: usnm_ocr_contract::routing::EngineKind::Tesseract,
                image: format!("sha256:{}", "a".repeat(64)),
                version: "5.5.0".into(),
                profile: profile.into(),
                models: "sha256:001122334455".into(),
                psm: 3,
                dpi: 300,
                timeout_secs: 600,
            },
        };
        job.id = job.identity().unwrap();
        job
    }

    /// Writes a spot-check member's returned files for `job` into `spool`.
    pub(in crate::host_trust) fn returned(
        job: &boinc::Job,
        spool: &Path,
        workunit: &str,
        text: &str,
    ) {
        const ALTO: &str = r#"<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><sourceImageInformation><fileName>page</fileName></sourceImageInformation></Description><Layout><Page WIDTH="1000" HEIGHT="1000"><PrintSpace><TextBlock><TextLine><String CONTENT="HELLO"/></TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#;
        let manifest = ResultManifest {
            v: 1,
            job_id: job.id.clone(),
            scan_sha256: job.scan.sha256.clone(),
            engine: job.engine.clone(),
            text_sha256: boinc::sha256(text.as_bytes()),
            alto_sha256: boinc::sha256(ALTO.as_bytes()),
            elapsed_ms: 100,
            completed_at: "2026-10-18T00:00:00Z".into(),
        };
        let file = |name: &str| spool.join(format!("{workunit}__file_{name}"));
        std::fs::write(file("result.json"), serde_json::to_vec(&manifest).unwrap()).unwrap();
        std::fs::write(file("text.txt"), text).unwrap();
        std::fs::write(file("layout.xml"), ALTO).unwrap();
    }

    /// The two-host quorum compares `result`s for equality, and `import`
    /// stores the same text (`crate::normalized_text`), so for a German job
    /// two hosts that end a line differently still agree (#1065).
    #[test]
    fn a_spot_result_is_normalized_as_import_stores_it() {
        let tmp = tempfile::tempdir().unwrap();
        let spool = tmp.path();
        let read = |job: &boinc::Job, text: &str| {
            returned(job, spool, "wu", text);
            let got = result(job, spool, "wu").unwrap();
            assert_eq!(got.image, job.engine.image);
            assert_eq!(got.text_sha256, boinc::sha256(text.as_bytes()));
            assert_eq!(
                got.text,
                crate::normalized_text(job, text.as_bytes()).unwrap()
            );
            got.text
        };
        // One host's engine printed an em dash at the line end, another's a
        // `⸗`, a third's a hyphen: the same word.
        let german = job("deu", "frak2021");
        let dash = read(&german, "durch den engen Haus\u{2014}\ngang schritt");
        let double = read(&german, "durch den engen Haus\u{2E17}\ngang schritt");
        let hyphen = read(&german, "durch den engen Haus-\ngang schritt");
        assert_eq!(dash, "durch den engen Hausgang schritt");
        assert_eq!((&double, &hyphen), (&dash, &dash));
        // An English job keeps a dash at a line end, so those hosts disagree.
        let english = job("eng", "eng");
        let dash = read(&english, "durch den engen Haus\u{2014}\ngang schritt");
        let hyphen = read(&english, "durch den engen Haus-\ngang schritt");
        assert_eq!(dash, "durch den engen Haus\u{2014}\ngang schritt");
        assert_ne!(dash, hyphen);
        assert_eq!(
            read(&english, "durch den engen Haus\u{2E17}\ngang schritt"),
            hyphen
        );
    }
}
