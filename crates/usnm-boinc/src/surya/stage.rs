//! Get each page's full-size IIIF JPEG and write its unit.
//!
//! The scan comes from [`crate::scans`]: the scan archive first, then
//! tile.loc.gov through the page's batch (bag manifest and METS on the
//! storage service, then the IIIF JPEG), never www.loc.gov's item API.
//! Politeness as for the assessment (`assess::stage`): one LoC request at a
//! time and at least `gap` (5 s or more) between any two; no retries; a
//! 403, 429 or 5xx, a network failure, or an HTML page in place of a file
//! writes a hold and stops the run. A page no source has is marked missing.
use super::{check_no_hold, check_word, units, Page, JOB_FILE, PAGE_FILE};
use crate::{atomic, scans};
use anyhow::{ensure, Context};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::{
    boinc::{is_sha256, sha256},
    surya::{Engine, Input, InputMode, Job, MAX_SCAN, VERSION},
};
use usnm_ocr_worker::loc::FetchError;

pub const MIN_GAP: Duration = scans::MIN_GAP;

#[derive(Debug, Clone)]
pub struct Options {
    pub pages: Vec<Page>,
    pub root: PathBuf,
    pub engine: Engine,
    pub gpu_modes: Vec<InputMode>,
    pub cpu_modes: Vec<InputMode>,
    pub gpu_timeout_secs: u32,
    pub cpu_timeout_secs: u32,
    pub gap: Duration,
    pub max_pages: usize,
    /// The scan archive's container URL; None: LoC only.
    pub scan_archive: Option<String>,
    /// Where batch manifests and METS are kept between runs.
    pub batch_cache: Option<PathBuf>,
    /// The sample rows the pages came from (`--sample`), by doc id: each is
    /// kept in its unit as `sample.json`, as `quality stage` does, so a
    /// collector can weigh the page.
    pub sample_rows: BTreeMap<String, crate::assess::SampleRow>,
}

/// The sample row kept in a unit staged from `--sample`.
pub const SAMPLE_FILE: &str = crate::quality::stage::SAMPLE_FILE;

#[derive(Debug, Default)]
pub struct Summary {
    pub staged: usize,
    pub missing: usize,
    pub already: usize,
}

pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    ensure!(opts.gap >= MIN_GAP, "--gap-secs must be at least 5");
    ensure!(
        opts.engine
            .image
            .strip_prefix("sha256:")
            .is_some_and(is_sha256),
        "--engine-image must be the worker image's full sha256 digest"
    );
    check_no_hold(&opts.root)?;
    let dir = units(&opts.root);
    std::fs::create_dir_all(&dir)?;
    let src = scans::Sources::new(
        scans::loc_client(opts.gap, opts.batch_cache.clone())?,
        scans::archive(opts.scan_archive.as_deref())?,
        MAX_SCAN,
    );
    let mut summary = Summary::default();
    for p in &opts.pages {
        if summary.staged + summary.missing >= opts.max_pages {
            break;
        }
        check_word(&p.group, "group")?;
        check_word(&p.lang, "language")?;
        let unit = dir.join(&p.doc_id);
        if unit.join(JOB_FILE).exists() || unit.join("missing").exists() {
            summary.already += 1;
            continue;
        }
        std::fs::create_dir_all(&unit)?;
        match page(&src, p, &unit, opts).await {
            Ok(true) => summary.staged += 1,
            Ok(false) => summary.missing += 1,
            Err(e) => {
                atomic(
                    &crate::assess::hold_path(&opts.root),
                    format!("{}: {e:#}\n", p.doc_id).as_bytes(),
                )?;
                return Err(e.context(format!("stopped at {}", p.doc_id)));
            }
        }
    }
    Ok(summary)
}

fn missing(unit: &Path, why: &str) -> anyhow::Result<bool> {
    atomic(&unit.join("missing"), format!("{why}\n").as_bytes())?;
    Ok(false)
}

async fn page(src: &scans::Sources, p: &Page, unit: &Path, opts: &Options) -> anyhow::Result<bool> {
    let key = PageKey::from_doc_id(&p.doc_id)?;
    let jpg = match src.jpeg(&key, &p.batches).await {
        Ok(Some(g)) => g,
        Ok(None) => {
            return missing(
                unit,
                "not in the scan archive, and none of its batches places it on tile.loc.gov",
            )
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    };
    match src.alto(&key, &jpg.iiif_base).await {
        Ok(Some(b)) => atomic(&unit.join("loc.xml"), &b)?,
        Ok(None) | Err(FetchError::Permanent(_)) => {
            atomic(&unit.join("loc.xml.missing"), b"404\n")?
        }
        Err(e) => return Err(anyhow::anyhow!("{e}")),
    }
    eprintln!(
        "{}: {} bytes from {}",
        p.doc_id,
        jpg.bytes.len(),
        if jpg.from_archive {
            "the scan archive"
        } else {
            "tile.loc.gov"
        }
    );
    ensure!(jpg.bytes.len() <= MAX_SCAN, "JPEG over size limit");
    atomic(&unit.join(PAGE_FILE), &jpg.bytes)?;
    if let Some(row) = opts.sample_rows.get(&p.doc_id) {
        atomic(&unit.join(SAMPLE_FILE), &serde_json::to_vec(row)?)?;
    }
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        doc_id: p.doc_id.clone(),
        group: p.group.clone(),
        language: p.lang.clone(),
        input: Input {
            bytes: jpg.bytes.len() as u64,
            sha256: sha256(&jpg.bytes),
            url: jpg.url.clone(),
        },
        engine: opts.engine.clone(),
        gpu_modes: opts.gpu_modes.clone(),
        cpu_modes: opts.cpu_modes.clone(),
        gpu_timeout_secs: opts.gpu_timeout_secs,
        cpu_timeout_secs: opts.cpu_timeout_secs,
        crop: None,
    };
    job.id = job.identity()?;
    job.validate().context("staged job")?;
    atomic(&unit.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
    Ok(true)
}

/// Writes a unit from a local image, for checking a host or an image
/// without LoC or BOINC: `dir` gets `page.jpg` and `surya.json`, and
/// `usnm-boinc work --dir <dir>` then runs it. The URL is a placeholder.
pub fn local_unit(
    image: &Path,
    dir: &Path,
    engine: Engine,
    modes: Vec<InputMode>,
    timeout_secs: u32,
) -> anyhow::Result<Job> {
    let bytes = crate::read(image, MAX_SCAN)?;
    std::fs::create_dir_all(dir)?;
    atomic(&dir.join(PAGE_FILE), &bytes)?;
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        doc_id: "local".into(),
        group: "local".into(),
        language: "und".into(),
        input: Input {
            bytes: bytes.len() as u64,
            sha256: sha256(&bytes),
            url: "https://tile.loc.gov/local".into(),
        },
        engine,
        gpu_modes: modes.clone(),
        cpu_modes: modes,
        gpu_timeout_secs: timeout_secs,
        cpu_timeout_secs: timeout_secs,
        crop: None,
    };
    job.id = job.identity()?;
    job.validate()?;
    atomic(&dir.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
    Ok(job)
}

/// Immutable, operator-pinned benchmark bundle. Reference files never enter BOINC downloads.
#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BenchmarkInput {
    pub sample_id: String,
    pub group: String,
    pub language: String,
    pub image: PathBuf,
    pub image_sha256: String,
    pub reference: PathBuf,
    pub reference_sha256: String,
    pub source_url: String,
    pub license: String,
}

/// Stage a bounded local bundle without changing pixels or fetching external URLs.
/// Uses the existing local-unit URL sentinel; actual provenance stays in benchmark-inputs.json.
pub fn benchmark(
    manifest: &Path,
    manifest_sha256: &str,
    root: &Path,
    engine: Engine,
) -> anyhow::Result<usize> {
    benchmark_with_cpu_modes(manifest, manifest_sha256, root, engine, &[InputMode::Tiled])
}

/// Opt in to identical scheduled modes on CPU and GPU without changing signed workers.
pub fn benchmark_with_cpu_modes(
    manifest: &Path,
    manifest_sha256: &str,
    root: &Path,
    engine: Engine,
    cpu_modes: &[InputMode],
) -> anyhow::Result<usize> {
    ensure!(
        !cpu_modes.is_empty()
            && cpu_modes
                .iter()
                .enumerate()
                .all(|(i, mode)| !cpu_modes[..i].contains(mode)),
        "CPU modes must be nonempty and unique"
    );
    check_no_hold(root)?;
    let raw = crate::read(manifest, 1024 * 1024)?;
    ensure!(
        is_sha256(manifest_sha256) && sha256(&raw) == manifest_sha256,
        "benchmark manifest checksum mismatch"
    );
    let inputs: Vec<BenchmarkInput> = serde_json::from_slice(&raw)?;
    ensure!(
        (1..=30).contains(&inputs.len()),
        "benchmark probe requires 1 to 30 inputs"
    );
    ensure!(
        !root.exists(),
        "benchmark campaign already exists; never overwrite it"
    );
    let base = manifest
        .canonicalize()?
        .parent()
        .context("manifest parent")?
        .to_owned();
    let mut staged = Vec::new();
    let mut ids = std::collections::BTreeSet::new();
    for input in &inputs {
        ensure!(
            !input.sample_id.is_empty()
                && input.sample_id.len() <= 120
                && input
                    .sample_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
            "invalid sample ID"
        );
        ensure!(
            ids.insert(&input.sample_id),
            "duplicate benchmark sample ID"
        );
        check_word(&input.group, "group")?;
        check_word(&input.language, "language")?;
        ensure!(
            input.source_url.starts_with("https://") && !input.license.trim().is_empty(),
            "source provenance and license required"
        );
        let load = |path: &Path, digest: &str, limit| -> anyhow::Result<Vec<u8>> {
            ensure!(!path.is_absolute(), "benchmark paths must be relative");
            let path = base.join(path).canonicalize()?;
            ensure!(path.starts_with(&base), "benchmark path escapes bundle");
            let data = crate::read(&path, limit)?;
            ensure!(
                is_sha256(digest) && sha256(&data) == digest,
                "benchmark input checksum mismatch"
            );
            Ok(data)
        };
        let image = load(&input.image, &input.image_sha256, MAX_SCAN)?;
        let reference = load(
            &input.reference,
            &input.reference_sha256,
            usnm_ocr_contract::surya::MAX_TEXT,
        )?;
        std::str::from_utf8(&reference).context("benchmark reference must be UTF-8")?;
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            doc_id: input.sample_id.clone(),
            group: input.group.clone(),
            language: input.language.clone(),
            input: Input {
                bytes: image.len() as u64,
                sha256: sha256(&image),
                url: "https://tile.loc.gov/local".into(),
            },
            engine: engine.clone(),
            gpu_modes: usnm_ocr_contract::surya::MODES.to_vec(),
            cpu_modes: cpu_modes.to_vec(),
            gpu_timeout_secs: 1800,
            cpu_timeout_secs: 5400,
            crop: None,
        };
        job.id = job.identity()?;
        job.validate()?;
        staged.push((job, image, reference));
    }
    // All checks finish before any campaign files are written. A partial write is never reused.
    if let Some(parent) = root
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::create_dir(root).context("campaign created concurrently; refusing overwrite")?;
    atomic(&root.join("benchmark-inputs.json"), &raw)?;
    atomic(
        &root.join("benchmark-inputs.sha256"),
        manifest_sha256.as_bytes(),
    )?;
    for (job, image, reference) in &staged {
        let unit = units(root).join(&job.doc_id);
        std::fs::create_dir_all(&unit)?;
        atomic(&unit.join(PAGE_FILE), image)?;
        atomic(&unit.join("reference.txt"), reference)?;
        atomic(&unit.join(JOB_FILE), &serde_json::to_vec_pretty(job)?)?;
    }
    atomic(&root.join("benchmark-ready"), manifest_sha256.as_bytes())?;
    Ok(staged.len())
}

/// Reject partially staged or modified benchmark campaigns before any create_work call.
pub fn verify_benchmark(root: &Path) -> anyhow::Result<()> {
    let manifest = root.join("benchmark-inputs.json");
    if !manifest.exists() {
        ensure!(
            !root.join("benchmark-ready").exists()
                && !root.join("benchmark-inputs.sha256").exists(),
            "benchmark manifest missing with stale verification markers"
        );
        return Ok(());
    }
    let raw = crate::read(&manifest, 1024 * 1024)?;
    let digest = sha256(&raw);
    ensure!(
        std::fs::read_to_string(root.join("benchmark-ready"))? == digest
            && std::fs::read_to_string(root.join("benchmark-inputs.sha256"))? == digest,
        "benchmark manifest changed or staging is incomplete"
    );
    let inputs: Vec<BenchmarkInput> = serde_json::from_slice(&raw)?;
    ensure!(
        std::fs::read_dir(units(root))?.count() == inputs.len(),
        "benchmark unit count changed"
    );
    for input in inputs {
        let unit = units(root).join(&input.sample_id);
        let job: Job = serde_json::from_slice(&crate::read(
            &unit.join(JOB_FILE),
            usnm_ocr_contract::surya::MAX_JSON,
        )?)?;
        job.validate()?;
        ensure!(
            job.doc_id == input.sample_id
                && job.group == input.group
                && job.language == input.language
                && job.input.sha256 == input.image_sha256,
            "benchmark job differs from input lock"
        );
        ensure!(
            sha256(&crate::read(&unit.join(PAGE_FILE), MAX_SCAN)?) == input.image_sha256
                && sha256(&crate::read(
                    &unit.join("reference.txt"),
                    usnm_ocr_contract::surya::MAX_TEXT
                )?) == input.reference_sha256,
            "benchmark image/reference changed after staging"
        );
    }
    Ok(())
}

#[cfg(test)]
mod benchmark_tests {
    use super::*;
    #[test]
    fn immutable_bundle_checks_all_inputs_before_writing() {
        let dir = tempfile::tempdir().unwrap();
        let manifest = dir.path().join("inputs.json");
        let root = dir.path().join("campaign");
        std::fs::write(dir.path().join("input.png"), b"image").unwrap();
        std::fs::write(dir.path().join("reference.txt"), b"reference").unwrap();
        let row = serde_json::json!({"sample_id":"overproof-1", "group":"english", "language":"eng",
            "image":"input.png", "image_sha256":sha256(b"image"), "reference":"reference.txt",
            "reference_sha256":sha256(b"reference"), "source_url":"https://example.org/published", "license":"Public domain"});
        let raw = serde_json::to_vec(&vec![row.clone()]).unwrap();
        std::fs::write(&manifest, &raw).unwrap();
        let engine = super::super::work::tests::job().engine;
        assert!(benchmark(&manifest, &"0".repeat(64), &root, engine.clone()).is_err());
        assert!(!root.exists());
        assert_eq!(
            benchmark(&manifest, &sha256(&raw), &root, engine.clone()).unwrap(),
            1
        );
        verify_benchmark(&root).unwrap();
        let original: Job = serde_json::from_slice(
            &std::fs::read(root.join("units/overproof-1/surya.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(original.cpu_modes, vec![InputMode::Tiled]);
        let paired = dir.path().join("paired");
        benchmark_with_cpu_modes(
            &manifest,
            &sha256(&raw),
            &paired,
            engine.clone(),
            &usnm_ocr_contract::surya::MODES,
        )
        .unwrap();
        verify_benchmark(&paired).unwrap();
        let paired_job: Job = serde_json::from_slice(
            &std::fs::read(paired.join("units/overproof-1/surya.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(paired_job.cpu_modes, paired_job.gpu_modes);
        assert_eq!(paired_job.input, original.input);
        assert_ne!(paired_job.id, original.id);
        assert_eq!(paired_job.id, paired_job.identity().unwrap());
        for modes in [vec![], vec![InputMode::Tiled, InputMode::Tiled]] {
            let invalid = dir.path().join("invalid-modes");
            assert!(benchmark_with_cpu_modes(
                &manifest,
                &sha256(&raw),
                &invalid,
                engine.clone(),
                &modes,
            )
            .is_err());
            assert!(!invalid.exists());
        }
        assert!(benchmark(&manifest, &sha256(&raw), &root, engine.clone()).is_err());
        std::fs::write(root.join("units/overproof-1/reference.txt"), b"changed").unwrap();
        assert!(verify_benchmark(&root).is_err());
        std::fs::remove_file(root.join("benchmark-inputs.json")).unwrap();
        assert!(verify_benchmark(&root).is_err());

        let mut bad = row;
        bad["image"] = serde_json::json!("../escape");
        let raw = serde_json::to_vec(&vec![bad]).unwrap();
        std::fs::write(&manifest, &raw).unwrap();
        assert!(benchmark(&manifest, &sha256(&raw), &dir.path().join("bad"), engine).is_err());
        assert!(!dir.path().join("bad").exists());
    }
}
