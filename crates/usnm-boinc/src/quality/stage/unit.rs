//! Writing and packing staged quality units.

use super::{ALTO_FILE, JOB_FILE, OURS_ALTO_FILE, PAGE_FILE, PLACES_FILE, QE_FILE, TEXTS_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::quality::{
    BundlePage, BundlePageInputs, BundleSlice, Engine, Input, Inputs, Job, QeInput, ReaderSpec,
    Texts, MAX_ALTO, MAX_BUNDLE_ALTO, MAX_BUNDLE_PAGES, MAX_BUNDLE_SCAN, MAX_BUNDLE_TEXTS,
    MAX_JOB_JSON, MAX_PLACES, MAX_QE, MAX_SCAN, MAX_TEXTS, VERSION,
};

/// The names of the bundle directories under `units_dir`. Only the names:
/// a listing is cheap on the share, a stat per bundle is not.
pub(super) fn bundle_names(units_dir: &Path) -> anyhow::Result<Vec<String>> {
    if !units_dir.exists() {
        return Ok(vec![]);
    }
    let mut names: Vec<String> = std::fs::read_dir(units_dir)?
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter(|n| n.starts_with("bundle-"))
        .collect();
    names.sort();
    Ok(names)
}

/// The page a bundle's name starts with: `bundle-<n>-<doc_id>`.
pub(super) fn first_doc(name: &str) -> Option<&str> {
    name.strip_prefix("bundle-")?
        .split_once('-')
        .map(|(_, d)| d)
}

/// The pages the bundles `names` under `units_dir` hold, after clearing
/// what a crash left behind, and how many of them were removed. A bundle that
/// is neither `submitted` nor `submitting` is removed when its `quality.json`
/// is missing (packing stopped part way; nothing can submit it) or when any of
/// its inputs no longer matches the hash the job records (the share lost or
/// cut short a write it had acknowledged, as it can with synchronous writes
/// off), and every page it holds is one of `run`'s, which packs them again.
/// One holding another page is left for submit's own check to refuse, so no
/// page loses its only bundle. A submitted or submitting bundle is never
/// touched: BOINC may already hold it. Without `run` nothing is removed.
pub(super) fn bundled(
    units_dir: &Path,
    names: &[&str],
    run: Option<&BTreeSet<&str>>,
) -> anyhow::Result<Bundled> {
    let mut docs = BTreeSet::new();
    let mut removed = 0usize;
    let mut repack = Vec::new();
    for name in names {
        let p = units_dir.join(name);
        if !p.is_dir() {
            continue;
        }
        let sent = p.join("submitted").exists() || p.join("submitting").exists();
        if !p.join(JOB_FILE).exists() {
            if run.is_some() && !sent {
                eprintln!("{}: no quality.json; packing it again", p.display());
                std::fs::remove_dir_all(&p)?;
                removed += 1;
            }
            continue;
        }
        let job = read(&p.join(JOB_FILE), MAX_JOB_JSON)
            .ok()
            .and_then(|b| serde_json::from_slice::<Job>(&b).ok());
        let members: Vec<String> = match &job {
            Some(job) if job.pages.is_empty() => vec![job.doc_id.clone()],
            Some(job) => job.pages.iter().map(|bp| bp.doc_id.clone()).collect(),
            None => vec![],
        };
        if let Some(run) = run.filter(|_| !sent) {
            if let Some(why) = damage(&p, job.as_ref()) {
                if job.is_some() && members.iter().all(|d| run.contains(d.as_str())) {
                    eprintln!("{}: {why}; packing it again", p.display());
                    std::fs::remove_dir_all(&p)?;
                    removed += 1;
                    repack.extend(members);
                    continue;
                }
                eprintln!(
                    "{}: {why}; it holds pages this run doesn't, so it stays",
                    p.display()
                );
            }
        }
        docs.extend(members);
    }
    Ok(Bundled {
        docs,
        removed,
        repack,
    })
}

/// What [`bundled`] found.
pub(super) struct Bundled {
    /// The pages the bundles hold.
    pub docs: BTreeSet<String>,
    /// How many bundles it removed.
    pub removed: usize,
    /// The pages of the removed bundles, which the run must pack again even
    /// if its page limit stops it before them.
    pub repack: Vec<String>,
}

/// Why a staged unit's inputs can't be trusted, or `None` when they all
/// match the hashes in its `quality.json`. A missing input is damage too: a
/// page unit freed after its bundle was submitted is never checked, as its
/// bundle claims it first.
pub(super) fn damage(dir: &Path, job: Option<&Job>) -> Option<String> {
    let Some(job) = job else {
        return Some("quality.json does not parse".into());
    };
    let files = [
        (PAGE_FILE, &job.inputs.page, MAX_BUNDLE_SCAN.max(MAX_SCAN)),
        (ALTO_FILE, &job.inputs.alto, MAX_BUNDLE_ALTO.max(MAX_ALTO)),
        (
            TEXTS_FILE,
            &job.inputs.texts,
            MAX_BUNDLE_TEXTS.max(MAX_TEXTS),
        ),
    ];
    for (file, input, max) in files.iter().chain(std::iter::once(&(
        PLACES_FILE,
        &job.inputs.places,
        MAX_PLACES,
    ))) {
        match read(&dir.join(file), *max) {
            Ok(bytes) if input.matches(&bytes) => {}
            Ok(_) => return Some(format!("{file} differs from its hash")),
            Err(e) => return Some(format!("{file}: {e:#}")),
        }
    }
    if let Some(qe) = &job.qe {
        match read(&dir.join(QE_FILE), MAX_QE) {
            Ok(bytes) if qe.input.matches(&bytes) => {}
            Ok(_) => return Some(format!("{QE_FILE} differs from its hash")),
            Err(e) => return Some(format!("{QE_FILE}: {e:#}")),
        }
    }
    None
}

/// Packs the staged single-page units of `docs` in `page_dir` that no
/// bundle holds yet (`bundled`) into multi-page bundle units of up to
/// `bundle_size` pages under `units_dir`, numbered after its `existing`
/// bundles. Only the run's own pages are looked at: `page_dir` keeps every
/// page the campaign ever staged, and listing it all over the share took
/// minutes once it held tens of thousands.
pub(super) fn pack_bundles(
    page_dir: &Path,
    units_dir: &Path,
    bundle_size: usize,
    docs: &[String],
    bundled_docs: &BTreeSet<String>,
    existing_bundles: usize,
) -> anyhow::Result<usize> {
    let mut unbundled: Vec<PathBuf> = docs
        .iter()
        .filter(|d| !bundled_docs.contains(*d))
        .map(|d| page_dir.join(d))
        .filter(|p| p.join(JOB_FILE).exists())
        .collect();
    unbundled.sort();
    unbundled.dedup();
    let mut created = 0usize;
    let mut chunk: Vec<PathBuf> = Vec::new();
    let mut scan_bytes = 0usize;
    let mut alto_bytes = 0usize;
    let mut texts_bytes = 0usize;
    // A bundle carries one quality-measure file or none, so a page whose
    // unit carries another starts a new bundle.
    let mut chunk_qe: Option<Option<QeInput>> = None;
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
        let qe = unit_qe(&u)?;
        let other_qe = chunk_qe.as_ref().is_some_and(|q| *q != qe);
        let over_size = !chunk.is_empty()
            && (other_qe
                || chunk.len() >= bundle_size
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
        chunk_qe = Some(qe);
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
    /// The quality measure's resource file for the unit's language and its
    /// job input (`quality::qe::Dir::input`), when the unit carries it.
    pub qe: Option<(&'a Path, QeInput)>,
}

/// Writes the input files of a unit into `dir` (five, or six with the
/// quality measure's `qe.bin`), the job last, and returns the job.
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
        qe: u.qe.as_ref().map(|(_, q)| q.clone()),
        readers,
    };
    job.id = job.identity()?;
    job.validate().context("staged job")?;
    u.texts.validate(&job)?;
    atomic(&u.dir.join(PAGE_FILE), u.page)?;
    atomic(&u.dir.join(ALTO_FILE), u.alto)?;
    atomic(&u.dir.join(TEXTS_FILE), &texts_bytes)?;
    atomic(&u.dir.join(PLACES_FILE), u.places.as_bytes())?;
    if let Some((src, _)) = &u.qe {
        crate::quality::qe::place(src, u.dir)?;
    }
    atomic(&u.dir.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
    Ok(job)
}

/// The quality-measure file a staged page unit's job carries.
fn unit_qe(unit: &Path) -> anyhow::Result<Option<QeInput>> {
    let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)?;
    Ok(job.qe)
}

/// Packs `page_units` (1 to [`MAX_BUNDLE_PAGES`] staged single-page unit
/// directories) into a single multi-page bundle unit directory `dir`, with
/// the pages' sizes in `dims.json` beside the job.
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
    // The bundle carries its pages' quality-measure file; `pack_bundles`
    // starts a new bundle where it changes.
    let qe = fj.qe.clone();
    for u in &page_units[1..] {
        ensure!(
            unit_qe(u)? == qe,
            "{}: its quality-measure file differs from the bundle's first page",
            u.display()
        );
    }

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
        qe,
        readers: fj.readers,
    };
    job.id = job.identity()?;
    job.validate().context("staged bundle job")?;
    // Collect reads the pages' sizes from here, not from the image (#1743).
    let dims = crate::quality::page_dims::PageDims::of_blob(&job, &page_blob)?;
    atomic(&dir.join(PAGE_FILE), &page_blob)?;
    atomic(&dir.join(ALTO_FILE), &alto_blob)?;
    atomic(&dir.join(TEXTS_FILE), &texts_blob)?;
    atomic(&dir.join(PLACES_FILE), &places_bytes)?;
    if let Some(q) = &job.qe {
        let src = page_units[0].join(QE_FILE);
        ensure!(
            q.input.matches(&read(&src, MAX_QE)?),
            "{}: qe.bin checksum mismatch",
            src.display()
        );
        crate::quality::qe::place(&src, dir)?;
    }
    dims.write(dir)?;
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
        qe: None,
    })
    .context("local unit")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn units_and_bundles_carry_the_quality_measure_file() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let base = crate::quality::work::tests::unit(&src);
        let file = |f: &str| std::fs::read(src.join(f)).unwrap();
        let texts: Texts = serde_json::from_slice(&file(TEXTS_FILE)).unwrap();
        let (page, alto) = (file(PAGE_FILE), file(ALTO_FILE));
        let places = String::from_utf8(file(PLACES_FILE)).unwrap();
        let qe_path = tmp.path().join("eng.qe");
        std::fs::write(&qe_path, b"resource bytes").unwrap();
        let qe = QeInput {
            input: Input::of(b"resource bytes", "lake:qe-resources/qe-v1/eng.qe"),
            language: base.language.clone(),
        };
        let mut dirs = vec![];
        let mut jobs = vec![];
        for doc in ["sn1_1900-01-01_ed-1_seq-1", "sn1_1900-01-01_ed-1_seq-2"] {
            let dir = tmp.path().join("pages").join(doc);
            let job = write_unit(&WriteUnit {
                dir: &dir,
                doc_id: doc,
                group: &base.group,
                language: &base.language,
                page: &page,
                page_source: "local:page.jpg",
                alto: &alto,
                alto_source: "local:loc.xml",
                texts: &texts,
                texts_source: "local:texts.json",
                places: &places,
                places_source: "campaign:places.txt",
                engine: base.engine.clone(),
                function_words: base.function_words.clone(),
                crop_seed: base.crop_seed,
                k_crops: base.k_crops,
                qe: Some((&qe_path, qe.clone())),
            })
            .unwrap();
            assert_eq!(job.qe.as_ref(), Some(&qe));
            assert!(damage(&dir, Some(&job)).is_none());
            dirs.push(dir);
            jobs.push(job);
        }
        let bundle_dir = tmp.path().join("bundle");
        let bundle = write_bundle(&bundle_dir, &dirs).unwrap();
        assert_eq!(bundle.qe.as_ref(), Some(&qe));
        assert!(damage(&bundle_dir, Some(&bundle)).is_none());
        assert_eq!(
            std::fs::read(bundle_dir.join(QE_FILE)).unwrap(),
            b"resource bytes"
        );
        // A page without the file starts a bundle of its own.
        let plain = tmp.path().join("pages").join("sn1_1900-01-01_ed-1_seq-3");
        write_unit(&WriteUnit {
            dir: &plain,
            doc_id: "sn1_1900-01-01_ed-1_seq-3",
            group: &base.group,
            language: &base.language,
            page: &page,
            page_source: "local:page.jpg",
            alto: &alto,
            alto_source: "local:loc.xml",
            texts: &texts,
            texts_source: "local:texts.json",
            places: &places,
            places_source: "campaign:places.txt",
            engine: base.engine.clone(),
            function_words: base.function_words.clone(),
            crop_seed: base.crop_seed,
            k_crops: base.k_crops,
            qe: None,
        })
        .unwrap();
        assert!(write_bundle(&tmp.path().join("mixed"), &[dirs[0].clone(), plain]).is_err());
        let units = tmp.path().join("units");
        let docs: Vec<String> = ["1", "2", "3"]
            .iter()
            .map(|n| format!("sn1_1900-01-01_ed-1_seq-{n}"))
            .collect();
        let made = pack_bundles(
            &tmp.path().join("pages"),
            &units,
            20,
            &docs,
            &BTreeSet::new(),
            0,
        )
        .unwrap();
        assert_eq!(made, 2);
        // A unit whose qe.bin is not the job's is damaged and staged again.
        std::fs::remove_file(dirs[0].join(QE_FILE)).unwrap();
        std::fs::write(dirs[0].join(QE_FILE), b"other bytes").unwrap();
        assert!(damage(&dirs[0], Some(&jobs[0])).is_some());
    }
}
