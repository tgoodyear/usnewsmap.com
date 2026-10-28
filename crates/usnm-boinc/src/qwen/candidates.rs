//! A bounded, offline evidence join. Candidates are observations, not canonical science.
use super::{validate, DIAG_FILE, READINGS_FILE, RESULT_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use serde_json::{json, Value};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};
use usnm_core::damage;
use usnm_ocr_contract::{
    boinc::sha256,
    qwen::{Job, MAX_JOB_JSON},
};
use usnm_ocr_quality::{align, features};

fn text_metrics(text: &str, language: &str) -> Value {
    let score = damage::score_forced(text, language);
    let words = usnm_core::page_language::text_words(text);
    let tokens: Vec<_> = text.split_whitespace().collect();
    let distinct: BTreeSet<_> = tokens.iter().copied().collect();
    let mut previous = "";
    let mut run = 0usize;
    let mut longest_run = 0usize;
    for token in &tokens {
        run = if *token == previous { run + 1 } else { 1 };
        longest_run = longest_run.max(run);
        previous = token;
    }
    json!({
        "chars": text.chars().count(),
        "whitespace_tokens": tokens.len(),
        "word_tokens": words.words.len(),
        "uncertainty_markers": text.matches("[?]").count(),
        "distinct_token_share": (!tokens.is_empty()).then(|| distinct.len() as f64 / tokens.len() as f64),
        "longest_identical_token_run": longest_run,
        "forced_language": language,
        "damage_rate": score.and_then(|s| s.damage_rate),
        "damage_head": score.map(|s| s.damage.head),
        "damage_near": score.map(|s| s.damage.near),
        "function_share": score.and_then(|s| s.function_share),
        "known_words": score.map(|s| s.known_words),
        "damage_model_available": score.is_some(),
        "damage_is_ground_truth": false,
    })
}

fn generation_flags(text: &str, tokens: u32, budget: u32) -> Vec<&'static str> {
    let mut flags = Vec::new();
    if text.trim().is_empty() {
        flags.push("empty_reading");
    } else if text.replace("[?]", "").trim().is_empty() {
        flags.push("uncertainty_only");
    }
    if tokens >= budget {
        flags.push("completion_token_budget_exhausted");
    }
    if text
        .lines()
        .zip(text.lines().skip(1))
        .any(|(a, b)| !a.trim().is_empty() && a.trim() == b.trim())
    {
        flags.push("consecutive_duplicate_lines");
    }
    flags
}

pub fn run(job: &Path, page: &Path, candidates: &[PathBuf], out: &Path) -> anyhow::Result<()> {
    ensure!(
        (1..=16).contains(&candidates.len()),
        "expected 1 to 16 candidates"
    );
    let parent = out
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let destination = parent
        .canonicalize()?
        .join(out.file_name().context("output file name")?);
    let destination = if destination.exists() {
        destination.canonicalize()?
    } else {
        destination
    };
    ensure!(
        destination != job.canonicalize()? && destination != page.canonicalize()?,
        "report must not overwrite an input"
    );
    for dir in candidates {
        ensure!(
            !destination.starts_with(dir.canonicalize()?),
            "report must not overwrite retained evidence"
        );
    }
    let job_bytes = read(job, MAX_JOB_JSON)?;
    let job: Job = serde_json::from_slice(&job_bytes)?;
    job.validate()?;
    ensure!(
        job.pages.is_empty(),
        "candidate audit requires a single-page job"
    );
    let bytes = read(page, usnm_ocr_contract::quality::MAX_SCAN)?;
    ensure!(
        bytes.len() as u64 == job.input.bytes && sha256(&bytes) == job.input.sha256,
        "page does not match job input"
    );
    let image = image::load_from_memory(&bytes)?.to_luma8();
    let expected: BTreeSet<_> = job.crops.iter().map(|c| c.id.as_str()).collect();
    let mut names = BTreeSet::new();
    let mut loaded = Vec::new();
    let mut observations = Vec::new();
    for dir in candidates {
        let name = dir
            .file_name()
            .and_then(|n| n.to_str())
            .context("candidate name")?;
        ensure!(names.insert(name), "duplicate candidate name");
        let files = [READINGS_FILE, RESULT_FILE, DIAG_FILE].map(|f| dir.join(f));
        let l = validate::load(&files)?;
        ensure!(
            l.result.job_id == job.id && l.result.engine == job.engine,
            "candidate job or engine mismatch"
        );
        ensure!(
            l.readings.prompt_version == job.prompt_version,
            "candidate prompt mismatch"
        );
        ensure!(
            l.result.crops as usize == job.crops.len(),
            "candidate crop count mismatch"
        );
        ensure!(
            l.result.model.as_ref().is_none_or(|name| job
                .engine
                .models
                .iter()
                .any(|m| &m.name == name)),
            "candidate model not in job"
        );
        ensure!(
            l.readings
                .readings
                .iter()
                .all(|r| expected.contains(r.id.as_str())),
            "candidate contains unknown crop"
        );
        observations.push(json!({
            "name": name,
            "status": "noncanonical_observation",
            "manifest_sha256": sha256(&read(&files[1], usnm_ocr_contract::qwen::MAX_RESULT_JSON)?),
            "result": l.result,
            "readings": l.readings,
            "diagnostics": String::from_utf8_lossy(&l.diag),
        }));
        loaded.push((name, l));
    }
    let mut crops = Vec::new();
    for crop in &job.crops {
        let [x0, y0, x1, y1] = crop.rect;
        ensure!(
            x1 <= image.width() && y1 <= image.height(),
            "crop outside image"
        );
        let pixels = image::imageops::crop_imm(&image, x0, y0, x1 - x0, y1 - y0).to_image();
        let image_features = features::image_features(&pixels);
        let mut readings = Vec::new();
        let mut pairs = Vec::new();
        for (i, (name, l)) in loaded.iter().enumerate() {
            let r = l.readings.readings.iter().find(|r| r.id == crop.id);
            readings.push(json!({
                "candidate": name,
                "present": r.is_some(),
                "text": r.map(|r| &r.text),
                "metrics": r.map(|r| text_metrics(&r.text, &job.language)),
                "loc_comparison": r.map(|r| align::pair(&r.text, &crop.loc_text)),
                "completion_tokens": r.map(|r| r.tokens),
                "token_budget": job.max_tokens(crop),
                "at_token_budget": r.map(|r| r.tokens >= job.max_tokens(crop)),
                "generation_flags": r.map(|r| generation_flags(&r.text, r.tokens, job.max_tokens(crop))),
                "scientific_acceptance": "not_established_by_this_report",
            }));
            for (other, b) in loaded.iter().skip(i + 1) {
                let rb = b.readings.readings.iter().find(|r| r.id == crop.id);
                pairs.push(json!({
                    "a": name, "b": other,
                    "same_model": l.result.model == b.result.model,
                    "metrics": r.zip(rb).map(|(a,b)| align::pair(&a.text, &b.text)),
                    "validator_dice": r.zip(rb).map(|(a,b)| usnm_ocr_contract::boinc::dice(&a.text, &b.text)),
                    "exact_text": r.zip(rb).map(|(a,b)| a.text == b.text),
                }));
            }
        }
        crops.push(json!({
            "id": crop.id, "box": crop.rect, "stratum": crop.stratum,
            "image_features": image_features,
            "loc_text": crop.loc_text,
            "loc_metrics": text_metrics(&crop.loc_text, &job.language),
            "readings": readings, "pairs": pairs,
        }));
    }
    let report = json!({
        "schema": "usnm_qwen_candidates_v1",
        "status": "diagnostic_only_not_canonical",
        "pair_metrics_normalization": "NFC, joined line-end hyphens, collapsed whitespace",
        "validator_dice_normalization": "validator word bags without quality text normalization",
        "job": job, "job_file_sha256": sha256(&job_bytes),
        "candidates": observations, "crops": crops,
        "limitations": [
            "Only explicitly supplied retained results are included; not a database completeness certificate.",
            "Host metadata is client-reported; BOINC result identity is the retained directory name.",
            "LoC text and forced-language damage are fallible diagnostics, not transcription truth.",
            "Image features describe pixels, not annotated physical damage or legibility.",
            "No acceptance threshold, canonical selection, or validator state is changed."
            ,"Generation flags are observations requiring review, not calibrated failure probabilities or a new BOINC verdict."
        ],
    });
    atomic(&destination, &serde_json::to_vec_pretty(&report)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(root: &Path, tag: &str, texts: &[(&str, &str)]) -> PathBuf {
        let files = validate::tests::write(root, tag, texts);
        let mut manifest: Value =
            serde_json::from_slice(&std::fs::read(&files[1]).unwrap()).unwrap();
        if manifest["read"] != manifest["crops"] {
            let mut readings: Value =
                serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
            readings["outcome"] = json!("timeout");
            let bytes = serde_json::to_vec(&readings).unwrap();
            manifest["outcome"] = json!("timeout");
            manifest["readings_sha256"] = json!(sha256(&bytes));
            manifest["readings_bytes"] = json!(bytes.len());
            std::fs::write(&files[0], bytes).unwrap();
            std::fs::write(&files[1], serde_json::to_vec(&manifest).unwrap()).unwrap();
        }
        let seen = root.join("seen");
        validate::check(&files, Some(&seen)).unwrap();
        seen.join(files[1].file_name().unwrap())
    }

    #[test]
    fn preserves_alternatives_and_distinguishes_empty_from_missing() {
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(
            dir.path(),
            "a",
            &[("c01", "the and the and [?]"), ("c02", "")],
        );
        let b = fixture(dir.path(), "b", &[("c01", "a different reading")]);
        let out = dir.path().join("evidence.json");
        run(
            &dir.path().join("unit_a/qwen.json"),
            &dir.path().join("unit_a/page.jpg"),
            &[a, b],
            &out,
        )
        .unwrap();
        let report: Value = serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap();
        assert_eq!(report["candidates"].as_array().unwrap().len(), 2);
        assert_eq!(
            report["candidates"][0]["readings"]["readings"][0]["text"],
            "the and the and [?]"
        );
        assert_eq!(report["crops"][0]["pairs"][0]["exact_text"], false);
        assert_eq!(
            report["crops"][0]["readings"][0]["metrics"]["uncertainty_markers"],
            1
        );
        assert_eq!(report["crops"][1]["readings"][0]["present"], true);
        assert_eq!(report["crops"][1]["readings"][1]["present"], false);
        assert!(report["crops"][1]["pairs"][0]["metrics"].is_null());
        assert_eq!(report["status"], "diagnostic_only_not_canonical");
    }

    #[test]
    fn rejects_wrong_input_and_unknown_crop_without_publishing() {
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(dir.path(), "a", &[("not_a_crop", "text")]);
        let out = dir.path().join("evidence.json");
        let job = dir.path().join("unit_a/qwen.json");
        let page = dir.path().join("unit_a/page.jpg");
        assert!(run(&job, &page, &[a], &out)
            .unwrap_err()
            .to_string()
            .contains("unknown crop"));
        let b = fixture(dir.path(), "b", &[("c01", "text")]);
        std::fs::write(&page, b"wrong input").unwrap();
        assert!(run(&job, &page, &[b], &out)
            .unwrap_err()
            .to_string()
            .contains("page does not match"));
        assert!(!out.exists());
    }

    #[test]
    fn rejects_duplicate_and_mismatched_candidates() {
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(dir.path(), "a", &[("c01", "text")]);
        let out = dir.path().join("evidence.json");
        let job = dir.path().join("unit_a/qwen.json");
        let page = dir.path().join("unit_a/page.jpg");
        assert!(run(&job, &page, &[a.clone(), a.clone()], &out)
            .unwrap_err()
            .to_string()
            .contains("duplicate"));
        let manifest = a.join(RESULT_FILE);
        let mut r: Value = serde_json::from_slice(&std::fs::read(&manifest).unwrap()).unwrap();
        r["engine"]["weights"] = json!(format!("sha256:{}", "f".repeat(64)));
        std::fs::write(manifest, serde_json::to_vec(&r).unwrap()).unwrap();
        assert!(run(&job, &page, &[a], &out)
            .unwrap_err()
            .to_string()
            .contains("job or engine"));
        assert!(!out.exists());
    }

    #[test]
    fn keeps_validator_dice_separate_from_normalized_quality_metrics() {
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(dir.path(), "a", &[("c01", "inter-\nnational")]);
        let b = fixture(dir.path(), "b", &[("c01", "international")]);
        let out = dir.path().join("evidence.json");
        run(
            &dir.path().join("unit_a/qwen.json"),
            &dir.path().join("unit_a/page.jpg"),
            &[a, b],
            &out,
        )
        .unwrap();
        let report: Value = serde_json::from_slice(&std::fs::read(out).unwrap()).unwrap();
        assert_eq!(report["crops"][0]["pairs"][0]["metrics"]["dice"], 1.0);
        assert_eq!(report["crops"][0]["pairs"][0]["validator_dice"], 0.0);
    }

    #[test]
    fn refuses_to_overwrite_job_or_retained_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(dir.path(), "a", &[("c01", "text")]);
        let job = dir.path().join("unit_a/qwen.json");
        let page = dir.path().join("unit_a/page.jpg");
        let bytes = std::fs::read(&job).unwrap();
        assert!(run(&job, &page, std::slice::from_ref(&a), &job).is_err());
        assert_eq!(std::fs::read(&job).unwrap(), bytes);
        let readings = a.join(READINGS_FILE);
        let bytes = std::fs::read(&readings).unwrap();
        assert!(run(&job, &page, &[a], &readings).is_err());
        assert_eq!(std::fs::read(readings).unwrap(), bytes);
    }

    #[test]
    fn labels_generation_failures_without_treating_agreement_as_accuracy() {
        let repeated = "CIGARETTES\nספרים\nספרים\nספרים\nספרים";
        assert_eq!(usnm_ocr_contract::boinc::dice(repeated, repeated), 1.0);
        assert_eq!(
            generation_flags(repeated, 1304, 1304),
            [
                "completion_token_budget_exhausted",
                "consecutive_duplicate_lines"
            ]
        );
        assert_eq!(
            generation_flags("[?] [?]\n[?]", 7, 100),
            ["uncertainty_only"]
        );
        assert_eq!(generation_flags(" \n ", 0, 100), ["empty_reading"]);
        assert!(generation_flags("real printed text", 4, 100).is_empty());
        assert_eq!(
            text_metrics(repeated, "yid")["longest_identical_token_run"],
            4
        );
    }

    #[test]
    fn publishes_bare_output_filename_without_error_after_persisting() {
        let dir = tempfile::tempdir().unwrap();
        let a = fixture(dir.path(), "a", &[("c01", "text")]);
        let output = tempfile::NamedTempFile::new_in(".").unwrap();
        let bare = Path::new(output.path().file_name().unwrap());
        run(
            &dir.path().join("unit_a/qwen.json"),
            &dir.path().join("unit_a/page.jpg"),
            &[a],
            bare,
        )
        .unwrap();
        let report: Value = serde_json::from_slice(&std::fs::read(bare).unwrap()).unwrap();
        assert_eq!(report["candidates"].as_array().unwrap().len(), 1);
    }
}
