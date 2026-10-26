//! Runs a benchmark release subset (#738) through the `usnm_qwen` app (#522) outside BOINC, so
//! any host can read the exact benchmark crops with the production prompt, model and settings
//! (#786, #787).
//!
//! [`stage`] writes one BOINC-style slot per bundle of samples: `qwen.json`, a bundled `page.jpg`
//! and the app's `weights.lock`. Each sample is one bundle page whose single crop is the whole
//! image, as `usnm-boinc qwen benchmark-stage` does, with the same token budget (4 per 1,000
//! square pixels). References never enter a slot. [`collect`] reads the app's `readings.json`
//! and `result.json` back into `bench-score` outputs and a timing summary.

use crate::bench::{self, Manifest};
use crate::bundle::sha256_hex;
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use usnm_ocr_contract::quality::{BundleSlice, Input};
use usnm_ocr_contract::qwen::{
    BundlePage, Crop, Engine, Job, Readings, ResultManifest, MAX_BUNDLE_PAGES, PROMPT_VERSION,
    VERSION,
};

/// The stage record beside the slots.
pub const STAGE_FILE: &str = "qwen-bench.json";
/// The token budget of `usnm-boinc qwen benchmark-stage`.
pub const MAX_TOKENS_PER_1000PX2: u32 = 4;
const CROP: &str = "c";

#[derive(Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Staged {
    pub release: String,
    pub subset: String,
    pub subset_digest: String,
    /// Slot directory name (the job id) to the sample ids it reads, in order.
    pub slots: BTreeMap<String, Vec<String>>,
    /// `<doc_id>:c` to sample id.
    pub crops: BTreeMap<String, String>,
}

pub struct StageOptions {
    pub release: PathBuf,
    pub subset: String,
    pub engine: Engine,
    /// The app's `ocr/boinc/qwen/weights.lock`, copied into every slot unchanged.
    pub weights_lock: PathBuf,
    pub pages_per_slot: usize,
    pub timeout_secs: u32,
    /// Written to each slot's `init_data.xml`, as BOINC would for the app version under test.
    pub plan_class: String,
    /// `gpu_type` in `init_data.xml` (`intel_gpu`, `NVIDIA`, ...), if the class is a GPU class.
    pub gpu_type: Option<String>,
    pub root: PathBuf,
}

/// The fields of BOINC's `init_data.xml` that the app reads to pick its device.
fn init_data(plan_class: &str, gpu_type: Option<&str>) -> String {
    let gpu = gpu_type
        .map(|g| format!("<gpu_type>{g}</gpu_type><gpu_device_num>0</gpu_device_num>"))
        .unwrap_or_default();
    format!("<app_init_data><plan_class>{plan_class}</plan_class>{gpu}</app_init_data>\n")
}

fn doc_id(sample: &str) -> String {
    format!("bench-{}", &sha256_hex(sample.as_bytes())[..32])
}

/// Writes the slots for a subset; returns the stage record.
pub fn stage(o: &StageOptions) -> anyhow::Result<Staged> {
    ensure!(
        (1..=MAX_BUNDLE_PAGES).contains(&o.pages_per_slot),
        "--pages-per-slot must be 1 to {MAX_BUNDLE_PAGES}"
    );
    ensure!(
        o.engine.models.len() == 1,
        "the engine must name exactly one model, the one being benchmarked"
    );
    ensure!(
        !o.root.exists(),
        "{} exists; stage into a new directory",
        o.root.display()
    );
    let m: Manifest = bench::load(&o.release)?;
    let subset = m
        .subsets
        .get(&o.subset)
        .with_context(|| format!("no subset {:?}", o.subset))?;
    let by_id: BTreeMap<&str, &bench::Sample> =
        m.samples.iter().map(|s| (s.id.as_str(), s)).collect();
    let weights = std::fs::read(&o.weights_lock).context("reading weights.lock")?;
    let mut staged = Staged {
        release: m.release.clone(),
        subset: o.subset.clone(),
        subset_digest: subset.digest.clone(),
        slots: BTreeMap::new(),
        crops: BTreeMap::new(),
    };
    let ok_name = |s: &str| {
        !s.is_empty() && s.len() <= 64 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    ensure!(ok_name(&o.plan_class), "invalid --plan-class");
    ensure!(
        o.gpu_type.as_deref().is_none_or(ok_name),
        "invalid --gpu-type"
    );
    let parent = o
        .root
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    // A fresh directory beside the destination, renamed into place at the end.
    let tmp = tempfile::Builder::new()
        .prefix(".qwen-bench-")
        .tempdir_in(parent)?;
    let part = tmp.path().to_path_buf();
    let init = init_data(&o.plan_class, o.gpu_type.as_deref());
    for chunk in subset.members.chunks(o.pages_per_slot) {
        let mut bundle = Vec::new();
        let mut pages = Vec::new();
        let mut crops = Vec::new();
        let language = by_id[chunk[0].as_str()].languages[0].clone();
        for id in chunk {
            let s = by_id[id.as_str()];
            let bytes = std::fs::read(o.release.join(&s.image.path))?;
            ensure!(sha256_hex(&bytes) == s.image.sha256, "{id}: image checksum");
            let doc = doc_id(id);
            let crop = Crop {
                rect: [0, 0, s.image_width, s.image_height],
                id: CROP.into(),
                loc_text: String::new(),
                stratum: "benchmark".into(),
            };
            crops.push(Crop {
                id: format!("{doc}:{CROP}"),
                ..crop.clone()
            });
            pages.push(BundlePage {
                crops: vec![crop],
                doc_id: doc.clone(),
                group: "bench".into(),
                input: BundleSlice::of(bundle.len() as u64, &bytes, &format!("bench:{id}")),
                language: s.languages[0].clone(),
            });
            staged.crops.insert(format!("{doc}:{CROP}"), id.clone());
            bundle.extend_from_slice(&bytes);
        }
        let mut job = Job {
            v: VERSION,
            id: String::new(),
            cpu_timeout_secs: o.timeout_secs,
            crops,
            doc_id: doc_id(&chunk.join("\n")),
            engine: o.engine.clone(),
            gpu_timeout_secs: o.timeout_secs,
            group: "bench".into(),
            input: Input::of(&bundle, &format!("bench:{}:{}", m.release, o.subset)),
            language,
            max_tokens_per_1000px2: MAX_TOKENS_PER_1000PX2,
            pages,
            prompt_version: PROMPT_VERSION,
        };
        job.id = job.identity()?;
        job.validate()?;
        let dir = part.join(&job.id);
        std::fs::create_dir_all(&dir)?;
        std::fs::write(dir.join("qwen.json"), serde_json::to_vec_pretty(&job)?)?;
        std::fs::write(dir.join("page.jpg"), &bundle)?;
        std::fs::write(dir.join("weights.lock"), &weights)?;
        std::fs::write(dir.join("init_data.xml"), &init)?;
        staged.slots.insert(job.id.clone(), chunk.to_vec());
    }
    std::fs::write(part.join(STAGE_FILE), serde_json::to_vec_pretty(&staged)?)?;
    std::fs::rename(tmp.keep(), &o.root)?;
    Ok(staged)
}

/// Reads every slot's `readings.json` and `result.json` into scorer outputs (`{"id", "text"}`)
/// and a summary of outcomes and timings. A slot that wrote nothing counts as missing.
pub fn collect(root: &Path) -> anyhow::Result<(String, Value)> {
    let staged: Staged = serde_json::from_slice(&std::fs::read(root.join(STAGE_FILE))?)
        .context("reading the stage record")?;
    let mut outputs = BTreeMap::new();
    let mut slots = Vec::new();
    let mut ms = Vec::new();
    let mut tokens: u64 = 0;
    for (job_id, members) in &staged.slots {
        let dir = root.join(job_id);
        let readings = std::fs::read(dir.join("readings.json"));
        let result = std::fs::read(dir.join("result.json"));
        let diag = std::fs::read(dir.join("diag.txt")).unwrap_or_default();
        let (Ok(readings), Ok(result)) = (readings, result) else {
            slots.push(json!({"job_id": job_id, "outcome": "missing", "samples": members.len()}));
            continue;
        };
        let r: ResultManifest = serde_json::from_slice(&result).context("parsing result.json")?;
        let checked: Readings = r
            .check(&readings, &diag)
            .with_context(|| format!("{job_id}: result.json does not match its readings"))?;
        ensure!(&r.job_id == job_id, "{job_id}: result names another job");
        for reading in &checked.readings {
            let id = staged
                .crops
                .get(&reading.id)
                .with_context(|| format!("{job_id}: reading for unknown crop {}", reading.id))?;
            ensure!(
                members.contains(id),
                "{job_id}: reading for a sample of another slot"
            );
            outputs.insert(id.clone(), reading.text.clone());
            ms.push(reading.ms);
            tokens += u64::from(reading.tokens);
        }
        slots.push(json!({
            "device": r.device, "engine_ms": r.engine_ms, "gpu_mem_peak_mib": r.gpu_mem_peak_mib,
            "job_id": job_id, "load_ms": r.load_ms, "model": r.model, "outcome": r.outcome,
            "peak_rss_kib": r.peak_rss_kib, "read": r.read, "samples": members.len(),
        }));
    }
    ms.sort_unstable();
    let pct = |p: usize| (!ms.is_empty()).then(|| ms[(ms.len() * p).div_ceil(100).max(1) - 1]);
    let mut text = String::new();
    for (id, t) in &outputs {
        text.push_str(&serde_json::to_string(&json!({"id": id, "text": t}))?);
        text.push('\n');
    }
    let summary = json!({
        "read": outputs.len(),
        "reading_ms": {"max": ms.last(), "p50": pct(50), "p95": pct(95)},
        "release": staged.release,
        "samples": staged.crops.len(),
        "slots": slots,
        "subset": staged.subset,
        "subset_digest": staged.subset_digest,
        "tokens": tokens,
    });
    Ok((text, summary))
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::qwen::{Model, Outcome, Reading};

    fn engine() -> Engine {
        let d = format!("sha256:{}", "a".repeat(64));
        Engine {
            image: d.clone(),
            llama: d.clone(),
            models: vec![Model {
                file: "m.gguf".into(),
                min_gpu_mb: 7000,
                mmproj: "p.gguf".into(),
                name: "qwen3-vl-8b-q4km".into(),
            }],
            weights: d,
        }
    }

    #[test]
    fn stages_valid_bundles_and_collects_readings_back_to_sample_ids() {
        let f = crate::bench::tests::fixture(None);
        let built = bench::build(&f.opts).unwrap();
        let tmp = tempfile::tempdir().unwrap();
        let lock = tmp.path().join("weights.lock");
        std::fs::write(&lock, "# lock\n").unwrap();
        let root = tmp.path().join("slots");
        let s = stage(&StageOptions {
            release: built.dir.clone(),
            subset: "all-5".into(),
            engine: engine(),
            weights_lock: lock,
            pages_per_slot: 2,
            timeout_secs: 600,
            plan_class: "intel_vulkan_8b".into(),
            gpu_type: Some("intel_gpu".into()),
            root: root.clone(),
        })
        .unwrap();
        assert_eq!(s.slots.len(), 3);
        assert_eq!(s.crops.len(), 5);
        // Every slot is a valid bundle job whose page.jpg matches it, with no reference text.
        for (job_id, members) in &s.slots {
            let dir = root.join(job_id);
            let job: Job =
                serde_json::from_slice(&std::fs::read(dir.join("qwen.json")).unwrap()).unwrap();
            job.validate().unwrap();
            assert_eq!(&job.id, job_id);
            assert_eq!(job.pages.len(), members.len());
            assert!(job
                .input
                .matches(&std::fs::read(dir.join("page.jpg")).unwrap()));
            assert!(job.crops.iter().all(|c| c.loc_text.is_empty()));
            let init = std::fs::read_to_string(dir.join("init_data.xml")).unwrap();
            assert!(init.contains("<plan_class>intel_vulkan_8b</plan_class>"));
        }
        // One slot reads its crops; the others never ran.
        let (job_id, members) = s.slots.iter().next().unwrap();
        let job: Job =
            serde_json::from_slice(&std::fs::read(root.join(job_id).join("qwen.json")).unwrap())
                .unwrap();
        let readings = Readings {
            v: VERSION,
            job_id: job_id.clone(),
            device: "vulkan".into(),
            diag: BTreeMap::new(),
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::Ok,
            prompt_version: PROMPT_VERSION,
            readings: job
                .crops
                .iter()
                .map(|c| Reading {
                    id: c.id.clone(),
                    ms: 1000,
                    text: "read".into(),
                    tokens: 3,
                })
                .collect(),
        };
        let rbytes = serde_json::to_vec(&readings).unwrap();
        let diag = b"d".to_vec();
        let host = usnm_ocr_contract::surya::Host::default();
        let result = ResultManifest {
            v: VERSION,
            job_id: job_id.clone(),
            completed_at: "2026-10-17T00:00:00Z".into(),
            crops: job.crops.len() as u32,
            device: "vulkan".into(),
            diag_bytes: diag.len() as u64,
            diag_sha256: sha256_hex(&diag),
            engine: engine(),
            engine_ms: 2000,
            gpu_mem_peak_mib: None,
            host,
            load_ms: 5000,
            model: Some("qwen3-vl-8b-q4km".into()),
            outcome: Outcome::Ok,
            peak_rss_kib: None,
            read: job.crops.len() as u32,
            readings_bytes: rbytes.len() as u64,
            readings_sha256: sha256_hex(&rbytes),
            unit_error: None,
        };
        let dir = root.join(job_id);
        std::fs::write(dir.join("readings.json"), &rbytes).unwrap();
        std::fs::write(dir.join("diag.txt"), &diag).unwrap();
        std::fs::write(
            dir.join("result.json"),
            serde_json::to_vec(&result).unwrap(),
        )
        .unwrap();
        let (text, summary) = collect(&root).unwrap();
        let outputs = bench::read_outputs(&text).unwrap();
        assert_eq!(
            outputs.keys().collect::<Vec<_>>(),
            members.iter().collect::<Vec<_>>()
        );
        assert_eq!(summary["read"], members.len());
        assert_eq!(
            summary["slots"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|s| s["outcome"] == "missing")
                .count(),
            2
        );
        // The scorer then counts the unread samples as missing.
        let m = bench::load(&built.dir).unwrap();
        let (t, _) = bench::score(&built.dir, &m, "all-5", &outputs, "raw-v1").unwrap();
        assert_eq!((t.scored, t.missing), (members.len(), 5 - members.len()));
    }
}
