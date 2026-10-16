//! Japanese BOINC work, separate from the native Tesseract reservation ledger.
use crate::{atomic, read, Lock};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use usnm_core::ids::PageKey;
use usnm_ocr_adapter::ndlocr::{self, Geometry, COMMIT, PROFILE};
use usnm_ocr_contract::{
    boinc::{self, sha256},
    routing::{EngineKind, EngineRoute},
};
use usnm_store::ObjectStore;

pub const APP: &str = "usnm_ndlocr";
pub const JOB_FILE: &str = "ndlocr.json";
pub const OUTPUTS: [&str; 3] = ["text.txt", "layout.xml", "result.json"];
pub const MIN_SIMILARITY: f64 = 0.99;
pub const MEMORY_BOUND: &str = "4294967296";
pub const FPOPS_EST: &str = "1e12";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Job {
    pub v: u32,
    pub id: String,
    pub page: String,
    pub batch: String,
    pub batch_version: u16,
    /// LoC failure classification, not the generic queue reason `bulk`.
    pub loc_text: String,
    pub route: EngineRoute,
    pub engine: boinc::Engine,
    pub scan: boinc::Scan,
}
impl Job {
    pub fn identity(&self) -> anyhow::Result<String> {
        let mut job = self.clone();
        job.id.clear();
        Ok(sha256(&serde_json::to_vec(&job)?))
    }
    pub fn workunit(&self) -> String {
        format!("usnm_ndlocr_{}", self.id)
    }
    pub fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.v == 1 && boinc::is_sha256(&self.id) && self.id == self.identity()?,
            "invalid Japanese job identity"
        );
        let key: PageKey = self.page.parse()?;
        ensure!(key.to_string() == self.page, "noncanonical page key");
        ensure!(
            matches!(
                self.loc_text.as_str(),
                "missing" | "empty" | "short" | "garbled" | "mixed"
            ),
            "invalid LoC text classification"
        );
        ensure!(
            !self.batch.is_empty()
                && self.batch.len() <= 128
                && self
                    .batch
                    .bytes()
                    .all(|c| c.is_ascii_alphanumeric() || c == b'_'),
            "invalid batch"
        );
        ensure!(
            (1..=32767).contains(&self.batch_version),
            "invalid batch version"
        );
        let route = usnm_ocr_worker::engine::candidates("jpn")
            .into_iter()
            .next()
            .context("no Japanese route")?;
        ensure!(
            self.route == route && route.kind == EngineKind::NdlocrLite && route.profile == PROFILE,
            "Japanese route differs"
        );
        ensure!(
            self.engine.kind == route.kind
                && self.engine.profile == route.profile
                && self.engine.version == COMMIT
                && self.engine.models == model_digest(),
            "Japanese engine differs from lock"
        );
        ensure!(
            self.engine
                .image
                .strip_prefix("sha256:")
                .is_some_and(boinc::is_sha256),
            "engine image must be a full digest"
        );
        ensure!(
            self.engine.psm == 3
                && self.engine.dpi == 300
                && (1..=3600).contains(&self.engine.timeout_secs),
            "invalid engine options"
        );
        ensure!(
            self.scan.bytes > 0
                && self.scan.bytes <= boinc::MAX_SCAN as u64
                && boinc::is_sha256(&self.scan.sha256),
            "invalid scan"
        );
        for url in [&self.scan.url, &self.scan.iiif_base] {
            ensure!(
                url.starts_with("https://tile.loc.gov/")
                    && url.len() <= 512
                    && !url.chars().any(|c| c.is_control() || c.is_whitespace()),
                "invalid scan URL"
            );
        }
        Ok(())
    }
}
pub fn model_digest() -> String {
    ndlocr::models_digest()[..19].to_owned()
}
pub fn engine(image: String, timeout_secs: u32) -> boinc::Engine {
    boinc::Engine {
        kind: EngineKind::NdlocrLite,
        image,
        version: COMMIT.into(),
        profile: PROFILE.into(),
        models: model_digest(),
        psm: 3,
        dpi: 300,
        timeout_secs,
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ResultManifest {
    pub job: Job,
    pub text_sha256: String,
    pub alto_sha256: String,
    pub elapsed_ms: u64,
    pub completed_at: String,
}

pub fn load_job(unit: &Path) -> anyhow::Result<Job> {
    let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), boinc::MAX_JSON)?)?;
    job.validate()?;
    Ok(job)
}

/// `reason:jpn:doc_id:batch_verNN` pages use the existing polite scan source.
pub async fn stage(
    root: &Path,
    pages: &[crate::surya::Page],
    spec: boinc::Engine,
    archive: Option<&str>,
    cache: Option<PathBuf>,
) -> anyhow::Result<usize> {
    let _lock = Lock::take(root)?;
    crate::assess::check_no_hold(root)?;
    let sources = crate::scans::Sources::new(
        crate::scans::loc_client(crate::scans::MIN_GAP, cache)?,
        crate::scans::archive(archive)?,
        boinc::MAX_SCAN,
    );
    let mut staged = 0;
    for page in pages {
        ensure!(
            page.lang == "jpn" && page.batches.len() == 1,
            "Japanese staging needs jpn and one published batch"
        );
        let key = PageKey::from_doc_id(&page.doc_id)?;
        let (batch, version) = &page.batches[0];
        let unit = root.join("units").join(key.doc_id());
        // Validate all metadata before making a network request.
        let mut job = Job {
            v: 1,
            id: String::new(),
            page: key.to_string(),
            batch: batch.clone(),
            batch_version: *version,
            loc_text: page.group.clone(),
            route: usnm_ocr_worker::engine::candidates("jpn").remove(0),
            engine: spec.clone(),
            scan: boinc::Scan {
                url: "https://tile.loc.gov/pending".into(),
                iiif_base: "https://tile.loc.gov/pending".into(),
                sha256: sha256(b"pending"),
                bytes: 1,
                md5: None,
            },
        };
        job.id = job.identity()?;
        job.validate()?;
        if unit.join(JOB_FILE).exists() {
            let old = load_job(&unit)?;
            ensure!(
                old.page == job.page
                    && old.batch == job.batch
                    && old.batch_version == job.batch_version
                    && old.loc_text == job.loc_text
                    && old.engine == job.engine,
                "staged page specification changed"
            );
            continue;
        }
        let scan = match sources.jpeg(&key, &page.batches).await {
            Ok(Some(scan)) => scan,
            Ok(None) => anyhow::bail!("{}: scan not found", key.doc_id()),
            Err(error) => {
                atomic(
                    &crate::assess::hold_path(root),
                    format!("{}: {error}\n", key.doc_id()).as_bytes(),
                )?;
                return Err(anyhow::anyhow!("{error}"));
            }
        };
        let iiif_base = scan
            .url
            .split("/full/")
            .next()
            .context("IIIF URL")?
            .to_owned();
        job.scan = boinc::Scan {
            url: scan.url,
            iiif_base,
            sha256: sha256(&scan.bytes),
            bytes: scan.bytes.len() as u64,
            md5: None,
        };
        job.id = job.identity()?;
        job.validate()?;
        std::fs::create_dir_all(&unit)?;
        atomic(&unit.join("page"), &scan.bytes)?;
        atomic(&unit.join(JOB_FILE), &serde_json::to_vec_pretty(&job)?)?;
        staged += 1;
    }
    Ok(staged)
}

pub fn create_work_args(job: &Job) -> Vec<String> {
    [
        "--appname",
        APP,
        "--wu_name",
        &job.workunit(),
        "--wu_template",
        "templates/usnm_ndlocr_in",
        "--result_template",
        "templates/usnm_ndlocr_out",
        "--min_quorum",
        "2",
        "--target_nresults",
        "2",
        "--max_error_results",
        "3",
        "--max_total_results",
        "5",
        "--max_success_results",
        "3",
        "--rsc_fpops_est",
        FPOPS_EST,
        "--rsc_fpops_bound",
        "1e14",
        "--rsc_memory_bound",
        MEMORY_BOUND,
        "--rsc_disk_bound",
        "1073741824",
        "--delay_bound",
        "259200",
        &format!("{}_page", job.id),
        &format!("{}_ndlocr_json", job.id),
    ]
    .into_iter()
    .map(str::to_owned)
    .collect()
}
pub async fn submit(root: &Path, project: &Path, doc: &str) -> anyhow::Result<()> {
    crate::assess::check_doc_id(doc)?;
    let _lock = Lock::take(root)?;
    let unit = root.join("units").join(doc);
    let job = load_job(&unit)?;
    ensure!(
        PageKey::from_doc_id(doc)?.to_string() == job.page,
        "unit/page mismatch"
    );
    if unit.join("submitted").exists() {
        return Ok(());
    }
    ensure!(
        !unit.join("submitting").exists(),
        "uncertain submission; inspect workunit and reconcile markers manually"
    );
    let scan = read(&unit.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        scan.len() as u64 == job.scan.bytes && sha256(&scan) == job.scan.sha256,
        "staged scan changed"
    );
    let project = project.canonicalize()?;
    atomic(&unit.join("submitting"), job.workunit().as_bytes())?;
    for (suffix, file, cap) in [
        ("page", "page", boinc::MAX_SCAN),
        ("ndlocr_json", JOB_FILE, boinc::MAX_JSON),
    ] {
        crate::assess::submit::stage_file(
            &project,
            &format!("{}_{suffix}", job.id),
            &read(&unit.join(file), cap)?,
        )
        .await?;
    }
    let status = tokio::process::Command::new(project.join("bin/create_work"))
        .current_dir(project)
        .args(create_work_args(&job))
        .status()
        .await?;
    ensure!(status.success(), "create_work failed; submission uncertain");
    std::fs::rename(unit.join("submitting"), unit.join("submitted"))?;
    Ok(())
}

pub async fn work(dir: &Path) -> anyhow::Result<()> {
    let job = load_job(dir)?;
    for name in OUTPUTS {
        let p = dir.join(name);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    let scan = read(&dir.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        scan.len() as u64 == job.scan.bytes && sha256(&scan) == job.scan.sha256,
        "scan differs from job"
    );
    ensure!(
        std::env::var("USNM_NDLOCR_IMAGE").ok().as_deref() == Some(job.engine.image.as_str()),
        "engine image differs from job"
    );
    let root = PathBuf::from(std::env::var_os("NDLOCR_ROOT").context("NDLOCR_ROOT")?);
    let hello = ndlocr::hello(&root)?;
    ensure!(
        hello.engine.version == job.engine.version
            && hello
                .profiles
                .iter()
                .any(|p| p.id == job.route.profile && p.languages.iter().any(|l| l == "jpn")),
        "installed engine route differs"
    );
    let started = Instant::now();
    let deadline = started + Duration::from_secs(job.engine.timeout_secs.into());
    let runner = ndlocr::Ndlocr {
        root,
        python: "python3".into(),
        scratch: dir.to_owned(),
    };
    let output = runner.ocr(&dir.join("page"), deadline)?;
    let now = chrono::Utc::now();
    let alto = ndlocr::ndlocr_to_alto(
        &output.xml,
        Geometry {
            dpi: 300,
            ..Default::default()
        },
        &job.scan.url,
        COMMIT,
        1,
        &now.to_rfc3339_opts(chrono::SecondsFormat::Secs, false),
    )?;
    ensure!(Instant::now() < deadline, "conversion exceeded deadline");
    let result = ResultManifest {
        job,
        text_sha256: sha256(output.text.as_bytes()),
        alto_sha256: sha256(&alto),
        elapsed_ms: started.elapsed().as_millis().try_into().unwrap_or(u64::MAX),
        completed_at: now.to_rfc3339(),
    };
    check_bytes(&result, output.text.as_bytes(), &alto)?;
    atomic(&dir.join("text.txt"), output.text.as_bytes())?;
    atomic(&dir.join("layout.xml"), &alto)?;
    atomic(
        &dir.join("result.json"),
        &serde_json::to_vec_pretty(&result)?,
    )?;
    atomic(&dir.join("fraction_done"), b"1.0\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in OUTPUTS {
            std::fs::set_permissions(dir.join(name), std::fs::Permissions::from_mode(0o644))?;
        }
    }
    Ok(())
}

pub struct Loaded {
    pub result: ResultManifest,
    pub text: Vec<u8>,
    pub alto: Vec<u8>,
}
fn check_bytes(result: &ResultManifest, text: &[u8], alto: &[u8]) -> anyhow::Result<()> {
    result.job.validate()?;
    ensure!(
        text.len() <= boinc::MAX_TEXT
            && alto.len() <= boinc::MAX_ALTO
            && sha256(text) == result.text_sha256
            && sha256(alto) == result.alto_sha256,
        "result checksum/size differs"
    );
    let text = std::str::from_utf8(text)?;
    ensure!(
        ndlocr::normalize(text) == text,
        "text is not Japanese-normalized"
    );
    ensure!(
        result.elapsed_ms <= u64::from(result.job.engine.timeout_secs) * 1000,
        "result exceeded deadline"
    );
    chrono::DateTime::parse_from_rfc3339(&result.completed_at)?;
    let page = usnm_alto::parse(
        alto,
        &usnm_alto::ParseOptions {
            dpi: Some(300.0),
            ..Default::default()
        },
    )?;
    let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
    ensure!(
        compact(&usnm_alto::text(&page)) == compact(text),
        "ALTO/text disagree"
    );
    Ok(())
}
pub fn load(files: &[PathBuf]) -> anyhow::Result<Loaded> {
    ensure!(files.len() == 3, "expected text, ALTO and result files");
    let result = serde_json::from_slice(&read(&files[2], boinc::MAX_JSON)?)?;
    let text = read(&files[0], boinc::MAX_TEXT)?;
    let alto = read(&files[1], boinc::MAX_ALTO)?;
    check_bytes(&result, &text, &alto)?;
    Ok(Loaded { result, text, alto })
}
/// Ordered character bigram multiset Dice, not whitespace-delimited words.
pub fn similarity(a: &str, b: &str) -> f64 {
    fn grams(s: &str) -> BTreeMap<(char, char), usize> {
        let chars: Vec<_> = ndlocr::normalize(s)
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let mut out = BTreeMap::new();
        for pair in chars.windows(2) {
            *out.entry((pair[0], pair[1])).or_insert(0) += 1;
        }
        out
    }
    if a == b {
        return 1.0;
    }
    let (a, b) = (grams(a), grams(b));
    let total = a.values().sum::<usize>() + b.values().sum::<usize>();
    if total == 0 {
        return 0.0;
    }
    2.0 * a
        .iter()
        .map(|(g, n)| (*n).min(*b.get(g).unwrap_or(&0)))
        .sum::<usize>() as f64
        / total as f64
}
pub fn compare(a: &[PathBuf], b: &[PathBuf]) -> anyhow::Result<()> {
    let (a, b) = (load(a)?, load(b)?);
    ensure!(
        a.result.job == b.result.job,
        "results belong to different jobs"
    );
    let (a, b) = (std::str::from_utf8(&a.text)?, std::str::from_utf8(&b.text)?);
    let score = similarity(a, b);
    eprintln!("Japanese character-bigram Dice: {score:.6}; floor {MIN_SIMILARITY}");
    ensure!(
        score >= MIN_SIMILARITY,
        "Japanese character bigram similarity {score:.6} below {MIN_SIMILARITY}"
    );
    Ok(())
}

pub async fn collect(
    root: &Path,
    spool: &Path,
    doc: &str,
    curated: &dyn ObjectStore,
) -> anyhow::Result<()> {
    crate::assess::check_doc_id(doc)?;
    let _lock = Lock::take(root)?;
    let unit = root.join("units").join(doc);
    let job = load_job(&unit)?;
    ensure!(
        PageKey::from_doc_id(doc)?.to_string() == job.page,
        "unit/page mismatch"
    );
    ensure!(unit.join("submitted").exists(), "unit not submitted");
    if unit.join("published").exists() {
        return Ok(());
    }
    let wu = job.workunit();
    ensure!(
        !spool.join(format!("{wu}_error")).exists()
            && !spool.join(format!("{wu}_no_output_files")).exists(),
        "BOINC terminal failure"
    );
    let accepted = unit.join("accepted");
    let files: Vec<_> = if accepted.join("result.json").exists() {
        OUTPUTS.iter().map(|n| accepted.join(n)).collect()
    } else {
        OUTPUTS
            .iter()
            .map(|n| spool.join(format!("{wu}__file_{n}")))
            .collect()
    };
    let loaded = load(&files)?;
    ensure!(
        loaded.result.job == job,
        "canonical result does not match staged job"
    );
    if !accepted.join("result.json").exists() {
        atomic(&accepted.join("text.txt"), &loaded.text)?;
        atomic(&accepted.join("layout.xml"), &loaded.alto)?;
        atomic(
            &accepted.join("result.json"),
            &serde_json::to_vec_pretty(&loaded.result)?,
        )?;
    }
    let row = usnm_overlay::OverlayRow {
        key: job.page.parse()?,
        batch: job.batch,
        ocr_source: "usnm-ndlocr-lite".into(),
        ocr_engine: format!("ndlocr-lite/{COMMIT} {PROFILE}"),
        loc_text: job.loc_text,
        text: String::from_utf8(loaded.text)?,
        image_url: job.scan.iiif_base,
        ocred_at: chrono::DateTime::parse_from_rfc3339(&loaded.result.completed_at)?
            .with_timezone(&chrono::Utc),
    };
    let stem = format!("{doc}.boinc-{}", job.id);
    crate::put_same(
        curated,
        &format!("ocr-ja/alto/{stem}.xml"),
        loaded.alto,
        "application/xml",
    )
    .await?;
    crate::put_same(
        curated,
        &format!("ocr-ja/pages/{stem}.parquet"),
        usnm_overlay::write_part(&[row])?,
        "application/vnd.apache.parquet",
    )
    .await?;
    atomic(&unit.join("published"), job.id.as_bytes())?;
    Ok(())
}
