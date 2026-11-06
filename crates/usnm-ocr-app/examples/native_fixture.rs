//! Writes a unit into a slot directory for local runs: one `--scan` makes a page, more make a
//! bundle, and `--gold-root` makes a gold unit of a prepared campaign. For the parity runs and
//! smoke tests in docs/boinc-native-ocr.md; the jobs are never submitted, and the inputs are
//! whatever public files are passed in.
use anyhow::Context;
use clap::Parser;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use usnm_boinc::gold;
use usnm_ocr_contract::boinc::{self, bundle, Job};
use usnm_ocr_worker::engine;

#[derive(Parser)]
struct Args {
    #[arg(long)]
    scan: Vec<PathBuf>,
    /// ISO 639-3, routed as the worker routes it.
    #[arg(long, default_value = "eng")]
    language: String,
    /// A prepared gold campaign (`usnm-boinc gold prepare`).
    #[arg(long)]
    gold_root: Option<PathBuf>,
    /// With `--gold-root`, the engine base the job names.
    #[arg(long)]
    gold_base: Option<String>,
    /// The `engine.image` the job names.
    #[arg(long)]
    image: String,
    #[arg(long)]
    dir: PathBuf,
    #[arg(long, default_value_t = 1200)]
    timeout_secs: u32,
}

fn job(args: &Args, seq: usize, scan: &[u8]) -> anyhow::Result<Job> {
    let installed: BTreeSet<String> = engine::MODELS_LOCK
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| l.split_whitespace().next())
        .filter_map(|l| l.strip_prefix("tessdata/")?.strip_suffix(".traineddata"))
        .map(str::to_owned)
        .collect();
    let route = engine::route(&args.language, &installed).context("no route")?;
    let models = engine::models_digest(engine::MODELS_LOCK, &route.profile).context("no models")?;
    let mut job: Job = serde_json::from_value(serde_json::json!({
        "v": 1, "id": "",
        "message": {"v": 1, "page": format!("sn00000001/1900-01-01/ed-1/seq-{}", seq + 1),
            "batch": "native_fixture", "language": args.language, "reason": "bulk",
            "queued_at": "2026-11-01T00:00:00Z"},
        "batch_version": 1,
        "scan": {"url": "https://tile.loc.gov/fixture.jp2", "iiif_base": "https://tile.loc.gov/fixture",
            "sha256": boinc::sha256(scan), "bytes": scan.len(), "md5": null},
        "engine": {"image": args.image, "version": "5.5.0", "profile": route.profile,
            "models": models.to_string(), "psm": 3, "dpi": 300,
            "timeout_secs": args.timeout_secs}
    }))?;
    job.id = job.identity()?;
    job.validate()?;
    Ok(job)
}

/// A gold unit, and the validator's expectations for it beside the campaign.
fn gold_unit(args: &Args, root: &Path) -> anyhow::Result<()> {
    let prepared = gold::prepare::read(root)?;
    let job = gold::submit::job(
        &prepared,
        &gold::job::Engine {
            base: args.gold_base.clone().context("--gold-base")?,
            dpi: 300,
            image: args.image.clone(),
            psm: 3,
            timeout_secs: 300,
            version: "5.5.0".into(),
        },
        0,
    )?;
    let expected = root.join("expected");
    std::fs::create_dir_all(&expected)?;
    gold::validate::add_jobs(&expected, &prepared, std::slice::from_ref(&job))?;
    std::fs::write(
        args.dir.join(gold::job::JOB_FILE),
        serde_json::to_vec(&job)?,
    )?;
    std::fs::copy(
        root.join(gold::prepare::BUNDLE_COPY),
        args.dir.join(gold::job::BUNDLE_FILE),
    )?;
    Ok(())
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    std::fs::create_dir_all(&args.dir)?;
    if let Some(root) = &args.gold_root {
        return gold_unit(&args, root);
    }
    let scans = args
        .scan
        .iter()
        .map(std::fs::read)
        .collect::<Result<Vec<_>, _>>()?;
    if let [scan] = &scans[..] {
        std::fs::write(args.dir.join("page"), scan)?;
        let job = job(&args, 0, scan)?;
        std::fs::write(args.dir.join("request.json"), serde_json::to_vec(&job)?)?;
        return Ok(());
    }
    let jobs = scans
        .iter()
        .enumerate()
        .map(|(seq, scan)| job(&args, seq, scan))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let unit = bundle::Bundle::new(jobs)?;
    std::fs::write(args.dir.join(bundle::PAGES_FILE), scans.concat())?;
    std::fs::write(args.dir.join(bundle::JOB_FILE), serde_json::to_vec(&unit)?)?;
    Ok(())
}
