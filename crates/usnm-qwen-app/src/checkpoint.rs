//! The per-crop checkpoint a task resumes from.
use std::path::Path;
use usnm_ocr_contract::qwen::{Engine, Job, Reading, VERSION};
use usnm_ocr_quality::slot::write_atomic;

pub const CHECKPOINT_FILE: &str = "qwen_checkpoint.json";

/// Per-crop checkpoint state persisted atomically in `qwen_checkpoint.json`
/// so interrupted single-page or multi-page `usnm_qwen` workunits resume from
/// the last completed crop without re-running completed crops.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QwenCheckpoint {
    pub v: u32,
    pub job_id: String,
    pub engine: Engine,
    pub model: String,
    pub device: String,
    pub readings: Vec<Reading>,
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub generation_metadata: std::collections::BTreeMap<String, String>,
}

impl QwenCheckpoint {
    pub fn load_valid(work: &Path, job: &Job, model: &str, device: &str) -> Option<Self> {
        let bytes = std::fs::read(work.join(CHECKPOINT_FILE)).ok()?;
        let cp: Self = serde_json::from_slice(&bytes).ok()?;
        if cp.v != VERSION
            || cp.job_id != job.id
            || cp.engine != job.engine
            || cp.model != model
            || cp.device != device
            || cp.readings.len() > job.crops.len()
        {
            return None;
        }
        for (got, want) in cp.readings.iter().zip(&job.crops) {
            if got.id != want.id {
                return None;
            }
        }
        Some(cp)
    }

    pub fn save(&self, work: &Path) -> anyhow::Result<()> {
        write_atomic(
            &work.join(CHECKPOINT_FILE),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}
