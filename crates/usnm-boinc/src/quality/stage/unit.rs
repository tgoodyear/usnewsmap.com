//! Writing and packing staged quality units.

use super::{ALTO_FILE, JOB_FILE, OURS_ALTO_FILE, PAGE_FILE, PLACES_FILE, TEXTS_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::quality::{
    BundlePage, BundlePageInputs, BundleSlice, Engine, Input, Inputs, Job, ReaderSpec, Texts,
    MAX_ALTO, MAX_BUNDLE_ALTO, MAX_BUNDLE_PAGES, MAX_BUNDLE_SCAN, MAX_BUNDLE_TEXTS, MAX_JOB_JSON,
    MAX_PLACES, MAX_SCAN, MAX_TEXTS, VERSION,
};

/// Packs any staged single-page units in `page_dir` that are not yet present in
/// a bundle under `units_dir` into multi-page bundle units of up to `bundle_size` pages.
pub(super) fn pack_bundles(
    page_dir: &Path,
    units_dir: &Path,
    bundle_size: usize,
) -> anyhow::Result<usize> {
    let mut bundled_docs = BTreeSet::new();
    let mut existing_bundles = 0usize;
    if units_dir.exists() {
        for entry in std::fs::read_dir(units_dir)?.filter_map(|e| e.ok()) {
            let p = entry.path();
            if p.join(JOB_FILE).exists() {
                existing_bundles += 1;
                if let Ok(job) =
                    serde_json::from_slice::<Job>(&read(&p.join(JOB_FILE), MAX_JOB_JSON)?)
                {
                    if job.pages.is_empty() {
                        bundled_docs.insert(job.doc_id);
                    } else {
                        for bp in job.pages {
                            bundled_docs.insert(bp.doc_id);
                        }
                    }
                }
            }
        }
    }
    let mut unbundled: Vec<PathBuf> = std::fs::read_dir(page_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.join(JOB_FILE).exists()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| !bundled_docs.contains(n))
        })
        .collect();
    unbundled.sort();
    let mut created = 0usize;
    let mut chunk: Vec<PathBuf> = Vec::new();
    let mut scan_bytes = 0usize;
    let mut alto_bytes = 0usize;
    let mut texts_bytes = 0usize;
    let flush = |chunk: &mut Vec<PathBuf>, idx: usize| -> anyhow::Result<()> {
        if chunk.is_empty() {
            return Ok(());
        }
        let first_doc = chunk[0]
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bundle");
        let dir = units_dir.join(format!("bundle-{idx:05}-{first_doc}"));
        write_bundle(&dir, chunk)?;
        chunk.clear();
        Ok(())
    };
    for u in unbundled {
        let p_len = std::fs::metadata(u.join(PAGE_FILE))
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let a_len = std::fs::metadata(u.join(ALTO_FILE))
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let t_len = std::fs::metadata(u.join(TEXTS_FILE))
            .map(|m| m.len() as usize)
            .unwrap_or(0);
        let over_size = !chunk.is_empty()
            && (chunk.len() >= bundle_size
                || scan_bytes + p_len > MAX_BUNDLE_SCAN
                || alto_bytes + a_len > MAX_BUNDLE_ALTO
                || texts_bytes + t_len > MAX_BUNDLE_TEXTS);
        if over_size {
            created += 1;
            flush(&mut chunk, existing_bundles + created)?;
            scan_bytes = 0;
            alto_bytes = 0;
            texts_bytes = 0;
        }
        scan_bytes += p_len;
        alto_bytes += a_len;
        texts_bytes += t_len;
        chunk.push(u);
    }
    if !chunk.is_empty() {
        created += 1;
        flush(&mut chunk, existing_bundles + created)?;
    }
    Ok(created)
}

/// Everything a unit directory is written from.
pub struct WriteUnit<'a> {
    pub dir: &'a Path,
    pub doc_id: &'a str,
    pub group: &'a str,
    pub language: &'a str,
    pub page: &'a [u8],
    pub page_source: &'a str,
    pub alto: &'a [u8],
    pub alto_source: &'a str,
    pub texts: &'a Texts,
    pub texts_source: &'a str,
    pub places: &'a str,
    pub places_source: &'a str,
    pub engine: Engine,
    pub function_words: Vec<String>,
    pub crop_seed: u64,
    pub k_crops: u32,
}

/// Writes the five input files of a unit into `dir`, the job last, and
/// returns the job.
pub fn write_unit(u: &WriteUnit) -> anyhow::Result<Job> {
    ensure!(u.page.len() <= MAX_SCAN, "page over size limit");
    ensure!(u.alto.len() <= MAX_ALTO, "ALTO over size limit");
    ensure!(u.places.len() <= MAX_PLACES, "places over size limit");
    ensure!(
        !u.texts.readers.is_empty(),
        "a unit needs at least one text"
    );
    let texts_bytes = serde_json::to_vec_pretty(u.texts)?;
    ensure!(texts_bytes.len() <= MAX_TEXTS, "texts over size limit");
    std::fs::create_dir_all(u.dir)?;
    let readers: Vec<ReaderSpec> = u
        .texts
        .readers
        .iter()
        .map(|r| ReaderSpec {
            name: r.name.clone(),
            source: r.source.clone(),
        })
        .collect();
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        crop_seed: u.crop_seed,
        doc_id: u.doc_id.to_owned(),
        engine: u.engine.clone(),
        function_words: u.function_words.clone(),
        group: u.group.to_owned(),
        inputs: Inputs {
            alto: Input::of(u.alto, u.alto_source),
            page: Input::of(u.page, u.page_source),
            places: Input::of(u.places.as_bytes(), u.places_source),
            texts: Input::of(&texts_bytes, u.texts_source),
        },
        k_crops: u.k_crops,
        language: u.language.to_owned(),
        pages: vec![],
        readers,
    };
    job.id = job.identity()?;
    job.validate().context("staged job")?;
    u.texts.validate(&job)?;
    atomic(&u.dir.join(PAGE_FILE), u.page)?;
    atomic(&u.dir.join(ALTO_FILE), u.alto)?;
    atomic(&u.dir.join(TEXTS_FILE), &texts_bytes)?;
    atomic(&u.dir.join(PLACES_FILE), u.places.as_bytes())?;
    atomic(&u.dir.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
    Ok(job)
}

/// Packs `page_units` (1 to [`MAX_BUNDLE_PAGES`] staged single-page unit
/// directories) into a single multi-page bundle unit directory `dir`.
pub fn write_bundle(dir: &Path, page_units: &[PathBuf]) -> anyhow::Result<Job> {
    ensure!(
        !page_units.is_empty() && page_units.len() <= MAX_BUNDLE_PAGES,
        "a bundle requires 1 to {MAX_BUNDLE_PAGES} page units"
    );
    std::fs::create_dir_all(dir)?;
    let mut page_blob = Vec::new();
    let mut alto_blob = Vec::new();
    let mut texts_blob = Vec::new();
    let mut bundle_pages = Vec::with_capacity(page_units.len());
    let mut first_job: Option<Job> = None;
    let mut places_bytes: Option<Vec<u8>> = None;

    for u_dir in page_units {
        let pj: Job = serde_json::from_slice(&read(&u_dir.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("reading {}/quality.json", u_dir.display()))?;
        pj.validate()?;
        ensure!(
            pj.pages.is_empty(),
            "{} is already a multi-page bundle",
            u_dir.display()
        );
        let p_bytes = read(&u_dir.join(PAGE_FILE), MAX_SCAN)?;
        ensure!(
            pj.inputs.page.matches(&p_bytes),
            "{}: page.jpg checksum mismatch",
            pj.doc_id
        );
        let a_bytes = read(&u_dir.join(ALTO_FILE), MAX_ALTO)?;
        ensure!(
            pj.inputs.alto.matches(&a_bytes),
            "{}: loc.xml checksum mismatch",
            pj.doc_id
        );
        let t_bytes = read(&u_dir.join(TEXTS_FILE), MAX_TEXTS)?;
        ensure!(
            pj.inputs.texts.matches(&t_bytes),
            "{}: texts.json checksum mismatch",
            pj.doc_id
        );
        if places_bytes.is_none() {
            let pl = read(&u_dir.join(PLACES_FILE), MAX_PLACES)?;
            ensure!(
                pj.inputs.places.matches(&pl),
                "{}: places.txt checksum mismatch",
                pj.doc_id
            );
            places_bytes = Some(pl);
        }

        let page_slice = BundleSlice::of(page_blob.len() as u64, &p_bytes, &pj.inputs.page.source);
        page_blob.extend_from_slice(&p_bytes);

        let alto_slice = BundleSlice::of(alto_blob.len() as u64, &a_bytes, &pj.inputs.alto.source);
        alto_blob.extend_from_slice(&a_bytes);

        let texts_slice =
            BundleSlice::of(texts_blob.len() as u64, &t_bytes, &pj.inputs.texts.source);
        texts_blob.extend_from_slice(&t_bytes);

        let ours = u_dir.join(OURS_ALTO_FILE);
        if ours.exists() {
            atomic(
                &dir.join(format!("{}.ours.xml", pj.doc_id)),
                &read(&ours, MAX_ALTO)?,
            )?;
        }

        let fw = match &first_job {
            Some(fj) if fj.function_words == pj.function_words => vec![],
            None => vec![],
            Some(_) => pj.function_words.clone(),
        };

        bundle_pages.push(BundlePage {
            crop_seed: pj.crop_seed,
            doc_id: pj.doc_id.clone(),
            function_words: fw,
            group: pj.group.clone(),
            inputs: BundlePageInputs {
                alto: alto_slice,
                page: page_slice,
                texts: texts_slice,
            },
            k_crops: pj.k_crops,
            language: pj.language.clone(),
            readers: pj.readers.clone(),
        });

        if first_job.is_none() {
            first_job = Some(pj);
        }
    }

    ensure!(
        page_blob.len() <= MAX_BUNDLE_SCAN,
        "bundle page.jpg over size limit"
    );
    ensure!(
        alto_blob.len() <= MAX_BUNDLE_ALTO,
        "bundle loc.xml over size limit"
    );
    ensure!(
        texts_blob.len() <= MAX_BUNDLE_TEXTS,
        "bundle texts.json over size limit"
    );
    let fj = first_job.expect("non-empty page_units");
    let places_bytes = places_bytes.expect("non-empty page_units");

    let mut job = Job {
        v: VERSION,
        id: String::new(),
        crop_seed: fj.crop_seed,
        doc_id: fj.doc_id,
        engine: fj.engine,
        function_words: fj.function_words,
        group: fj.group,
        inputs: Inputs {
            alto: Input::of(&alto_blob, "bundle:loc.xml"),
            page: Input::of(&page_blob, "bundle:page.jpg"),
            places: Input::of(&places_bytes, &fj.inputs.places.source),
            texts: Input::of(&texts_blob, "bundle:texts.json"),
        },
        k_crops: fj.k_crops,
        language: fj.language,
        pages: bundle_pages,
        readers: fj.readers,
    };
    job.id = job.identity()?;
    job.validate().context("staged bundle job")?;
    atomic(&dir.join(PAGE_FILE), &page_blob)?;
    atomic(&dir.join(ALTO_FILE), &alto_blob)?;
    atomic(&dir.join(TEXTS_FILE), &texts_blob)?;
    atomic(&dir.join(PLACES_FILE), &places_bytes)?;
    atomic(&dir.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
    Ok(job)
}

/// A unit from local files, for tests and for checking a host or the
/// image without LoC or BOINC.
#[derive(Debug, Clone)]
pub struct LocalUnit<'a> {
    pub page: &'a [u8],
    pub alto: &'a [u8],
    pub texts: &'a Texts,
    pub places: &'a str,
    pub dir: &'a Path,
    pub engine: Engine,
    pub function_words: Vec<String>,
    pub crop_seed: u64,
    pub k_crops: u32,
}

/// Writes the five input files of a unit into `dir` and returns its job.
/// The sources are placeholders; `doc_id`, `group` and `language` are
/// `local`, `local` and `und`.
pub fn local_unit(u: &LocalUnit) -> anyhow::Result<Job> {
    write_unit(&WriteUnit {
        dir: u.dir,
        doc_id: "local",
        group: "local",
        language: "und",
        page: u.page,
        page_source: "local:page.jpg",
        alto: u.alto,
        alto_source: "local:loc.xml",
        texts: u.texts,
        texts_source: "local:texts.json",
        places: u.places,
        places_source: "local:places.txt",
        engine: u.engine.clone(),
        function_words: u.function_words.clone(),
        crop_seed: u.crop_seed,
        k_crops: u.k_crops,
    })
    .context("local unit")
}
