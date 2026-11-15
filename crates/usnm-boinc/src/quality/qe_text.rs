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
use usnm_core::ids::PageKey;
use usnm_ocr_contract::{
    boinc::sha256,
    qe_text::{
        is_spot, QeTextJob, QeTextPage, QeTextResult, JOB_FILE, MAX_JOB, MAX_PAGES, MAX_RESULT,
        NO_WORDS, RESULT_FILE, SOURCE_NONE, SOURCE_PAGE, SOURCE_TITLE, VERSION,
    },
};

pub const GROUP: &str = "qe-loc-v1";
const MAX_PAGE_ALTO: u64 = 64 << 20;

#[derive(Serialize, Deserialize)]
struct Line {
    doc_id: String,
    text: String,
}

/// The page an archive member holds, by the layouts `usnm-ingest`'s archive reader takes:
/// `<lccn>/<yyyy-mm-dd>/ed-N/seq-N/ocr.xml` or `<lccn>/<yyyy>/<mm>/<dd>/ed-N/seq-N/ocr.xml`,
/// under any prefix. The id is `PageKey`'s.
pub fn doc_id_of(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.trim_start_matches("./").split('/').collect();
    let n = parts.len();
    if parts.last() != Some(&"ocr.xml") || n < 5 {
        return None;
    }
    let num = |s: &str, prefix: &str| s.strip_prefix(prefix)?.parse::<u16>().ok();
    let (seq, ed) = (num(parts[n - 2], "seq-")?, num(parts[n - 3], "ed-")?);
    let (date, lccn) = if let Ok(d) = chrono::NaiveDate::parse_from_str(parts[n - 4], "%Y-%m-%d") {
        (d, parts[n - 5])
    } else if n >= 7 {
        let ymd = format!("{}-{}-{}", parts[n - 6], parts[n - 5], parts[n - 4]);
        (
            chrono::NaiveDate::parse_from_str(&ymd, "%Y-%m-%d").ok()?,
            parts[n - 7],
        )
    } else {
        return None;
    };
    Some(PageKey::new(lccn, date, ed, seq).ok()?.doc_id())
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
    /// `doc_id<TAB>lang` lines for the pages whose `quality.loc_alto.archive` is this archive:
    /// `quality.pages.lang`, the detected language. Pages not listed are left out.
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
    /// Pages of the extract that `--languages` does not list for this archive: another copy of
    /// an overlapping archive holds the page the corpus uses, so no unit is made of them.
    pub not_authoritative: u64,
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
    let lock_digest = sha256(super::qe::LOCK.as_bytes());
    let mut groups: BTreeMap<Option<String>, Vec<QeTextPage>> = BTreeMap::new();
    let mut s = StageSummary::default();
    for line in std::io::BufReader::new(std::fs::File::open(&o.extract)?).lines() {
        let l: Line = serde_json::from_str(&line?)?;
        if !detected.contains_key(&l.doc_id) {
            s.not_authoritative += 1;
            continue;
        }
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
                lock: lock_digest.clone(),
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

/// A unit's attempt: how many times it has been sent again after its host failed a spot check.
fn attempt(unit: &Path) -> u32 {
    std::fs::read_to_string(unit.join("resend"))
        .ok()
        .and_then(|t| t.trim().parse().ok())
        .unwrap_or(0)
}

/// The workunit of a unit's attempt, and of its replica on a second host (`spot` 1, 2, ...:
/// a replica that lands on the same host is sent again under the next number).
pub fn workunit_of(job: &QeTextJob, attempt: u32, spot: u32) -> String {
    let base = if attempt == 0 {
        job.id.clone()
    } else {
        format!("{}-r{attempt}", job.id)
    };
    match spot {
        0 => format!("usnm_quality_qetext_{base}"),
        n => format!("usnm_quality_qetextspot{n}_{base}"),
    }
}

pub fn create_work_args(job: &QeTextJob, wu_name: &str) -> Vec<String> {
    // One result is enough (quorum 1): the validator checks the result against the unit's
    // pages, and 3 in 100 units are replicated on a second host and compared.
    let mut args: Vec<String> = [
        "--appname",
        super::APP,
        "--wu_name",
        wu_name,
        "--wu_template",
        if job.qe.is_some() {
            "templates/usnm_quality_qetext_in"
        } else {
            "templates/usnm_quality_qetext_noqe_in"
        },
        "--result_template",
        "templates/usnm_quality_qetext_out",
        "--min_quorum",
        "1",
        "--target_nresults",
        "1",
        "--max_error_results",
        "3",
        "--max_total_results",
        "4",
        "--max_success_results",
        "1",
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
    pub spot_replicas: u64,
}

/// Where the validator finds a unit's manifest: `<project>/qetext/jobs/<id>.json`.
pub fn manifest_path(project: &Path, id: &str) -> PathBuf {
    project.join("qetext/jobs").join(format!("{id}.json"))
}

async fn create_work(project: &Path, job: &QeTextJob, wu: &str) -> anyhow::Result<()> {
    let status = tokio::process::Command::new(project.join("bin/create_work"))
        .current_dir(project)
        .args(create_work_args(job, wu))
        .status()
        .await?;
    ensure!(status.success(), "create_work failed for {wu}");
    Ok(())
}

/// Sends one unit's input files and workunit; a spot replica shares the input files.
async fn send(
    project: &Path,
    unit: &Path,
    job: &QeTextJob,
    wu: &str,
    files: bool,
) -> anyhow::Result<()> {
    if files {
        for (physical, file) in physical_names(job) {
            crate::assess::submit::stage_file(
                project,
                &physical,
                &read(&unit.join(file), MAX_JOB)?,
            )
            .await?;
        }
    }
    create_work(project, job, wu).await
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
        let job: QeTextJob = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB)?)?;
        // A replica asked for by the collector (the first landed on the same host).
        let want_spot = std::fs::read_to_string(unit.join("spot-wanted"))
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .unwrap_or(0);
        let spot_sent = std::fs::read_to_string(unit.join("spot-sent"))
            .ok()
            .and_then(|t| t.trim().parse::<u32>().ok())
            .unwrap_or(0);
        if unit.join("submitted").exists() && want_spot > spot_sent {
            create_work(
                &project,
                &job,
                &workunit_of(&job, attempt(&unit), want_spot),
            )
            .await?;
            atomic(&unit.join("spot-sent"), want_spot.to_string().as_bytes())?;
            s.spot_replicas += 1;
            continue;
        }
        if s.submitted as usize >= max_units {
            continue;
        }
        if unit.join("submitted").exists() || unit.join("submitting").exists() {
            continue;
        }
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
        let n = attempt(&unit);
        let wu = workunit_of(&job, n, 0);
        atomic(&unit.join("submitting"), wu.as_bytes())?;
        // The validator's copy of what the unit holds, before any result can come back.
        let m = manifest_path(&project, &job.id);
        std::fs::create_dir_all(m.parent().expect("has a parent"))?;
        atomic(&m, &serde_json::to_vec(&job.manifest())?)?;
        send(&project, &unit, &job, &wu, true).await?;
        if is_spot(&job.id) {
            send(&project, &unit, &job, &workunit_of(&job, n, 1), false).await?;
            atomic(&unit.join("spot-wanted"), b"1")?;
            atomic(&unit.join("spot-sent"), b"1")?;
            s.spot_replicas += 1;
        }
        std::fs::rename(unit.join("submitting"), unit.join("submitted"))?;
        s.submitted += 1;
    }
    Ok(s)
}

/// `wu_name<TAB>host_id<TAB>app version`, one line per canonical result, exported from the BOINC
/// database:
/// `SELECT w.name, r.hostid, CONCAT(av.version_num, ' ', p.name) FROM workunit w JOIN result r
///  ON r.id = w.canonical_resultid JOIN app_version av ON av.id = r.app_version_id JOIN platform p
///  ON p.id = av.platformid WHERE w.name LIKE 'usnm_quality_qetext%'`.
pub fn read_hosts(path: &Path) -> anyhow::Result<BTreeMap<String, (i64, String)>> {
    let mut out = BTreeMap::new();
    for line in std::fs::read_to_string(path)?.lines() {
        let mut f = line.splitn(3, '\t');
        if let (Some(wu), Some(host), Some(app)) = (f.next(), f.next(), f.next()) {
            if let Ok(host) = host.parse() {
                out.insert(wu.to_owned(), (host, app.trim().to_owned()));
            }
        }
    }
    Ok(out)
}

/// The rows of one unit's result, from the values it reported, for the host that computed them.
pub fn rows_of(
    job: &QeTextJob,
    result: &QeTextResult,
    host: &(i64, String),
) -> anyhow::Result<Vec<usnm_ocr_local::quality_db::QeLocScore>> {
    use usnm_ocr_local::quality_db::QeLocScore;
    result.check(job)?;
    result.check_manifest(&job.manifest())?;
    ensure!(!host.1.is_empty(), "no app version for host {}", host.0);
    let mut out = Vec::with_capacity(result.rows.len());
    for (row, page) in result.rows.iter().zip(&job.pages) {
        if let Some(q) = &row.qe {
            ensure!(
                q.version == usnm_core::qe::VERSION,
                "{}: the measure is not {}",
                row.doc_id,
                usnm_core::qe::VERSION
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
            resources_lock: job.lock.clone(),
            oov: row.qe.as_ref().and_then(|q| q.oov),
            charlm_bpc: row.qe.as_ref().and_then(|q| q.charlm_bpc),
            lm_oov: row.qe.as_ref().and_then(|q| q.lm_oov),
            known3: row.qe.as_ref().map(|q| q.known3 as i64),
            no_words: row.qe.as_ref().map(|q| q.no_words),
            words: row.qe.as_ref().map(|q| q.words as i64),
            null_reason,
            host_id: host.0,
            app_version: host.1.clone(),
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
    pub spot_checked: u64,
    pub spot_same_host: u64,
    pub hosts_failed: Vec<i64>,
    pub units_resent: u64,
}

fn read_result(spool: &Path, wu: &str) -> anyhow::Result<Option<QeTextResult>> {
    let file = spool.join(format!("{wu}__file_{RESULT_FILE}"));
    if !file.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_slice(&read(&file, MAX_RESULT)?)?))
}

/// What a spot check says about a unit.
#[derive(Debug, PartialEq, Eq)]
pub enum Spot {
    /// The replica has not come back (or its host is not exported yet).
    Waiting,
    /// Both ran on one host: send another replica.
    SameHost,
    Agree,
    /// The hosts disagree; neither is trusted.
    Disagree(i64, i64),
}

pub fn spot_verdict(
    first: &(QeTextResult, (i64, String)),
    replica: Option<&(QeTextResult, (i64, String))>,
) -> Spot {
    let Some(replica) = replica else {
        return Spot::Waiting;
    };
    if first.1 .0 == replica.1 .0 {
        return Spot::SameHost;
    }
    match first.0.spot_agrees(&replica.0) {
        Ok(()) => Spot::Agree,
        Err(_) => Spot::Disagree(first.1 .0, replica.1 .0),
    }
}

/// Sends every unit a failed host computed again: its markers go, its attempt number goes up.
fn resend_units(root: &Path, bad: &[i64]) -> anyhow::Result<u64> {
    let mut n = 0;
    for e in std::fs::read_dir(units(root))? {
        let unit = e?.path();
        let Ok(host) = std::fs::read_to_string(unit.join("collected")) else {
            continue;
        };
        if host.trim().parse::<i64>().is_ok_and(|h| bad.contains(&h)) {
            let next = attempt(&unit) + 1;
            for m in ["collected", "submitted", "spot-wanted", "spot-sent"] {
                let _ = std::fs::remove_file(unit.join(m));
            }
            atomic(&unit.join("resend"), next.to_string().as_bytes())?;
            n += 1;
        }
    }
    Ok(n)
}

pub async fn collect(
    root: &Path,
    spool: &Path,
    hosts: &BTreeMap<String, (i64, String)>,
    writer: Option<&usnm_ocr_local::quality_db::QeLocWriter>,
    batch: usize,
) -> anyhow::Result<CollectSummary> {
    let mut s = CollectSummary::default();
    let mut dirs: Vec<_> = std::fs::read_dir(units(root))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join("submitted").exists() && !p.join("collected").exists())
        .collect();
    dirs.sort();
    let mut pending_rows = Vec::new();
    let mut pending_units: Vec<(PathBuf, i64)> = Vec::new();
    let mut bad: Vec<i64> = Vec::new();
    for unit in dirs {
        let job: QeTextJob = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB)?)?;
        let n = attempt(&unit);
        let wu = workunit_of(&job, n, 0);
        if spool.join(format!("{wu}_error")).exists()
            || spool.join(format!("{wu}_no_output_files")).exists()
        {
            s.units_failed += 1;
            continue;
        }
        let (Some(result), Some(host)) = (read_result(spool, &wu)?, hosts.get(&wu)) else {
            s.units_pending += 1;
            continue;
        };
        let first = (result, host.clone());
        if is_spot(&job.id) {
            let wanted = std::fs::read_to_string(unit.join("spot-wanted"))
                .ok()
                .and_then(|t| t.trim().parse::<u32>().ok())
                .unwrap_or(1);
            let rwu = workunit_of(&job, n, wanted);
            let replica = match (read_result(spool, &rwu)?, hosts.get(&rwu)) {
                (Some(r), Some(h)) => Some((r, h.clone())),
                _ => None,
            };
            match spot_verdict(&first, replica.as_ref()) {
                Spot::Waiting => {
                    s.units_pending += 1;
                    continue;
                }
                Spot::SameHost => {
                    s.spot_same_host += 1;
                    atomic(
                        &unit.join("spot-wanted"),
                        (wanted + 1).to_string().as_bytes(),
                    )?;
                    s.units_pending += 1;
                    continue;
                }
                Spot::Disagree(a, b) => {
                    bad.extend([a, b]);
                    continue;
                }
                Spot::Agree => s.spot_checked += 1,
            }
        }
        pending_rows.extend(rows_of(&job, &first.0, &first.1)?);
        pending_units.push((unit, first.1 .0));
        if pending_rows.len() >= batch {
            flush(writer, &mut pending_rows, &mut pending_units, batch, &mut s).await?;
        }
    }
    flush(writer, &mut pending_rows, &mut pending_units, batch, &mut s).await?;
    if !bad.is_empty() {
        bad.sort_unstable();
        bad.dedup();
        if let Some(w) = writer {
            w.mark_invalid(&bad).await?;
            s.units_resent = resend_units(root, &bad)?;
        }
        s.hosts_failed = bad;
    }
    Ok(s)
}

async fn flush(
    writer: Option<&usnm_ocr_local::quality_db::QeLocWriter>,
    rows: &mut Vec<usnm_ocr_local::quality_db::QeLocScore>,
    units: &mut Vec<(PathBuf, i64)>,
    batch: usize,
    s: &mut CollectSummary,
) -> anyhow::Result<()> {
    if let Some(w) = writer {
        s.rows_new += w.insert(rows, batch).await?;
        for (u, host) in units.iter() {
            atomic(&u.join("collected"), host.to_string().as_bytes())?;
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
        assert_eq!(
            doc_id_of("batch_x_ver01/data/sn84021930/1898-11-23/ed-1/seq-12/ocr.xml").as_deref(),
            Some("sn84021930_1898-11-23_ed-1_seq-12")
        );
        assert_eq!(doc_id_of("sn1/1898/11/23/ed-1/seq-1/ocr.txt"), None);
        assert_eq!(doc_id_of("sn1/1898/13/23/ed-1/seq-1/ocr.xml"), None);
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
            ("sn1/1900/01/01/ed-1/seq-3/ocr.xml", xml.as_bytes()),
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
        assert_eq!((sum.pages, sum.left_out), (3, 0));

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
        assert_eq!((sum.pages, sum.units, sum.not_authoritative), (2, 2, 1));
        assert_eq!(sum.by_source[SOURCE_PAGE], 1);
        assert_eq!(sum.by_source[SOURCE_NONE], 1);
        assert_eq!(stage(&o).unwrap().already, 2);
    }

    fn job_of(n: usize) -> QeTextJob {
        let mut j = QeTextJob {
            v: VERSION,
            id: String::new(),
            archive: "x_ver01.tar.bz2".into(),
            group: GROUP.into(),
            lock: "a".repeat(64),
            language: None,
            pages: (1..=n)
                .map(|i| QeTextPage {
                    doc_id: format!("sn1_1900-01-01_ed-1_seq-{i}"),
                    language_source: SOURCE_NONE.into(),
                    text: "text".into(),
                })
                .collect(),
            qe: None,
        };
        j.id = j.identity().unwrap();
        j
    }

    fn result_for(j: &QeTextJob) -> QeTextResult {
        QeTextResult {
            v: VERSION,
            id: j.id.clone(),
            rows: j
                .pages
                .iter()
                .map(|p| usnm_ocr_contract::qe_text::QeTextRow {
                    doc_id: p.doc_id.clone(),
                    qe: None,
                    null_reason: Some(usnm_ocr_contract::qe_text::NO_LANGUAGE.into()),
                })
                .collect(),
        }
    }

    #[test]
    fn units_are_sent_for_one_result_and_rows_name_their_host() {
        let j = job_of(2);
        let args = create_work_args(&j, &workunit_of(&j, 0, 0));
        let at = |k: &str| {
            args.iter()
                .position(|a| a == k)
                .map(|i| args[i + 1].clone())
        };
        assert_eq!(at("--min_quorum").as_deref(), Some("1"));
        assert_eq!(at("--target_nresults").as_deref(), Some("1"));
        assert_ne!(workunit_of(&j, 0, 0), workunit_of(&j, 0, 1));
        assert_ne!(workunit_of(&j, 0, 0), workunit_of(&j, 1, 0));
        let rows = rows_of(&j, &result_for(&j), &(42, "1.5 x86_64-pc-linux-gnu".into())).unwrap();
        assert!(rows
            .iter()
            .all(|r| r.host_id == 42 && r.app_version == "1.5 x86_64-pc-linux-gnu"));
        assert!(rows_of(&j, &result_for(&j), &(42, String::new())).is_err());
        // A result with a page missing is not turned into rows.
        let mut short = result_for(&j);
        short.rows.pop();
        assert!(rows_of(&j, &short, &(42, "1.5 p".into())).is_err());
    }

    #[test]
    fn a_spot_check_compares_two_hosts() {
        let j = job_of(1);
        let a = (result_for(&j), (1, "1.5 linux".to_owned()));
        assert_eq!(spot_verdict(&a, None), Spot::Waiting);
        let same = (result_for(&j), (1, "1.5 linux".to_owned()));
        assert_eq!(spot_verdict(&a, Some(&same)), Spot::SameHost);
        let other = (result_for(&j), (2, "1.5 windows".to_owned()));
        assert_eq!(spot_verdict(&a, Some(&other)), Spot::Agree);
        let mut bad = other.clone();
        bad.0.rows[0].null_reason = Some("no_resource_file".into());
        assert_eq!(spot_verdict(&a, Some(&bad)), Spot::Disagree(1, 2));
    }

    #[test]
    fn a_failed_host_has_its_units_sent_again_under_a_new_name() {
        let tmp = tempfile::tempdir().unwrap();
        let mut ids = vec![];
        for (n, host) in [(1usize, "7"), (2, "8")] {
            let j = job_of(n);
            let unit = units(tmp.path()).join(&j.id);
            std::fs::create_dir_all(&unit).unwrap();
            for m in ["submitted", "spot-sent"] {
                std::fs::write(unit.join(m), "1").unwrap();
            }
            std::fs::write(unit.join("collected"), host).unwrap();
            ids.push((j, unit));
        }
        assert_eq!(resend_units(tmp.path(), &[7]).unwrap(), 1);
        let (j, unit) = &ids[0];
        assert!(!unit.join("submitted").exists() && !unit.join("collected").exists());
        assert_eq!(attempt(unit), 1);
        assert_eq!(
            workunit_of(j, attempt(unit), 0),
            format!("usnm_quality_qetext_{}-r1", j.id)
        );
        // The other host's unit stays collected.
        assert!(ids[1].1.join("collected").exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn submit_sends_the_manifest_and_a_replica_of_a_spot_unit() {
        use std::os::unix::fs::PermissionsExt as _;
        let tmp = tempfile::tempdir().unwrap();
        let project = tmp.path().join("project");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        let log = tmp.path().join("created");
        for (name, body) in [
            (
                "dir_hier_path",
                "#!/bin/sh\nprintf 'download/%s\\n' \"$1\"\n".to_owned(),
            ),
            (
                "create_work",
                format!("#!/bin/sh\necho \"$@\" >> {}\n", log.display()),
            ),
        ] {
            let p = project.join("bin").join(name);
            std::fs::write(&p, body).unwrap();
            std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        // Find a page count whose unit is a spot unit and one that is not.
        let spot = (1..200).map(job_of).find(|j| is_spot(&j.id)).unwrap();
        let plain = (1..200).map(job_of).find(|j| !is_spot(&j.id)).unwrap();
        for j in [&spot, &plain] {
            let unit = units(tmp.path()).join(&j.id);
            std::fs::create_dir_all(&unit).unwrap();
            std::fs::write(unit.join(JOB_FILE), serde_json::to_vec(j).unwrap()).unwrap();
        }
        let s = submit(tmp.path(), &project, 10).await.unwrap();
        assert_eq!((s.submitted, s.spot_replicas), (2, 1));
        let created = std::fs::read_to_string(&log).unwrap();
        assert_eq!(
            created.lines().count(),
            3,
            "two units and one replica: {created}"
        );
        assert!(created.contains("usnm_quality_qetextspot1_"));
        for j in [&spot, &plain] {
            let m: usnm_ocr_contract::qe_text::QeTextManifest = serde_json::from_slice(
                &std::fs::read(manifest_path(&project.canonicalize().unwrap(), &j.id)).unwrap(),
            )
            .unwrap();
            assert_eq!(m, j.manifest());
        }
    }
}
