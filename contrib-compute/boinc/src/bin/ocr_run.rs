//! Client-side runner started by BOINC's wrapper (see boinc/apps/usnm_ocr/job.xml).
//!
//!   usnm-ocr-run --manifest manifest.json --tesseract NAME --tessdata-dir DIR \
//!       --engine-version 5.5.0 --out result.json [--slot-dir DIR]
//!
//! Reads the workunit manifest, runs Tesseract on each page one after another (one thread),
//! hashes the traineddata files it used, and writes `result.json`.
//!
//! BOINC app files appear in the slot directory as small link files. `--tesseract` names the
//! logical file for the static Tesseract binary; each `<lang>.traineddata` is linked into
//! `--tessdata-dir` (created under the slot directory) from its own link file.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};
use usnm_boinc::cli::Flags;
use usnm_boinc::result::sha256_hex;
use usnm_boinc::runner::{
    build_result, load_checkpoints, page_result, resolve_link, save_checkpoint, tesseract_args,
    tesseract_lang,
};
use usnm_boinc::workgen::WorkunitManifest;

fn resolve_in_slot(slot: &Path, name: &str) -> Result<PathBuf, String> {
    let p = slot.join(name);
    let bytes = std::fs::read(&p).map_err(|e| format!("{}: {e}", p.display()))?;
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]).into_owned();
    Ok(resolve_link(slot, name, &head))
}

fn link_traineddata(slot: &Path, dir: &Path, name: &str) -> Result<PathBuf, String> {
    let file = format!("{name}.traineddata");
    let target = resolve_in_slot(slot, &file)?;
    std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    let dest = dir.join(&file);
    if !dest.exists() {
        #[cfg(unix)]
        std::os::unix::fs::symlink(&target, &dest)
            .map_err(|e| format!("{}: {e}", dest.display()))?;
        #[cfg(not(unix))]
        std::fs::copy(&target, &dest).map_err(|e| format!("{}: {e}", dest.display()))?;
    }
    Ok(dest)
}

fn run(f: &Flags) -> Result<(), String> {
    let slot = PathBuf::from(f.get("slot-dir").unwrap_or("."));
    // Absolute, so symlinks into the tessdata directory do not dangle.
    let slot = std::fs::canonicalize(&slot).map_err(|e| format!("{}: {e}", slot.display()))?;
    let mpath = slot.join(f.require("manifest")?);
    let m: WorkunitManifest = serde_json::from_slice(
        &std::fs::read(&mpath).map_err(|e| format!("{}: {e}", mpath.display()))?,
    )
    .map_err(|e| e.to_string())?;
    let tesseract = resolve_in_slot(&slot, f.require("tesseract")?)?;
    let tessdata = slot.join(f.require("tessdata-dir")?);

    let mut hashes: BTreeMap<String, String> = BTreeMap::new();
    // Pages finished before a stop (BOINC quit, host reboot, Spot eviction) are kept.
    let ckpt = slot.join("checkpoint");
    let ids: Vec<&str> = m.pages.iter().map(|p| p.page_id.as_str()).collect();
    let mut pages = load_checkpoints(&ckpt, &ids);
    if !pages.is_empty() {
        eprintln!(
            "usnm-ocr-run: resuming after {} finished page(s)",
            pages.len()
        );
    }
    for (i, p) in m.pages.iter().enumerate() {
        let lang = tesseract_lang(&p.language, &p.profile);
        for name in lang.split('+') {
            if !hashes.contains_key(name) {
                let file = link_traineddata(&slot, &tessdata, name)?;
                let bytes = std::fs::read(&file).map_err(|e| format!("{}: {e}", file.display()))?;
                hashes.insert(name.to_string(), sha256_hex(&bytes));
            }
        }
        if i < pages.len() {
            continue; // finished before the last stop; its traineddata hash is recorded above
        }
        let link_path = slot.join(&p.file);
        let head =
            std::fs::read(&link_path).map_err(|e| format!("{}: {e}", link_path.display()))?;
        let head = String::from_utf8_lossy(&head[..head.len().min(512)]).into_owned();
        let image = resolve_link(&slot, &p.file, &head);
        let out = Command::new(&tesseract)
            .args(tesseract_args(&image, &tessdata, &lang))
            .env("OMP_THREAD_LIMIT", "1")
            .output()
            .map_err(|e| format!("tesseract: {e}"))?;
        if !out.status.success() {
            return Err(format!("tesseract failed on {}: {}", p.page_id, out.status));
        }
        pages.push(page_result(
            &p.page_id,
            &p.md5,
            String::from_utf8_lossy(&out.stdout).into_owned(),
        ));
        save_checkpoint(&ckpt, i, &pages[i])?;
    }
    let r = build_result(&m, f.require("engine-version")?, hashes, pages);
    let json = serde_json::to_vec(&r).map_err(|e| e.to_string())?;
    std::fs::write(slot.join(f.require("out")?), json).map_err(|e| e.to_string())
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let f = match Flags::parse(&args, &[]) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("usnm-ocr-run: {e}");
            return ExitCode::from(2);
        }
    };
    match run(&f) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("usnm-ocr-run: {e}");
            ExitCode::FAILURE
        }
    }
}
