//! The pages of `usnm_qwen` bundle units, each with what its pair records
//! say (#529): `accepted` when two hosts agreed on the page, `disagreed`
//! when every pair so far differed, `pending` with no pair yet, and
//! `membership_invalid` when the bundle's `membership.json` is missing or
//! does not match its job, so none of its pages can be traced to the page
//! job they came from. Accepted means two hosts' readings agree, not that
//! they are right. A unit's BOINC state is not read or changed: a page can
//! be accepted while its unit stays inconclusive.
use super::{
    stage::{Membership, MEMBERSHIP_FILE},
    units,
    validate::{PAGE_AGREED, PAGE_DISAGREED},
    JOB_FILE,
};
use crate::read;
use anyhow::Context;
use std::path::{Path, PathBuf};
use usnm_ocr_contract::qwen::{Job, MAX_JOB_JSON};

const MAX_MEMBERSHIP: usize = 256 * 1024;
const MAX_RECORD: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct PageStatus {
    pub workunit: String,
    pub job_id: String,
    pub doc_id: String,
    /// The page unit the page came from, by job id and `qwen.json` checksum.
    pub page_job_id: Option<String>,
    pub page_job_sha256: Option<String>,
    pub status: &'static str,
    /// The pair records, by file name, agreeing ones first.
    pub records: Vec<String>,
    pub accuracy_established: bool,
}

/// Every bundle page under `root`'s units, in unit then page order.
pub fn run(root: &Path, pages: &Path) -> anyhow::Result<Vec<PageStatus>> {
    let mut out = Vec::new();
    let dir = units(root);
    if !dir.exists() {
        return Ok(out);
    }
    let mut unit_dirs: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(JOB_FILE).exists())
        .collect();
    unit_dirs.sort();
    for unit in unit_dirs {
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("{}", unit.display()))?;
        if job.pages.is_empty() {
            continue;
        }
        let membership = read(&unit.join(MEMBERSHIP_FILE), MAX_MEMBERSHIP)
            .ok()
            .and_then(|b| serde_json::from_slice::<Membership>(&b).ok())
            .filter(|m| m.check(&job).is_ok() && m.check_sources(&root.join("pages")).is_ok());
        for (i, bp) in job.pages.iter().enumerate() {
            let member = membership.as_ref().map(|m| &m.pages[i]);
            let (status, records) = match member {
                None => ("membership_invalid", Vec::new()),
                Some(_) => {
                    let prefix = format!("{}:", bp.doc_id);
                    let expected: Vec<&str> = job
                        .crops
                        .iter()
                        .map(|c| c.id.as_str())
                        .filter(|id| id.starts_with(&prefix))
                        .collect();
                    page_records(pages, &job.id, &bp.doc_id, &expected)?
                }
            };
            out.push(PageStatus {
                workunit: job.workunit(),
                job_id: job.id.clone(),
                doc_id: bp.doc_id.clone(),
                page_job_id: member.map(|m| m.job_id.clone()),
                page_job_sha256: member.map(|m| m.job_sha256.clone()),
                status,
                records,
                accuracy_established: false,
            });
        }
    }
    Ok(out)
}

/// The page's status from its pair records. A record that does not name
/// this job and page, or whose sides and crop verdicts do not each hold
/// exactly the page's staged crops (`expected`) once, is not counted.
fn page_records(
    pages: &Path,
    job_id: &str,
    doc_id: &str,
    expected: &[&str],
) -> anyhow::Result<(&'static str, Vec<String>)> {
    let mut want: Vec<&str> = expected.to_vec();
    want.sort_unstable();
    let exact = |items: &serde_json::Value| {
        items.as_array().is_some_and(|xs| {
            let mut ids: Vec<&str> = xs.iter().filter_map(|x| x["id"].as_str()).collect();
            ids.sort_unstable();
            ids.len() == xs.len() && ids == want
        })
    };
    let dir = pages.join(job_id).join(doc_id);
    let mut agreed = Vec::new();
    let mut disagreed = Vec::new();
    if dir.exists() {
        let mut files: Vec<PathBuf> = std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        for f in files {
            let Ok(r) = read(&f, MAX_RECORD)
                .and_then(|b| Ok(serde_json::from_slice::<serde_json::Value>(&b)?))
            else {
                continue;
            };
            let own = r["v"] == 1
                && r["job_id"] == job_id
                && r["doc_id"] == doc_id
                && r["accuracy_established"] == false
                && !want.is_empty()
                && exact(&r["crops"])
                && r["results"].as_array().is_some_and(|sides| {
                    sides.len() == 2 && sides.iter().all(|s| exact(&s["readings"]))
                });
            let name = f
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default()
                .to_owned();
            match r["status"].as_str() {
                Some(PAGE_AGREED) if own => agreed.push(name),
                Some(PAGE_DISAGREED) if own => disagreed.push(name),
                _ => {}
            }
        }
    }
    let status = if !agreed.is_empty() {
        "accepted"
    } else if !disagreed.is_empty() {
        "disagreed"
    } else {
        "pending"
    };
    agreed.extend(disagreed);
    Ok((status, agreed))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::qwen::{stage, validate, DIAG_FILE, READINGS_FILE, RESULT_FILE};
    use std::collections::BTreeMap;
    use usnm_ocr_contract::{
        boinc::sha256,
        qwen::{Outcome, Reading, Readings, ResultManifest, PROMPT_VERSION, VERSION},
    };

    const DOCS: [&str; 3] = [
        "sn83045462_1890-05-03_ed-1_seq-1",
        "sn83045462_1890-05-03_ed-1_seq-2",
        "sn83045462_1890-05-03_ed-1_seq-3",
    ];

    /// One host's three output files for the bundle `job`, reading crop
    /// `<doc>:<crop>` as `text(doc, crop)`.
    fn result(
        dir: &Path,
        job: &Job,
        tag: &str,
        text: impl Fn(&str, &str) -> String,
    ) -> Vec<PathBuf> {
        let readings = Readings {
            v: VERSION,
            job_id: job.id.clone(),
            device: "vulkan".into(),
            diag: BTreeMap::new(),
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::Ok,
            prompt_version: PROMPT_VERSION,
            readings: job
                .crops
                .iter()
                .map(|c| {
                    let (doc, crop) = c.id.split_once(':').unwrap();
                    Reading {
                        id: c.id.clone(),
                        ms: 1,
                        text: text(doc, crop),
                        tokens: 3,
                    }
                })
                .collect(),
        };
        let rb = serde_json::to_vec(&readings).unwrap();
        let r = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            completed_at: "2026-10-28T00:00:00Z".into(),
            crops: job.crops.len() as u32,
            device: "vulkan".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: job.engine.clone(),
            engine_ms: 5,
            gpu_mem_peak_mib: None,
            host: Default::default(),
            load_ms: 1,
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::Ok,
            peak_rss_kib: None,
            read: job.crops.len() as u32,
            readings_bytes: rb.len() as u64,
            readings_sha256: sha256(&rb),
            unit_error: None,
        };
        let files: Vec<PathBuf> = [READINGS_FILE, RESULT_FILE, DIAG_FILE]
            .iter()
            .enumerate()
            .map(|(i, _)| dir.join(format!("{}_{tag}_{i}", job.workunit())))
            .collect();
        std::fs::write(&files[0], rb).unwrap();
        std::fs::write(&files[1], serde_json::to_vec(&r).unwrap()).unwrap();
        std::fs::write(&files[2], "diag").unwrap();
        files
    }

    fn bundle(root: &Path) -> Job {
        let page_dir = root.join("pages");
        for d in DOCS {
            stage::tests::page_unit(&page_dir, d, 2);
        }
        let dirs: Vec<PathBuf> = DOCS.iter().map(|d| page_dir.join(d)).collect();
        stage::write_bundle(&units(root).join("bundle-00001"), &dirs).unwrap()
    }

    #[test]
    fn keeps_agreed_pages_of_an_inconclusive_bundle_once_each() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let job = bundle(&root);
        assert_eq!(job.gpu_timeout_secs, stage::bundle_deadline(14_400, 3));
        let args = crate::qwen::submit::create_work_args(&job);
        assert!(args.windows(2).any(|w| w == ["--rsc_fpops_est", "3e13"]));
        assert!(args.windows(2).any(|w| w == ["--rsc_fpops_bound", "3e16"]));
        let membership: Membership = serde_json::from_slice(
            &std::fs::read(units(&root).join("bundle-00001").join(MEMBERSHIP_FILE)).unwrap(),
        )
        .unwrap();
        let page_job = std::fs::read(root.join("pages").join(DOCS[1]).join(JOB_FILE)).unwrap();
        assert_eq!(membership.pages[1].job_sha256, sha256(&page_job));
        let pages = tmp.path().join("qwen/pages");
        let log = tmp.path().join("qwen/pairs.jsonl");
        let words = |doc: &str, crop: &str| format!("{doc} {crop} the ship sailed for england");
        let a = result(tmp.path(), &job, "a", words);
        // Host b loops on page 2's second crop; pages 1 and 3 read alike.
        let b = result(tmp.path(), &job, "b", |doc, crop| {
            if doc == DOCS[1] && crop == "c02" {
                "[?] ".repeat(30)
            } else {
                words(doc, crop)
            }
        });
        assert!(validate::compare(&a, &b, 0.5, Some(&log), Some(&pages)).is_err());
        let ledger = run(&root, &pages).unwrap();
        let status: Vec<&str> = ledger.iter().map(|p| p.status).collect();
        assert_eq!(status, ["accepted", "disagreed", "accepted"]);
        assert!(ledger.iter().all(|p| !p.accuracy_established));
        assert_eq!(
            ledger[1].page_job_id.as_deref(),
            Some(membership.pages[1].job_id.as_str())
        );
        let line: serde_json::Value = serde_json::from_str(
            std::fs::read_to_string(&log)
                .unwrap()
                .lines()
                .next()
                .unwrap(),
        )
        .unwrap();
        assert_eq!(line["agree"], false);
        assert_eq!(line["pages"][1]["agree"], false);

        // The validator compares the same pair again: the same records, no new ones.
        let before = std::fs::read(
            pages
                .join(&job.id)
                .join(DOCS[0])
                .read_dir()
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap();
        assert!(validate::compare(&b, &a, 0.5, None, Some(&pages)).is_err());
        assert_eq!(
            pages
                .join(&job.id)
                .join(DOCS[0])
                .read_dir()
                .unwrap()
                .count(),
            1
        );
        let again = std::fs::read(
            pages
                .join(&job.id)
                .join(DOCS[0])
                .read_dir()
                .unwrap()
                .next()
                .unwrap()
                .unwrap()
                .path(),
        )
        .unwrap();
        assert_eq!(before, again);

        // A late third result agrees with a on page 2: its own record, and the page is kept.
        let c = result(tmp.path(), &job, "c", words);
        validate::compare(&a, &c, 0.5, None, Some(&pages)).unwrap();
        let ledger = run(&root, &pages).unwrap();
        assert_eq!(ledger[1].status, "accepted");
        assert_eq!(ledger[1].records.len(), 2);

        // A record for another page in the wrong place is not counted.
        let stray = pages.join(&job.id).join(DOCS[2]);
        for f in stray.read_dir().unwrap() {
            std::fs::remove_file(f.unwrap().path()).unwrap();
        }
        let misplaced = pages
            .join(&job.id)
            .join(DOCS[0])
            .read_dir()
            .unwrap()
            .next()
            .unwrap()
            .unwrap()
            .path();
        std::fs::copy(misplaced, stray.join("0000000000000000.json")).unwrap();
        assert_eq!(run(&root, &pages).unwrap()[2].status, "pending");
    }

    #[test]
    fn no_page_is_kept_without_a_matching_membership() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let job = bundle(&root);
        let pages = tmp.path().join("pages");
        let a = result(tmp.path(), &job, "a", |d, c| format!("{d} {c} words"));
        let b = result(tmp.path(), &job, "b", |d, c| format!("{d} {c} words"));
        validate::compare(&a, &b, 0.5, None, Some(&pages)).unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "accepted"));
        let path = units(&root).join("bundle-00001").join(MEMBERSHIP_FILE);
        let original = std::fs::read(&path).unwrap();
        let mut m: Membership = serde_json::from_slice(&original).unwrap();
        m.pages.swap(0, 1);
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "membership_invalid" && p.page_job_id.is_none()));
        // Members trading source identities do not trace to their own pages.
        let mut m: Membership = serde_json::from_slice(&original).unwrap();
        let (j0, s0) = (m.pages[0].job_id.clone(), m.pages[0].job_sha256.clone());
        m.pages[0].job_id = m.pages[1].job_id.clone();
        m.pages[0].job_sha256 = m.pages[1].job_sha256.clone();
        m.pages[1].job_id = j0;
        m.pages[1].job_sha256 = s0;
        std::fs::write(&path, serde_json::to_vec(&m).unwrap()).unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "membership_invalid"));
        // Nor does a source page unit changed after the bundle was written.
        std::fs::write(&path, &original).unwrap();
        let source = root.join("pages").join(DOCS[2]).join(JOB_FILE);
        let mut sj = std::fs::read(&source).unwrap();
        sj.push(b'\n');
        std::fs::write(&source, sj).unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "membership_invalid"));
        std::fs::write(&path, b"{").unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "membership_invalid"));
        std::fs::remove_file(&path).unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "membership_invalid"));
    }

    #[test]
    fn a_pair_compared_either_way_round_writes_one_record() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let job = bundle(&root);
        let pages = tmp.path().join("pages");
        let a = result(tmp.path(), &job, "a", |d, c| format!("{d} {c} words"));
        // b leaves page 2's first crop blank: a page where the hosts' read flags differ.
        let b = result(tmp.path(), &job, "b", |d, c| {
            if d == DOCS[1] && c == "c01" {
                String::new()
            } else {
                format!("{d} {c} words")
            }
        });
        let dir = pages.join(&job.id).join(DOCS[1]);
        assert!(validate::compare(&b, &a, 0.5, None, Some(&pages)).is_err());
        let first = std::fs::read(dir.read_dir().unwrap().next().unwrap().unwrap().path()).unwrap();
        assert!(validate::compare(&a, &b, 0.5, None, Some(&pages)).is_err());
        assert_eq!(dir.read_dir().unwrap().count(), 1);
        let path = dir.read_dir().unwrap().next().unwrap().unwrap().path();
        assert_eq!(std::fs::read(&path).unwrap(), first);
        let r: serde_json::Value = serde_json::from_slice(&first).unwrap();
        assert_eq!(r["status"], validate::PAGE_DISAGREED);
        // The record's first result is a's, so each crop's flags are (a, b).
        let blank = r["crops"]
            .as_array()
            .unwrap()
            .iter()
            .find(|c| c["id"] == format!("{}:c01", DOCS[1]))
            .unwrap();
        assert!(
            r["results"][0]["result"].as_str().unwrap()
                < r["results"][1]["result"].as_str().unwrap()
        );
        assert_eq!(blank["read"], serde_json::json!([true, false]));
    }

    #[test]
    fn a_page_both_hosts_left_short_is_not_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let job = bundle(&root);
        let pages = tmp.path().join("pages");
        let dropped = format!("{}:c02", DOCS[2]);
        // Both results claim to be complete but leave out the same crop of page 3.
        let short = |files: Vec<PathBuf>| {
            let mut r: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
            r["readings"]
                .as_array_mut()
                .unwrap()
                .retain(|x| x["id"] != dropped.as_str());
            let rb = serde_json::to_vec(&r).unwrap();
            let mut m: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&files[1]).unwrap()).unwrap();
            let n = job.crops.len() - 1;
            m["crops"] = n.into();
            m["read"] = n.into();
            m["readings_bytes"] = rb.len().into();
            m["readings_sha256"] = sha256(&rb).into();
            std::fs::write(&files[0], rb).unwrap();
            std::fs::write(&files[1], serde_json::to_vec(&m).unwrap()).unwrap();
            files
        };
        let a = short(result(tmp.path(), &job, "a", |d, c| {
            format!("{d} {c} words")
        }));
        let b = short(result(tmp.path(), &job, "b", |d, c| {
            format!("{d} {c} words")
        }));
        let _ = validate::compare(&a, &b, 0.5, None, Some(&pages));
        assert_eq!(
            std::fs::read_dir(pages.join(&job.id).join(DOCS[2]))
                .unwrap()
                .count(),
            1
        );
        let status: Vec<&str> = run(&root, &pages)
            .unwrap()
            .iter()
            .map(|p| p.status)
            .collect();
        assert_eq!(status, ["accepted", "accepted", "pending"]);
    }

    #[test]
    fn page_setup_failures_leave_the_unit_verdict_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let job = bundle(&root);
        let pages = tmp.path().join("pages");
        // Names the page records cannot use (a dot): the unit still agrees.
        let named = |files: Vec<PathBuf>, tag: &str| -> Vec<PathBuf> {
            files
                .iter()
                .enumerate()
                .map(|(i, f)| {
                    let to = f.with_file_name(format!("{tag}.{i}.json"));
                    std::fs::rename(f, &to).unwrap();
                    to
                })
                .collect()
        };
        let a = named(
            result(tmp.path(), &job, "a", |d, c| format!("{d} {c} words")),
            "a",
        );
        let b = named(
            result(tmp.path(), &job, "b", |d, c| format!("{d} {c} words")),
            "b",
        );
        validate::compare(&a, &b, 0.5, None, Some(&pages)).unwrap();
        assert!(!pages.exists());

        // A second staging run cannot write over a bundle already there.
        let dir = units(&root).join("bundle-00001");
        let before = std::fs::read(dir.join(MEMBERSHIP_FILE)).unwrap();
        let dirs: Vec<PathBuf> = DOCS[..2]
            .iter()
            .map(|d| root.join("pages").join(d))
            .collect();
        assert!(stage::write_bundle(&dir, &dirs).is_err());
        assert_eq!(std::fs::read(dir.join(MEMBERSHIP_FILE)).unwrap(), before);
    }
}
