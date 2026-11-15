//! What stock `script_validator` runs for `usnm_quality`.
//!
//! It passes a result's three files in output template order
//! (`result.json`, `confusions.json`, `diag.txt`) and reads the exit
//! status. `check` looks at one result; with `--keep` it also copies the
//! result into that directory, named after its uploaded `result.json`, so
//! every host's diagnostics and timings are kept. `compare` looks at two
//! results of one unit: same outcome, canonical records equal (keys
//! sorted, floats to six decimals); with `--log` it appends what it found
//! to a JSON Lines file.
use super::DIAG_FILE;
use crate::{atomic, read};
use anyhow::{ensure, Context};
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::quality::{self, ResultManifest, MAX_CONFUSIONS, MAX_DIAG, MAX_RESULT_JSON};

pub struct Loaded {
    pub result: ResultManifest,
    pub confusions: Vec<u8>,
    pub diag: Vec<u8>,
}

pub fn load(files: &[PathBuf]) -> anyhow::Result<Loaded> {
    ensure!(
        files.len() == 3,
        "expected 3 output files (result.json, confusions.json, {DIAG_FILE}), got {}",
        files.len()
    );
    let result: ResultManifest =
        serde_json::from_slice(&read(&files[0], MAX_RESULT_JSON)?).context("result.json")?;
    let confusions = read(&files[1], MAX_CONFUSIONS)?;
    let diag = read(&files[2], MAX_DIAG)?;
    result.check(&confusions, &diag)?;
    Ok(Loaded {
        result,
        confusions,
        diag,
    })
}

/// A name for the result from its uploaded `result.json` (BOINC names it
/// `<result name>_<n>`): letters, digits, `_` and `-` only.
pub(crate) fn result_name(file: &Path) -> anyhow::Result<String> {
    let n = file
        .file_name()
        .and_then(|n| n.to_str())
        .context("result file name")?;
    ensure!(
        !n.is_empty()
            && n.len() <= 200
            && n.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)),
        "unexpected result file name {n:?}"
    );
    Ok(n.to_owned())
}

/// A text-only qe unit's result: `qe_text_result.json` and `diag.txt`.
pub fn load_text(
    files: &[PathBuf],
) -> anyhow::Result<(usnm_ocr_contract::qe_text::QeTextResult, Vec<u8>)> {
    use usnm_ocr_contract::qe_text::{QeTextResult, MAX_RESULT, RESULT_FILE};
    ensure!(
        files.len() == 2,
        "expected 2 output files ({RESULT_FILE}, {DIAG_FILE}), got {}",
        files.len()
    );
    let r: QeTextResult =
        serde_json::from_slice(&read(&files[0], MAX_RESULT)?).context(RESULT_FILE)?;
    r.check_shape()?;
    Ok((r, read(&files[1], MAX_DIAG)?))
}

fn check_text(files: &[PathBuf], keep: Option<&Path>) -> anyhow::Result<()> {
    let (r, diag) = load_text(files)?;
    if let Some(dir) = keep {
        let to = dir.join(result_name(&files[0])?);
        std::fs::create_dir_all(&to)?;
        atomic(
            &to.join(usnm_ocr_contract::qe_text::RESULT_FILE),
            &serde_json::to_vec(&r)?,
        )?;
        atomic(&to.join(DIAG_FILE), &diag)?;
    }
    Ok(())
}

pub fn check(files: &[PathBuf], keep: Option<&Path>) -> anyhow::Result<()> {
    if files.len() == 2 {
        return check_text(files, keep);
    }
    let l = load(files)?;
    if let Some(dir) = keep {
        let name = result_name(&files[0])?;
        let to = dir.join(&name);
        std::fs::create_dir_all(&to)?;
        atomic(
            &to.join(super::RESULT_FILE),
            &serde_json::to_vec_pretty(&l.result)?,
        )?;
        atomic(&to.join(super::CONFUSIONS_FILE), &l.confusions)?;
        atomic(&to.join(DIAG_FILE), &l.diag)?;
    }
    Ok(())
}

pub fn compare(a: &[PathBuf], b: &[PathBuf], log: Option<&Path>) -> anyhow::Result<()> {
    if a.len() == 2 && b.len() == 2 {
        // Text-only qe units: the measures are a pure function of the unit, so two results
        // agree exactly or not at all.
        let ((ra, _), (rb, _)) = (load_text(a)?, load_text(b)?);
        ensure!(ra == rb, "the text-only qe results differ");
        return Ok(());
    }
    let (la, lb) = (load(a)?, load(b)?);
    let verdict = quality::agree(&la.result, &lb.result);
    if let Some(log) = log {
        let line = serde_json::json!({
            "a": {"result": result_name(&a[0])?, "plan_class": la.result.host.plan_class, "total_ms": la.result.timings.total_ms},
            "b": {"result": result_name(&b[0])?, "plan_class": lb.result.host.plan_class, "total_ms": lb.result.timings.total_ms},
            "agree": verdict.is_ok(),
            "at": chrono::Utc::now().to_rfc3339(),
            "job_id": la.result.job_id,
            "outcomes": [la.result.outcome, lb.result.outcome],
            "reason": verdict.as_ref().err(),
        });
        if let Some(parent) = log.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(log)?;
        writeln!(f, "{line}")?;
    }
    verdict.map_err(|e| anyhow::anyhow!(e))
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn result_files(dir: &Path, tag: &str) -> Vec<PathBuf> {
        let unit = dir.join(format!("unit_{tag}"));
        crate::quality::work::tests::unit(&unit);
        crate::quality::work::run(&unit).await.unwrap();
        let files: Vec<PathBuf> = ["r", "c", "d"]
            .iter()
            .map(|n| dir.join(format!("usnm_quality_x_{tag}_{n}")))
            .collect();
        for (f, name) in files.iter().zip(crate::quality::outputs()) {
            std::fs::copy(unit.join(name), f).unwrap();
        }
        files
    }

    #[test]
    fn text_only_results_are_checked_and_compared() {
        use usnm_ocr_contract::qe_text::{
            QeTextResult, QeTextRow, NO_LANGUAGE, RESULT_FILE, VERSION,
        };
        let tmp = tempfile::tempdir().unwrap();
        let r = QeTextResult {
            v: VERSION,
            id: "a".repeat(64),
            rows: vec![QeTextRow {
                doc_id: "sn1_1900-01-01_ed-1_seq-1".into(),
                qe: None,
                null_reason: Some(NO_LANGUAGE.into()),
            }],
        };
        let files = |tag: &str, r: &QeTextResult| {
            let dir = tmp.path().join(tag);
            std::fs::create_dir_all(&dir).unwrap();
            std::fs::write(dir.join(RESULT_FILE), serde_json::to_vec(r).unwrap()).unwrap();
            std::fs::write(dir.join(DIAG_FILE), b"diag").unwrap();
            vec![dir.join(RESULT_FILE), dir.join(DIAG_FILE)]
        };
        let (a, b) = (files("a", &r), files("b", &r));
        check(&a, None).unwrap();
        compare(&a, &b, None).unwrap();
        let mut other = r.clone();
        other.rows[0].null_reason = Some("no_resource_file".into());
        assert!(compare(&a, &files("c", &other), None).is_err());
        other.rows[0].null_reason = None;
        assert!(check(&files("d", &other), None).is_err());
    }

    #[tokio::test]
    async fn keeps_results_and_logs_pairs() {
        let tmp = tempfile::tempdir().unwrap();
        let a = result_files(tmp.path(), "a").await;
        let b = result_files(tmp.path(), "b").await;
        let keep = tmp.path().join("seen");
        check(&a, Some(&keep)).unwrap();
        assert!(keep.join("usnm_quality_x_a_r/result.json").exists());
        let log = tmp.path().join("pairs.jsonl");
        compare(&a, &b, Some(&log)).unwrap();
        // A changed record does not agree; a changed host or time does.
        let mut r: ResultManifest = serde_json::from_slice(&std::fs::read(&b[0]).unwrap()).unwrap();
        r.host.ncpus = Some(4.0);
        r.timings.total_ms += 1000;
        std::fs::write(&b[0], serde_json::to_vec(&r).unwrap()).unwrap();
        compare(&a, &b, Some(&log)).unwrap();
        r.readers[0].words += 1;
        std::fs::write(&b[0], serde_json::to_vec(&r).unwrap()).unwrap();
        assert!(compare(&a, &b, Some(&log)).is_err());
        let lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[2]["agree"], false);
        assert_eq!(lines[2]["reason"], "readers differs");
        std::fs::write(&a[1], "tampered").unwrap();
        assert!(check(&a, None).is_err());
    }
}
