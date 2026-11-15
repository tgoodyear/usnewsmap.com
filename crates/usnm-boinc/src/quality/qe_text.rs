//! The `qe-loc-v1` run: the quality measure (`usnm_core::qe`) of LoC's own OCR text for the
//! whole corpus, as text-only `usnm_quality` units (`usnm_ocr_contract::qe_text`).
//!
//! - `qe-text-extract` reads one bulk OCR archive (`<batch>_verNN.tar.bz2`) once, in order, and
//!   writes each page's LoC text, the way `stage` reads it from the ALTO: `usnm_alto::text` of
//!   the parsed page. One JSON line `{"doc_id","text"}` a page.
//! - `qe-text-stage` turns an extract into units: one language per unit, at most
//!   `--pages-per-unit` pages, with the language's `qe.bin` when the lock pins one. A page's
//!   language is `ocr_local::enqueue::message_language(detected, title languages)`, the rule
//!   `quality sample` and the OCR queue apply; `language_source` records which half answered.
//! - `qe-text-submit` hands staged units to `create_work`, two results each.
//! - `qe-text-collect` reads the canonical results and writes `quality.qe_loc_scores`, only
//!   from the values the units reported.
use super::{qe::Dir, units, QE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{BufRead as _, BufWriter, Read as _, Write as _},
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    boinc::sha256,
    qe_text::{
        QeTextJob, QeTextPage, QeTextResult, JOB_FILE, MAX_JOB, MAX_PAGES, MAX_RESULT, NO_WORDS,
        RESULT_FILE, SOURCE_NONE, SOURCE_PAGE, SOURCE_TITLE, VERSION,
    },
};

pub const GROUP: &str = "qe-loc-v1";
const MAX_PAGE_ALTO: u64 = 64 << 20;

#[derive(Serialize, Deserialize)]
struct Line {
    doc_id: String,
    text: String,
}

/// `sn84021930/1898/11/23/ed-1/seq-1/ocr.xml` to `sn84021930_1898-11-23_ed-1_seq-1`.
pub fn doc_id_of(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.trim_start_matches("./").split('/').collect();
    let [lccn, y, m, d, ed, seq, "ocr.xml"] = parts.as_slice() else {
        return None;
    };
    let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    let num = |s: &str, p: &str| {
        s.strip_prefix(p)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map(str::to_owned)
    };
    if lccn.is_empty()
        || !lccn
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !digits(y, 4)
        || !digits(m, 2)
        || !digits(d, 2)
    {
        return None;
    }
    Some(format!(
        "{lccn}_{y}-{m}-{d}_ed-{}_seq-{}",
        num(ed, "ed-")?,
        num(seq, "seq-")?
    ))
}

/// LoC's text of one page's ALTO: what `stage` joins as the `loc` reader.
pub fn alto_text(xml: &[u8]) -> anyhow::Result<String> {
    // The text does not depend on the size, only the parse needs one.
    let page = super::parse_alto(xml, 1.0, 1.0)?;
    Ok(usnm_alto::text(&page))
}

#[derive(Debug, Default, Serialize)]
pub struct ExtractSummary {
    pub pages: u64,
    pub left_out: u64,
}

/// One pass over an archive. Reads only; the extract goes to `out` through a `.part` file.
pub fn extract(archive: &Path, out: &Path) -> anyhow::Result<ExtractSummary> {
    let file = std::fs::File::open(archive).with_context(|| archive.display().to_string())?;
    let bz = bzip2::read::MultiBzDecoder::new(std::io::BufReader::with_capacity(1 << 20, file));
    let mut tar = tar::Archive::new(bz);
    let part = out.with_extension("part");
    let mut w = BufWriter::new(std::fs::File::create(&part)?);
    let mut s = ExtractSummary::default();
    let mut seen = std::collections::BTreeSet::new();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let path = entry.path()?.to_string_lossy().into_owned();
        let Some(doc_id) = doc_id_of(&path) else {
            continue;
        };
        let mut xml = Vec::new();
        entry.by_ref().take(MAX_PAGE_ALTO).read_to_end(&mut xml)?;
        if !seen.insert(doc_id.clone()) {
            continue;
        }
        match alto_text(&xml) {
            Ok(text) => {
                serde_json::to_writer(&mut w, &Line { doc_id, text })?;
                w.write_all(b"\n")?;
                s.pages += 1;
            }
            Err(e) => {
                eprintln!("page left out: {doc_id}: {e:#}");
                s.left_out += 1;
            }
        }
    }
    w.flush()?;
    drop(w);
    std::fs::rename(&part, out)?;
    Ok(s)
}

/// The page's language and where it came from. The language is
/// `message_language`'s; it came from the page when the detected language alone gives it.
pub fn language_of(detected: Option<&str>, titles: &[String]) -> (Option<String>, &'static str) {
    use usnm_ocr_local::enqueue::message_language;
    match message_language(detected, titles) {
        None => (None, SOURCE_NONE),
        Some(l) if message_language(detected, &[]).as_deref() == Some(&l) => (Some(l), SOURCE_PAGE),
        Some(l) => (Some(l), SOURCE_TITLE),
    }
}

pub struct StageOptions {
    pub extract: PathBuf,
    pub archive: String,
    /// `doc_id<TAB>lang` lines: `quality.pages.lang`, the detected language.
    pub languages: PathBuf,
    /// JSON object: LCCN to the title's MARC language codes (as `quality sample` reads).
    pub title_languages: Option<PathBuf>,
    pub qe_dir: PathBuf,
    pub root: PathBuf,
    pub pages_per_unit: usize,
}

#[derive(Debug, Default, Serialize)]
pub struct StageSummary {
    pub pages: u64,
    pub units: u64,
    pub already: u64,
    pub by_source: BTreeMap<String, u64>,
}

pub fn stage(o: &StageOptions) -> anyhow::Result<StageSummary> {
    ensure!(
        (1..=MAX_PAGES).contains(&o.pages_per_unit),
        "--pages-per-unit is 1 to {MAX_PAGES}"
    );
    let detected: BTreeMap<String, String> = std::fs::read_to_string(&o.languages)?
        .lines()
        .filter_map(|l| {
            let (d, lang) = l.split_once('\t')?;
            Some((d.to_owned(), lang.trim().to_owned()))
        })
        .collect();
    let titles: BTreeMap<String, Vec<String>> = match &o.title_languages {
        Some(p) => serde_json::from_slice(&std::fs::read(p)?).context("reading title languages")?,
        None => BTreeMap::new(),
    };
    let dir = Dir::open(&o.qe_dir)?;
    let mut groups: BTreeMap<Option<String>, Vec<QeTextPage>> = BTreeMap::new();
    let mut s = StageSummary::default();
    for line in std::io::BufReader::new(std::fs::File::open(&o.extract)?).lines() {
        let l: Line = serde_json::from_str(&line?)?;
        let lccn = l.doc_id.split('_').next().unwrap_or_default();
        let none = vec![];
        let (lang, source) = language_of(
            detected.get(&l.doc_id).map(String::as_str),
            titles.get(lccn).unwrap_or(&none),
        );
        *s.by_source.entry(source.to_owned()).or_default() += 1;
        groups.entry(lang).or_default().push(QeTextPage {
            doc_id: l.doc_id,
            language_source: source.to_owned(),
            text: l.text,
        });
    }
    let root = units(&o.root);
    for (lang, mut pages) in groups {
        pages.sort_by(|a, b| a.doc_id.cmp(&b.doc_id));
        let qe = match &lang {
            Some(l) => dir.input(l)?,
            None => None,
        };
        let mut rest = pages.as_slice();
        while !rest.is_empty() {
            let (chunk, tail) = rest.split_at(rest.len().min(o.pages_per_unit));
            rest = tail;
            let mut job = QeTextJob {
                v: VERSION,
                id: String::new(),
                archive: o.archive.clone(),
                group: GROUP.to_owned(),
                language: lang.clone(),
                pages: chunk.to_vec(),
                qe: qe.as_ref().map(|(_, q)| q.clone()),
            };
            job.id = job.identity()?;
            job.validate()?;
            let bytes = serde_json::to_vec(&job)?;
            ensure!(
                bytes.len() <= MAX_JOB,
                "unit over size limit; lower --pages-per-unit"
            );
            let unit = root.join(&job.id);
            s.pages += chunk.len() as u64;
            if unit.join(JOB_FILE).exists() {
                s.already += 1;
                continue;
            }
            std::fs::create_dir_all(&unit)?;
            if let Some((src, _)) = &qe {
                super::qe::place(src, &unit)?;
            }
            atomic(&unit.join(JOB_FILE), &bytes)?;
            s.units += 1;
        }
    }
    Ok(s)
}

pub fn workunit(job: &QeTextJob) -> String {
    format!("usnm_quality_qetext_{}", job.id)
}

pub fn create_work_args(job: &QeTextJob) -> Vec<String> {
    let mut args: Vec<String> = [
        "--appname",
        super::APP,
        "--wu_name",
        &workunit(job),
        "--wu_template",
        if job.qe.is_some() {
            "templates/usnm_quality_qetext_in"
        } else {
            "templates/usnm_quality_qetext_noqe_in"
        },
        "--result_template",
        "templates/usnm_quality_qetext_out",
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
        "1e13",
        "--rsc_fpops_bound",
        "1e15",
        "--rsc_memory_bound",
        "1610612736",
        "--rsc_disk_bound",
        "1073741824",
        "--delay_bound",
        "259200",
    ]
    .iter()
    .map(|s| (*s).to_owned())
    .collect();
    args.extend(physical_names(job).into_iter().map(|(p, _)| p));
    args
}

/// Physical names in the download tree, in input template order, with the unit file each comes from.
pub fn physical_names(job: &QeTextJob) -> Vec<(String, &'static str)> {
    let mut names = vec![(format!("{}_qe_text_json", job.id), JOB_FILE)];
    if let Some(qe) = &job.qe {
        names.push((format!("usnm_quality_qe_v1_{}", qe.input.sha256), QE_FILE));
    }
    names
}

#[derive(Debug, Default, Serialize)]
pub struct SubmitSummary {
    pub submitted: u64,
}

pub async fn submit(
    root: &Path,
    project: &Path,
    max_units: usize,
) -> anyhow::Result<SubmitSummary> {
    let project = project.canonicalize()?;
    let mut s = SubmitSummary::default();
    let mut names: Vec<_> = std::fs::read_dir(units(root))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(JOB_FILE).exists())
        .collect();
    names.sort();
    for unit in names {
        if s.submitted as usize >= max_units {
            break;
        }
        if unit.join("submitted").exists() || unit.join("submitting").exists() {
            continue;
        }
        let job: QeTextJob = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB)?)?;
        job.validate()?;
        if let Some(qe) = &job.qe {
            ensure!(
                qe.input.matches(&read(
                    &unit.join(QE_FILE),
                    usnm_ocr_contract::quality::MAX_QE
                )?),
                "staged {QE_FILE} changed for {}",
                job.id
            );
        }
        atomic(&unit.join("submitting"), workunit(&job).as_bytes())?;
        for (physical, file) in physical_names(&job) {
            crate::assess::submit::stage_file(
                &project,
                &physical,
                &read(&unit.join(file), MAX_JOB)?,
            )
            .await?;
        }
        let status = tokio::process::Command::new(project.join("bin/create_work"))
            .current_dir(&project)
            .args(create_work_args(&job))
            .status()
            .await?;
        ensure!(
            status.success(),
            "create_work failed for {}; {} is uncertain until checked",
            workunit(&job),
            unit.display()
        );
        std::fs::rename(unit.join("submitting"), unit.join("submitted"))?;
        s.submitted += 1;
    }
    Ok(s)
}

/// The rows of one unit's result, from the values it reported.
pub fn rows_of(
    job: &QeTextJob,
    result: &QeTextResult,
    lock_digest: &str,
) -> anyhow::Result<Vec<usnm_ocr_local::quality_db::QeLocScore>> {
    use usnm_ocr_local::quality_db::QeLocScore;
    result.check(job)?;
    let lock = super::qe::lock()?;
    let want = job
        .language
        .as_deref()
        .and_then(|l| lock.languages.get(super::qe::iso3(l)));
    let mut out = Vec::with_capacity(result.rows.len());
    for (row, page) in result.rows.iter().zip(&job.pages) {
        if let Some(q) = &row.qe {
            ensure!(
                q.version == usnm_core::qe::VERSION
                    && want.is_some_and(|e| e.sha256 == q.resources),
                "{}: the measure is not the lock's for {:?}",
                row.doc_id,
                job.language
            );
        }
        let null_reason = row.null_reason.clone().or_else(|| {
            row.qe
                .as_ref()
                .filter(|q| q.no_words)
                .map(|_| NO_WORDS.to_owned())
        });
        out.push(QeLocScore {
            doc_id: row.doc_id.clone(),
            archive: job.archive.clone(),
            language: job.language.clone(),
            language_source: page.language_source.clone(),
            qe_version: usnm_core::qe::VERSION.to_owned(),
            resources_lock: lock_digest.to_owned(),
            oov: row.qe.as_ref().and_then(|q| q.oov),
            charlm_bpc: row.qe.as_ref().and_then(|q| q.charlm_bpc),
            lm_oov: row.qe.as_ref().and_then(|q| q.lm_oov),
            known3: row.qe.as_ref().map(|q| q.known3 as i64),
            no_words: row.qe.as_ref().map(|q| q.no_words),
            words: row.qe.as_ref().map(|q| q.words as i64),
            null_reason,
        });
    }
    Ok(out)
}

#[derive(Debug, Default, Serialize)]
pub struct CollectSummary {
    pub units_collected: u64,
    pub units_pending: u64,
    pub units_failed: u64,
    pub rows_new: u64,
}

pub async fn collect(
    root: &Path,
    spool: &Path,
    writer: Option<&usnm_ocr_local::quality_db::QeLocWriter>,
    batch: usize,
) -> anyhow::Result<CollectSummary> {
    let lock_digest = sha256(super::qe::LOCK.as_bytes());
    let mut s = CollectSummary::default();
    let mut dirs: Vec<_> = std::fs::read_dir(units(root))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("submitted").exists() && !p.join("collected").exists())
        .collect();
    dirs.sort();
    let mut pending_rows = Vec::new();
    let mut pending_units = Vec::new();
    for unit in dirs {
        let job: QeTextJob = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB)?)?;
        let wu = workunit(&job);
        if spool.join(format!("{wu}_error")).exists()
            || spool.join(format!("{wu}_no_output_files")).exists()
        {
            s.units_failed += 1;
            continue;
        }
        let file = spool.join(format!("{wu}__file_{RESULT_FILE}"));
        if !file.exists() {
            s.units_pending += 1;
            continue;
        }
        let result: QeTextResult = serde_json::from_slice(&read(&file, MAX_RESULT)?)?;
        pending_rows.extend(rows_of(&job, &result, &lock_digest)?);
        pending_units.push(unit);
        if pending_rows.len() >= batch {
            flush(writer, &mut pending_rows, &mut pending_units, batch, &mut s).await?;
        }
    }
    flush(writer, &mut pending_rows, &mut pending_units, batch, &mut s).await?;
    Ok(s)
}

async fn flush(
    writer: Option<&usnm_ocr_local::quality_db::QeLocWriter>,
    rows: &mut Vec<usnm_ocr_local::quality_db::QeLocScore>,
    units: &mut Vec<PathBuf>,
    batch: usize,
    s: &mut CollectSummary,
) -> anyhow::Result<()> {
    if let Some(w) = writer {
        s.rows_new += w.insert(rows, batch).await?;
        for u in units.iter() {
            atomic(&u.join("collected"), b"")?;
        }
    }
    s.units_collected += units.len() as u64;
    rows.clear();
    units.clear();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| (*x).to_owned()).collect()
    }

    #[test]
    fn doc_ids_come_from_member_paths() {
        assert_eq!(
            doc_id_of("./sn84021930/1898/11/23/ed-1/seq-1/ocr.xml").as_deref(),
            Some("sn84021930_1898-11-23_ed-1_seq-1")
        );
        assert_eq!(doc_id_of("sn1/1898/11/23/ed-1/seq-1/ocr.txt"), None);
        assert_eq!(doc_id_of("../x/1898/11/23/ed-1/seq-1/ocr.xml"), None);
    }

    #[test]
    fn a_pages_language_is_the_ocr_queues() {
        // Detected and mapped: the page's.
        assert_eq!(
            language_of(Some("ger"), &s(&["eng"])),
            (Some("deu".into()), SOURCE_PAGE)
        );
        // `und`, or a code with no mapping: the title's first language.
        assert_eq!(
            language_of(Some("und"), &s(&["fre", "eng"])),
            (Some("fra".into()), SOURCE_TITLE)
        );
        assert_eq!(
            language_of(None, &s(&["spa"])),
            (Some("spa".into()), SOURCE_TITLE)
        );
        // Neither: none.
        assert_eq!(language_of(Some("und"), &[]), (None, SOURCE_NONE));
        assert_eq!(language_of(Some("und"), &s(&["zzz"])), (None, SOURCE_NONE));
    }

    #[test]
    fn text_comes_from_the_alto_the_way_stage_reads_it() {
        let xml = crate::quality::alto_xml(
            1000,
            1000,
            &[vec![
                vec![
                    (10, 10, 60, 30, "The", 0.9),
                    (70, 10, 150, 30, "mayor", 0.9),
                ],
                vec![(10, 40, 90, 60, "spoke", 0.9)],
            ]],
        );
        let page = crate::quality::parse_alto(xml.as_bytes(), 1000.0, 1000.0).unwrap();
        assert_eq!(alto_text(xml.as_bytes()).unwrap(), usnm_alto::text(&page));
        assert_eq!(alto_text(xml.as_bytes()).unwrap(), "The mayor\nspoke");
    }

    #[test]
    fn extract_reads_an_archive_once_and_stage_makes_units() {
        let tmp = tempfile::tempdir().unwrap();
        let xml = crate::quality::alto_xml(1000, 1000, &[vec![vec![(10, 10, 60, 30, "The", 0.9)]]]);
        let mut tar = tar::Builder::new(Vec::new());
        for (path, body) in [
            ("sn1/1900/01/01/ed-1/seq-1/ocr.xml", xml.as_bytes()),
            ("sn1/1900/01/01/ed-1/seq-2/ocr.xml", xml.as_bytes()),
            ("sn1/1900/01/01/ed-1/seq-1/ocr.txt", b"skipped".as_slice()),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tar.append_data(&mut h, path, body).unwrap();
        }
        let mut enc = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        enc.write_all(&tar.into_inner().unwrap()).unwrap();
        let archive = tmp.path().join("x_ver01.tar.bz2");
        std::fs::write(&archive, enc.finish().unwrap()).unwrap();
        let ex = tmp.path().join("x.jsonl");
        let sum = extract(&archive, &ex).unwrap();
        assert_eq!((sum.pages, sum.left_out), (2, 0));

        // Italian has no file in the lock, so its unit carries no qe.bin; the `und` page has
        // no language and no title.
        let qe_dir = tmp.path().join("qe");
        std::fs::create_dir(&qe_dir).unwrap();
        let langs = tmp.path().join("langs.tsv");
        std::fs::write(
            &langs,
            "sn1_1900-01-01_ed-1_seq-1\tita\nsn1_1900-01-01_ed-1_seq-2\tund\n",
        )
        .unwrap();
        let o = StageOptions {
            extract: ex,
            archive: "x_ver01.tar.bz2".into(),
            languages: langs,
            title_languages: None,
            qe_dir,
            root: tmp.path().join("root"),
            pages_per_unit: 10,
        };
        let sum = stage(&o).unwrap();
        assert_eq!((sum.pages, sum.units), (2, 2));
        assert_eq!(sum.by_source[SOURCE_PAGE], 1);
        assert_eq!(sum.by_source[SOURCE_NONE], 1);
        assert_eq!(stage(&o).unwrap().already, 2);
    }
}
