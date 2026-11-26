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
    /// Crops the server refused (a 4xx reply, such as a request past the context) or that could
    /// not be cut, in order: each fails alone and is left out of the readings, so the rest of
    /// the unit is still read. Counted with the readings for where a resumed task goes on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub crop_errors: Vec<String>,
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
            || cp.readings.len() + cp.crop_errors.len() > job.crops.len()
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
        // Read in order: the readings and the crop errors together are exactly the job's
        // first crops, each once.
        let done = cp.readings.len() + cp.crop_errors.len();
        let want: std::collections::BTreeSet<&str> =
            job.crops[..done].iter().map(|c| c.id.as_str()).collect();
        let mut got = std::collections::BTreeSet::new();
        for id in cp
            .readings
            .iter()
            .map(|r| r.id.as_str())
            .chain(cp.crop_errors.iter().map(String::as_str))
        {
            if !want.contains(id) || !got.insert(id) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::qwen::Reading;

    fn reading(id: &str) -> Reading {
        Reading {
            id: id.into(),
            ms: 1,
            text: "t".into(),
            tokens: 1,
        }
    }

    #[test]
    fn a_checkpoint_counts_crops_that_failed_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let job = crate::test_support::write_qwen_unit(tmp.path());
        assert!(job.crops.len() >= 2);
        let ids: Vec<String> = job.crops.iter().map(|c| c.id.clone()).collect();
        let cp = |readings: Vec<Reading>, errors: Vec<String>| QwenCheckpoint {
            v: VERSION,
            job_id: job.id.clone(),
            engine: job.engine.clone(),
            model: "m".into(),
            device: "cpu".into(),
            readings,
            generation_metadata: Default::default(),
            temporary_exits: 0,
            llama_restarts: 0,
            gpu_ms: 0,
            run_ms: 0,
            restart_log: vec![],
            crop_errors: errors,
        };
        let check = |c: QwenCheckpoint| {
            c.save(tmp.path()).unwrap();
            QwenCheckpoint::load_valid(tmp.path(), &job, "m", "cpu")
        };
        // The first crop failed alone and the second was read: both are the first two crops.
        let ok = check(cp(vec![reading(&ids[1])], vec![ids[0].clone()])).unwrap();
        assert_eq!(ok.readings.len() + ok.crop_errors.len(), 2);
        // A crop error that isn't among the first crops done, or a crop twice, is refused.
        assert!(check(cp(vec![reading(&ids[0])], vec![ids[0].clone()])).is_none());
        assert!(check(cp(vec![], vec!["nope".into()])).is_none());
        // An old checkpoint without the field still loads.
        let mut v = serde_json::to_value(cp(vec![reading(&ids[0])], vec![])).unwrap();
        v.as_object_mut().unwrap().remove("crop_errors");
        std::fs::write(
            tmp.path().join(CHECKPOINT_FILE),
            serde_json::to_vec(&v).unwrap(),
        )
        .unwrap();
        assert!(QwenCheckpoint::load_valid(tmp.path(), &job, "m", "cpu").is_some());
    }
}
