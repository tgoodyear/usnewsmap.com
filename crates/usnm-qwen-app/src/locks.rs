//! `llama.lock` and `weights.lock`: the engine and weights this host accepts, the check of a
//! unit against them, and the model files found and verified against `weights.lock`.
use crate::verified::{self, WeightsCheck};
use anyhow::{bail, ensure, Context};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    boinc::sha256,
    qwen::{is_xiaomi_model, Job, Model},
};
use usnm_ocr_quality::slot::{read_bounded, resolve_slot_file};

pub const ENV_LLAMA_IMAGE: &str = "USNM_QWEN_LLAMA_IMAGE";
pub const ENV_WEIGHTS: &str = "USNM_QWEN_WEIGHTS";
pub const ENV_MODEL_DIR: &str = "USNM_QWEN_MODEL_DIR";
pub const DEFAULT_MODEL_DIR: &str = "/opt/qwen/models";
pub const EMBEDDED_LLAMA_LOCK: &[u8] = include_bytes!("../../../boinc/apps/qwen/llama.lock");
pub const EMBEDDED_WEIGHTS_LOCK: &[u8] = include_bytes!("../../../boinc/apps/qwen/weights.lock");

/// Whether `bytes` holds a carriage return. A CRLF checkout (Windows CI's default) would embed
/// locks that never match the LF copies shipped in the slot (.gitattributes pins them to LF).
const fn has_cr(bytes: &[u8]) -> bool {
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'\r' {
            return true;
        }
        i += 1;
    }
    false
}
const _: () = assert!(
    !has_cr(EMBEDDED_LLAMA_LOCK) && !has_cr(EMBEDDED_WEIGHTS_LOCK),
    "llama.lock or weights.lock was checked out with CRLF; see .gitattributes"
);

#[derive(serde::Deserialize, Default)]
struct LlamaLockFile {
    #[serde(default)]
    assets: BTreeMap<String, LlamaLockAsset>,
    #[serde(default)]
    platforms: BTreeMap<String, LlamaLockAsset>,
}

#[derive(serde::Deserialize, Default)]
struct LlamaLockAsset {
    #[serde(default)]
    sha256: Option<String>,
    #[serde(default)]
    llama_server_sha256: Option<String>,
}

#[derive(serde::Deserialize, Default)]
struct WeightsLockJson {
    #[serde(default)]
    models: Vec<WeightsLockModelJson>,
}

#[derive(serde::Deserialize)]
struct WeightsLockModelJson {
    file: String,
    sha256: String,
    mmproj: WeightsLockMmprojJson,
}

#[derive(serde::Deserialize)]
struct WeightsLockMmprojJson {
    file: String,
    sha256: String,
}

fn active_llama_lock_bytes(work: &Path) -> anyhow::Result<Vec<u8>> {
    let slot_lock = resolve_slot_file(work, "llama.lock");
    if slot_lock.is_file() {
        let bytes = read_bounded(&slot_lock, 64 * 1024)?;
        #[cfg(not(test))]
        ensure!(
            bytes.as_slice() == EMBEDDED_LLAMA_LOCK,
            "slot llama.lock does not match embedded llama.lock"
        );
        return Ok(bytes);
    }
    Ok(EMBEDDED_LLAMA_LOCK.to_vec())
}

fn active_weights_lock_bytes(work: &Path) -> anyhow::Result<Vec<u8>> {
    let slot_lock = resolve_slot_file(work, "weights.lock");
    if slot_lock.is_file() {
        let bytes = read_bounded(&slot_lock, 64 * 1024)?;
        #[cfg(not(test))]
        ensure!(
            bytes.as_slice() == EMBEDDED_WEIGHTS_LOCK,
            "slot weights.lock does not match embedded weights.lock"
        );
        return Ok(bytes);
    }
    Ok(EMBEDDED_WEIGHTS_LOCK.to_vec())
}

fn llama_lock_identities(bytes: &[u8]) -> anyhow::Result<Vec<String>> {
    let parsed: LlamaLockFile = serde_json::from_slice(bytes).context("parsing llama.lock JSON")?;
    let mut out = vec![format!("sha256:{}", sha256(bytes))];
    for entry in parsed.assets.values().chain(parsed.platforms.values()) {
        for cand in [&entry.sha256, &entry.llama_server_sha256]
            .into_iter()
            .flatten()
        {
            let hex = cand.strip_prefix("sha256:").unwrap_or(cand);
            if usnm_ocr_contract::boinc::is_sha256(hex) {
                out.push(format!("sha256:{hex}"));
            }
        }
    }
    Ok(out)
}

fn weights_lock_identities(bytes: &[u8]) -> Vec<String> {
    let mut out = vec![format!("sha256:{}", sha256(bytes))];
    if let Ok(text) = std::str::from_utf8(bytes) {
        let mut non_comments = String::new();
        for line in text.lines().filter(|l| !l.starts_with('#')) {
            non_comments.push_str(line);
            non_comments.push('\n');
        }
        let d = format!("sha256:{}", sha256(non_comments.as_bytes()));
        if !out.contains(&d) {
            out.push(d);
        }
    }
    out
}

pub fn parse_weights_lock(bytes: &[u8]) -> anyhow::Result<BTreeMap<String, String>> {
    let text = std::str::from_utf8(bytes).context("weights.lock is not UTF-8")?;
    let trimmed = text.trim();
    let mut map = BTreeMap::new();
    if trimmed.starts_with('{') {
        let parsed: WeightsLockJson =
            serde_json::from_str(trimmed).context("parsing weights.lock JSON")?;
        for m in parsed.models {
            let m_hex = m.sha256.strip_prefix("sha256:").unwrap_or(&m.sha256);
            ensure!(
                usnm_ocr_contract::boinc::is_sha256(m_hex),
                "invalid sha256 for {} in weights.lock",
                m.file
            );
            map.insert(m.file, m_hex.to_owned());
            let mm_hex = m
                .mmproj
                .sha256
                .strip_prefix("sha256:")
                .unwrap_or(&m.mmproj.sha256);
            ensure!(
                usnm_ocr_contract::boinc::is_sha256(mm_hex),
                "invalid sha256 for {} in weights.lock",
                m.mmproj.file
            );
            map.insert(m.mmproj.file, mm_hex.to_owned());
        }
        return Ok(map);
    }
    for line in trimmed.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        ensure!(parts.len() >= 2, "malformed line in weights.lock: {line}");
        let file = parts[0];
        let digest = parts[parts.len() - 1]
            .strip_prefix("sha256:")
            .unwrap_or(parts[parts.len() - 1]);
        ensure!(
            usnm_ocr_contract::boinc::is_sha256(digest),
            "invalid sha256 for {file} in weights.lock"
        );
        map.insert(file.to_owned(), digest.to_owned());
    }
    Ok(map)
}

pub(crate) fn check_engine(work: &Path, job: &Job) -> anyhow::Result<()> {
    match std::env::var(ENV_LLAMA_IMAGE) {
        Ok(llama) if !llama.is_empty() => {
            ensure!(
                llama == job.engine.llama,
                "unit is for llama image {}, host has {llama}",
                job.engine.llama
            );
        }
        _ => {
            let lock_bytes = active_llama_lock_bytes(work)?;
            let allowed = llama_lock_identities(&lock_bytes)?;
            ensure!(
                allowed.iter().any(|a| a == &job.engine.llama),
                "unit is for llama {}, host llama.lock allows {:?}",
                job.engine.llama,
                allowed
            );
        }
    }
    match std::env::var(ENV_WEIGHTS) {
        Ok(weights) if !weights.is_empty() => {
            ensure!(
                weights == job.engine.weights,
                "unit is for weights {}, host has {weights}",
                job.engine.weights
            );
        }
        _ => {
            let lock_bytes = active_weights_lock_bytes(work)?;
            let allowed = weights_lock_identities(&lock_bytes);
            ensure!(
                allowed.iter().any(|a| a == &job.engine.weights),
                "unit is for weights {}, host weights.lock allows {:?}",
                job.engine.weights,
                allowed
            );
        }
    }
    Ok(())
}

/// Locates `(model_path, mmproj_path)` for `m` either in the BOINC slot
/// directory (via direct files or `<soft_link>` XML pointers) or in
/// `USNM_QWEN_MODEL_DIR` / `/opt/qwen/models`.
pub fn locate_model_files(work: &Path, m: &Model) -> Option<(PathBuf, PathBuf)> {
    let slot_model = resolve_slot_file(work, &m.file);
    let slot_mmproj = resolve_slot_file(work, &m.mmproj);
    if slot_model.is_file() && slot_mmproj.is_file() {
        return Some((slot_model, slot_mmproj));
    }
    let dir = std::env::var_os(ENV_MODEL_DIR)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(DEFAULT_MODEL_DIR));
    let dir_model = dir.join(&m.file);
    let dir_mmproj = dir.join(&m.mmproj);
    if dir_model.is_file() && dir_mmproj.is_file() {
        return Some((dir_model, dir_mmproj));
    }
    None
}

/// The fixed phrase every refusal of a unit's pinned model starts with, which
/// the `USNM_QWEN_FAILURE` report flags as `model_pin_refused`.
pub const PIN_REFUSED: &str = "model pin refused";

/// Whether a plan class reads Xiaomi-OCR-0 units (#1776): `mt_x08` and the
/// `_x08` GPU classes.
pub fn xiaomi_plan_class(plan_class: &str) -> bool {
    plan_class.ends_with("_x08")
}

/// Whether a pinned model is one `plan_class` reads with: Xiaomi-OCR-0 only on
/// the `_x08` classes and they read nothing else; a Qwen pin as the class's own
/// choice would be (2B on CPU and `_2b` classes, 8B on `_8b` classes). Without a
/// plan class (a local run) any pin is read.
fn pin_fits_class(m: &Model, plan_class: Option<&str>) -> bool {
    let xiaomi = is_xiaomi_model(&m.name);
    match plan_class {
        None => true,
        Some(p) if xiaomi_plan_class(p) => xiaomi,
        _ if xiaomi => false,
        Some(p) if p == "mt" || p == "docker_mt" || p == "cpu" || p.ends_with("_2b") => {
            m.min_gpu_mb == 0 || m.name.contains("2b")
        }
        Some(p) if p.ends_with("_8b") => m.min_gpu_mb > 0 || m.name.contains("8b"),
        Some(_) => true,
    }
}

/// The unit's pinned model, `pin`, with its files: the files of that one
/// model, verified against `weights.lock`, or an error starting with
/// [`PIN_REFUSED`]. Nothing else is ever read in its place.
fn resolve_pinned<'a>(
    work: &Path,
    models: &'a [Model],
    plan_class: Option<&str>,
    pin: &str,
    log: &mut Vec<WeightsCheck>,
) -> anyhow::Result<(&'a Model, PathBuf, PathBuf)> {
    let m = models.iter().find(|m| m.name == pin).with_context(|| {
        format!("{PIN_REFUSED}: the unit is pinned to {pin}, which its engine does not list")
    })?;
    ensure!(
        pin_fits_class(m, plan_class),
        "{PIN_REFUSED}: the unit is pinned to {pin}, which plan class {} does not read with",
        plan_class.unwrap_or("")
    );
    let (mp, mmp) = locate_model_files(work, m).with_context(|| {
        format!(
            "{PIN_REFUSED}: the unit is pinned to {pin}, but {} and {} are not both in the slot or model directory; no other model is read in its place",
            m.file, m.mmproj
        )
    })?;
    verify_weights(work, m, &mp, &mmp, log).with_context(|| {
        format!("{PIN_REFUSED}: the unit is pinned to {pin}, whose files don't match weights.lock")
    })?;
    Ok((m, mp, mmp))
}

/// Checks the model and projector against `weights.lock`. Each file is hashed in full unless
/// its verified-file record proves it unchanged since a full hash matched the same pin
/// ([`verified::check`], #1722); `USNM_QWEN_FULL_HASH=1` hashes every time. A full hash
/// that doesn't match fails exactly as it did before the record existed. `log` gets one
/// entry per file checked.
fn verify_weights(
    work: &Path,
    m: &Model,
    model: &Path,
    mmproj: &Path,
    log: &mut Vec<WeightsCheck>,
) -> anyhow::Result<()> {
    let weights_bytes = active_weights_lock_bytes(work)?;
    let expected = parse_weights_lock(&weights_bytes)?;
    let force_full = verified::full_hash_forced();
    for (logical, actual_path) in [(&m.file, model), (&m.mmproj, mmproj)] {
        let want = expected
            .get(logical.as_str())
            .with_context(|| format!("{} is missing from weights.lock", logical))?;
        let (got, entry) = verified::check(
            logical,
            actual_path,
            want,
            force_full,
            verified::Policy::LIVE,
        )?;
        log.push(entry);
        let Some(got) = got else {
            continue;
        };
        ensure!(
            &got == want,
            "model file {} ({}) sha256 mismatch: expected {want}, got {got}",
            logical,
            actual_path.display()
        );
    }
    Ok(())
}

/// Chooses the model to read with and verifies its files' SHA-256 digests
/// against `weights.lock`. A unit pinned to a model (`pin`, the job's
/// `model`) reads with that model or not at all ([`resolve_pinned`]).
/// Otherwise the best available model from `models` whose files exist in the
/// slot directory or model directory is chosen by plan class; a Xiaomi-OCR-0
/// model is never chosen that way, and the `_x08` classes read pinned units only.
#[cfg(test)]
pub fn resolve_model<'a>(
    work: &Path,
    models: &'a [Model],
    gpu_free_mb: Option<u64>,
    use_gpu: bool,
    plan_class: Option<&str>,
    pin: Option<&str>,
) -> anyhow::Result<(&'a Model, PathBuf, PathBuf)> {
    resolve_model_logged(
        work,
        models,
        gpu_free_mb,
        use_gpu,
        plan_class,
        pin,
        &mut Vec::new(),
    )
}

/// [`resolve_model`], recording how each model file was checked in `log`.
pub fn resolve_model_logged<'a>(
    work: &Path,
    models: &'a [Model],
    gpu_free_mb: Option<u64>,
    use_gpu: bool,
    plan_class: Option<&str>,
    pin: Option<&str>,
    log: &mut Vec<WeightsCheck>,
) -> anyhow::Result<(&'a Model, PathBuf, PathBuf)> {
    if let Some(pin) = pin {
        return resolve_pinned(work, models, plan_class, pin, log);
    }
    if let Some(p) = plan_class.filter(|p| xiaomi_plan_class(p)) {
        bail!("{PIN_REFUSED}: plan class {p} reads only units pinned to a Xiaomi-OCR-0 model");
    }
    let available: Vec<(&'a Model, PathBuf, PathBuf)> = models
        .iter()
        .filter(|m| !is_xiaomi_model(&m.name))
        .filter_map(|m| {
            let (mp, mmp) = locate_model_files(work, m)?;
            Some((m, mp, mmp))
        })
        .collect();
    if available.is_empty() {
        let names: Vec<&str> = models.iter().map(|m| m.file.as_str()).collect();
        bail!(
            "no model files found in slot {} or model dir for {:?}",
            work.display(),
            names
        );
    }
    let chosen = match plan_class {
        Some(p) if p == "mt" || p == "docker_mt" || p == "cpu" || p.ends_with("_2b") => available
            .iter()
            .find(|(m, _, _)| m.min_gpu_mb == 0 || m.name.contains("2b"))
            .with_context(|| format!("plan class {p} requires a 2B model, none found"))?,
        Some(p) if p.ends_with("_8b") => available
            .iter()
            .find(|(m, _, _)| m.min_gpu_mb > 0 || m.name.contains("8b"))
            .with_context(|| format!("plan class {p} requires an 8B model, none found"))?,
        _ => match gpu_free_mb {
            Some(free) => available
                .iter()
                .find(|(m, _, _)| u64::from(m.min_gpu_mb) <= free)
                .or_else(|| available.last())
                .unwrap(),
            None if use_gpu => available.first().unwrap(),
            None => available.last().unwrap(),
        },
    };

    verify_weights(work, chosen.0, &chosen.1, &chosen.2, log)?;
    Ok((chosen.0, chosen.1.clone(), chosen.2.clone()))
}
