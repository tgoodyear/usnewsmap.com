use super::{validate::Loaded, DIAG_FILE, READINGS_FILE, RESULT_FILE};
use crate::{atomic, read, Lock};
use anyhow::{ensure, Context};
use serde_json::json;
use std::path::{Path, PathBuf};
use usnm_ocr_contract::{boinc::sha256, qwen::MAX_RESULT_JSON};

/// Stock script_validator retries exit 3 instead of invalidating the volunteer result.
#[derive(Debug)]
pub struct CaptureError(pub anyhow::Error);

impl std::fmt::Display for CaptureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Qwen evidence capture must retry: {:#}", self.0)
    }
}

impl std::error::Error for CaptureError {}

fn staging_directory(root: &Path, name: &str) -> anyhow::Result<tempfile::TempDir> {
    let parent = root.join(".capture-locks").join(name);
    Ok(tempfile::Builder::new()
        .prefix(".capture-")
        .tempdir_in(parent)?)
}

pub fn capture(files: &[PathBuf], loaded: &Loaded, root: &Path) -> anyhow::Result<()> {
    capture_with_sync(files, loaded, root, |dir| {
        std::fs::File::open(dir)?.sync_all()?;
        Ok(())
    })
}

fn capture_with_sync(
    files: &[PathBuf],
    loaded: &Loaded,
    root: &Path,
    mut sync: impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    std::fs::create_dir_all(root)?;
    let root = root.canonicalize()?;
    let name = crate::quality::validate::result_name(&files[1])?;
    let _lock = Lock::take(&root.join(".capture-locks").join(&name))?;
    let manifest = read(&files[1], MAX_RESULT_JSON)?;
    let parsed: usnm_ocr_contract::qwen::ResultManifest = serde_json::from_slice(&manifest)?;
    ensure!(parsed == loaded.result, "manifest changed during capture");
    let outputs = [
        (READINGS_FILE, loaded.readings_bytes.as_slice()),
        ("result.raw.json", manifest.as_slice()),
        (DIAG_FILE, loaded.diag.as_slice()),
    ];
    let provenance = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "job_id": loaded.result.job_id,
        "canonical": null,
        "selection": "every_checked_result_before_validation",
        "files": outputs.iter().zip(files).map(|((logical, bytes), source)| {
            json!({"logical": logical, "source_name": source.file_name(),
                   "sha256": sha256(bytes), "bytes": bytes.len()})
        }).collect::<Vec<_>>(),
    }))?;
    let metrics = serde_json::to_vec_pretty(&json!({
        "schema_version": 1,
        "job_id": loaded.result.job_id,
        "diag_sha256": sha256(&loaded.diag),
        "generation_diagnostics": "exact_versioned_sections_retained_in_diag.txt",
        "source_image_metrics": null,
        "damage_rate": null,
        "unavailable_reason": "validator_output_has_no_source_image_or_source_language; use qwen candidates",
        "readings": loaded.readings.readings.iter().map(|r| {
            let tokens: Vec<_> = r.text.split_whitespace().collect();
            let mut longest_run = 0usize;
            let mut run = 0usize;
            let mut previous = "";
            for token in &tokens {
                run = if *token == previous { run + 1 } else { 1 };
                longest_run = longest_run.max(run);
                previous = token;
            }
            json!({"id": r.id, "text_sha256": sha256(r.text.as_bytes()),
                   "chars": r.text.chars().count(), "completion_tokens_reported": r.tokens,
                   "uncertainty_markers": r.text.matches("[?]").count(),
                   "uncertainty_only": !r.text.trim().is_empty() && r.text.replace("[?]", "").trim().is_empty(),
                   "longest_identical_token_run": longest_run,
                   "accuracy_established": false})
        }).collect::<Vec<_>>(),
    }))?;
    let to = root.join(&name);
    if to.exists() {
        // Legacy --keep manifests were normalized; preserve them, but bind the exact raw bytes too.
        for (logical, bytes) in outputs.iter().chain(
            [
                ("capture-v1.json", provenance.as_slice()),
                ("readings-metrics-v1.json", metrics.as_slice()),
            ]
            .iter(),
        ) {
            let path = to.join(logical);
            if path.exists() {
                ensure!(
                    read(&path, bytes.len())? == *bytes,
                    "retained evidence differs: {logical}"
                );
            }
        }
        let legacy = to.join(RESULT_FILE);
        if legacy.exists() {
            let old: usnm_ocr_contract::qwen::ResultManifest =
                serde_json::from_slice(&read(&legacy, MAX_RESULT_JSON)?)?;
            ensure!(old == loaded.result, "retained manifest differs");
        }
        for (logical, bytes) in outputs {
            if !to.join(logical).exists() {
                atomic(&to.join(logical), bytes)?;
            }
        }
        if !legacy.exists() {
            atomic(&legacy, &manifest)?;
        }
        if !to.join("readings-metrics-v1.json").exists() {
            atomic(&to.join("readings-metrics-v1.json"), &metrics)?;
        }
        if !to.join("capture-v1.json").exists() {
            atomic(&to.join("capture-v1.json"), &provenance)?;
        }
    } else {
        let staged = staging_directory(&root, &name)?;
        for (logical, bytes) in outputs {
            atomic(&staged.path().join(logical), bytes)?;
        }
        atomic(&staged.path().join(RESULT_FILE), &manifest)?;
        atomic(&staged.path().join("readings-metrics-v1.json"), &metrics)?;
        atomic(&staged.path().join("capture-v1.json"), &provenance)?;
        std::fs::rename(staged.path(), &to).context("commit Qwen evidence directory")?;
    }
    sync(&to)?;
    // A prior failed attempt may have created any of these entries without syncing its parent.
    for ancestor in root.ancestors() {
        sync(ancestor)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replay_retries_failed_directory_durability_barriers() {
        let tmp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let files = super::super::validate::tests::write(
            tmp.path(),
            "sync",
            &[("c01", "one two"), ("c02", "three four")],
        );
        let loaded = super::super::validate::load(&files).unwrap();
        let root = tmp.path().join("new/archive/seen");
        let to = root.join(crate::quality::validate::result_name(&files[1]).unwrap());
        for fail_at in [
            root.as_path(),
            to.as_path(),
            root.parent().unwrap(),
            root.parent().unwrap(),
        ] {
            let result = capture_with_sync(&files, &loaded, &root, |dir| {
                ensure!(dir != fail_at, "injected directory sync failure");
                Ok(())
            });
            assert!(result.is_err());
            assert!(to.join("capture-v1.json").is_file());
            assert!(files.iter().all(|f| f.is_file()));
        }
        let mut synced = Vec::new();
        capture_with_sync(&files, &loaded, &root, |dir| {
            synced.push(dir.to_path_buf());
            Ok(())
        })
        .unwrap();
        let expected: Vec<_> = std::iter::once(to)
            .chain(root.ancestors().map(Path::to_path_buf))
            .collect();
        assert_eq!(synced, expected);
    }

    #[test]
    fn interrupted_staging_is_not_discovered_as_an_extra_replica() {
        let tmp = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let files = super::super::validate::tests::write(
            tmp.path(),
            "interrupted",
            &[("c01", "one two"), ("c02", "three four")],
        );
        let loaded = super::super::validate::load(&files).unwrap();
        let root = tmp.path().join("seen");
        capture(&files, &loaded, &root).unwrap();
        let name = crate::quality::validate::result_name(&files[1]).unwrap();
        let orphan = staging_directory(&root, &name).unwrap();
        std::fs::copy(&files[1], orphan.path().join(RESULT_FILE)).unwrap();
        let orphan = orphan.keep();
        capture(&files, &loaded, &root).unwrap();
        let summary =
            super::super::summary::run(tmp.path(), &root, &tmp.path().join("no-pairs")).unwrap();
        assert_eq!(summary.matches(&format!("| {name} |")).count(), 1);
        assert!(!summary.contains(".capture-"));
        assert!(orphan.join(RESULT_FILE).is_file());
    }
}
