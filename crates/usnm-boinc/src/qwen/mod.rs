//! The vision-language readings (#522) as a BOINC app, `usnm_qwen`: one
//! page per unit, a stratified sample of crops cut from LoC's ALTO, each
//! transcribed by Qwen3-VL on llama.cpp's server. The model is a reader,
//! never a judge: the unit returns its text per crop and nothing else.
//!
//! - [`stage`] cuts the crops of each staged `usnm_quality` page (the same
//!   seed, so both apps agree on them) and writes one unit per page.
//! - [`submit`] hands staged units to stock `create_work`, two results each.
//! - [`work`] runs one unit on a volunteer host, GPU or CPU, and always
//!   writes the readings, a result and the diagnostics.
//! - [`validate`] is what stock `script_validator` calls: a copy of every
//!   result, and per-crop word-bag Dice between two hosts.
//! - [`consensus`] is the crop-by-crop rule `compare --rule` can apply, and
//!   [`replay`] measures any rule against the retained results.
//! - [`summary`] reads those copies back into one table.
//! - [`ledger`] lists each bundle page with its own pair verdicts.
//! - [`doc_id`] maps a unit's `doc_id` to the page it names and back.
use anyhow::ensure;
use std::path::{Path, PathBuf};
use usnm_ocr_contract::qwen::Model;

pub mod benchmark;
pub mod candidates;
pub mod cli;
pub mod consensus;
pub mod doc_id;
pub mod grounded;
pub mod ledger;
pub mod replay;
pub mod retain;
pub mod stage;
pub mod submit;
pub mod summary;
pub mod validate;
pub mod work;

/// The unit's request in the BOINC slot. Its presence makes `usnm-boinc work`
/// run a Qwen unit.
pub const JOB_FILE: &str = "qwen.json";
pub const PAGE_FILE: &str = "page.jpg";
pub const READINGS_FILE: &str = "readings.json";
pub const RESULT_FILE: &str = "result.json";
pub const DIAG_FILE: &str = "diag.txt";
pub const APP: &str = "usnm_qwen";

/// The image records its llama.cpp base and weights lock digest; a unit
/// carries the same values and the host refuses one staged for another.
pub const ENV_LLAMA_IMAGE: &str = "USNM_QWEN_LLAMA_IMAGE";
pub const ENV_WEIGHTS: &str = "USNM_QWEN_WEIGHTS";
pub const ENV_MODEL: &str = "USNM_QWEN_MODEL";
/// Where the image keeps the models (`boinc/apps/qwen/weights.lock`).
pub const MODEL_DIR: &str = "/opt/qwen/models";

/// GPU memory, in MB, free on the host's GPU for the 8B model to be chosen.
pub const GPU_MB_FOR_8B: u32 = 7000;

/// Allowed model alternatives, largest first; each split image carries only one.
pub fn default_models() -> Vec<Model> {
    vec![
        Model {
            file: "Qwen3VL-8B-Instruct-Q4_K_M.gguf".into(),
            min_gpu_mb: GPU_MB_FOR_8B,
            mmproj: "mmproj-Qwen3VL-8B-Instruct-Q8_0.gguf".into(),
            name: "qwen3-vl-8b-q4km".into(),
        },
        Model {
            file: "Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
            min_gpu_mb: 0,
            mmproj: "mmproj-Qwen3-VL-2B-Instruct-Q8_0.gguf".into(),
            name: "qwen3-vl-2b-q8".into(),
        },
    ]
}

/// GPU memory, in MB, free on the host's GPU for Xiaomi-OCR-0 (0.8B at Q8_0
/// with an F16 projector), which opens 4 GB cards (#1776).
pub const GPU_MB_FOR_XIAOMI: u32 = 2000;
/// Xiaomi-OCR-0's model name, and its GGUF and projector as Study 9's
/// conversion named them (`docs/reports/2026-11-gpu-studies/scripts/s9/convert.sh`).
pub const XIAOMI_MODEL: &str = "xiaomi-ocr-0-q8";
pub const XIAOMI_FILE: &str = "xiaomi-ocr-0-q8_0.gguf";
pub const XIAOMI_MMPROJ: &str = "mmproj-xiaomi-ocr-0-f16.gguf";

/// The models a Xiaomi-OCR-0 unit lists: Xiaomi-OCR-0 alone, so no host can
/// read it with a Qwen file.
pub fn xiaomi_models() -> Vec<Model> {
    vec![Model {
        file: XIAOMI_FILE.into(),
        min_gpu_mb: GPU_MB_FOR_XIAOMI,
        mmproj: XIAOMI_MMPROJ.into(),
        name: XIAOMI_MODEL.into(),
    }]
}

/// The engine models and request version `qwen stage` stages for a pin:
/// Xiaomi-OCR-0 alone at request version 5, or the Qwen models at the
/// version asked for (1 by default). Job validation refuses any other mix.
pub fn stage_reader(
    model: Option<&str>,
    prompt_version: Option<u32>,
) -> anyhow::Result<(Vec<Model>, u32)> {
    use usnm_ocr_contract::qwen::{is_xiaomi_model, PROMPT_VERSION, PROMPT_VERSION_XIAOMI};
    match model {
        Some(m) if is_xiaomi_model(m) => {
            ensure!(m == XIAOMI_MODEL, "unknown Xiaomi-OCR-0 build {m}");
            let v = prompt_version.unwrap_or(PROMPT_VERSION_XIAOMI);
            ensure!(
                v == PROMPT_VERSION_XIAOMI,
                "Xiaomi-OCR-0 reads with request version {PROMPT_VERSION_XIAOMI} only"
            );
            Ok((xiaomi_models(), v))
        }
        Some(m) => {
            let models = default_models();
            ensure!(
                models.iter().any(|x| x.name == m),
                "--model {m} is not one of the staged models"
            );
            Ok((models, prompt_version.unwrap_or(PROMPT_VERSION)))
        }
        None => Ok((default_models(), prompt_version.unwrap_or(PROMPT_VERSION))),
    }
}

/// The model for a host: on a GPU with `free_mb` free, the first whose
/// `min_gpu_mb` fits; on CPU, the last (the smallest).
pub fn choose_model(models: &[Model], gpu_free_mb: Option<u64>) -> Option<&Model> {
    match gpu_free_mb {
        Some(free) => models
            .iter()
            .find(|m| u64::from(m.min_gpu_mb) <= free)
            .or(models.last()),
        None => models.last(),
    }
}

/// A split container must never fall back to a model it does not carry, or run
/// the GPU-only payload on a CPU after GPU pass-through failed.
pub fn payload_model<'a>(
    models: &'a [Model],
    payload: &str,
    gpu_free_mb: Option<u64>,
) -> Option<&'a Model> {
    let name = match payload {
        "2b" => "qwen3-vl-2b-q8",
        "8b" if gpu_free_mb.is_some_and(|free| free >= u64::from(GPU_MB_FOR_8B)) => {
            "qwen3-vl-8b-q4km"
        }
        _ => return None,
    };
    models.iter().find(|m| m.name == name)
}

/// Where the units live under the staging root.
pub fn units(root: &Path) -> PathBuf {
    root.join("units")
}

pub fn check_no_hold(root: &Path) -> anyhow::Result<()> {
    crate::assess::check_no_hold(root)
}

/// The output files in output template order.
pub fn outputs() -> [&'static str; 3] {
    [READINGS_FILE, RESULT_FILE, DIAG_FILE]
}

/// Checks a digest argument.
pub fn check_digest(s: &str, what: &str) -> anyhow::Result<()> {
    ensure!(
        s.strip_prefix("sha256:")
            .is_some_and(usnm_ocr_contract::boinc::is_sha256),
        "{what} must be a full sha256 digest"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_choice() {
        let m = default_models();
        assert_eq!(
            choose_model(&m, Some(8000)).unwrap().name,
            "qwen3-vl-8b-q4km"
        );
        assert_eq!(choose_model(&m, Some(4000)).unwrap().name, "qwen3-vl-2b-q8");
        assert_eq!(choose_model(&m, None).unwrap().name, "qwen3-vl-2b-q8");
        assert!(choose_model(&[], None).is_none());
    }

    #[test]
    fn split_payload_never_uses_another_model_or_cpu_fallback() {
        let models = default_models();
        assert_eq!(
            payload_model(&models, "2b", Some(24000)).unwrap().name,
            "qwen3-vl-2b-q8"
        );
        assert_eq!(
            payload_model(&models, "8b", Some(7000)).unwrap().name,
            "qwen3-vl-8b-q4km"
        );
        assert!(payload_model(&models, "8b", Some(6999)).is_none());
        assert!(payload_model(&models, "8b", None).is_none());
        assert!(payload_model(&models[..1], "2b", None).is_none());
        assert!(payload_model(&models, "all", Some(24000)).is_none());
        assert_eq!(outputs(), ["readings.json", "result.json", "diag.txt"]);
    }

    #[test]
    fn stage_reader_pairs_each_pin_with_its_models_and_request() {
        let (m, v) = stage_reader(Some(XIAOMI_MODEL), None).unwrap();
        assert_eq!(v, 5);
        assert_eq!(m.len(), 1);
        assert_eq!(
            (m[0].file.as_str(), m[0].mmproj.as_str(), m[0].min_gpu_mb),
            (XIAOMI_FILE, XIAOMI_MMPROJ, 2000)
        );
        assert!(stage_reader(Some(XIAOMI_MODEL), Some(4)).is_err());
        assert!(stage_reader(Some("xiaomi-ocr-0-f16"), None).is_err());
        for pin in ["qwen3-vl-8b-q4km", "qwen3-vl-2b-q8"] {
            let (m, v) = stage_reader(Some(pin), Some(3)).unwrap();
            assert_eq!((m, v), (default_models(), 3));
        }
        assert!(stage_reader(Some("qwen3-vl-4b"), None).is_err());
        assert_eq!(stage_reader(None, None).unwrap(), (default_models(), 1));
    }
}
