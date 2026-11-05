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
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::qwen::{Job, Outcome, Reading, Readings, MAX_JOB_JSON, VERSION};

pub(crate) const MAX_MEMBERSHIP: usize = 256 * 1024;
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
        out.extend(unit_pages(root, &unit, &job, pages)?);
    }
    Ok(out)
}

/// The pages of the bundle unit staged at `unit` under `root`, whose job is
/// `job`, in page order; none for a single-page unit.
pub fn unit_pages(
    root: &Path,
    unit: &Path,
    job: &Job,
    pages: &Path,
) -> anyhow::Result<Vec<PageStatus>> {
    if job.pages.is_empty() {
        return Ok(Vec::new());
    }
    let membership = read(&unit.join(MEMBERSHIP_FILE), MAX_MEMBERSHIP)
        .ok()
        .and_then(|b| serde_json::from_slice::<Membership>(&b).ok())
        .filter(|m| m.check(job).is_ok() && m.check_sources(&root.join("pages")).is_ok());
    let mut out = Vec::new();
    for (i, bp) in job.pages.iter().enumerate() {
        let member = membership.as_ref().map(|m| &m.pages[i]);
        let (status, records) = match member {
            None => ("membership_invalid", Vec::new()),
            Some(_) => page_records(pages, &job.id, &bp.doc_id, &expected(job, &bp.doc_id))?,
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
    Ok(out)
}

/// The bundle's crop ids on the page `doc_id`, `<doc_id>:<crop>`.
fn expected<'a>(job: &'a Job, doc_id: &str) -> Vec<&'a str> {
    let prefix = format!("{doc_id}:");
    job.crops
        .iter()
        .map(|c| c.id.as_str())
        .filter(|id| id.starts_with(&prefix))
        .collect()
}

/// The pair records of one bundle page, by file name: every file
/// `qwen compare --pages` publishes there, whether or not it is counted.
pub fn record_paths(pages: &Path, job_id: &str, doc_id: &str) -> anyhow::Result<Vec<PathBuf>> {
    // Both name directories: a job id is its sha256, and `record_page`
    // checks the doc id the same way before writing.
    anyhow::ensure!(
        usnm_ocr_contract::boinc::is_sha256(job_id),
        "invalid qwen job id {job_id:?}"
    );
    usnm_ocr_contract::quality::check_doc_id(doc_id)?;
    let entries = match std::fs::read_dir(pages.join(job_id).join(doc_id)) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("reading page records"),
    };
    let mut files = Vec::new();
    for entry in entries {
        // As before bundles, an entry that can't be read is skipped, not fatal.
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        // `record_page` writes regular files with UTF-8 names, staging them
        // through temporary files in the same directory.
        if entry.file_type().is_ok_and(|t| t.is_file())
            && path.file_name().is_some_and(|n| n.to_str().is_some())
            && path.extension().is_some_and(|x| x == "json")
        {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

/// The readings two hosts agreed on for each `accepted` page of the bundle
/// unit, by doc id, as that page's own readings: each crop id's
/// `<doc_id>:` prefix is taken off (#1435). They are the first agreeing
/// record's (by file name), from the side whose result name sorts first
/// (the two names must differ), which must have run to the end and name its
/// device, so the same records always give the same readings. Agreeing is
/// not being right: nothing here establishes accuracy.
pub fn accepted_readings(
    root: &Path,
    unit: &Path,
    job: &Job,
    pages: &Path,
) -> anyhow::Result<BTreeMap<String, Readings>> {
    let mut out = BTreeMap::new();
    for page in unit_pages(root, unit, job, pages)? {
        if page.status != "accepted" {
            continue;
        }
        let first = page
            .records
            .first()
            .with_context(|| format!("{}: accepted with no record", page.doc_id))?;
        let path = pages.join(&job.id).join(&page.doc_id).join(first);
        let record: PageRecord = serde_json::from_slice(&read(&path, MAX_RECORD)?)
            .with_context(|| format!("{}", path.display()))?;
        anyhow::ensure!(
            record.status == PAGE_AGREED,
            "{}: not an agreeing record",
            path.display()
        );
        // The ledger counted it as this page's; it must still be.
        anyhow::ensure!(
            record.v == 1
                && record.job_id == job.id
                && record.doc_id == page.doc_id
                && !record.accuracy_established,
            "{}: not a record of this job's page",
            path.display()
        );
        let [a, b] = record.results;
        anyhow::ensure!(
            a.result != b.result,
            "{}: both sides name result {:?}",
            path.display(),
            a.result
        );
        let side = if b.result < a.result { b } else { a };
        let mut ids: Vec<&str> = side.readings.iter().map(|r| r.id.as_str()).collect();
        ids.sort_unstable();
        let mut want = expected(job, &page.doc_id);
        want.sort_unstable();
        anyhow::ensure!(
            !want.is_empty() && ids == want,
            "{}: the agreeing side does not read the page's crops once each",
            path.display()
        );
        anyhow::ensure!(
            side.outcome == Outcome::Ok,
            "{}: the agreeing side did not run to the end",
            path.display()
        );
        anyhow::ensure!(
            !side.device.trim().is_empty(),
            "{}: the agreeing side names no device",
            path.display()
        );
        let prefix = format!("{}:", page.doc_id);
        let readings = side
            .readings
            .into_iter()
            .map(|r| {
                let id = r.id.strip_prefix(&prefix).map(str::to_owned);
                id.map(|id| Reading { id, ..r })
            })
            .collect::<Option<Vec<_>>>()
            .with_context(|| format!("{}: a reading of another page", path.display()))?;
        let readings = Readings {
            v: VERSION,
            job_id: job.id.clone(),
            device: side.device,
            diag: BTreeMap::new(),
            model: side.model,
            outcome: Outcome::Ok,
            prompt_version: job.prompt_version,
            readings,
        };
        out.insert(page.doc_id, readings);
    }
    Ok(out)
}

/// What `accepted_readings` reads of a pair record (`validate::record_page`).
#[derive(serde::Deserialize)]
struct PageRecord {
    v: u32,
    job_id: String,
    doc_id: String,
    accuracy_established: bool,
    status: String,
    results: [RecordSide; 2],
}

#[derive(serde::Deserialize)]
struct RecordSide {
    result: String,
    device: String,
    model: Option<String>,
    outcome: Outcome,
    readings: Vec<Reading>,
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
    let mut agreed = Vec::new();
    let mut disagreed = Vec::new();
    for f in record_paths(pages, job_id, doc_id)? {
        let Ok(r) =
            read(&f, MAX_RECORD).and_then(|b| Ok(serde_json::from_slice::<serde_json::Value>(&b)?))
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
        let Some(name) = f.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        match r["status"].as_str() {
            Some(PAGE_AGREED) if own => agreed.push(name.to_owned()),
            Some(PAGE_DISAGREED) if own => disagreed.push(name.to_owned()),
            _ => {}
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
pub(crate) mod tests {
    use super::*;
    use crate::qwen::{stage, validate, DIAG_FILE, READINGS_FILE, RESULT_FILE};
    use usnm_ocr_contract::{
        boinc::sha256,
        qwen::{ResultManifest, PROMPT_VERSION},
    };

    const DOCS: [&str; 3] = [
        "sn83045462_1890-05-03_ed-1_seq-1",
        "sn83045462_1890-05-03_ed-1_seq-2",
        "sn83045462_1890-05-03_ed-1_seq-3",
    ];

    /// One host's three output files for the bundle `job`, reading crop
    /// `<doc>:<crop>` as `text(doc, crop)`.
    pub(crate) fn result(
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
        // The accepted pages' readings, as their own pages' crops.
        let kept =
            accepted_readings(&root, &units(&root).join("bundle-00001"), &job, &pages).unwrap();
        assert_eq!(kept.keys().collect::<Vec<_>>(), [DOCS[0], DOCS[2]]);
        for (doc, r) in &kept {
            assert_eq!(r.job_id, job.id);
            assert_eq!(r.outcome, Outcome::Ok);
            let got: Vec<(&str, String)> = r
                .readings
                .iter()
                .map(|x| (x.id.as_str(), x.text.clone()))
                .collect();
            assert_eq!(
                got,
                [("c01", words(doc, "c01")), ("c02", words(doc, "c02"))]
            );
        }
        // From the side whose result name sorts first, in whatever order the record lists them.
        let entries: Vec<PathBuf> = pages
            .join(&job.id)
            .join(DOCS[0])
            .read_dir()
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        let [record_path] = <[PathBuf; 1]>::try_from(entries).unwrap();
        let original = std::fs::read(&record_path).unwrap();
        let mut record: serde_json::Value = serde_json::from_slice(&original).unwrap();
        let sides = record["results"].as_array_mut().unwrap();
        for (side, text) in sides.iter_mut().zip(["first", "second"]) {
            for r in side["readings"].as_array_mut().unwrap() {
                r["text"] = text.into();
            }
        }
        sides.reverse();
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let kept =
            accepted_readings(&root, &units(&root).join("bundle-00001"), &job, &pages).unwrap();
        assert!(kept[DOCS[0]].readings.iter().all(|r| r.text == "first"));
        // A side with no device, or a blank one, is refused, and a directory is not a record.
        record["results"][1]
            .as_object_mut()
            .unwrap()
            .remove("device");
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let err =
            accepted_readings(&root, &units(&root).join("bundle-00001"), &job, &pages).unwrap_err();
        assert!(
            format!("{err:#}").contains("missing field `device`"),
            "{err:#}"
        );
        record["results"][1]["device"] = " ".into();
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let err =
            accepted_readings(&root, &units(&root).join("bundle-00001"), &job, &pages).unwrap_err();
        assert!(format!("{err:#}").contains("names no device"), "{err:#}");
        // Nor are two sides that name one result, whichever reads.
        record["results"][1]["device"] = record["results"][0]["device"].clone();
        record["results"][1]["result"] = record["results"][0]["result"].clone();
        std::fs::write(&record_path, serde_json::to_vec(&record).unwrap()).unwrap();
        let err =
            accepted_readings(&root, &units(&root).join("bundle-00001"), &job, &pages).unwrap_err();
        assert!(
            format!("{err:#}").contains("both sides name result"),
            "{err:#}"
        );
        let stray = record_path.with_file_name("stray.json");
        std::fs::create_dir(&stray).unwrap();
        assert_eq!(
            record_paths(&pages, &job.id, DOCS[0]).unwrap(),
            std::slice::from_ref(&record_path)
        );
        std::fs::remove_dir(&stray).unwrap();
        // Nor is a file whose name isn't UTF-8 (Linux allows one; APFS doesn't).
        #[cfg(target_os = "linux")]
        {
            use std::os::unix::ffi::OsStrExt;
            let odd = record_path.with_file_name(std::ffi::OsStr::from_bytes(b"\xff.json"));
            std::fs::write(&odd, &original).unwrap();
            assert_eq!(
                record_paths(&pages, &job.id, DOCS[0]).unwrap(),
                std::slice::from_ref(&record_path)
            );
            std::fs::remove_file(&odd).unwrap();
        }
        std::fs::write(&record_path, &original).unwrap();
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
    fn changed_bundle_contents_cannot_reuse_old_pair_records() {
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
        let path = units(&root).join("bundle-00001").join(JOB_FILE);
        for change_crop in [false, true] {
            let mut changed = job.clone();
            if change_crop {
                changed.crops[0].loc_text.push_str(" changed");
                changed.pages[0].crops[0].loc_text.push_str(" changed");
            } else {
                changed.max_tokens_per_1000px2 += 1;
            }
            assert_ne!(changed.identity().unwrap(), job.id);
            std::fs::write(&path, serde_json::to_vec(&changed).unwrap()).unwrap();
            assert!(run(&root, &pages)
                .unwrap()
                .iter()
                .all(|p| p.status == "membership_invalid" && p.page_job_id.is_none()));
        }
    }

    #[test]
    fn a_source_job_checksum_does_not_replace_identity_validation() {
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
        let membership_path = units(&root).join("bundle-00001").join(MEMBERSHIP_FILE);
        let mut membership: Membership =
            serde_json::from_slice(&std::fs::read(&membership_path).unwrap()).unwrap();
        let source = root.join("pages").join(DOCS[0]).join(JOB_FILE);
        let mut changed: Job = serde_json::from_slice(&std::fs::read(&source).unwrap()).unwrap();
        changed.max_tokens_per_1000px2 += 1;
        assert_ne!(changed.identity().unwrap(), changed.id);
        let bytes = serde_json::to_vec(&changed).unwrap();
        membership.pages[0].job_sha256 = sha256(&bytes);
        std::fs::write(source, bytes).unwrap();
        std::fs::write(membership_path, serde_json::to_vec(&membership).unwrap()).unwrap();
        assert!(run(&root, &pages)
            .unwrap()
            .iter()
            .all(|p| p.status == "membership_invalid" && p.page_job_id.is_none()));
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

    #[test]
    fn a_bundle_of_a_loc_page_and_a_sample_keeps_each_page_apart() {
        use crate::qwen::doc_id::DocId;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("root");
        let sample = DocId::Page("sample:overproof-ca/pub-overproof-ca-001".parse().unwrap());
        let docs = [DOCS[0].to_owned(), sample.wire().unwrap()];
        let page_dir = root.join("pages");
        for d in &docs {
            stage::tests::page_unit(&page_dir, d, 2);
        }
        let dirs: Vec<PathBuf> = docs.iter().map(|d| page_dir.join(d)).collect();
        let job = stage::write_bundle(&units(&root).join("bundle-00001"), &dirs).unwrap();
        assert_eq!(job.crops.len(), 4);
        let pages = tmp.path().join("qwen/pages");
        let words = |doc: &str, crop: &str| format!("{doc} {crop} the ship sailed for england");
        let a = result(tmp.path(), &job, "a", words);
        // Host b loops on the sample only.
        let b = result(tmp.path(), &job, "b", |doc, crop| {
            if doc == docs[1] {
                "[?] ".repeat(30)
            } else {
                words(doc, crop)
            }
        });
        assert!(validate::compare(&a, &b, 0.5, None, Some(&pages)).is_err());
        let ledger = run(&root, &pages).unwrap();
        let found: Vec<(DocId, &str)> = ledger
            .iter()
            .map(|p| (DocId::parse(&p.doc_id).unwrap(), p.status))
            .collect();
        assert_eq!(
            found,
            [
                (DocId::Page(DOCS[0].parse().unwrap()), "accepted"),
                (sample, "disagreed"),
            ]
        );
        for p in &ledger {
            assert_eq!(p.records.len(), 1, "{}", p.doc_id);
            assert!(pages
                .join(&job.id)
                .join(&p.doc_id)
                .join(&p.records[0])
                .exists());
        }
    }
}
