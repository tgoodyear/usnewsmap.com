//! Fetch each page's JPEG and ALTO from LoC (or the scan archive), join the
//! texts exported for it, and write its unit.
//!
//! Scans are fetched through [`crate::scans::Sources`]: the scan archive
//! first when configured, then LoC's batch route (BagIt `manifest-md5.txt`
//! and issue METS, never `www.loc.gov`'s item API) for the page's IIIF
//! JPEG and the storage service for its ALTO next to the JP2. When the
//! hand-off container's `scans/` holds the page (`--scans-dir`), it is read
//! from there and LoC is not asked.
//!
//! `texts.json` comes from `--texts-dir/<doc_id>.json`, written by the
//! hand-off job's `texts-export` step from the curated store (American
//! Stories' text and regions, our OCR where we have it); LoC's text is
//! added here from the ALTO, so every reader is in one file. An
//! `<doc_id>.ours.xml` next to it (our ALTO) is kept in the unit directory
//! as `ours.xml` for `collect`; it is never sent to hosts. `places.txt` is
//! the campaign's, the same file for every unit; the function words are
//! the page's language's, from `--function-words-dir/<lang>.txt`.
use super::{
    check_no_hold, units, PageSpec, ALTO_FILE, JOB_FILE, PAGE_FILE, PLACES_FILE, TEXTS_FILE,
};
use crate::{assess::SampleRow, atomic, read};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::{
    boinc::is_sha256,
    quality::{
        Engine, PageSize, TextReader, Texts, MAX_ALTO, MAX_BUNDLE_PAGES, MAX_PLACES, MAX_SCAN,
        MAX_TEXTS,
    },
};
use usnm_ocr_worker::loc::FetchError;

mod inputs;
mod texts_export;
mod unit;

pub use inputs::{places_txt, read_places, read_words, write_function_words};
pub use texts_export::{texts_export, TextsExportOptions, TextsExportSummary};
use unit::pack_bundles;
pub use unit::{local_unit, write_bundle, write_unit, LocalUnit, WriteUnit};

pub const MIN_GAP: Duration = Duration::from_secs(5);
/// Our ALTO in the unit directory (server side only).
pub const OURS_ALTO_FILE: &str = "ours.xml";
/// The sample row the page came from, when staged from a sample.
pub const SAMPLE_FILE: &str = "sample.json";
/// The LoC reader's name and source in `texts.json`.
pub const LOC_READER: &str = "loc";
pub const LOC_SOURCE: &str = "loc-alto";
/// The words `ocr-local`'s `loc_text` column holds: why we OCRed the page
/// (the queue message's reason, `bulk` or `user`, as
/// `usnm_ocr_contract::message::Reason` names them; `priority` for the
/// queue's own name; or the Japanese job's reading of LoC's text). The
/// column is a label, never LoC's text, but its name invites the mistake:
/// v1-wave6 was staged with `loc-hist` as `bulk` on all 10,000 pages (#1079).
pub const LOC_TEXT_LABELS: [&str; 8] = [
    "bulk", "user", "priority", "missing", "empty", "short", "garbled", "mixed",
];

#[derive(Debug, Clone)]
pub struct Options {
    /// The pages, as `group:lang:doc_id:batch[+batch...]`.
    pub pages: Vec<PageSpec>,
    /// Or a sample (`quality sample`): its rows become pages of `group`.
    pub sample: Option<PathBuf>,
    pub group: String,
    pub root: PathBuf,
    pub engine: Engine,
    /// The hand-off job's `texts/` export, one `<doc_id>.json` per page.
    pub texts_dir: PathBuf,
    /// `surya collect`'s export (#968): `<doc_id>.surya.json`, the page's
    /// Surya text as the reader `ours-surya`, joined into the unit's texts
    /// when `--readers` names it.
    pub surya_texts: Option<PathBuf>,
    /// The hand-off container's `scans/`, when the pages were fetched by
    /// the node: `<doc_id>.jpg` and `<doc_id>.xml`.
    pub scans_dir: Option<PathBuf>,
    /// The campaign's place names: a `.txt` (one per line) or the
    /// catalog's `places.json`.
    pub places: PathBuf,
    /// `<lang>.txt` per language, one word per line.
    pub function_words_dir: PathBuf,
    /// The readers every unit compares, in report order; a page whose
    /// texts lack one is staged without it.
    pub readers: Vec<String>,
    pub crop_seed: u64,
    pub k_crops: u32,
    pub gap: Duration,
    pub max_pages: usize,
    pub bundle_size: usize,
    pub scan_archive: Option<String>,
    pub batch_cache: Option<PathBuf>,
}

#[derive(Debug, Default)]
pub struct Summary {
    pub staged: usize,
    pub missing: usize,
    pub already: usize,
    /// Pages whose export has no text but LoC's.
    pub loc_only: usize,
    /// Multi-page bundles written when `bundle_size > 1`.
    pub bundles: usize,
}

/// The pages to stage: `--page` specs, plus the sample's rows as pages of
/// the run's group, with the row kept for the unit's `sample.json`.
pub fn pages_of(opts: &Options) -> anyhow::Result<Vec<(PageSpec, Option<SampleRow>)>> {
    let mut out: Vec<(PageSpec, Option<SampleRow>)> =
        opts.pages.iter().cloned().map(|p| (p, None)).collect();
    if let Some(sample) = &opts.sample {
        usnm_ocr_contract::quality::check_name(&opts.group, "group")?;
        for row in crate::assess::read_sample(sample)? {
            crate::assess::check_doc_id(&row.doc_id)?;
            crate::surya::check_word(&row.lang, "language")?;
            out.push((
                PageSpec {
                    batches: vec![(row.batch.clone(), row.batch_version)],
                    doc_id: row.doc_id.clone(),
                    group: opts.group.clone(),
                    lang: row.lang.clone(),
                },
                Some(row),
            ));
        }
    }
    ensure!(!out.is_empty(), "no pages: give --page or --sample");
    Ok(out)
}

/// Stages up to `max_pages` units; stops at the first LoC refusal.
pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    ensure!(opts.gap >= MIN_GAP, "--gap-secs must be at least 5");
    let src = crate::scans::Sources::new(
        crate::scans::loc_client(opts.gap, opts.batch_cache.clone())?,
        crate::scans::archive(opts.scan_archive.as_deref())?,
        MAX_SCAN,
    );
    run_with(opts, &src).await
}

/// Stages up to `max_pages` units using `src`; stops at the first LoC refusal.
pub async fn run_with(opts: &Options, src: &crate::scans::Sources) -> anyhow::Result<Summary> {
    let bundle_size = if opts.bundle_size == 0 {
        1
    } else {
        opts.bundle_size
    };
    ensure!(
        (1..=MAX_BUNDLE_PAGES).contains(&bundle_size),
        "--bundle-size must be 1 to {MAX_BUNDLE_PAGES}"
    );
    ensure!(
        opts.engine
            .image
            .strip_prefix("sha256:")
            .is_some_and(is_sha256),
        "--engine-image must be the worker image's full sha256 digest"
    );
    ensure!(!opts.readers.is_empty(), "--readers must name at least one");
    check_no_hold(&opts.root)?;
    let pages = pages_of(opts)?;
    let units_dir = units(&opts.root);
    std::fs::create_dir_all(&units_dir)?;
    let page_dir = if bundle_size > 1 {
        let p = opts.root.join("pages");
        std::fs::create_dir_all(&p)?;
        p
    } else {
        units_dir.clone()
    };
    let places = read_places(&opts.places)?;
    ensure!(places.len() <= MAX_PLACES, "places over size limit");
    let mut function_words: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut summary = Summary::default();
    for (p, row) in &pages {
        if summary.staged + summary.missing >= opts.max_pages {
            break;
        }
        usnm_ocr_contract::quality::check_name(&p.group, "group")?;
        crate::surya::check_word(&p.lang, "language")?;
        let unit = page_dir.join(&p.doc_id);
        if unit.join(JOB_FILE).exists() || unit.join("missing").exists() {
            summary.already += 1;
            continue;
        }
        if !function_words.contains_key(&p.lang) {
            let path = opts.function_words_dir.join(format!("{}.txt", p.lang));
            ensure!(
                path.exists(),
                "no function words for {}: {} (usnm-boinc quality function-words --lang {} --out-dir {})",
                p.lang,
                path.display(),
                p.lang,
                opts.function_words_dir.display()
            );
            function_words.insert(p.lang.clone(), read_words(&path)?);
        }
        std::fs::create_dir_all(&unit)?;
        if let Some(row) = row {
            atomic(&unit.join(SAMPLE_FILE), &serde_json::to_vec(row)?)?;
        }
        let fw = function_words[&p.lang].clone();
        match page(src, p, &unit, opts, &places, fw).await {
            Ok(Some(loc_only)) => {
                summary.staged += 1;
                if loc_only {
                    summary.loc_only += 1;
                }
            }
            Ok(None) => summary.missing += 1,
            Err(e) => {
                atomic(
                    &crate::assess::hold_path(&opts.root),
                    format!("{}: {e:#}\n", p.doc_id).as_bytes(),
                )?;
                return Err(e.context(format!("stopped at {}", p.doc_id)));
            }
        }
    }
    if bundle_size > 1 {
        summary.bundles = pack_bundles(&page_dir, &units_dir, bundle_size)?;
    }
    Ok(summary)
}

fn missing(unit: &Path, why: &str) -> anyhow::Result<Option<bool>> {
    atomic(&unit.join("missing"), format!("{why}\n").as_bytes())?;
    Ok(None)
}

/// The page's JPEG and ALTO with their sources.
struct Scan {
    page: Vec<u8>,
    page_source: String,
    alto: Vec<u8>,
    alto_source: String,
}

/// The ALTO's URL on the storage service for a page's IIIF base: the base
/// is `service:<path with : for />` under the IIIF root, and the ALTO sits
/// next to the JP2 at `<path>.xml`.
pub fn alto_url_of(iiif_base: &str) -> Option<String> {
    alto_url_of_roots(
        usnm_ocr_worker::loc::IIIF_ROOT,
        usnm_ocr_worker::loc::STORAGE_SERVICE_ROOT,
        iiif_base,
    )
}

pub fn alto_url_of_roots(iiif_root: &str, storage_root: &str, iiif_base: &str) -> Option<String> {
    let path = iiif_base
        .strip_prefix(iiif_root)?
        .strip_prefix("service:")?;
    if path.is_empty() || path.contains("..") {
        return None;
    }
    Some(format!(
        "{}{}.xml",
        storage_root.strip_suffix("ndnp/").unwrap_or(storage_root),
        path.replace(':', "/")
    ))
}

/// The hand-off container's copy of the page, when it has both files.
fn from_scans(scans: &Path, doc_id: &str) -> anyhow::Result<Option<Scan>> {
    let jpg = scans.join(format!("{doc_id}.jpg"));
    let xml = scans.join(format!("{doc_id}.xml"));
    if !jpg.exists() || !xml.exists() {
        return Ok(None);
    }
    Ok(Some(Scan {
        page: read(&jpg, MAX_SCAN)?,
        page_source: format!("handoff:scans/{doc_id}.jpg"),
        alto: read(&xml, MAX_ALTO)?,
        alto_source: format!("handoff:scans/{doc_id}.xml"),
    }))
}

/// The page from the scan archive or LoC's batch route. `Ok(Err(why))` is a
/// page LoC does not have.
async fn from_loc(
    src: &crate::scans::Sources,
    key: &PageKey,
    batches: &[(String, u16)],
) -> anyhow::Result<Result<Scan, String>> {
    let jpg = match src.jpeg(key, batches).await {
        Ok(Some(s)) => s,
        Ok(None) => {
            return Ok(Err(
                "not in the scan archive, and none of its batches places it on tile.loc.gov".into(),
            ))
        }
        Err(FetchError::Permanent(m)) => return Ok(Err(m)),
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    ensure!(jpg.bytes.len() <= MAX_SCAN, "JPEG over size limit");
    let alto_url = alto_url_of_roots(&src.iiif_root, &src.storage_root, &jpg.iiif_base)
        .context("the IIIF base has no storage path")?;
    let alto = match src.loc.storage_file(&alto_url, &key.lccn, MAX_ALTO).await {
        Ok(Some(b)) => b,
        Ok(None) => return Ok(Err(format!("no ALTO at {alto_url}"))),
        Err(FetchError::Permanent(m)) => return Ok(Err(m)),
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    Ok(Ok(Scan {
        page: jpg.bytes.to_vec(),
        page_source: jpg.url,
        alto: alto.to_vec(),
        alto_source: alto_url,
    }))
}

/// `Some(loc_only)` when staged, `None` when LoC lacks the page.
async fn page(
    src: &crate::scans::Sources,
    p: &PageSpec,
    unit: &Path,
    opts: &Options,
    places: &str,
    function_words: Vec<String>,
) -> anyhow::Result<Option<bool>> {
    let key = PageKey::from_doc_id(&p.doc_id)?;
    let scan = match opts
        .scans_dir
        .as_deref()
        .map(|d| from_scans(d, &p.doc_id))
        .transpose()?
        .flatten()
    {
        Some(s) => s,
        None => match from_loc(src, &key, &p.batches).await? {
            Ok(s) => s,
            Err(why) => return missing(unit, &why),
        },
    };
    // The export, with LoC's text added from the ALTO.
    let export_path = opts.texts_dir.join(format!("{}.json", p.doc_id));
    let mut exported: Texts = if export_path.exists() {
        serde_json::from_slice(&read(&export_path, MAX_TEXTS)?)
            .with_context(|| format!("reading {}", export_path.display()))?
    } else {
        Texts {
            page: None,
            readers: vec![],
        }
    };
    check_loc_texts(&exported).with_context(|| format!("{}", export_path.display()))?;
    if let Some(reader) = surya_reader(opts.surya_texts.as_deref(), &p.doc_id)? {
        exported.readers.retain(|r| r.name != reader.name);
        exported.readers.push(reader);
    }
    let (width, height) = image::load_from_memory(&scan.page)
        .context("decoding page.jpg")?
        .to_luma8()
        .dimensions();
    let alto = usnm_ocr_quality::slot::normalize_alto_ns(&scan.alto);
    let parsed = super::parse_alto(&alto, f64::from(width), f64::from(height))?;
    let texts = join_texts(
        exported,
        &usnm_alto::text(&parsed),
        &opts.readers,
        width,
        height,
    );
    let loc_only = texts.readers.len() == 1;
    let ours = opts.texts_dir.join(format!("{}.ours.xml", p.doc_id));
    let job = write_unit(&WriteUnit {
        dir: unit,
        doc_id: &p.doc_id,
        group: &p.group,
        language: &p.lang,
        page: &scan.page,
        page_source: &scan.page_source,
        alto: &alto,
        alto_source: &scan.alto_source,
        texts: &texts,
        texts_source: &format!("handoff:texts/{}.json", p.doc_id),
        places,
        places_source: "campaign:places.txt",
        engine: opts.engine.clone(),
        function_words,
        crop_seed: opts.crop_seed,
        k_crops: opts.k_crops,
    })?;
    if ours.exists() {
        atomic(&unit.join(OURS_ALTO_FILE), &read(&ours, MAX_ALTO)?)?;
    }
    debug_assert_eq!(job.doc_id, p.doc_id);
    Ok(Some(loc_only))
}

/// The page's `ours-surya` reader from `surya collect`'s export, when the
/// directory is given and the page has one.
pub fn surya_reader(dir: Option<&Path>, doc_id: &str) -> anyhow::Result<Option<TextReader>> {
    let Some(dir) = dir else {
        return Ok(None);
    };
    let path = dir.join(crate::surya::collect::export_name(doc_id));
    if !path.exists() {
        return Ok(None);
    }
    let reader: TextReader = serde_json::from_slice(&read(&path, MAX_TEXTS)?)
        .with_context(|| format!("reading {}", path.display()))?;
    anyhow::ensure!(
        reader.name == crate::surya::collect::READER,
        "{}: reader is {:?}, not {}",
        path.display(),
        reader.name,
        crate::surya::collect::READER
    );
    Ok(Some(reader))
}

/// Refuses an exported LoC reader (`loc` or `loc-hist`) whose text is one
/// of [`LOC_TEXT_LABELS`]: that is `ocr-local`'s `loc_text` label put where
/// LoC's text belongs (#1079), and staging it would compare every reader
/// against a single word. Stage stops at the page and writes `HOLD`.
pub fn check_loc_texts(exported: &Texts) -> anyhow::Result<()> {
    for r in &exported.readers {
        if r.name != LOC_READER && r.name != "loc-hist" {
            continue;
        }
        let t = r.text.trim();
        if let Some(label) = LOC_TEXT_LABELS.iter().find(|l| t.eq_ignore_ascii_case(l)) {
            anyhow::bail!(
                "the {} text is {label:?}, an ocr-local loc_text label (why the page was OCRed), not LoC's text",
                r.name
            );
        }
    }
    Ok(())
}

/// The unit's `texts.json`: LoC's text first, then the exported readers,
/// in `order`; readers not in `order`, and names in `order` the export
/// lacks, are left out. The page size is the export's (American Stories'
/// regions are in its scan's pixels) or the JPEG's.
pub fn join_texts(exported: Texts, loc_text: &str, order: &[String], w: u32, h: u32) -> Texts {
    let mut all = vec![TextReader {
        alto: false,
        name: LOC_READER.into(),
        regions: vec![],
        source: LOC_SOURCE.into(),
        text: loc_text.to_owned(),
    }];
    all.extend(exported.readers.into_iter().map(|mut r| {
        if (r.name == LOC_READER || r.name == "loc-hist")
            && (r.text.trim().is_empty() || r.text.trim().eq_ignore_ascii_case("none"))
        {
            r.text = loc_text.to_owned();
        }
        r.source = usnm_ocr_contract::quality::normalize_source(&r.source);
        r.regions
            .retain(|g| g.rect[0] < g.rect[2] && g.rect[1] < g.rect[3]);
        for g in &mut r.regions {
            g.legibility = g
                .legibility
                .take()
                .filter(|l| ["legible", "borderline", "illegible"].contains(&l.as_str()));
        }
        r
    }));
    if !all.iter().any(|r| r.name == "loc-hist") {
        all.push(TextReader {
            alto: false,
            name: "loc-hist".into(),
            regions: vec![],
            source: LOC_SOURCE.into(),
            text: loc_text.to_owned(),
        });
    }
    let readers = order
        .iter()
        .filter_map(|name| all.iter().find(|r| &r.name == name).cloned())
        .collect();
    Texts {
        page: exported.page.or(Some(PageSize {
            height: h,
            width: w,
        })),
        readers,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use usnm_ocr_contract::quality::Job;

    #[test]
    fn alto_url_follows_the_iiif_base() {
        let base = "https://tile.loc.gov/image-services/iiif/service:ndnp:dlc:batch_dlc_abc_ver01:data:sn83045462:00280654021:1890050301:0001";
        assert_eq!(
            alto_url_of(base).unwrap(),
            "https://tile.loc.gov/storage-services/service/ndnp/dlc/batch_dlc_abc_ver01/data/sn83045462/00280654021/1890050301/0001.xml"
        );
        assert_eq!(
            alto_url_of_roots(
                "http://example.test/iiif/",
                "http://example.test/storage-services/service/ndnp/",
                "http://example.test/iiif/service:ndnp:xx:batch_xx_test_ver01:data:sn83045462:001:1890050301:0001"
            )
            .unwrap(),
            "http://example.test/storage-services/service/ndnp/xx/batch_xx_test_ver01/data/sn83045462/001/1890050301/0001.xml"
        );
        assert!(alto_url_of("https://example.org/x").is_none());
    }

    #[test]
    fn places_from_the_catalog_are_names_and_variants() {
        let places: Vec<usnm_ingest::catalog::Place> = serde_json::from_str(
            r#"[{"id":"a","ordinal":1,"name":"Troy","state":"NY","lat":1.0,"lon":2.0,"precision":"city",
                 "variants":[{"name":"Troy City","rule":"x_city"}]},
                {"id":"b","ordinal":2,"name":"Albany","state":"NY","lat":1.0,"lon":2.0,"precision":"city"}]"#,
        )
        .unwrap();
        assert_eq!(places_txt(&places), "Albany\nTroy\nTroy City\n");
    }

    #[test]
    fn function_words_come_from_the_embedded_lists() {
        let tmp = tempfile::tempdir().unwrap();
        write_function_words(&["eng".into(), "deu".into()], 100, tmp.path()).unwrap();
        let eng = read_words(&tmp.path().join("eng.txt")).unwrap();
        assert_eq!(eng.len(), 100);
        assert!(eng.contains(&"the".to_string()));
        let deu = read_words(&tmp.path().join("deu.txt")).unwrap();
        assert!(deu.contains(&"und".to_string()));
        assert!(write_function_words(&["xxx".into()], 100, tmp.path()).is_err());
    }

    #[test]
    fn loc_text_labels_are_refused_as_loc_text() {
        let texts = |name: &str, text: &str| Texts {
            page: None,
            readers: vec![TextReader {
                alto: false,
                name: name.into(),
                regions: vec![],
                source: name.into(),
                text: text.into(),
            }],
        };
        for (name, text) in [
            ("loc-hist", "bulk"),
            ("loc", " Priority\n"),
            ("loc-hist", "missing"),
        ] {
            let e = check_loc_texts(&texts(name, text)).unwrap_err().to_string();
            assert!(e.contains(name) && e.contains("loc_text label"), "{e}");
        }
        // A text that only starts with a label, another reader's, and the
        // empty or `none` text join_texts fills from the ALTO all pass.
        for (name, text) in [
            ("loc-hist", "bulk carrier sails"),
            ("as", "bulk"),
            ("loc-hist", ""),
            ("loc", "none"),
        ] {
            check_loc_texts(&texts(name, text)).unwrap();
        }
    }

    #[test]
    fn joined_texts_put_loc_first_and_follow_the_order() {
        let exported = Texts {
            page: Some(PageSize {
                height: 20,
                width: 10,
            }),
            readers: vec![
                TextReader {
                    alto: true,
                    name: "ours-tesseract".into(),
                    regions: vec![],
                    source: "ocr-local:usnm-tesseract".into(),
                    text: "b".into(),
                },
                TextReader {
                    alto: false,
                    name: "as".into(),
                    regions: vec![],
                    source: "american-stories".into(),
                    text: "a".into(),
                },
            ],
        };
        let t = join_texts(
            exported,
            "l",
            &["loc".into(), "as".into(), "missing".into()],
            1,
            2,
        );
        let names: Vec<&str> = t.readers.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["loc", "as"]);
        assert_eq!(t.page.unwrap().width, 10);
        let t = join_texts(
            Texts {
                page: None,
                readers: vec![],
            },
            "l",
            &["loc".into()],
            1,
            2,
        );
        assert_eq!(t.page.unwrap().height, 2);
    }

    /// Stages one page from a hand-off `scans/` copy, without LoC.
    #[tokio::test]
    async fn stages_from_the_handoff_scans() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = tmp.path().join("fixture");
        let j = crate::quality::work::tests::unit(&fixture);
        let doc_id = "sn83045462_1890-05-03_ed-1_seq-1";
        let scans = tmp.path().join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        std::fs::copy(fixture.join(PAGE_FILE), scans.join(format!("{doc_id}.jpg"))).unwrap();
        std::fs::copy(fixture.join(ALTO_FILE), scans.join(format!("{doc_id}.xml"))).unwrap();
        let texts_dir = tmp.path().join("texts");
        std::fs::create_dir_all(&texts_dir).unwrap();
        let fixture_texts: Texts =
            serde_json::from_slice(&std::fs::read(fixture.join(TEXTS_FILE)).unwrap()).unwrap();
        let export = Texts {
            page: fixture_texts.page,
            readers: fixture_texts
                .readers
                .iter()
                .filter(|r| r.name != "loc")
                .cloned()
                .collect(),
        };
        std::fs::write(
            texts_dir.join(format!("{doc_id}.json")),
            serde_json::to_vec(&export).unwrap(),
        )
        .unwrap();
        std::fs::write(
            texts_dir.join(format!("{doc_id}.ours.xml")),
            std::fs::read(fixture.join(ALTO_FILE)).unwrap(),
        )
        .unwrap();
        let fw = tmp.path().join("fw");
        write_function_words(&["eng".into()], 100, &fw).unwrap();
        std::fs::write(tmp.path().join("places.txt"), "Albany\n").unwrap();
        let sample = tmp.path().join("sample.jsonl");
        let row = SampleRow {
            batch: "dlc_abc".into(),
            batch_version: 1,
            crash_weight: 1.0,
            damage: "low".into(),
            damage_rate: 0.01,
            decade: 1890,
            doc_id: doc_id.into(),
            lang: "eng".into(),
            mode: usnm_ocr_contract::assess::Mode::Ocr,
            prefix: "dlc".into(),
            stratum_pages: 10,
            weight: 10.0,
            year: 1890,
        };
        std::fs::write(
            &sample,
            format!("{}\n", serde_json::to_string(&row).unwrap()),
        )
        .unwrap();
        let opts = Options {
            pages: vec![],
            sample: Some(sample),
            group: "probe".into(),
            root: tmp.path().join("root"),
            engine: j.engine.clone(),
            texts_dir,
            surya_texts: None,
            scans_dir: Some(scans),
            places: tmp.path().join("places.txt"),
            function_words_dir: fw,
            readers: vec!["loc".into(), "as".into(), "ours-tesseract".into()],
            crop_seed: 522,
            k_crops: 12,
            gap: MIN_GAP,
            max_pages: 10,
            bundle_size: 1,
            scan_archive: None,
            batch_cache: None,
        };
        let s = run(&opts).await.unwrap();
        assert_eq!((s.staged, s.missing, s.already, s.loc_only), (1, 0, 0, 0));
        let unit = units(&opts.root).join(doc_id);
        let job: Job =
            serde_json::from_slice(&std::fs::read(unit.join(JOB_FILE)).unwrap()).unwrap();
        job.validate().unwrap();
        assert_eq!(job.doc_id, doc_id);
        assert_eq!(job.group, "probe");
        assert_eq!(job.language, "eng");
        assert_eq!(job.readers.len(), 3);
        assert_eq!(job.readers[0].name, "loc");
        assert_eq!(job.function_words.len(), 100);
        assert_eq!(
            job.inputs.page.source,
            format!("handoff:scans/{doc_id}.jpg")
        );
        assert!(unit.join(OURS_ALTO_FILE).exists());
        assert!(unit.join(SAMPLE_FILE).exists());
        // The unit runs as the worker would.
        let l = crate::quality::work::prepare(&unit).unwrap();
        assert_eq!(
            l.texts.readers[0].text.replace('\n', " "),
            crate::quality::work::tests::TRUTH
        );
        // A second run stages nothing new.
        let s = run(&opts).await.unwrap();
        assert_eq!((s.staged, s.already), (0, 1));
        // With surya collect's export and the reader named, the page's Surya
        // text joins the unit as ours-surya, after the hand-off's readers.
        let surya_dir = tmp.path().join("surya-texts");
        std::fs::create_dir_all(&surya_dir).unwrap();
        std::fs::write(
            surya_dir.join(crate::surya::collect::export_name(doc_id)),
            serde_json::to_vec(&TextReader {
                alto: false,
                name: crate::surya::collect::READER.into(),
                regions: vec![],
                source: "surya:whole:abc".into(),
                text: "The mayor spoke to the council\n".into(),
            })
            .unwrap(),
        )
        .unwrap();
        let with_surya = Options {
            root: tmp.path().join("root-surya"),
            surya_texts: Some(surya_dir.clone()),
            readers: vec![
                "loc".into(),
                "as".into(),
                "ours-tesseract".into(),
                "ours-surya".into(),
            ],
            ..opts.clone()
        };
        let s = run(&with_surya).await.unwrap();
        assert_eq!((s.staged, s.loc_only), (1, 0));
        let unit = units(&with_surya.root).join(doc_id);
        let texts: Texts =
            serde_json::from_slice(&std::fs::read(unit.join(TEXTS_FILE)).unwrap()).unwrap();
        let names: Vec<&str> = texts.readers.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["loc", "as", "ours-tesseract", "ours-surya"]);
        let surya = texts.readers.last().unwrap();
        assert_eq!(surya.source, "surya:whole:abc");
        assert!(surya.text.contains("mayor"));
        let job: Job =
            serde_json::from_slice(&std::fs::read(unit.join(JOB_FILE)).unwrap()).unwrap();
        assert_eq!(job.readers.len(), 4);
        // Not named in --readers: left out, as any exported reader is.
        let unnamed = Options {
            root: tmp.path().join("root-unnamed"),
            surya_texts: Some(surya_dir),
            ..opts.clone()
        };
        run(&unnamed).await.unwrap();
        let texts: Texts = serde_json::from_slice(
            &std::fs::read(units(&unnamed.root).join(doc_id).join(TEXTS_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(texts.readers.len(), 3);
        // v1-wave6 (#1079): the export's loc-hist reader carried ocr-local's
        // loc_text label, "bulk", as its text. Stage stops at the page with a
        // HOLD instead of comparing every reader against one word.
        let wave6_texts = tmp.path().join("texts-wave6");
        std::fs::create_dir_all(&wave6_texts).unwrap();
        let mut bulk = export.clone();
        bulk.readers.insert(
            0,
            TextReader {
                alto: false,
                name: "loc-hist".into(),
                regions: vec![],
                source: "loc-hist".into(),
                text: "bulk".into(),
            },
        );
        std::fs::write(
            wave6_texts.join(format!("{doc_id}.json")),
            serde_json::to_vec(&bulk).unwrap(),
        )
        .unwrap();
        let wave6 = Options {
            root: tmp.path().join("root-wave6"),
            texts_dir: wave6_texts.clone(),
            readers: vec!["loc-hist".into(), "as".into(), "ours-tesseract".into()],
            ..opts.clone()
        };
        let err = run(&wave6).await.unwrap_err();
        assert!(format!("{err:#}").contains("\"bulk\""), "{err:#}");
        let hold = std::fs::read_to_string(crate::assess::hold_path(&wave6.root)).unwrap();
        assert!(hold.starts_with(doc_id) && hold.contains("loc_text label"));
        assert!(!units(&wave6.root).join(doc_id).join(JOB_FILE).exists());
        // LoC's real text in the export stages as before.
        bulk.readers[0].text = "The mayor spoke to the council".into();
        std::fs::write(
            wave6_texts.join(format!("{doc_id}.json")),
            serde_json::to_vec(&bulk).unwrap(),
        )
        .unwrap();
        let fixed = Options {
            root: tmp.path().join("root-wave6-fixed"),
            ..wave6.clone()
        };
        assert_eq!(run(&fixed).await.unwrap().staged, 1);
        // A page the scans lack would go to LoC; a language without a list stops first.
        let bad = Options {
            pages: vec!["probe:xxx:sn83045462_1890-05-04_ed-1_seq-1:dlc_abc_ver01"
                .parse()
                .unwrap()],
            sample: None,
            ..opts.clone()
        };
        assert!(run(&bad)
            .await
            .unwrap_err()
            .to_string()
            .contains("function words"));
    }

    #[tokio::test]
    async fn stages_from_batch_route_without_item_api() {
        use usnm_ocr_worker::{
            loc::{iiif_base, image_url, ImageSize, LocClient, LocConfig},
            standin::{Page as SPage, StandIn},
        };

        let tmp = tempfile::tempdir().unwrap();
        let fixture = tmp.path().join("fixture");
        let j = crate::quality::work::tests::unit(&fixture);
        let page_bytes = std::fs::read(fixture.join(PAGE_FILE)).unwrap();
        let alto_bytes = std::fs::read(fixture.join(ALTO_FILE)).unwrap();
        let doc_id = "sn83045462_1890-05-03_ed-1_seq-1";

        let s = StandIn::start().await;
        let batch_path = "/storage-services/service/ndnp/xx/batch_xx_test_ver01/";
        s.add_issue(
            batch_path,
            "sn83045462",
            "001",
            "1890050301",
            &[SPage {
                seq: 1,
                frame: "0042",
                jp2: b"jp2",
            }],
        );
        let jp2_url = format!(
            "{}{batch_path}data/sn83045462/001/1890050301/0042.jp2",
            s.root
        );
        let base = iiif_base(&s.iiif_root(), &jp2_url).unwrap();
        let jpg_url = image_url(&base, ImageSize::Full);
        s.put(jpg_url.strip_prefix(&s.root).unwrap(), page_bytes);
        s.put(
            &format!("{batch_path}data/sn83045462/001/1890050301/0042.xml"),
            alto_bytes,
        );

        let loc = LocClient::new(LocConfig {
            api_gap: Duration::from_millis(1),
            tile_gap: Duration::from_millis(1),
            storage_gap: Duration::from_millis(1),
            attempts: 1,
            backoff: Duration::from_millis(1),
            api_root: s.root.clone(),
            iiif_root: s.iiif_root(),
            storage_root: s.storage_root(),
            batches_root: s.batches_root(),
            item_probes: false,
            html_is_block: true,
            ..LocConfig::default()
        })
        .unwrap();
        let src = crate::scans::Sources {
            loc,
            archive: None,
            iiif_root: s.iiif_root(),
            storage_root: s.storage_root(),
            max: MAX_SCAN,
        };

        let texts_dir = tmp.path().join("texts");
        std::fs::create_dir_all(&texts_dir).unwrap();
        let fw = tmp.path().join("fw");
        write_function_words(&["eng".into()], 100, &fw).unwrap();
        std::fs::write(tmp.path().join("places.txt"), "Albany\n").unwrap();

        let opts = Options {
            pages: vec![format!("smoke:eng:{doc_id}:xx_test_ver01").parse().unwrap()],
            sample: None,
            group: "smoke".into(),
            root: tmp.path().join("root"),
            engine: j.engine.clone(),
            texts_dir,
            surya_texts: None,
            scans_dir: None,
            places: tmp.path().join("places.txt"),
            function_words_dir: fw,
            readers: vec!["loc".into()],
            crop_seed: 522,
            k_crops: 12,
            gap: MIN_GAP,
            max_pages: 10,
            bundle_size: 1,
            scan_archive: None,
            batch_cache: None,
        };

        let sum = run_with(&opts, &src).await.unwrap();
        assert_eq!(
            (sum.staged, sum.missing, sum.already, sum.loc_only),
            (1, 0, 0, 1)
        );
        assert_eq!(s.hits("/item/"), 0);
        assert_eq!(s.hits("manifest-md5.txt"), 1);
        assert_eq!(s.hits("1890050301.xml"), 1);
        assert_eq!(s.hits("default.jpg"), 1);
        assert_eq!(s.hits("0042.xml"), 1);
        let unit = units(&opts.root).join(doc_id);
        let job: Job =
            serde_json::from_slice(&std::fs::read(unit.join(JOB_FILE)).unwrap()).unwrap();
        job.validate().unwrap();
    }

    #[tokio::test]
    async fn stages_bundled_units_and_normalizes_sources_and_regions() {
        let tmp = tempfile::tempdir().unwrap();
        let fixture = tmp.path().join("fixture");
        let j = crate::quality::work::tests::unit(&fixture);
        let doc1 = "sn83045462_1890-05-03_ed-1_seq-1";
        let doc2 = "sn83045462_1890-05-03_ed-1_seq-2";
        let scans = tmp.path().join("scans");
        std::fs::create_dir_all(&scans).unwrap();
        let texts_dir = tmp.path().join("texts");
        std::fs::create_dir_all(&texts_dir).unwrap();
        for doc_id in [doc1, doc2] {
            std::fs::copy(fixture.join(PAGE_FILE), scans.join(format!("{doc_id}.jpg"))).unwrap();
            std::fs::copy(fixture.join(ALTO_FILE), scans.join(format!("{doc_id}.xml"))).unwrap();
            let raw = serde_json::json!({
                "page": {"width": 1000, "height": 2000},
                "readers": [
                    {
                        "name": "as",
                        "source": "american stories v1",
                        "text": "American Stories text",
                        "regions": [
                            {"rect": [10, 20, 300, 400], "kind": "article", "legibility": "legible"},
                            {"rect": [500, 20, 300, 400], "kind": "article", "legibility": "unknown"}
                        ]
                    },
                    {
                        "name": "ours-tesseract",
                        "source": "ocr-local:tesseract 5.5.0 --oem 1",
                        "text": "Our Tesseract text",
                        "alto": false
                    }
                ]
            });
            std::fs::write(
                texts_dir.join(format!("{doc_id}.json")),
                serde_json::to_vec(&raw).unwrap(),
            )
            .unwrap();
        }
        let fw = tmp.path().join("fw");
        write_function_words(&["eng".into()], 100, &fw).unwrap();
        std::fs::write(tmp.path().join("places.txt"), "Albany\n").unwrap();
        let sample = tmp.path().join("sample.jsonl");
        let mut sample_lines = String::new();
        for doc_id in [doc1, doc2] {
            let row = SampleRow {
                batch: "dlc_abc".into(),
                batch_version: 1,
                crash_weight: 1.0,
                damage: "low".into(),
                damage_rate: 0.01,
                decade: 1890,
                doc_id: doc_id.into(),
                lang: "eng".into(),
                mode: usnm_ocr_contract::assess::Mode::Ocr,
                prefix: "dlc".into(),
                stratum_pages: 10,
                weight: 10.0,
                year: 1890,
            };
            sample_lines.push_str(&serde_json::to_string(&row).unwrap());
            sample_lines.push('\n');
        }
        std::fs::write(&sample, sample_lines).unwrap();
        let opts = Options {
            pages: vec![],
            sample: Some(sample),
            group: "v1-wave5".into(),
            root: tmp.path().join("root"),
            engine: j.engine.clone(),
            texts_dir,
            surya_texts: None,
            scans_dir: Some(scans),
            places: tmp.path().join("places.txt"),
            function_words_dir: fw,
            readers: vec!["loc".into(), "as".into(), "ours-tesseract".into()],
            crop_seed: 522,
            k_crops: 12,
            gap: MIN_GAP,
            max_pages: 10,
            bundle_size: 2,
            scan_archive: None,
            batch_cache: None,
        };
        let s = run(&opts).await.unwrap();
        assert_eq!((s.staged, s.bundles), (2, 1));
        let bundle_dir = units(&opts.root).join(format!("bundle-00001-{doc1}"));
        let job: Job =
            serde_json::from_slice(&std::fs::read(bundle_dir.join(JOB_FILE)).unwrap()).unwrap();
        job.validate().unwrap();
        assert_eq!(job.pages.len(), 2);
        assert_eq!(job.pages[0].doc_id, doc1);
        assert_eq!(job.pages[1].doc_id, doc2);
        assert_eq!(job.pages[0].group, "v1-wave5");
        assert_eq!(job.pages[0].readers[1].source, "american;stories;v1");
        assert_eq!(
            job.pages[0].readers[2].source,
            "ocr-local:tesseract;5.5.0;--oem;1"
        );
    }
}
