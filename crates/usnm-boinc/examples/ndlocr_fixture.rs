//! A local, real-image BOINC unit for the offline container smoke.
use clap::Parser;
use std::path::PathBuf;
use usnm_boinc::{atomic, ndlocr};
use usnm_ocr_contract::boinc::{self, sha256};
#[derive(Parser)]
struct Args {
    #[arg(long)]
    scan: PathBuf,
    #[arg(long)]
    dir: PathBuf,
    #[arg(long)]
    engine_image: String,
}
fn main() -> anyhow::Result<()> {
    let a = Args::parse();
    let scan = usnm_boinc::read(&a.scan, boinc::MAX_SCAN)?;
    let mut job = ndlocr::Job {
        v: 1,
        id: String::new(),
        page: "sn83025517/1945-01-01/ed-1/seq-4".into(),
        batch: "dlc_ballston".into(),
        batch_version: 1,
        loc_text: "missing".into(),
        route: usnm_ocr_worker::engine::candidates("jpn").remove(0),
        engine: ndlocr::engine(a.engine_image, 180),
        scan: boinc::Scan {
            url: "https://tile.loc.gov/fixture/full/full/0/default.jpg".into(),
            iiif_base: "https://tile.loc.gov/fixture".into(),
            bytes: scan.len() as u64,
            sha256: sha256(&scan),
            md5: None,
        },
    };
    job.id = job.identity()?;
    job.validate()?;
    atomic(&a.dir.join("page"), &scan)?;
    atomic(
        &a.dir.join(ndlocr::JOB_FILE),
        &serde_json::to_vec_pretty(&job)?,
    )?;
    Ok(())
}
