//! Held-out transcription experiments on public gold crops. References never enter a workunit.
use super::{outputs, units, JOB_FILE, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use image::GenericImageView as _;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    collections::{BTreeMap, HashMap},
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{Input, MAX_SCAN},
    qwen::{Crop, Engine, Job, Outcome, MAX_JOB_JSON, PROMPT_VERSION, VERSION},
};
use usnm_ocr_quality::align::{align, Op};
use usnm_ocr_survey::score;

const MANIFEST: &str = "benchmark.json";
pub const NOTICE_FILE: &str = "NOTICE.txt";
pub const MAX_NOTICE: usize = 64 * 1024;

fn notice(provenance: &Value) -> anyhow::Result<Vec<u8>> {
    let field = |name: &str| {
        provenance[name]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .with_context(|| format!("missing source {name}"))
    };
    let license = field("source_license")?;
    let upper = license.to_uppercase().replace(' ', "-");
    let url = if upper.contains("BY-NC-SA") {
        "https://creativecommons.org/licenses/by-nc-sa/4.0/"
    } else if upper.contains("BY-NC") {
        "https://creativecommons.org/licenses/by-nc/4.0/"
    } else if upper.contains("BY-SA") {
        "https://creativecommons.org/licenses/by-sa/4.0/"
    } else if upper.contains("BY") {
        "https://creativecommons.org/licenses/by/4.0/"
    } else if upper.contains("CC0") {
        "https://creativecommons.org/publicdomain/zero/1.0/"
    } else {
        anyhow::bail!("unsupported source licence");
    };
    let bytes = format!(
        "Source: {}\nAttribution: {}\nLicence: {license}\nLicence terms: {url}\n\
         Modifications: cropped/re-encoded by the public dataset converter where applicable; \
         this campaign redistributes those crop bytes unchanged. No reference text is included.\n",
        field("source_url")?,
        field("source_attribution")?,
    )
    .into_bytes();
    ensure!(
        bytes.len() <= MAX_NOTICE,
        "source notices exceed size limit"
    );
    Ok(bytes)
}

#[derive(Debug, Clone)]
pub struct Options {
    pub crops: PathBuf,
    pub root: PathBuf,
    pub engine: Engine,
    pub max_crops: usize,
    pub timeout_secs: u32,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Entry {
    job: Job,
    reference: Value,
    provenance: Value,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
struct Manifest {
    v: u32,
    source_sha256: String,
    source_manifest_sha256: String,
    entries: Vec<Entry>,
}

/// One model per campaign prevents a hardware-dependent fallback from confounding a comparison.
pub fn stage(opts: &Options) -> anyhow::Result<usize> {
    super::check_no_hold(&opts.root)?;
    ensure!(opts.max_crops > 0, "--max-crops must be positive");
    ensure!(
        opts.engine.models.len() == 1,
        "benchmark requires exactly one model"
    );
    let source = std::fs::read(&opts.crops)?;
    let rows = score::read_jsonl(&opts.crops)?;
    ensure!(!rows.is_empty(), "no benchmark crops");
    ensure!(
        score::invalid_crops(&rows, true).is_empty(),
        "benchmark requires valid, reviewed human transcriptions"
    );
    let base = opts
        .crops
        .parent()
        .context("crops has no parent")?
        .canonicalize()?;
    ensure!(
        base.join("LICENSES").is_dir(),
        "dataset has no LICENSES provenance directory"
    );
    let source_manifest = std::fs::read(base.join("manifest.jsonl"))?;
    let mut metadata = BTreeMap::new();
    for row in score::read_jsonl(&base.join("manifest.jsonl"))? {
        let cid = row["crop_id"]
            .as_str()
            .context("manifest has no crop_id")?
            .to_string();
        ensure!(
            metadata.insert(cid, row).is_none(),
            "duplicate manifest crop"
        );
    }
    let mut entries = Vec::new();
    let mut images = Vec::new();
    let mut notices = Vec::new();
    for row in rows.into_iter().take(opts.max_crops) {
        ensure!(
            row["for_scoring"] == true,
            "comparison-only crop is not ground truth"
        );
        let cid = row["crop_id"].as_str().context("missing crop_id")?;
        let provenance = metadata
            .get(cid)
            .context("crop missing from source manifest")?;
        let license = provenance["source_license"]
            .as_str()
            .context("missing source licence")?;
        ensure!(
            !license
                .split(|c: char| !c.is_ascii_alphanumeric())
                .any(|part| part.eq_ignore_ascii_case("ND")
                    || part.eq_ignore_ascii_case("NoDerivatives")),
            "NoDerivatives source requires original-file transport, not crop workunits"
        );
        let relative = row["image"].as_str().context("missing crop image")?;
        ensure!(
            !Path::new(relative).is_absolute(),
            "image must be relative to crops.jsonl"
        );
        let image = base.join(relative).canonicalize()?;
        ensure!(image.starts_with(&base), "image escapes dataset directory");
        let bytes = read(&image, MAX_SCAN)?;
        let notice = notice(provenance)?;
        ensure!(
            provenance["crop_sha256"].as_str() == Some(&sha256(&bytes)),
            "crop checksum differs from source manifest"
        );
        let (w, h) = image::load_from_memory(&bytes)
            .with_context(|| format!("{cid}: decoding crop"))?
            .dimensions();
        ensure!(
            w > 0 && h > 0 && w <= 16384 && h <= 16384,
            "invalid crop dimensions"
        );
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: opts.timeout_secs,
            gpu_timeout_secs: opts.timeout_secs,
            crops: vec![Crop {
                id: cid.into(),
                rect: [0, 0, w, h],
                loc_text: String::new(),
                stratum: "benchmark".into(),
            }],
            doc_id: format!("benchmark-{}", sha256(cid.as_bytes())),
            engine: opts.engine.clone(),
            group: row["group"].as_str().context("missing group")?.into(),
            input: Input::of(&bytes, &format!("benchmark:{}", sha256(&notice))),
            language: row["lang"].as_str().context("missing lang")?.into(),
            max_tokens_per_1000px2: 4,
            prompt_version: PROMPT_VERSION,
        };
        job.id = job.identity()?;
        job.validate()?;
        entries.push(Entry {
            job,
            reference: row,
            provenance: provenance.clone(),
        });
        images.push(bytes);
        notices.push(notice);
    }
    let manifest = Manifest {
        v: 1,
        source_sha256: sha256(&source),
        source_manifest_sha256: sha256(&source_manifest),
        entries,
    };
    std::fs::create_dir_all(&opts.root)?;
    let path = opts.root.join(MANIFEST);
    if path.exists() {
        let old: Manifest = serde_json::from_slice(&std::fs::read(&path)?)?;
        ensure!(old == manifest, "campaign is immutable; use a new --root");
    } else {
        ensure!(
            !units(&opts.root).exists(),
            "root already contains non-benchmark units"
        );
        atomic(&path, &serde_json::to_vec_pretty(&manifest)?)?;
    }
    for ((entry, bytes), notice) in manifest.entries.iter().zip(images).zip(notices) {
        let dir = units(&opts.root).join(&entry.job.id);
        std::fs::create_dir_all(&dir)?;
        if dir.join(JOB_FILE).exists() {
            let old: Job = serde_json::from_slice(&read(&dir.join(JOB_FILE), MAX_JOB_JSON)?)?;
            ensure!(old == entry.job, "staged job changed");
            ensure!(
                entry
                    .job
                    .input
                    .matches(&read(&dir.join(PAGE_FILE), MAX_SCAN)?),
                "staged image changed"
            );
            ensure!(
                read(&dir.join(NOTICE_FILE), MAX_NOTICE)? == notice,
                "staged notices changed"
            );
        } else {
            atomic(&dir.join(PAGE_FILE), &bytes)?;
            atomic(&dir.join(NOTICE_FILE), &notice)?;
            atomic(&dir.join(JOB_FILE), &serde_json::to_vec_pretty(&entry.job)?)?;
        }
    }
    Ok(manifest.entries.len())
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub expected: usize,
    pub returned: usize,
    pub pending: usize,
    pub failed: usize,
    pub incomplete: usize,
}

/// Only the assimilator's canonical outputs count; validator copies are not canonical results.
/// Empty hypotheses penalize missing output instead of silently shrinking the denominator.
pub fn collect(root: &Path, spool: &Path, out: &Path) -> anyhow::Result<Summary> {
    let manifest: Manifest = serde_json::from_slice(&std::fs::read(root.join(MANIFEST))?)?;
    ensure!(
        manifest.v == 1 && !manifest.entries.is_empty(),
        "invalid benchmark manifest"
    );
    let mut summary = Summary {
        expected: manifest.entries.len(),
        ..Default::default()
    };
    let mut hypotheses = HashMap::new();
    let mut references = Vec::new();
    let mut statuses = Vec::new();
    let mut model = None;
    for entry in manifest.entries {
        let job = entry.job;
        job.validate()?;
        ensure!(
            job.engine.models.len() == 1,
            "benchmark requires exactly one model"
        );
        let expected_model = &job.engine.models[0].name;
        if let Some(name) = &model {
            ensure!(name == expected_model, "mixed models in benchmark");
        } else {
            model = Some(expected_model.clone());
        }
        let cid = entry.reference["crop_id"]
            .as_str()
            .context("reference has no crop_id")?;
        ensure!(
            job.crops.len() == 1 && job.crops[0].id == cid && job.crops[0].loc_text.is_empty(),
            "invalid benchmark crop"
        );
        ensure!(!hypotheses.contains_key(cid), "duplicate reference");
        let files: Vec<PathBuf> = outputs()
            .iter()
            .map(|n| spool.join(format!("{}__file_{n}", job.workunit())))
            .collect();
        let count = files.iter().filter(|f| f.exists()).count();
        let mut text = String::new();
        let mut detail = serde_json::json!({"crop_id": cid, "job_id": job.id});
        if count == 0 {
            let failed = ["error", "no_output_files"]
                .iter()
                .any(|suffix| spool.join(format!("{}_{suffix}", job.workunit())).exists());
            if failed {
                summary.failed += 1;
            } else {
                summary.pending += 1;
            }
            detail["status"] = if failed { "failed" } else { "pending" }.into();
        } else {
            ensure!(
                count == files.len(),
                "{}: partial spool; retry after assimilation finishes",
                job.workunit()
            );
            let loaded = super::validate::load(&files)?;
            let result = loaded.result;
            ensure!(
                result.job_id == job.id && result.engine == job.engine,
                "result belongs to another job or engine"
            );
            ensure!(result.crops == 1, "unexpected crop count");
            ensure!(
                result.model.as_ref().is_none_or(|m| m == expected_model),
                "unexpected model"
            );
            ensure!(
                loaded.readings.readings.iter().all(|r| r.id == cid)
                    && loaded.readings.readings.len() <= 1,
                "unexpected readings"
            );
            if result.outcome == Outcome::Ok {
                ensure!(
                    result.model.as_ref() == Some(expected_model),
                    "successful result has no expected model"
                );
                ensure!(
                    loaded.readings.readings.len() == 1,
                    "successful result has no reading"
                );
                summary.returned += 1;
            } else {
                summary.incomplete += 1;
            }
            if let Some(reading) = loaded.readings.readings.first() {
                text.clone_from(&reading.text);
            }
            detail["status"] = serde_json::to_value(result.outcome)?;
            detail["device"] = result.device.into();
            detail["model"] = serde_json::to_value(result.model)?;
            detail["load_ms"] = result.load_ms.into();
            detail["engine_ms"] = result.engine_ms.into();
            detail["peak_rss_kib"] = serde_json::to_value(result.peak_rss_kib)?;
            detail["gpu_mem_peak_mib"] = serde_json::to_value(result.gpu_mem_peak_mib)?;
            detail["unit_error"] = serde_json::to_value(result.unit_error)?;
        }
        let truth = entry.reference["transcription"]
            .as_str()
            .context("missing reference text")?;
        let ops = align(
            &usnm_ocr_survey::text::normalize(truth, true)
                .chars()
                .collect::<Vec<_>>(),
            &usnm_ocr_survey::text::normalize(&text, false)
                .chars()
                .collect::<Vec<_>>(),
        );
        detail["char_insertions"] = ops.iter().filter(|op| **op == Op::Ins).count().into();
        detail["char_deletions"] = ops.iter().filter(|op| **op == Op::Del).count().into();
        detail["char_substitutions"] = ops.iter().filter(|op| **op == Op::Sub).count().into();
        detail["counts"] = serde_json::to_value(score::cer_wer(truth, &text))?;
        hypotheses.insert(cid.to_string(), text);
        statuses.push(detail);
        references.push(entry.reference);
    }
    ensure!(
        score::invalid_crops(&references, true).is_empty(),
        "invalid benchmark references"
    );
    let hyps: BTreeMap<_, _> = [(model.context("benchmark has no model")?, hypotheses)].into();
    let scored = score::score(&references, &hyps, true)?;
    std::fs::create_dir_all(out)?;
    score::write_csvs(out, &scored)?;
    atomic(
        &out.join("coverage.json"),
        &serde_json::to_vec_pretty(&summary)?,
    )?;
    let mut lines = Vec::new();
    for status in statuses {
        serde_json::to_writer(&mut lines, &status)?;
        lines.push(b'\n');
    }
    atomic(&out.join("by-crop.jsonl"), &lines)?;
    let mut lines = Vec::new();
    let mut predictions: Vec<_> = hyps.values().flat_map(|h| h.iter()).collect();
    predictions.sort_by_key(|(id, _)| *id);
    for (cid, text) in predictions {
        serde_json::to_writer(
            &mut lines,
            &serde_json::json!({"crop_id": cid, "text": text}),
        )?;
        lines.push(b'\n');
    }
    atomic(&out.join("hypotheses.jsonl"), &lines)?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(dir: &Path) -> Options {
        let (image, _) = crate::qwen::stage::synthetic_page();
        std::fs::write(dir.join("image.jpg"), image).unwrap();
        let row = serde_json::json!({
            "crop_id": "public-001", "image": "image.jpg", "group": "public",
            "lang": "eng", "for_scoring": true, "transcription": "HELLO NEWSPAPER",
            "transcriber": "public dataset", "reviewer": "published review"
        });
        std::fs::write(dir.join("crops.jsonl"), row.to_string()).unwrap();
        std::fs::create_dir(dir.join("LICENSES")).unwrap();
        let provenance = serde_json::json!({
            "crop_id": "public-001", "source_license": "CC-BY-4.0",
            "source_url": "https://example.org/public",
            "source_attribution": "Synthetic dataset authors",
            "crop_sha256": sha256(&std::fs::read(dir.join("image.jpg")).unwrap()),
        });
        std::fs::write(dir.join("manifest.jsonl"), provenance.to_string()).unwrap();
        let mut engine = crate::qwen::stage::tests::engine();
        engine.models = vec![super::super::default_models().pop().unwrap()];
        Options {
            crops: dir.join("crops.jsonl"),
            root: dir.join("campaign"),
            engine,
            max_crops: 20,
            timeout_secs: 600,
        }
    }

    #[test]
    fn immutable_units_contain_no_truth_and_missing_results_count() {
        let tmp = tempfile::tempdir().unwrap();
        let mut opts = fixture(tmp.path());
        assert_eq!(stage(&opts).unwrap(), 1);
        assert_eq!(stage(&opts).unwrap(), 1);
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap();
        let job = &manifest.entries[0].job;
        let bytes = std::fs::read(units(&opts.root).join(&job.id).join(JOB_FILE)).unwrap();
        assert!(!String::from_utf8(bytes)
            .unwrap()
            .contains("HELLO NEWSPAPER"));
        assert_eq!(job.crops[0].loc_text, "");
        let notice = std::fs::read(units(&opts.root).join(&job.id).join(NOTICE_FILE)).unwrap();
        assert!(String::from_utf8_lossy(&notice).contains("Synthetic dataset authors"));
        assert!(!String::from_utf8_lossy(&notice).contains("HELLO NEWSPAPER"));
        assert_eq!(job.input.source, format!("benchmark:{}", sha256(&notice)));
        let out = tmp.path().join("out");
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let s = collect(&opts.root, &spool, &out).unwrap();
        assert_eq!((s.expected, s.pending, s.returned), (1, 1, 0));
        let csv = std::fs::read_to_string(out.join("cer-summary.csv")).unwrap();
        assert!(csv.contains("1.0"), "{csv}");
        let details = score::read_jsonl(&out.join("by-crop.jsonl")).unwrap();
        assert_eq!(details[0]["char_deletions"], "HELLO NEWSPAPER".len());
        assert_eq!(details[0]["char_insertions"], 0);
        std::fs::write(spool.join(format!("{}_error", job.workunit())), "").unwrap();
        assert_eq!(collect(&opts.root, &spool, &out).unwrap().failed, 1);
        std::fs::write(
            spool.join(format!("{}__file_readings.json", job.workunit())),
            "{}",
        )
        .unwrap();
        assert!(collect(&opts.root, &spool, &out).is_err());
        opts.engine.image = format!("sha256:{}", "f".repeat(64));
        assert!(stage(&opts).is_err());
    }

    #[test]
    fn refuses_weak_truth_unreviewed_rows_and_escaping_paths() {
        let tmp = tempfile::tempdir().unwrap();
        let opts = fixture(tmp.path());
        let original: Value = serde_json::from_slice(&std::fs::read(&opts.crops).unwrap()).unwrap();
        for (key, value) in [
            ("for_scoring", Value::Bool(false)),
            ("reviewer", Value::Null),
            ("image", Value::String("../outside.jpg".into())),
        ] {
            let mut row = original.clone();
            row[key] = value;
            std::fs::write(&opts.crops, row.to_string()).unwrap();
            assert!(stage(&opts).is_err(), "{key}");
        }
    }

    #[test]
    fn canonical_readings_score_and_foreign_engines_are_rejected() {
        use usnm_ocr_contract::qwen::{Reading, Readings, ResultManifest};
        let tmp = tempfile::tempdir().unwrap();
        let opts = fixture(tmp.path());
        stage(&opts).unwrap();
        let manifest: Manifest =
            serde_json::from_slice(&std::fs::read(opts.root.join(MANIFEST)).unwrap()).unwrap();
        let job = &manifest.entries[0].job;
        let readings = Readings {
            v: VERSION,
            job_id: job.id.clone(),
            device: "cpu".into(),
            diag: BTreeMap::new(),
            model: Some(job.engine.models[0].name.clone()),
            outcome: Outcome::Ok,
            prompt_version: PROMPT_VERSION,
            readings: vec![Reading {
                id: "public-001".into(),
                ms: 3,
                text: "HELLO NEWSPAPER".into(),
                tokens: 4,
            }],
        };
        let rb = serde_json::to_vec(&readings).unwrap();
        let mut result = ResultManifest {
            v: VERSION,
            job_id: job.id.clone(),
            completed_at: "2026-10-15T00:00:00Z".into(),
            crops: 1,
            device: "cpu".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: job.engine.clone(),
            engine_ms: 3,
            gpu_mem_peak_mib: None,
            host: Default::default(),
            load_ms: 1,
            model: readings.model.clone(),
            outcome: Outcome::Ok,
            peak_rss_kib: None,
            read: 1,
            readings_bytes: rb.len() as u64,
            readings_sha256: sha256(&rb),
            unit_error: None,
        };
        let spool = tmp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        for (name, bytes) in
            outputs()
                .iter()
                .zip([rb, serde_json::to_vec(&result).unwrap(), b"diag".to_vec()])
        {
            std::fs::write(
                spool.join(format!("{}__file_{name}", job.workunit())),
                bytes,
            )
            .unwrap();
        }
        let out = tmp.path().join("out");
        let s = collect(&opts.root, &spool, &out).unwrap();
        assert_eq!(
            (s.returned, s.pending, s.failed, s.incomplete),
            (1, 0, 0, 0)
        );
        let details = score::read_jsonl(&out.join("by-crop.jsonl")).unwrap();
        assert_eq!(details[0]["counts"]["char_edits"], 0);
        result.engine.weights = format!("sha256:{}", "e".repeat(64));
        std::fs::write(
            spool.join(format!("{}__file_result.json", job.workunit())),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        assert!(collect(&opts.root, &spool, &out).is_err());
    }
}
