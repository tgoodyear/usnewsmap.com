//! One page inside BOINC's job container. No Azure or LoC operations.
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use usnm_ocr_contract::boinc::{self, Job, ResultManifest};
use usnm_ocr_worker::engine::{self, Engine, Runner, Tesseract, TesseractOptions};

pub async fn run(work: &Path) -> anyhow::Result<()> {
    let work = work.canonicalize()?;
    if work.join(crate::assess::JOB_FILE).exists() {
        return crate::assess::work::run(&work).await;
    }
    if work.join(crate::gold::job::JOB_FILE).exists() {
        return crate::gold::work::run(&work).await;
    }
    if work.join(crate::surya::JOB_FILE).exists() {
        return crate::surya::work::run(&work).await;
    }
    if work.join(crate::quality::JOB_FILE).exists() {
        return crate::quality::work::run(&work).await;
    }
    if work.join(crate::qwen::JOB_FILE).exists() {
        return crate::qwen::work::run(&work).await;
    }
    if work.join(crate::ndlocr::JOB_FILE).exists() {
        return crate::ndlocr::work(&work).await;
    }
    if work
        .join(usnm_ocr_contract::boinc::bundle::JOB_FILE)
        .exists()
    {
        return crate::bundle::work::run(&work).await;
    }
    for name in ["result.json", "text.txt", "layout.xml", "fraction_done"] {
        let p = work.join(name);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    let job: Job = serde_json::from_slice(&read(&work.join("request.json"), boinc::MAX_JSON)?)?;
    job.validate()?;
    let image = read(&work.join("page"), boinc::MAX_SCAN)?;
    ensure!(
        image.len() as u64 == job.scan.bytes && boinc::sha256(&image) == job.scan.sha256,
        "input scan checksum mismatch"
    );
    check_models(&job.engine)?;
    let engine = tesseract(&job.engine).await?;
    check_route(&engine, &job)?;
    let (result, text, alto) = ocr_page(&engine, &job, &work.join("page")).await?;
    atomic(&work.join("text.txt"), text.as_bytes())?;
    atomic(&work.join("layout.xml"), &alto)?;
    atomic(
        &work.join("result.json"),
        &serde_json::to_vec_pretty(&result)?,
    )?;
    atomic(&work.join("fraction_done"), b"1.0\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in ["text.txt", "layout.xml", "result.json", "fraction_done"] {
            std::fs::set_permissions(work.join(name), std::fs::Permissions::from_mode(0o644))?;
        }
    }
    Ok(())
}

/// The image's Tesseract, set up for `spec` (one thread, the page deadline).
pub(crate) async fn tesseract(
    spec: &usnm_ocr_contract::boinc::Engine,
) -> anyhow::Result<Tesseract> {
    Tesseract::detect(TesseractOptions {
        runner: Runner::Host("tesseract".into()),
        psm: spec.psm,
        dpi: spec.dpi.into(),
        threads: 1,
        timeout: read_timeout(spec.timeout_secs),
    })
    .await
}

/// Set in the native worker's unit to the deadline of one read, in CPU seconds
/// ([`crate::native`]).
pub(crate) const CPU_DEADLINE_ENV: &str = "USNM_TESSERACT_CPU_SECS";
/// A native task's reads by the clock: long enough for any suspension BOINC allows a task.
const NATIVE_WALL_TIMEOUT: Duration = Duration::from_secs(30 * 24 * 3600);

/// The clock's deadline on one read. Under the native worker the engine has a CPU limit
/// instead, which doesn't run while BOINC has the task suspended; this clock would.
pub(crate) fn read_timeout(secs: u32) -> Duration {
    if std::env::var_os(CPU_DEADLINE_ENV).is_some() {
        NATIVE_WALL_TIMEOUT
    } else {
        Duration::from_secs(secs.into())
    }
}

/// The installed engine is the job's version and routes its language to its profile.
pub(crate) fn check_route(engine: &Tesseract, job: &Job) -> anyhow::Result<()> {
    ensure!(
        engine.version() == job.engine.version
            && engine::route(&job.message.language, engine.installed())
                == Some(engine::EngineRoute {
                    kind: job.engine.kind,
                    profile: job.engine.profile.clone()
                }),
        "engine version/route differs from job"
    );
    Ok(())
}

/// Reads one page's scan at `image` and checks the output: the result manifest, the text and
/// the ALTO, as a single-page unit returns them.
pub(crate) async fn ocr_page(
    engine: &dyn Engine,
    job: &Job,
    image: &Path,
) -> anyhow::Result<(ResultManifest, String, Vec<u8>)> {
    let started = Instant::now();
    let scratch = tempfile::tempdir()?;
    let output = engine
        .ocr(image, &job.message.language, scratch.path())
        .await?;
    ensure!(
        output.text.len() <= boinc::MAX_TEXT && output.alto.len() <= boinc::MAX_ALTO,
        "result over limit"
    );
    usnm_alto::check(&output.alto, &usnm_alto::Limits::default())?;
    let result = ResultManifest {
        v: job.v,
        job_id: job.id.clone(),
        scan_sha256: job.scan.sha256.clone(),
        engine: job.engine.clone(),
        text_sha256: boinc::sha256(output.text.as_bytes()),
        alto_sha256: boinc::sha256(&output.alto),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        completed_at: chrono::Utc::now().to_rfc3339(),
    };
    Ok((result, output.text, output.alto))
}

/// Checks the installed models of `engine.profile` against the lock and
/// the job's profile digest.
pub fn check_models(spec: &usnm_ocr_contract::boinc::Engine) -> anyhow::Result<()> {
    ensure!(
        spec.kind == engine::EngineKind::Tesseract,
        "this worker only supports Tesseract jobs"
    );
    let models = std::env::var_os("TESSDATA_PREFIX")
        .context("TESSDATA_PREFIX must point to the locked image's models")?;
    for name in spec.profile.split('+') {
        let physical = format!("tessdata/{name}.traineddata");
        let expected = engine::MODELS_LOCK
            .lines()
            .filter(|s| !s.starts_with('#'))
            .find_map(|s| {
                let fields: Vec<_> = s.split_whitespace().collect();
                if fields.len() == 3 && fields[0] == physical {
                    Some(fields[2])
                } else {
                    None
                }
            })
            .context("model not in lock")?;
        ensure!(
            boinc::sha256(&read(
                &Path::new(&models).join(format!("{name}.traineddata")),
                64 << 20
            )?) == expected,
            "installed model checksum mismatch"
        );
    }
    ensure!(
        engine::models_digest(engine::MODELS_LOCK, &spec.profile)
            .context("model digest")?
            .to_string()
            == spec.models,
        "profile digest mismatch"
    );
    Ok(())
}
