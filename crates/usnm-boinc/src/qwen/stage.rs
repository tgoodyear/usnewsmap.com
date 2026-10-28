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
    boinc::sha256,
    quality::{
        BundleSlice, Input, Job as QualityJob, MAX_ALTO, MAX_BUNDLE_SCAN,
        MAX_JOB_JSON as MAX_QUALITY_JOB, MAX_SCAN,
    },
    qwen::{
        BundlePage, Crop, Engine, Job, Outcome, Reading, Readings, MAX_BUNDLE_CROPS,
        MAX_BUNDLE_PAGES, MAX_CROP_TEXT, MAX_DEADLINE_SECS, MAX_JOB_JSON, MAX_READINGS,
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
    /// Pages exceeding the crop-text or manifest limit: marked `too_large`
    /// with retained evidence and never published.
    pub too_large: usize,
}

/// The source name of a bundle's `page.jpg`.
const BUNDLE_SOURCE: &str = "bundle:page.jpg";
/// The marker on a staged page too large to publish, alone or in a bundle.
const TOO_LARGE: &str = "too_large";
/// Room in a bundle's `readings.json` for its `diag` facts (the context
/// size, the GPU layers, the model file, the threads, a 256-character
/// server version line).
const READINGS_DIAG_RESERVE: usize = 16 * 1024;

/// Writes one unit per staged `usnm_quality` page (those with a
/// `quality.json`), in doc id order, up to `max_pages` new ones. When
/// `bundle_size > 1`, single-page units are staged under `<root>/pages/` and
/// packed into multi-page bundle workunits under `<root>/units/`. A root
/// staged with bundles keeps them: `--bundle-size 1` on it is refused, since
/// its pages would be staged again as single-page units. A page whose ALTO
/// has no line the recipe can cut gets a `no_crops` marker and no unit; one
/// whose `qwen.json` would pass `MAX_JOB_JSON` gets a `too_large` marker and
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
    let bundled_root = opts.root.join("pages");
    ensure!(
        bundle_size > 1 || !bundled_root.exists(),
        "{} was staged with --bundle-size above 1: keep bundling, or stage into a separate --root",
        opts.root.display()
    );
    std::fs::create_dir_all(&units_dir)?;
    let page_dir = if bundle_size > 1 {
        std::fs::create_dir_all(&bundled_root)?;
        bundled_root
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
        if summary.staged + summary.no_crops + summary.too_large >= opts.max_pages {
            break;
        }
        let name = page
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let unit = page_dir.join(&name);
        // A root first staged one page per unit keeps those units when it
        // moves to bundles: packing skips their doc ids, so they are not
        // staged again either.
        let staged = |dir: &Path| {
            [JOB_FILE, "no_crops", TOO_LARGE]
                .iter()
                .any(|f| dir.join(f).exists())
        };
        if staged(&unit) || (bundle_size > 1 && staged(&units_dir.join(&name))) {
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
        job.validate_source_candidate()
            .with_context(|| format!("{name}: staged source candidate"))?;
        let oversized: Vec<_> = job
            .crops
            .iter()
            .filter(|c| c.loc_text.len() > MAX_CROP_TEXT)
            .map(|c| {
                format!(
                    "crop {} {:?}: loc_text is {} UTF-8 bytes, over the {MAX_CROP_TEXT}-byte limit; sha256={}",
                    c.id, c.rect, c.loc_text.len(), usnm_ocr_contract::boinc::sha256(c.loc_text.as_bytes())
                )
            })
            .collect();
        if !oversized.is_empty() {
            // Evidence is not a publishable qwen.json. Keep the exact candidate
            // and source before the terminal marker makes retries skip this page.
            atomic(
                &unit.join("rejected-qwen.json"),
                &serde_json::to_vec_pretty(&job)?,
            )?;
            atomic(&unit.join(crate::quality::ALTO_FILE), &alto)?;
            atomic(
                &unit.join(crate::quality::JOB_FILE),
                &std::fs::read(page.join(crate::quality::JOB_FILE))?,
            )?;
            let why = format!(
                "{}: {}\nquality.json sha256={}; loc.xml sha256={}; page.jpg sha256={}\n",
                job.doc_id,
                oversized.join("; "),
                usnm_ocr_contract::boinc::sha256(&std::fs::read(
                    page.join(crate::quality::JOB_FILE)
                )?),
                quality.inputs.alto.sha256,
                quality.inputs.page.sha256,
            );
            eprintln!("{why}marked {TOO_LARGE}; source text retained without truncation");
            atomic(&unit.join(TOO_LARGE), why.as_bytes())?;
            summary.too_large += 1;
            continue;
        }
        job.validate()
            .with_context(|| format!("{name}: staged job"))?;
        let json = serde_json::to_vec_pretty(&job)?;
        if json.len() > MAX_JOB_JSON {
            mark_too_large(&unit, &job.doc_id, "qwen.json", json.len())?;
            summary.too_large += 1;
            continue;
        }
        atomic(&unit.join(JOB_FILE), &json)?;
        summary.staged += 1;
    }
    if bundle_size > 1 {
        let (bundles, too_large) = pack_bundles(&page_dir, &units_dir, bundle_size)?;
        summary.bundles = bundles;
        summary.too_large += too_large;
    }
    Ok(summary)
}

/// Marks a staged page `too_large`: its `qwen.json` (`what`) would be `len`
/// bytes, over the `MAX_JOB_JSON` submit and the workers read it with.
fn mark_too_large(unit: &Path, doc_id: &str, what: &str, len: usize) -> anyhow::Result<()> {
    let why = format!("{what} would be {len} bytes, over the {MAX_JOB_JSON}-byte limit");
    eprintln!("{doc_id}: {why}; marked {TOO_LARGE}");
    atomic(&unit.join(TOO_LARGE), format!("{why}\n").as_bytes())
}

/// Packs any staged single-page units in `page_dir` that are not yet present in
/// a bundle under `units_dir` into multi-page bundle units of up to
/// `bundle_size` pages, each within `MAX_BUNDLE_SCAN` and `MAX_BUNDLE_CROPS`,
/// with its `qwen.json` as written within `MAX_JOB_JSON` (the limit submit and
/// the workers read it with) and its longest possible `readings.json` within
/// `MAX_READINGS` (the limit the output upload and the validator allow). A
/// page whose one-page bundle would not fit gets a `too_large` marker and is
/// never published. Returns the bundles written and the pages marked.
fn bundle_lock(units_dir: &Path) -> anyhow::Result<PathBuf> {
    Ok(units_dir
        .parent()
        .context("units directory has no root")?
        .join(".bundle-lock"))
}

fn pack_bundles(
    page_dir: &Path,
    units_dir: &Path,
    bundle_size: usize,
) -> anyhow::Result<(usize, usize)> {
    // One staging run bundles a root at a time, from finding the pages
    // already bundled to publishing the new bundles' `qwen.json`.
    let _lock = crate::Lock::take(&bundle_lock(units_dir)?)
        .context("another staging run is bundling this root")?;
    let mut bundled_docs = BTreeSet::new();
    let mut existing_bundles = 0usize;
    if units_dir.exists() {
        for entry in std::fs::read_dir(units_dir)?.filter_map(|e| e.ok()) {
            let p = entry.path();
            // A bundle a crashed run left without its `qwen.json` keeps
            // its number: the next is never reused.
            let partial = !p.join(JOB_FILE).exists()
                && p.is_dir()
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with("bundle-"));
            if partial {
                existing_bundles += 1;
            } else if p.join(JOB_FILE).exists() {
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
        .filter(|p| p.join(JOB_FILE).exists() && !p.join(TOO_LARGE).exists())
        .collect();
    candidates.sort();
    let mut unbundled: Vec<(String, PathBuf, Job)> = Vec::new();
    for p in candidates {
        let pj: Job = serde_json::from_slice(&read(&p.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("reading {}/qwen.json", p.display()))?;
        if !bundled_docs.contains(&pj.doc_id) {
            unbundled.push((settings_key(&pj)?, p, pj));
        }
    }
    // Pages left from a run with other settings (engine, deadlines, token
    // allowance) are grouped apart: a bundle runs with one set of settings.
    unbundled.sort_by(|a, b| (&a.0, &a.1).cmp(&(&b.0, &b.1)));
    let mut created = 0usize;
    let mut too_large = 0usize;
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
    for (_, u, pj) in unbundled {
        let alone = bundle_json_len(std::slice::from_ref(&pj))?;
        if alone > MAX_JOB_JSON {
            mark_too_large(&u, &pj.doc_id, "a one-page bundle's qwen.json", alone)?;
            too_large += 1;
            continue;
        }
        let p_len = usize::try_from(pj.input.bytes).unwrap_or(usize::MAX);
        let mut fits = chunk_jobs.len() < bundle_size
            && scan_bytes.saturating_add(p_len) <= MAX_BUNDLE_SCAN
            && crop_count + pj.crops.len() <= MAX_BUNDLE_CROPS
            && chunk_jobs.first().is_none_or(|f| same_settings(f, &pj));
        if fits && !chunk_jobs.is_empty() {
            chunk_jobs.push(pj.clone());
            fits = bundle_json_len(&chunk_jobs)? <= MAX_JOB_JSON
                && longest_readings_len(&chunk_jobs)? <= MAX_READINGS;
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
    Ok((created, too_large))
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

/// Whether `a` and `b` share the settings a bundle runs every page with
/// (taken from its first page): the engine, deadlines and token allowance.
fn same_settings(a: &Job, b: &Job) -> bool {
    a.v == b.v
        && a.engine == b.engine
        && a.gpu_timeout_secs == b.gpu_timeout_secs
        && a.cpu_timeout_secs == b.cpu_timeout_secs
        && a.max_tokens_per_1000px2 == b.max_tokens_per_1000px2
        && a.prompt_version == b.prompt_version
}

/// [`same_settings`] as a sort key.
fn settings_key(j: &Job) -> anyhow::Result<String> {
    Ok(serde_json::to_string(&(
        j.v,
        &j.engine,
        j.gpu_timeout_secs,
        j.cpu_timeout_secs,
        j.max_tokens_per_1000px2,
        j.prompt_version,
    ))?)
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
        cpu_timeout_secs: bundle_deadline(first.cpu_timeout_secs, pages.len()),
        crops,
        doc_id: first.doc_id.clone(),
        engine: first.engine.clone(),
        gpu_timeout_secs: bundle_deadline(first.gpu_timeout_secs, pages.len()),
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

/// A bundle's deadline: each page's own deadline, once per page, up to the
/// longest a unit may have.
pub fn bundle_deadline(per_page_secs: u32, pages: usize) -> u32 {
    let pages = u32::try_from(pages.max(1)).unwrap_or(u32::MAX);
    per_page_secs.saturating_mul(pages).min(MAX_DEADLINE_SECS)
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

/// The longest `readings.json` a bundle of `pages` can produce, as the workers
/// write it: every crop read, each reading's text at [`MAX_CROP_TEXT`] bytes
/// that all escape to two (a cleaned reading keeps no control character but
/// newline and tab, and serde_json writes other non-ASCII text unescaped), the
/// largest time and token count, the longest device, model and outcome
/// names, and [`READINGS_DIAG_RESERVE`] for the run's `diag` facts.
fn longest_readings_len(pages: &[Job]) -> anyhow::Result<usize> {
    let first = pages.first().context("a qwen bundle needs a page")?;
    let readings = Readings {
        v: VERSION,
        job_id: "0".repeat(64),
        device: "vulkan".into(),
        diag: Default::default(),
        model: first
            .engine
            .models
            .iter()
            .map(|m| m.name.clone())
            .max_by_key(String::len),
        outcome: Outcome::UnitError,
        prompt_version: PROMPT_VERSION,
        readings: pages
            .iter()
            .flat_map(|p| {
                p.crops.iter().map(|c| Reading {
                    id: format!("{}:{}", p.doc_id, c.id),
                    ms: u64::MAX,
                    text: "\"".repeat(MAX_CROP_TEXT),
                    tokens: u32::MAX,
                })
            })
            .collect(),
    };
    Ok(serde_json::to_vec_pretty(&readings)?.len() + READINGS_DIAG_RESERVE)
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
    let mut members = Vec::with_capacity(page_units.len());
    for u_dir in page_units {
        let job_bytes = read(&u_dir.join(JOB_FILE), MAX_JOB_JSON)?;
        let pj: Job = serde_json::from_slice(&job_bytes)
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
        if let Some(first) = jobs.first() {
            ensure!(
                same_settings(first, &pj),
                "{}: staged with other engine, deadline or token settings than {}",
                pj.doc_id,
                first.doc_id
            );
        }
        members.push(Member {
            doc_id: pj.doc_id.clone(),
            unit: u_dir
                .file_name()
                .and_then(|n| n.to_str())
                .context("page unit directory name")?
                .to_owned(),
            job_id: pj.id.clone(),
            job_sha256: sha256(&job_bytes),
            page_sha256: pj.input.sha256.clone(),
            offset: page_blob.len() as u64,
            bytes: pj.input.bytes,
        });
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
    ensure!(
        longest_readings_len(&jobs)? <= MAX_READINGS,
        "the bundle's readings.json could pass the {MAX_READINGS}-byte limit"
    );
    job.validate().context("staged qwen bundle job")?;
    let json = serde_json::to_vec_pretty(&job)?;
    ensure!(
        json.len() <= MAX_JOB_JSON,
        "bundle qwen.json is {} bytes, over the {MAX_JOB_JSON}-byte limit",
        json.len()
    );
    let membership = Membership {
        v: MEMBERSHIP_VERSION,
        job_id: job.id.clone(),
        page_sha256: job.input.sha256.clone(),
        pages: members,
    };
    membership.check(&job)?;
    // Claims the directory, so two staging runs never interleave one
    // bundle's page, membership and job files.
    std::fs::create_dir_all(dir.parent().context("bundle directory")?)?;
    std::fs::create_dir(dir)
        .with_context(|| format!("{} already exists: another staging run", dir.display()))?;
    atomic(&dir.join(PAGE_FILE), &page_blob)?;
    atomic(
        &dir.join(MEMBERSHIP_FILE),
        &serde_json::to_vec_pretty(&membership)?,
    )?;
    atomic(&dir.join(JOB_FILE), &json)?;
    Ok(job)
}

/// A bundle unit's `membership.json`: which staged single-page unit each
/// of its pages came from, by that unit's job id and `qwen.json` checksum,
/// so a page kept from the bundle traces back to the page job the producer
/// audited (its page, ALTO and provenance). Written with the bundle and
/// never changed; `qwen.json` is written last.
pub const MEMBERSHIP_FILE: &str = "membership.json";
pub const MEMBERSHIP_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Membership {
    pub v: u32,
    /// The bundle's job id.
    pub job_id: String,
    /// The bundled `page.jpg`'s checksum.
    pub page_sha256: String,
    pub pages: Vec<Member>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub doc_id: String,
    /// The page unit's directory name under the root's `pages/`.
    pub unit: String,
    /// The page unit's own job id and `qwen.json` checksum.
    pub job_id: String,
    pub job_sha256: String,
    pub page_sha256: String,
    pub offset: u64,
    pub bytes: u64,
}

impl Membership {
    /// Checks the manifest against the bundle job it was written with:
    /// the same job, blob and pages, in order, at the same slices.
    pub fn check(&self, job: &Job) -> anyhow::Result<()> {
        ensure!(
            self.v == MEMBERSHIP_VERSION,
            "unsupported membership version"
        );
        ensure!(
            self.job_id == job.id && self.page_sha256 == job.input.sha256,
            "membership is for another bundle"
        );
        ensure!(
            !job.pages.is_empty() && self.pages.len() == job.pages.len(),
            "membership lists {} pages, the bundle has {}",
            self.pages.len(),
            job.pages.len()
        );
        for (m, bp) in self.pages.iter().zip(&job.pages) {
            ensure!(
                m.doc_id == bp.doc_id
                    && m.page_sha256 == bp.input.sha256
                    && m.offset == bp.input.offset
                    && m.bytes == bp.input.bytes,
                "membership page {} does not match the bundle",
                m.doc_id
            );
            ensure!(
                usnm_ocr_contract::boinc::is_sha256(&m.job_id)
                    && usnm_ocr_contract::boinc::is_sha256(&m.job_sha256),
                "membership page {} has no job checksum",
                m.doc_id
            );
            ensure!(
                !m.unit.is_empty() && !m.unit.contains('/') && !m.unit.starts_with('.'),
                "membership page {} has a bad unit name",
                m.doc_id
            );
        }
        Ok(())
    }

    /// Checks each page against the page unit it names under `pages`
    /// (the staging root's `pages/`): that unit's `qwen.json` must still
    /// hash to the recorded checksum and be the single-page job with the
    /// recorded id, page and checksum, so members cannot trade identities.
    pub fn check_sources(&self, pages: &Path) -> anyhow::Result<()> {
        for m in &self.pages {
            let bytes = read(&pages.join(&m.unit).join(JOB_FILE), MAX_JOB_JSON)
                .with_context(|| format!("membership page {} source unit", m.doc_id))?;
            ensure!(
                sha256(&bytes) == m.job_sha256,
                "membership page {} source job checksum differs",
                m.doc_id
            );
            let pj: Job = serde_json::from_slice(&bytes)?;
            ensure!(
                pj.pages.is_empty()
                    && pj.id == m.job_id
                    && pj.doc_id == m.doc_id
                    && pj.input.sha256 == m.page_sha256
                    && pj.input.bytes == m.bytes,
                "membership page {} is not its source unit's job",
                m.doc_id
            );
        }
        Ok(())
    }
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
    async fn oversized_crop_retains_exact_evidence_and_does_not_block_valid_pages() {
        for bundle_size in [1, 2] {
            let tmp = tempfile::tempdir().unwrap();
            let qroot = tmp.path().join("quality");
            let bad = "sn83045838_1930-06-18_ed-1_seq-3";
            let good = "sn99999999_1930-06-18_ed-1_seq-1";
            let source = crate::quality::units(&qroot).join(bad);
            crate::quality::work::tests::unit(&source);
            let page = std::fs::read(source.join(PAGE_FILE)).unwrap();
            let text = "יי".repeat(100);
            let words: Vec<_> = (0..32)
                .map(|i| (10 + i * 27, 10, 35 + i * 27, 40, text.as_str(), 0.9))
                .collect();
            let alto = crate::quality::alto_xml(1000, 1400, &[vec![words]]);
            let texts =
                serde_json::from_slice(&std::fs::read(source.join("texts.json")).unwrap()).unwrap();
            crate::quality::stage::local_unit(&crate::quality::stage::LocalUnit {
                page: &page,
                alto: alto.as_bytes(),
                texts: &texts,
                places: "Albany\n",
                dir: &source,
                engine: usnm_ocr_contract::quality::Engine {
                    image: format!("sha256:{}", "a".repeat(64)),
                },
                function_words: vec![],
                crop_seed: 522,
                k_crops: 12,
            })
            .unwrap();
            let quality = std::fs::read(source.join("quality.json")).unwrap();
            let valid = crate::quality::units(&qroot).join(good);
            let mut good_job = crate::quality::work::tests::unit(&valid);
            good_job.doc_id = good.into();
            good_job.id = good_job.identity().unwrap();
            std::fs::write(
                valid.join("quality.json"),
                serde_json::to_vec_pretty(&good_job).unwrap(),
            )
            .unwrap();
            let opts = Options {
                quality_root: qroot,
                root: tmp.path().join("qwen"),
                engine: engine(),
                gpu_timeout_secs: 14400,
                cpu_timeout_secs: 7200,
                max_tokens_per_1000px2: 4,
                max_pages: 1,
                bundle_size,
            };
            for invalid in [
                Options {
                    cpu_timeout_secs: 1,
                    ..opts.clone()
                },
                Options {
                    max_tokens_per_1000px2: 0,
                    ..opts.clone()
                },
            ] {
                let error = run(&invalid).await.unwrap_err();
                assert!(format!("{error:#}").contains("invalid"), "{error:#}");
                let rejected = invalid
                    .root
                    .join(if bundle_size == 1 { "units" } else { "pages" })
                    .join(bad);
                assert!(!rejected.join(TOO_LARGE).exists());
                assert!(!rejected.join(JOB_FILE).exists());
            }
            let first = run(&opts).await.unwrap();
            assert_eq!((first.too_large, first.staged), (1, 0));
            let rejected = opts
                .root
                .join(if bundle_size == 1 { "units" } else { "pages" })
                .join(bad);
            let evidence = std::fs::read(rejected.join("rejected-qwen.json")).unwrap();
            let candidate: Job = serde_json::from_slice(&evidence).unwrap();
            assert_eq!(
                candidate.crops,
                cut(&page, alto.as_bytes(), 522, 12).unwrap()
            );
            assert!(candidate.crops.iter().any(|c| c.loc_text.contains(&text)));
            assert!(candidate
                .validate()
                .unwrap_err()
                .to_string()
                .contains("crop text over size limit"));
            assert!(!rejected.join(JOB_FILE).exists());
            assert_eq!(
                std::fs::read(rejected.join("loc.xml")).unwrap(),
                alto.as_bytes()
            );
            assert_eq!(
                std::fs::read(rejected.join("quality.json")).unwrap(),
                quality
            );
            let reason = std::fs::read(rejected.join(TOO_LARGE)).unwrap();
            assert!(String::from_utf8_lossy(&reason).contains("8192-byte limit"));
            assert!(String::from_utf8_lossy(&reason)
                .contains(&usnm_ocr_contract::boinc::sha256(alto.as_bytes())));
            let second = run(&opts).await.unwrap();
            assert_eq!((second.already, second.staged, second.too_large), (1, 1, 0));
            for entry in std::fs::read_dir(units(&opts.root)).unwrap() {
                let path = entry.unwrap().path().join(JOB_FILE);
                if path.exists() {
                    let job: Job = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
                    job.validate().unwrap();
                    assert!(job.crops.iter().all(|c| c.loc_text.len() <= MAX_CROP_TEXT));
                }
            }
            let third = run(&opts).await.unwrap();
            assert_eq!((third.already, third.staged, third.too_large), (2, 0, 0));
            assert_eq!(
                std::fs::read(rejected.join("rejected-qwen.json")).unwrap(),
                evidence
            );
            assert_eq!(std::fs::read(rejected.join(TOO_LARGE)).unwrap(), reason);
        }
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
                too_large: 0,
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
                too_large: 0,
            }
        );

        // The bundled root refuses one-page staging, which would stage its
        // pages again as single-page units.
        let single = Options {
            bundle_size: 1,
            ..opts.clone()
        };
        let err = run(&single).await.unwrap_err();
        assert!(format!("{err:#}").contains("keep bundling"), "{err:#}");
        assert_eq!(std::fs::read_dir(units(&opts.root)).unwrap().count(), 2);
    }

    /// A staged single-page unit in `page_dir` with `crops` crops, each with
    /// the longest LoC text a crop may carry.
    pub(crate) fn page_unit(page_dir: &Path, doc_id: &str, crops: usize) {
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
        assert!(page_dir.join(big).join(TOO_LARGE).exists());
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
        {
            let _other = crate::Lock::take(&bundle_lock(&units_dir).unwrap()).unwrap();
            assert!(pack_bundles(&page_dir, &units_dir, 16).is_err());
        }
    }

    #[test]
    fn bundles_split_before_readings_could_pass_the_upload_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let page_dir = tmp.path().join("pages");
        let units_dir = tmp.path().join("units");
        std::fs::create_dir_all(&units_dir).unwrap();
        // 32 crops a page with short LoC text: the manifest stays small, but
        // each crop may come back as 8 KiB of text escaped to 16 KiB, so three
        // pages (96 crops) fit in 2 MiB of readings and four (128) do not.
        for i in 1..=8 {
            let doc = format!("sn83045462_1890-05-03_ed-1_seq-{i}");
            page_unit(&page_dir, &doc, 32);
            let path = page_dir.join(&doc).join(JOB_FILE);
            let mut job: Job = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            for c in &mut job.crops {
                c.loc_text = "a".into();
            }
            job.id = job.identity().unwrap();
            std::fs::write(&path, serde_json::to_vec_pretty(&job).unwrap()).unwrap();
        }
        assert_eq!(pack_bundles(&page_dir, &units_dir, 32).unwrap(), (3, 0));
        let mut sizes = Vec::new();
        for e in std::fs::read_dir(&units_dir).unwrap() {
            let job: Job =
                serde_json::from_slice(&std::fs::read(e.unwrap().path().join(JOB_FILE)).unwrap())
                    .unwrap();
            sizes.push(job.pages.len());
        }
        sizes.sort_unstable();
        assert_eq!(sizes, [2, 3, 3]);
    }

    #[test]
    fn longest_readings_len_is_the_written_worst_case() {
        // The count is the size of the worst readings.json written out, plus
        // the diag reserve.
        let tmp = tempfile::tempdir().unwrap();
        let page_dir = tmp.path().join("pages");
        let doc = "sn83045462_1890-05-03_ed-1_seq-1";
        page_unit(&page_dir, doc, 2);
        let job: Job =
            serde_json::from_slice(&std::fs::read(page_dir.join(doc).join(JOB_FILE)).unwrap())
                .unwrap();
        let readings = Readings {
            v: VERSION,
            job_id: "0".repeat(64),
            device: "vulkan".into(),
            diag: Default::default(),
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::UnitError,
            prompt_version: PROMPT_VERSION,
            readings: job
                .crops
                .iter()
                .map(|c| Reading {
                    id: format!("{doc}:{}", c.id),
                    ms: u64::MAX,
                    text: "\"".repeat(MAX_CROP_TEXT),
                    tokens: u32::MAX,
                })
                .collect(),
        };
        let written = serde_json::to_vec_pretty(&readings).unwrap().len();
        assert!(written > 2 * 2 * MAX_CROP_TEXT);
        assert_eq!(
            longest_readings_len(std::slice::from_ref(&job)).unwrap(),
            written + READINGS_DIAG_RESERVE
        );
    }

    #[test]
    fn pages_staged_with_other_settings_are_bundled_apart() {
        let tmp = tempfile::tempdir().unwrap();
        let page_dir = tmp.path().join("pages");
        let units_dir = tmp.path().join("units");
        std::fs::create_dir_all(&units_dir).unwrap();
        // Pages 1 and 3 left by an earlier run with a two-hour GPU deadline,
        // pages 2 and 4 staged now with four hours.
        for i in 1..=4 {
            let doc = format!("sn83045462_1890-05-03_ed-1_seq-{i}");
            page_unit(&page_dir, &doc, 2);
            if i % 2 == 1 {
                let path = page_dir.join(&doc).join(JOB_FILE);
                let mut job: Job = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                job.gpu_timeout_secs = 7200;
                job.id = job.identity().unwrap();
                std::fs::write(&path, serde_json::to_vec_pretty(&job).unwrap()).unwrap();
            }
        }
        assert_eq!(pack_bundles(&page_dir, &units_dir, 16).unwrap(), (2, 0));
        let mut seen = Vec::new();
        for e in std::fs::read_dir(&units_dir).unwrap() {
            let job: Job =
                serde_json::from_slice(&std::fs::read(e.unwrap().path().join(JOB_FILE)).unwrap())
                    .unwrap();
            let mut per_page = BTreeSet::new();
            for bp in &job.pages {
                let page: Job = serde_json::from_slice(
                    &std::fs::read(page_dir.join(&bp.doc_id).join(JOB_FILE)).unwrap(),
                )
                .unwrap();
                assert_eq!(
                    bundle_deadline(page.gpu_timeout_secs, job.pages.len()),
                    job.gpu_timeout_secs,
                    "{}",
                    bp.doc_id
                );
                per_page.insert(page.gpu_timeout_secs);
            }
            assert_eq!(per_page.len(), 1);
            seen.push((*per_page.first().unwrap(), job.pages.len()));
        }
        seen.sort_unstable();
        assert_eq!(seen, [(7200, 2), (14_400, 2)]);

        // write_bundle refuses a mixed set outright.
        let mixed = [
            page_dir.join("sn83045462_1890-05-03_ed-1_seq-1"),
            page_dir.join("sn83045462_1890-05-03_ed-1_seq-2"),
        ];
        assert!(write_bundle(&tmp.path().join("mixed"), &mixed).is_err());
        assert!(!tmp.path().join("mixed").exists());
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
