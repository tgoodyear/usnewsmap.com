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
    /// How many times this task has asked the BOINC client to restart it after `llama-server`
    /// kept failing on one crop. Kept here so it survives those restarts and can be bounded.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub temporary_exits: u32,
    /// How many times this task has restarted `llama-server` to retry a crop.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub llama_restarts: u32,
    /// GPU time (server starts and reads) of the runs of this task before the current one, for
    /// the cost of a page-mode unit that was restarted.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub gpu_ms: u64,
    /// The current run's GPU time as last saved, folded into `gpu_ms` when the next run starts.
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub run_ms: u64,
    /// Notes on those restarts, newest last, for the diagnostics.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restart_log: Vec<String>,
}

fn is_zero(n: &u32) -> bool {
    *n == 0
}

fn is_zero_u64(n: &u64) -> bool {
    *n == 0
}

/// The most restart notes the checkpoint keeps, and the longest each may be.
pub const MAX_RESTART_NOTES: usize = 16;
pub const MAX_RESTART_NOTE: usize = 240;

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
        if job.page_mode.is_some() {
            // Regions finish in any order: each reading must be one of the job's, once.
            let mut seen = std::collections::BTreeSet::new();
            let known: std::collections::BTreeSet<&str> =
                job.crops.iter().map(|c| c.id.as_str()).collect();
            for got in &cp.readings {
                if !known.contains(got.id.as_str()) || !seen.insert(got.id.as_str()) {
                    return None;
                }
            }
            return Some(cp);
        }
        for (got, want) in cp.readings.iter().zip(&job.crops) {
            if got.id != want.id {
                return None;
            }
        }
        Some(cp)
    }

    /// Adds a note about a `llama-server` restart, dropping the oldest notes past the limit.
    pub fn note_restart(&mut self, note: &str) {
        self.llama_restarts += 1;
        self.log_note(note);
    }

    /// Adds a note that is not a restart, such as a temporary exit.
    pub fn log_note(&mut self, note: &str) {
        let mut end = note.len().min(MAX_RESTART_NOTE);
        while !note.is_char_boundary(end) {
            end -= 1;
        }
        self.restart_log.push(note[..end].to_owned());
        if self.restart_log.len() > MAX_RESTART_NOTES {
            self.restart_log.remove(0);
        }
    }

    pub fn save(&self, work: &Path) -> anyhow::Result<()> {
        write_atomic(
            &work.join(CHECKPOINT_FILE),
            &serde_json::to_vec_pretty(self)?,
        )
    }
}
