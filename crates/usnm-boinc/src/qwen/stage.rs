//! Write one `usnm_qwen` unit per page: the page JPEG and the job with the
//! crops, cut from LoC's ALTO with the campaign's seed.
//!
//! The pages come from the `usnm_quality` staging root (`--quality-root`),
//! which already holds `page.jpg` and `loc.xml` for each, so LoC is not
//! asked again; the crop seed and count are read from each page's
//! `quality.json`, so both apps cut the same crops.
use super::{check_no_hold, units, JOB_FILE, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use image::GenericImageView as _;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    quality::{
        BundleSlice, Input, Job as QualityJob, MAX_ALTO, MAX_BUNDLE_SCAN,
        MAX_JOB_JSON as MAX_QUALITY_JOB, MAX_SCAN,
    },
    qwen::{
        BundlePage, Crop, Engine, Job, MAX_BUNDLE_CROPS, MAX_BUNDLE_PAGES, MAX_JOB_JSON,
        PROMPT_VERSION, VERSION,
    },
};
use usnm_ocr_quality::crops;

#[derive(Debug, Clone)]
pub struct Options {
    /// The `usnm_quality` staging root whose units to cut crops from.
    pub quality_root: PathBuf,
    pub root: PathBuf,
    pub engine: Engine,
    pub gpu_timeout_secs: u32,
    pub cpu_timeout_secs: u32,
    pub max_tokens_per_1000px2: u32,
    pub max_pages: usize,
    /// Pack staged pages into multi-page bundle workunits of up to this many pages
    /// (1 to [`MAX_BUNDLE_PAGES`]).
    pub bundle_size: usize,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub staged: usize,
    pub no_crops: usize,
    pub already: usize,
    pub bundles: usize,
    /// Pages whose one-page bundle would not fit in `qwen.json`: marked
    /// `no_bundle` and never published.
    pub no_bundle: usize,
}

/// The source name of a bundle's `page.jpg`.
const BUNDLE_SOURCE: &str = "bundle:page.jpg";
/// The marker on a staged page too large to publish in any bundle.
const NO_BUNDLE: &str = "no_bundle";

/// Writes one unit per staged `usnm_quality` page (those with a
/// `quality.json`), in doc id order, up to `max_pages` new ones. When
/// `bundle_size > 1`, single-page units are staged under `<root>/pages/` and
/// packed into multi-page bundle workunits under `<root>/units/`. A page
/// whose ALTO has no line the recipe can cut gets a `no_crops` marker and
/// no unit. The quality unit's `sample.json`, when there, is copied along
/// for `collect`.
pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    let bundle_size = if opts.bundle_size == 0 {
        1
    } else {
        opts.bundle_size
    };
    ensure!(
        (1..=MAX_BUNDLE_PAGES).contains(&bundle_size),
        "--bundle-size must be 1 to {MAX_BUNDLE_PAGES}"
    );
    super::check_digest(&opts.engine.image, "--engine-image")?;
    super::check_digest(&opts.engine.llama, "--engine-llama")?;
    super::check_digest(&opts.engine.weights, "--engine-weights")?;
    check_no_hold(&opts.root)?;
    let units_dir = units(&opts.root);
    std::fs::create_dir_all(&units_dir)?;
    let page_dir = if bundle_size > 1 {
        let p = opts.root.join("pages");
        std::fs::create_dir_all(&p)?;
        p
    } else {
        units_dir.clone()
    };
    let pages_dir = opts.quality_root.join("pages");
    let src = if pages_dir.exists() {
        pages_dir
    } else {
        crate::quality::units(&opts.quality_root)
    };
    let mut pages: Vec<PathBuf> = std::fs::read_dir(&src)
        .with_context(|| format!("reading {}", src.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(crate::quality::JOB_FILE).exists())
        .collect();
    pages.sort();
    let mut summary = Summary::default();
    for page in pages {
        if summary.staged + summary.no_crops >= opts.max_pages {
            break;
        }
        let name = page
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let unit = page_dir.join(&name);
        if unit.join(JOB_FILE).exists() || unit.join("no_crops").exists() {
            summary.already += 1;
            continue;
        }
        let quality: QualityJob = serde_json::from_slice(&read(
            &page.join(crate::quality::JOB_FILE),
            MAX_QUALITY_JOB,
        )?)
        .with_context(|| format!("{name}: quality.json"))?;
        quality.validate()?;
        let bytes = read(&page.join(crate::quality::PAGE_FILE), MAX_SCAN)?;
        ensure!(
            quality.inputs.page.matches(&bytes),
            "{name}: staged page changed"
        );
        let alto = read(&page.join(crate::quality::ALTO_FILE), MAX_ALTO)?;
        ensure!(
            quality.inputs.alto.matches(&alto),
            "{name}: staged ALTO changed"
        );
        let crops = cut(&bytes, &alto, quality.crop_seed, quality.k_crops)
            .with_context(|| format!("{name}: cutting crops"))?;
        std::fs::create_dir_all(&unit)?;
        let sample = page.join(crate::quality::stage::SAMPLE_FILE);
        if sample.exists() {
            atomic(
                &unit.join(crate::quality::stage::SAMPLE_FILE),
                &std::fs::read(&sample)?,
            )?;
        }
        if crops.is_empty() {
            atomic(
                &unit.join("no_crops"),
                b"the ALTO has no line the recipe can cut\n",
            )?;
            summary.no_crops += 1;
            continue;
        }
        atomic(&unit.join(PAGE_FILE), &bytes)?;
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: opts.cpu_timeout_secs,
            crops,
            doc_id: quality.doc_id.clone(),
            engine: opts.engine.clone(),
            gpu_timeout_secs: opts.gpu_timeout_secs,
            group: quality.group.clone(),
            input: quality.inputs.page.clone(),
            language: quality.language.clone(),
            max_tokens_per_1000px2: opts.max_tokens_per_1000px2,
            pages: vec![],
            prompt_version: PROMPT_VERSION,
        };
        job.id = job.identity()?;
        job.validate()
            .with_context(|| format!("{name}: staged job"))?;
        let json = serde_json::to_vec_pretty(&job)?;
        ensure!(
            json.len() <= MAX_JOB_JSON,
            "{name}: qwen.json is {} bytes, over the {MAX_JOB_JSON}-byte limit",
            json.len()
        );
        atomic(&unit.join(JOB_FILE), &json)?;
        summary.staged += 1;
    }
    if bundle_size > 1 {
        (summary.bundles, summary.no_bundle) = pack_bundles(&page_dir, &units_dir, bundle_size)?;
    }
    Ok(summary)
}

/// Packs any staged single-page units in `page_dir` that are not yet present in
/// a bundle under `units_dir` into multi-page bundle units of up to
/// `bundle_size` pages, each within `MAX_BUNDLE_SCAN`, `MAX_BUNDLE_CROPS` and,
/// measured on the manifest as it will be written, `MAX_JOB_JSON`, the limit
/// submit and the workers read `qwen.json` with. A page whose one-page bundle
/// would not fit gets a `no_bundle` marker and is never published. Returns the
/// bundles written and the pages marked.
fn pack_bundles(
    page_dir: &Path,
    units_dir: &Path,
    bundle_size: usize,
) -> anyhow::Result<(usize, usize)> {
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
    let mut candidates: Vec<PathBuf> = std::fs::read_dir(page_dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(JOB_FILE).exists() && !p.join(NO_BUNDLE).exists())
        .collect();
    candidates.sort();
    let mut unbundled: Vec<(PathBuf, Job)> = Vec::new();
    for p in candidates {
        let pj: Job = serde_json::from_slice(&read(&p.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("reading {}/qwen.json", p.display()))?;
        if !bundled_docs.contains(&pj.doc_id) {
            unbundled.push((p, pj));
        }
    }
    let mut created = 0usize;
    let mut no_bundle = 0usize;
    let mut chunk_dirs: Vec<PathBuf> = Vec::new();
    let mut chunk_jobs: Vec<Job> = Vec::new();
    let mut scan_bytes = 0usize;
    let mut crop_count = 0usize;
    let mut flush = |dirs: &mut Vec<PathBuf>, jobs: &mut Vec<Job>| -> anyhow::Result<()> {
        if dirs.is_empty() {
            return Ok(());
        }
        created += 1;
        let first_doc = dirs[0]
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("bundle");
        let idx = existing_bundles + created;
        let dir = units_dir.join(format!("bundle-{idx:05}-{first_doc}"));
        write_bundle(&dir, dirs)?;
        dirs.clear();
        jobs.clear();
        Ok(())
    };
    for (u, pj) in unbundled {
        let alone = bundle_json_len(std::slice::from_ref(&pj))?;
        if alone > MAX_JOB_JSON {
            eprintln!(
                "{}: a one-page bundle's qwen.json would be {alone} bytes, over the \
                 {MAX_JOB_JSON}-byte limit; marked {NO_BUNDLE}",
                pj.doc_id
            );
            atomic(
                &u.join(NO_BUNDLE),
                format!("a one-page bundle's qwen.json would be {alone} bytes\n").as_bytes(),
            )?;
            no_bundle += 1;
            continue;
        }
        let p_len = usize::try_from(pj.input.bytes).unwrap_or(usize::MAX);
        let mut fits = chunk_jobs.len() < bundle_size
            && scan_bytes.saturating_add(p_len) <= MAX_BUNDLE_SCAN
            && crop_count + pj.crops.len() <= MAX_BUNDLE_CROPS;
        if fits && !chunk_jobs.is_empty() {
            chunk_jobs.push(pj.clone());
            fits = bundle_json_len(&chunk_jobs)? <= MAX_JOB_JSON;
            chunk_jobs.pop();
        }
        if !fits {
            flush(&mut chunk_dirs, &mut chunk_jobs)?;
            scan_bytes = 0;
            crop_count = 0;
        }
        scan_bytes = scan_bytes.saturating_add(p_len);
        crop_count += pj.crops.len();
        chunk_dirs.push(u);
        chunk_jobs.push(pj);
    }
    flush(&mut chunk_dirs, &mut chunk_jobs)?;
    Ok((created, no_bundle))
}

/// The crops of a page: LoC's ALTO in the image's pixels, the recipe's
/// selection, LoC's text in each box.
pub fn cut(page: &[u8], alto: &[u8], seed: u64, k: u32) -> anyhow::Result<Vec<Crop>> {
    let (w, h) = image::load_from_memory(page)
        .context("decoding page.jpg")?
        .dimensions();
    let parsed = crate::quality::parse_alto(alto, f64::from(w), f64::from(h))?;
    Ok(crops::select(&parsed, seed, k as usize)
        .into_iter()
        .map(|c| Crop {
            rect: c.rect,
            id: c.id,
            loc_text: crops::text_in_box(&parsed, c.rect),
            stratum: c.stratum,
        })
        .collect())
}

/// Writes a unit from a local page and ALTO, for checking a host or the
/// image without LoC or BOINC: `dir` gets `page.jpg` and `qwen.json`, and
/// `usnm-boinc work --dir <dir>` then runs it.
#[allow(clippy::too_many_arguments)]
pub fn local_unit(
    image: &Path,
    alto: &Path,
    dir: &Path,
    engine: Engine,
    seed: u64,
    k: u32,
    timeout_secs: u32,
    max_tokens_per_1000px2: u32,
) -> anyhow::Result<Job> {
    let bytes = read(image, MAX_SCAN)?;
    let alto = read(alto, MAX_ALTO)?;
    let crops = cut(&bytes, &alto, seed, k)?;
    ensure!(!crops.is_empty(), "the ALTO has no line the recipe can cut");
    std::fs::create_dir_all(dir)?;
    atomic(&dir.join(PAGE_FILE), &bytes)?;
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        cpu_timeout_secs: timeout_secs,
        crops,
        doc_id: "local".into(),
        engine,
        gpu_timeout_secs: timeout_secs,
        group: "local".into(),
        input: Input::of(&bytes, "local:page.jpg"),
        language: "und".into(),
        max_tokens_per_1000px2,
        pages: vec![],
        prompt_version: PROMPT_VERSION,
    };
    job.id = job.identity()?;
    job.validate()?;
    atomic(&dir.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
    Ok(job)
}

/// The bundle job for `pages` (staged single-page jobs, in bundle order),
/// whose JPEGs lie end to end in that order in the `page.jpg` that `blob`
/// describes. Each page's slice is its own job's input at its offset.
fn bundle_job(pages: &[Job], blob: Input) -> anyhow::Result<Job> {
    let first = pages.first().context("a qwen bundle needs a page")?;
    let mut offset = 0u64;
    let mut bundle_pages = Vec::with_capacity(pages.len());
    let mut crops = Vec::new();
    for pj in pages {
        for c in &pj.crops {
            crops.push(Crop {
                rect: c.rect,
                id: format!("{}:{}", pj.doc_id, c.id),
                loc_text: c.loc_text.clone(),
                stratum: c.stratum.clone(),
            });
        }
        bundle_pages.push(BundlePage {
            crops: pj.crops.clone(),
            doc_id: pj.doc_id.clone(),
            group: pj.group.clone(),
            input: BundleSlice {
                bytes: pj.input.bytes,
                offset,
                sha256: pj.input.sha256.clone(),
                source: pj.input.source.clone(),
            },
            language: pj.language.clone(),
        });
        offset = offset
            .checked_add(pj.input.bytes)
            .context("bundle page.jpg size overflow")?;
    }
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        cpu_timeout_secs: first.cpu_timeout_secs,
        crops,
        doc_id: first.doc_id.clone(),
        engine: first.engine.clone(),
        gpu_timeout_secs: first.gpu_timeout_secs,
        group: first.group.clone(),
        input: blob,
        language: first.language.clone(),
        max_tokens_per_1000px2: first.max_tokens_per_1000px2,
        pages: bundle_pages,
        prompt_version: first.prompt_version,
    };
    job.id = job.identity()?;
    Ok(job)
}

/// The size of the `qwen.json` a bundle of `pages` would be written as,
/// without reading a page: the blob's checksum is a placeholder of the same
/// length as the real one.
fn bundle_json_len(pages: &[Job]) -> anyhow::Result<usize> {
    let bytes = pages
        .iter()
        .try_fold(0u64, |n, p| n.checked_add(p.input.bytes))
        .context("bundle page.jpg size overflow")?;
    let blob = Input {
        bytes,
        sha256: "0".repeat(64),
        source: BUNDLE_SOURCE.to_owned(),
    };
    Ok(serde_json::to_vec_pretty(&bundle_job(pages, blob)?)?.len())
}

/// Packs `page_units` (1 to [`MAX_BUNDLE_PAGES`] staged single-page `usnm_qwen`
/// unit directories) into a single multi-page bundle unit directory `dir`.
/// Nothing is written unless the bundle's `page.jpg` and `qwen.json` are
/// within the limits the workers read them with.
pub fn write_bundle(dir: &Path, page_units: &[PathBuf]) -> anyhow::Result<Job> {
    ensure!(
        !page_units.is_empty() && page_units.len() <= MAX_BUNDLE_PAGES,
        "a qwen bundle requires 1 to {MAX_BUNDLE_PAGES} page units"
    );
    let mut page_blob = Vec::new();
    let mut jobs = Vec::with_capacity(page_units.len());
    for u_dir in page_units {
        let pj: Job = serde_json::from_slice(&read(&u_dir.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("reading {}/qwen.json", u_dir.display()))?;
        pj.validate()?;
        ensure!(
            pj.pages.is_empty(),
            "{} is already a multi-page bundle",
            u_dir.display()
        );
        let p_bytes = read(&u_dir.join(PAGE_FILE), MAX_SCAN)?;
        ensure!(
            pj.input.matches(&p_bytes),
            "{}: page.jpg checksum mismatch",
            pj.doc_id
        );
        page_blob.extend_from_slice(&p_bytes);
        jobs.push(pj);
    }
    ensure!(
        page_blob.len() <= MAX_BUNDLE_SCAN,
        "bundle page.jpg over size limit"
    );
    let job = bundle_job(&jobs, Input::of(&page_blob, BUNDLE_SOURCE))?;
    ensure!(
        job.crops.len() <= MAX_BUNDLE_CROPS,
        "bundle crops exceed {MAX_BUNDLE_CROPS}"
    );
    job.validate().context("staged qwen bundle job")?;
    let json = serde_json::to_vec_pretty(&job)?;
    ensure!(
        json.len() <= MAX_JOB_JSON,
        "bundle qwen.json is {} bytes, over the {MAX_JOB_JSON}-byte limit",
        json.len()
    );
    std::fs::create_dir_all(dir)?;
    atomic(&dir.join(PAGE_FILE), &page_blob)?;
    atomic(&dir.join(JOB_FILE), &json)?;
    Ok(job)
}

/// The synthetic check page: two lines of block capitals in a 5 by 7
/// pixel font scaled up, with an ALTO naming each word's box, so the
/// image's build check can read a line with nothing but what the image
/// holds (no font, no Python). Returns the JPEG and the ALTO.
pub fn synthetic_page() -> (Vec<u8>, String) {
    const SCALE: u32 = 8;
    const ADVANCE: u32 = 6 * SCALE;
    const LINES: [&str; 2] = ["HELLO NEWSPAPER WORLD NOW", "QUICK BROWN FOX JUMPS"];
    let (width, height) = (1400u32, 1400u32);
    let mut img = image::GrayImage::from_pixel(width, height, image::Luma([245]));
    let mut blocks = Vec::new();
    for (li, line) in LINES.iter().enumerate() {
        let y = 100 + li as u32 * 200;
        let mut words = Vec::new();
        let mut x = 60u32;
        for word in line.split(' ') {
            let x0 = x;
            for ch in word.chars() {
                for (r, row) in glyph(ch).iter().enumerate() {
                    for (c, cell) in row.bytes().enumerate() {
                        if cell == b'#' {
                            for dy in 0..SCALE {
                                for dx in 0..SCALE {
                                    img.put_pixel(
                                        x + c as u32 * SCALE + dx,
                                        y + r as u32 * SCALE + dy,
                                        image::Luma([20]),
                                    );
                                }
                            }
                        }
                    }
                }
                x += ADVANCE;
            }
            words.push((x0, y, x - SCALE, y + 7 * SCALE, word, 0.9));
            x += ADVANCE;
        }
        blocks.push(words);
    }
    let mut jpeg = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageLuma8(img)
        .write_to(&mut jpeg, image::ImageFormat::Jpeg)
        .expect("encoding the synthetic page");
    (
        jpeg.into_inner(),
        crate::quality::alto_xml(width, height, &[blocks]),
    )
}

/// A 5 by 7 capital, rows top to bottom; anything else is blank.
fn glyph(ch: char) -> [&'static str; 7] {
    match ch {
        'A' => [
            " ### ", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'B' => [
            "#### ", "#   #", "#   #", "#### ", "#   #", "#   #", "#### ",
        ],
        'C' => [
            " ####", "#    ", "#    ", "#    ", "#    ", "#    ", " ####",
        ],
        'D' => [
            "#### ", "#   #", "#   #", "#   #", "#   #", "#   #", "#### ",
        ],
        'E' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#####",
        ],
        'F' => [
            "#####", "#    ", "#    ", "#### ", "#    ", "#    ", "#    ",
        ],
        'G' => [
            " ####", "#    ", "#    ", "# ###", "#   #", "#   #", " ####",
        ],
        'H' => [
            "#   #", "#   #", "#   #", "#####", "#   #", "#   #", "#   #",
        ],
        'I' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "#####",
        ],
        'J' => [
            "    #", "    #", "    #", "    #", "#   #", "#   #", " ### ",
        ],
        'K' => [
            "#   #", "#  # ", "# #  ", "##   ", "# #  ", "#  # ", "#   #",
        ],
        'L' => [
            "#    ", "#    ", "#    ", "#    ", "#    ", "#    ", "#####",
        ],
        'M' => [
            "#   #", "## ##", "# # #", "# # #", "#   #", "#   #", "#   #",
        ],
        'N' => [
            "#   #", "##  #", "# # #", "#  ##", "#   #", "#   #", "#   #",
        ],
        'O' => [
            " ### ", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'P' => [
            "#### ", "#   #", "#   #", "#### ", "#    ", "#    ", "#    ",
        ],
        'Q' => [
            " ### ", "#   #", "#   #", "#   #", "# # #", "#  # ", " ## #",
        ],
        'R' => [
            "#### ", "#   #", "#   #", "#### ", "# #  ", "#  # ", "#   #",
        ],
        'S' => [
            " ####", "#    ", "#    ", " ### ", "    #", "    #", "#### ",
        ],
        'T' => [
            "#####", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'U' => [
            "#   #", "#   #", "#   #", "#   #", "#   #", "#   #", " ### ",
        ],
        'V' => [
            "#   #", "#   #", "#   #", "#   #", " # # ", " # # ", "  #  ",
        ],
        'W' => [
            "#   #", "#   #", "#   #", "# # #", "# # #", "## ##", "#   #",
        ],
        'X' => [
            "#   #", "#   #", " # # ", "  #  ", " # # ", "#   #", "#   #",
        ],
        'Y' => [
            "#   #", "#   #", " # # ", "  #  ", "  #  ", "  #  ", "  #  ",
        ],
        'Z' => [
            "#####", "    #", "   # ", "  #  ", " #   ", "#    ", "#####",
        ],
        _ => ["     "; 7],
    }
}

/// Writes a unit from the synthetic page, for the image's build check.
pub fn synthetic_unit(
    dir: &Path,
    engine: Engine,
    k: u32,
    timeout_secs: u32,
    max_tokens_per_1000px2: u32,
) -> anyhow::Result<Job> {
    let (jpeg, alto) = synthetic_page();
    std::fs::create_dir_all(dir)?;
    let image = dir.join("synthetic-page.jpg");
    let alto_path = dir.join("synthetic-loc.xml");
    atomic(&image, &jpeg)?;
    atomic(&alto_path, alto.as_bytes())?;
    let job = local_unit(
        &image,
        &alto_path,
        dir,
        engine,
        522,
        k,
        timeout_secs,
        max_tokens_per_1000px2,
    )?;
    std::fs::remove_file(image)?;
    std::fs::remove_file(alto_path)?;
    Ok(job)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) fn engine() -> Engine {
        Engine {
            image: format!("sha256:{}", "a".repeat(64)),
            llama: format!("sha256:{}", "b".repeat(64)),
            models: super::super::default_models(),
            weights: format!("sha256:{}", "c".repeat(64)),
        }
    }

    #[test]
    fn local_unit_cuts_crops_from_the_alto() {
        let tmp = tempfile::tempdir().unwrap();
        let unit = tmp.path().join("q");
        crate::quality::work::tests::unit(&unit);
        let j = local_unit(
            &unit.join("page.jpg"),
            &unit.join("loc.xml"),
            &tmp.path().join("u"),
            engine(),
            522,
            4,
            600,
            4,
        )
        .unwrap();
        assert_eq!(j.crops.len(), 2);
        assert!(j.crops[0].loc_text.contains("mayor") || j.crops[1].loc_text.contains("mayor"));
        assert!(tmp.path().join("u/qwen.json").exists());
        let again = cut(
            &std::fs::read(unit.join("page.jpg")).unwrap(),
            &std::fs::read(unit.join("loc.xml")).unwrap(),
            522,
            4,
        )
        .unwrap();
        assert_eq!(again, j.crops);
    }

    #[tokio::test]
    async fn stages_every_quality_unit_once() {
        let tmp = tempfile::tempdir().unwrap();
        let qroot = tmp.path().join("quality");
        let unit = crate::quality::units(&qroot).join("sn83045462_1890-05-03_ed-1_seq-1");
        crate::quality::work::tests::unit(&unit);
        std::fs::write(unit.join("sample.json"), b"{}").unwrap();
        let opts = Options {
            quality_root: qroot.clone(),
            root: tmp.path().join("qwen"),
            engine: engine(),
            gpu_timeout_secs: 1800,
            cpu_timeout_secs: 7200,
            max_tokens_per_1000px2: 4,
            max_pages: 20,
            bundle_size: 1,
        };
        let s = run(&opts).await.unwrap();
        assert_eq!((s.staged, s.no_crops, s.already), (1, 0, 0));
        let out = units(&opts.root).join("sn83045462_1890-05-03_ed-1_seq-1");
        let job: Job = serde_json::from_slice(&std::fs::read(out.join(JOB_FILE)).unwrap()).unwrap();
        job.validate().unwrap();
        assert_eq!(job.doc_id, "local");
        assert!(!job.crops.is_empty());
        assert!(out.join("sample.json").exists());
        assert_eq!(
            std::fs::read(out.join(PAGE_FILE)).unwrap(),
            std::fs::read(unit.join("page.jpg")).unwrap()
        );
        let s = run(&opts).await.unwrap();
        assert_eq!((s.staged, s.already), (0, 1));
        // A page whose ALTO has no cuttable line gets a marker, not a unit.
        let empty = crate::quality::units(&qroot).join("sn83045462_1890-05-04_ed-1_seq-1");
        crate::quality::work::tests::unit(&empty);
        let page = std::fs::read(empty.join("page.jpg")).unwrap();
        let alto =
            crate::quality::alto_xml(1000, 1400, &[vec![vec![(10, 10, 60, 40, "one", 0.9)]]]);
        let texts: usnm_ocr_contract::quality::Texts =
            serde_json::from_slice(&std::fs::read(empty.join("texts.json")).unwrap()).unwrap();
        crate::quality::stage::local_unit(&crate::quality::stage::LocalUnit {
            page: &page,
            alto: alto.as_bytes(),
            texts: &texts,
            places: "Albany\n",
            dir: &empty,
            engine: usnm_ocr_contract::quality::Engine {
                image: format!("sha256:{}", "a".repeat(64)),
            },
            function_words: vec![],
            crop_seed: 1,
            k_crops: 4,
        })
        .unwrap();
        let s = run(&opts).await.unwrap();
        assert_eq!((s.staged, s.no_crops, s.already), (0, 1, 1));
        assert!(units(&opts.root)
            .join("sn83045462_1890-05-04_ed-1_seq-1/no_crops")
            .exists());
    }

    #[tokio::test]
    async fn stages_and_packs_multipage_qwen_bundles_idempotently() {
        let tmp = tempfile::tempdir().unwrap();
        let qroot = tmp.path().join("quality");
        for doc_id in [
            "sn83045462_1890-05-03_ed-1_seq-1",
            "sn83045462_1890-05-03_ed-1_seq-2",
            "sn83045462_1890-05-03_ed-1_seq-3",
        ] {
            let u = qroot.join("pages").join(doc_id);
            let mut qjob = crate::quality::work::tests::unit(&u);
            qjob.doc_id = doc_id.into();
            qjob.id = qjob.identity().unwrap();
            std::fs::write(
                u.join(crate::quality::JOB_FILE),
                serde_json::to_vec_pretty(&qjob).unwrap(),
            )
            .unwrap();
        }
        let opts = Options {
            quality_root: qroot,
            root: tmp.path().join("qwen-bundled"),
            engine: engine(),
            gpu_timeout_secs: 14_400,
            cpu_timeout_secs: 7200,
            max_tokens_per_1000px2: 4,
            max_pages: 20,
            bundle_size: 2,
        };
        let s = run(&opts).await.unwrap();
        assert_eq!(
            s,
            Summary {
                staged: 3,
                no_crops: 0,
                already: 0,
                bundles: 2,
                no_bundle: 0,
            }
        );
        let b1: Job = serde_json::from_slice(
            &std::fs::read(
                units(&opts.root)
                    .join("bundle-00001-sn83045462_1890-05-03_ed-1_seq-1")
                    .join(JOB_FILE),
            )
            .unwrap(),
        )
        .unwrap();
        b1.validate().unwrap();
        assert_eq!(b1.pages.len(), 2);
        assert!(b1.crops[0]
            .id
            .starts_with("sn83045462_1890-05-03_ed-1_seq-1:"));

        // Re-running with the same inputs creates zero duplicate pages or bundles.
        let s2 = run(&opts).await.unwrap();
        assert_eq!(
            s2,
            Summary {
                staged: 0,
                no_crops: 0,
                already: 3,
                bundles: 0,
                no_bundle: 0,
            }
        );
    }

    /// A staged single-page unit in `page_dir` with `crops` crops, each with
    /// the longest LoC text a crop may carry.
    fn page_unit(page_dir: &Path, doc_id: &str, crops: usize) {
        let (jpeg, _) = synthetic_page();
        let u = page_dir.join(doc_id);
        std::fs::create_dir_all(&u).unwrap();
        std::fs::write(u.join(PAGE_FILE), &jpeg).unwrap();
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: 7200,
            crops: (1..=crops)
                .map(|i| Crop {
                    rect: [0, 0, 10, 10],
                    id: format!("c{i:02}"),
                    loc_text: "a".repeat(usnm_ocr_contract::qwen::MAX_CROP_TEXT),
                    stratum: "body".into(),
                })
                .collect(),
            doc_id: doc_id.into(),
            engine: engine(),
            gpu_timeout_secs: 14_400,
            group: "test".into(),
            input: Input::of(&jpeg, "test:page.jpg"),
            language: "eng".into(),
            max_tokens_per_1000px2: 4,
            pages: vec![],
            prompt_version: PROMPT_VERSION,
        };
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        let json = serde_json::to_vec_pretty(&job).unwrap();
        assert!(json.len() <= MAX_JOB_JSON);
        std::fs::write(u.join(JOB_FILE), json).unwrap();
    }

    #[test]
    fn bundles_split_before_qwen_json_exceeds_the_reader_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let page_dir = tmp.path().join("pages");
        let units_dir = tmp.path().join("units");
        std::fs::create_dir_all(&units_dir).unwrap();
        // Eight 8 KiB crop texts per page: about 133 KB of bundle manifest
        // each, so three pages fit in 512 KiB and four do not.
        for i in 1..=6 {
            page_unit(&page_dir, &format!("sn83045462_1890-05-03_ed-1_seq-{i}"), 8);
        }
        // Forty: its one-page bundle (crops listed twice) is over the limit.
        let big = "sn83045462_1890-05-03_ed-1_seq-9";
        page_unit(&page_dir, big, 40);

        assert_eq!(pack_bundles(&page_dir, &units_dir, 16).unwrap(), (2, 1));
        assert!(page_dir.join(big).join(NO_BUNDLE).exists());
        let mut pages = 0;
        for e in std::fs::read_dir(&units_dir).unwrap() {
            let bytes = std::fs::read(e.unwrap().path().join(JOB_FILE)).unwrap();
            assert!(bytes.len() <= MAX_JOB_JSON, "{} bytes", bytes.len());
            let job: Job = serde_json::from_slice(&bytes).unwrap();
            job.validate().unwrap();
            assert_eq!(job.pages.len(), 3);
            pages += job.pages.len();
        }
        assert_eq!(pages, 6);

        // A rerun publishes nothing more.
        assert_eq!(pack_bundles(&page_dir, &units_dir, 16).unwrap(), (0, 0));
    }

    #[test]
    fn bundle_json_len_is_the_written_size() {
        let tmp = tempfile::tempdir().unwrap();
        let page_dir = tmp.path().join("pages");
        let docs = [
            "sn83045462_1890-05-03_ed-1_seq-1",
            "sn83045462_1890-05-03_ed-1_seq-2",
        ];
        for d in docs {
            page_unit(&page_dir, d, 2);
        }
        let dirs: Vec<PathBuf> = docs.iter().map(|d| page_dir.join(d)).collect();
        let jobs: Vec<Job> = dirs
            .iter()
            .map(|d| serde_json::from_slice(&std::fs::read(d.join(JOB_FILE)).unwrap()).unwrap())
            .collect();
        let out = tmp.path().join("bundle");
        write_bundle(&out, &dirs).unwrap();
        assert_eq!(
            bundle_json_len(&jobs).unwrap(),
            std::fs::read(out.join(JOB_FILE)).unwrap().len()
        );
    }

    #[test]
    fn synthetic_page_cuts_both_lines() {
        let tmp = tempfile::tempdir().unwrap();
        let j = synthetic_unit(tmp.path(), engine(), 2, 600, 4).unwrap();
        assert_eq!(j.crops.len(), 2);
        let mut texts: Vec<&str> = j.crops.iter().map(|c| c.loc_text.as_str()).collect();
        texts.sort();
        assert_eq!(
            texts,
            ["HELLO NEWSPAPER WORLD NOW", "QUICK BROWN FOX JUMPS"]
        );
        let page =
            image::load_from_memory(&std::fs::read(tmp.path().join(PAGE_FILE)).unwrap()).unwrap();
        for c in &j.crops {
            let png = crate::qwen::work::crop_png(&page, c).unwrap();
            let cut = image::load_from_memory(&png).unwrap().to_luma8();
            let ink = cut.pixels().filter(|p| p.0[0] < 128).count();
            assert!(ink > 2000, "crop {} has {ink} ink pixels", c.id);
        }
        assert!(!tmp.path().join("synthetic-page.jpg").exists());
    }
}
