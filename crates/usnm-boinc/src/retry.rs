//! Deliberate, journalled retry of a settled terminal single-page workunit.
use crate::{atomic, dir, load, read, save, LedgerRow, Lock, Record, Stage, TerminalFailure};
use anyhow::{ensure, Context};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use usnm_ocr_contract::boinc;
use usnm_ocr_local::boinc::Snapshot;
use usnm_state::state::State;
use usnm_store::ObjectStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Retry {
    pub previous_job: String,
    pub previous_attempt: u32,
    pub previous_workunit: String,
    pub previous_ledger: LedgerRow,
    pub terminal: TerminalFailure,
    pub previous_error: String,
    pub app_version_num: u32,
    pub reviewed_reason: String,
}

pub fn validate(record: &Record) -> anyhow::Result<()> {
    match &record.retry {
        None => ensure!(
            record.job.attempt == 0,
            "retry job lacks its reservation journal"
        ),
        Some(retry) => {
            ensure!(
                record.job.attempt > 0
                    && record.job.v == boinc::RETRY_VERSION
                    && boinc::is_sha256(&retry.previous_job)
                    && retry.previous_job != record.job.id
                    && record.job.retry_of.as_ref() == Some(&retry.previous_job)
                    && retry.previous_attempt.checked_add(1) == Some(record.job.attempt)
                    && retry.previous_workunit == format!("usnm_{}", retry.previous_job)
                    && retry.terminal.retryable()
                    && (1..=i32::MAX as u32).contains(&retry.app_version_num)
                    && !retry.reviewed_reason.trim().is_empty()
                    && retry.reviewed_reason.len() <= 512
                    && !retry.reviewed_reason.chars().any(char::is_control)
                    && record.ledger.is_some()
                    && record.bundle.is_none(),
                "invalid retry journal"
            );
            let key = record.job.message.key()?;
            let previous = retry
                .previous_ledger
                .to_queued(&key.doc_id(), &record.job.message.batch)?;
            let next = record.queued()?;
            ensure!(
                usnm_ocr_local::boinc::next_attempt(&previous, next.queued_at)? == next
                    && next.plan == record.plan
                    && next.batch_version == record.job.batch_version
                    && next.language == record.job.message.language
                    && next.reason == record.job.message.reason.as_str()
                    && Some(
                        chrono::DateTime::parse_from_rfc3339(&record.job.message.queued_at)?
                            .with_timezone(&Utc)
                    ) == next.retry_message_at,
                "retry job and native reservation lineage differ"
            );
        }
    }
    Ok(())
}

pub enum ExpectedTerminal {
    Mask(u32),
    NoOutputFiles,
}

pub struct Options<'a> {
    pub root: &'a Path,
    pub spool: &'a Path,
    pub previous_job: &'a str,
    pub expected: ExpectedTerminal,
    pub app_version_num: u32,
    pub image: &'a str,
    pub reviewed_reason: &'a str,
}

pub async fn prepare(
    opts: &Options<'_>,
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
) -> anyhow::Result<String> {
    let _lock = Lock::take(opts.root)?;
    let previous = load(opts.root, opts.previous_job)?;
    ensure!(
        previous.stage == Stage::Failed && previous.bundle.is_none(),
        "only a terminal failed single-page job may be retried"
    );
    let previous_dir = dir(opts.root, opts.previous_job)?;
    ensure!(
        !previous_dir.join("accepted").try_exists()?,
        "accepted output requires reconciliation, not retry"
    );
    let workunit = previous.job.workunit();
    for name in ["result.json", "text.txt", "layout.xml"] {
        ensure!(
            !opts
                .spool
                .join(format!("{workunit}__file_{name}"))
                .try_exists()?,
            "canonical or partial output requires reconciliation, not retry"
        );
    }
    let marker_path = opts.spool.join(format!("{workunit}_error"));
    let marker = if marker_path.try_exists()? {
        Some(read(&marker_path, boinc::MAX_JSON)?)
    } else {
        ensure!(
            opts.spool
                .join(format!("{workunit}_no_output_files"))
                .try_exists()?,
            "no actual assimilator terminal marker"
        );
        None
    };
    let terminal = TerminalFailure::from_marker(marker.as_deref());
    ensure!(
        terminal.retryable(),
        "unknown or zero error mask is not retryable"
    );
    match opts.expected {
        ExpectedTerminal::Mask(mask) => ensure!(
            terminal.error_mask == Some(mask) && !terminal.no_output_files,
            "terminal error mask differs from reviewed value"
        ),
        ExpectedTerminal::NoOutputFiles => ensure!(
            terminal.no_output_files,
            "terminal failure differs from reviewed no-output disposition"
        ),
    }
    if let Some(recorded) = &previous.terminal {
        ensure!(
            *recorded == terminal,
            "terminal marker changed since collection"
        );
    }
    let previous_row = previous.queued()?;
    let snapshot = Snapshot::load(state, reference).await?;
    let now = Utc::now();
    let mut job = previous.job.clone();
    job.v = boinc::RETRY_VERSION;
    job.attempt = job.attempt.checked_add(1).context("attempt overflow")?;
    job.retry_of = Some(previous.job.id.clone());
    job.engine.image = opts.image.into();
    job.message.queued_at = now.to_rfc3339_opts(SecondsFormat::Micros, true);
    job.id = job.identity()?;
    job.validate()?;
    let id = job.id.clone();
    let retry = Retry {
        previous_job: opts.previous_job.into(),
        previous_attempt: previous.job.attempt,
        previous_workunit: workunit,
        previous_ledger: LedgerRow::from_queued(&previous_row),
        terminal,
        previous_error: crate::terminal_reason(marker.as_deref(), previous.job.engine.timeout_secs),
        app_version_num: opts.app_version_num,
        reviewed_reason: opts.reviewed_reason.into(),
    };
    let child_dir = dir(opts.root, &id)?;
    let mut record = if child_dir.join("record.json").try_exists()? {
        let child = load(opts.root, &id)?;
        job.message.queued_at = child.job.message.queued_at.clone();
        ensure!(
            child.retry.as_ref() == Some(&retry) && child.job == job && child.plan == previous.plan,
            "existing retry differs; reconcile the exact prior reservation"
        );
        child
    } else {
        let next = usnm_ocr_local::boinc::next_attempt(&previous_row, now)?;
        let child = Record {
            job,
            plan: previous.plan.clone(),
            stage: Stage::Prepared,
            stamp: now.format("%Y%m%dT%H%M%S%6fZ").to_string(),
            error: None,
            terminal: None,
            retry: Some(retry),
            ledger: Some(LedgerRow::from_queued(&next)),
            bundle: None,
            unbundled: None,
        };
        validate(&child)?;
        let scan = read(&previous_dir.join("page"), boinc::MAX_SCAN)?;
        ensure!(
            scan.len() as u64 == child.job.scan.bytes
                && boinc::sha256(&scan) == child.job.scan.sha256,
            "original cached scan changed"
        );
        atomic(&child_dir.join("page"), &scan)?;
        atomic(
            &child_dir.join("request.json"),
            &serde_json::to_vec_pretty(&child.job)?,
        )?;
        save(opts.root, &child)?;
        child
    };
    if record.stage == Stage::Prepared {
        usnm_ocr_local::boinc::retry(
            state,
            curated,
            &snapshot,
            &previous_row,
            &record.queued()?,
            &format!("boinc-retry-{id}"),
        )
        .await?;
        record.stage = Stage::Reserved;
        save(opts.root, &record)?;
    }
    ensure!(
        record.stage != Stage::Submitting && record.stage != Stage::Failed,
        "existing retry is uncertain or failed; reconcile or explicitly review that child job"
    );
    Ok(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{os::unix::fs::PermissionsExt, sync::Arc};
    use usnm_ocr_contract::boinc::Job;
    use usnm_ocr_local::ledger;
    use usnm_state::docs::MemoryDocs;

    #[tokio::test]
    async fn reviewed_retry_is_idempotent_pins_worker_and_never_resubmits_uncertain_work() {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("jobs-root");
        let spool = temp.path().join("spool");
        std::fs::create_dir(&spool).unwrap();
        let curated = usnm_store::open(temp.path().join("curated").to_str().unwrap()).unwrap();
        let reference = usnm_store::open(temp.path().join("reference").to_str().unwrap()).unwrap();
        let state = State::new(Arc::new(MemoryDocs::default()));
        state.set_current_version("v1").await.unwrap();
        reference.put_new("v1/batches.json", br#"[{"batch":"test_batch","curated":{"version":1,"parts":[],"counts":"","pages":1,"ok_pages":1,"lccns":["sn00000001"],"source_sha256":"","first":"","last":""}}]"#.to_vec(), "application/json").await.unwrap();
        reference
            .put_new(
                "v1/titles.json",
                br#"[{"lccn":"sn00000001","languages":["eng"]}]"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let mut job: Job = serde_json::from_str(include_str!(
            "../../usnm-ocr-contract/src/fixtures/boinc-tesseract.json"
        ))
        .unwrap();
        let page = b"cached scan";
        job.scan.bytes = page.len() as u64;
        job.scan.sha256 = boinc::sha256(page);
        job.id = job.identity().unwrap();
        let parent = Record {
            job,
            plan: "reviewed-retry".into(),
            stage: Stage::Failed,
            stamp: "prior".into(),
            error: Some("BOINC error mask 0x2".into()),
            terminal: Some(TerminalFailure::from_marker(Some(b"0x2\n"))),
            retry: None,
            ledger: None,
            bundle: None,
            unbundled: None,
        };
        save(&root, &parent).unwrap();
        let parent_path = dir(&root, &parent.job.id).unwrap();
        atomic(&parent_path.join("page"), page).unwrap();
        atomic(
            &spool.join(format!("{}_error", parent.job.workunit())),
            b"0x2\n",
        )
        .unwrap();
        let original = std::fs::read(parent_path.join("record.json")).unwrap();
        let previous = parent.queued().unwrap();
        ledger::write_queued(
            curated.as_ref(),
            std::slice::from_ref(&previous),
            previous.queued_at,
            "prior",
        )
        .await
        .unwrap();
        let at = previous.queued_at + chrono::Duration::minutes(1);
        ledger::write_outcomes(
            curated.as_ref(),
            &[ledger::Outcome {
                doc_id: previous.doc_id.clone(),
                outcome: ledger::DEAD_LETTERED.into(),
                cause: Some("permanent".into()),
                kind: None,
                source: "prior-dead-letter".into(),
                at,
                run: "prior".into(),
            }],
            at,
            "prior",
        )
        .await
        .unwrap();
        let image = format!("sha256:{}", "b".repeat(64));
        let mut opts = Options {
            root: &root,
            spool: &spool,
            previous_job: &parent.job.id,
            expected: ExpectedTerminal::Mask(2),
            app_version_num: 27,
            image: &image,
            reviewed_reason: "reviewed terminal marker and settled native row",
        };
        for stage in [Stage::Submitted, Stage::Submitting] {
            let mut nonterminal = parent.clone();
            nonterminal.stage = stage;
            save(&root, &nonterminal).unwrap();
            assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
                .await
                .is_err());
        }
        save(&root, &parent).unwrap();
        let partial = spool.join(format!("{}__file_text.txt", parent.job.workunit()));
        atomic(&partial, b"partial output").unwrap();
        assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .is_err());
        std::fs::remove_file(partial).unwrap();
        let marker = spool.join(format!("{}_error", parent.job.workunit()));
        for content in [b"broken".as_slice(), b"0x0\n", b"0x4\n"] {
            atomic(&marker, content).unwrap();
            assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
                .await
                .is_err());
        }
        atomic(&marker, b"0x2\n").unwrap();
        opts.expected = ExpectedTerminal::Mask(4);
        assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .is_err());
        opts.expected = ExpectedTerminal::Mask(2);
        assert_eq!(crate::records(&root).unwrap().len(), 1);
        let id = prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .unwrap();
        assert_ne!(id, parent.job.id);
        assert_eq!(
            prepare(&opts, &state, curated.as_ref(), reference.as_ref())
                .await
                .unwrap(),
            id
        );
        let child = load(&root, &id).unwrap();
        assert_eq!(child.job.attempt, 1);
        assert_eq!(child.job.retry_of.as_ref(), Some(&parent.job.id));
        assert_eq!(child.job.engine.image, image);
        assert_eq!(child.retry.as_ref().unwrap().terminal.error_mask, Some(2));
        assert_eq!(
            std::fs::read(parent_path.join("record.json")).unwrap(),
            original
        );
        let mut interrupted = child.clone();
        interrupted.stage = Stage::Prepared;
        save(&root, &interrupted).unwrap();
        assert_eq!(
            prepare(&opts, &state, curated.as_ref(), reference.as_ref())
                .await
                .unwrap(),
            id
        );
        let mut changed = child.clone();
        changed.ledger.as_mut().unwrap().request_day = Some("2020-01-01".into());
        assert!(validate(&changed).is_err());
        changed = child.clone();
        changed.retry.as_mut().unwrap().previous_attempt += 1;
        assert!(validate(&changed).is_err());

        let project = temp.path().join("project");
        std::fs::create_dir_all(project.join("bin")).unwrap();
        for (name, body) in [
            (
                "dir_hier_path",
                "#!/bin/sh\nprintf '%s/download/%s\\n' \"$PWD\" \"$1\"\n",
            ),
            (
                "create_work",
                "#!/bin/sh\nprintf '%s\\n' \"$@\" >> attempted-args\nexit 1\n",
            ),
        ] {
            let path = project.join("bin").join(name);
            atomic(&path, body.as_bytes()).unwrap();
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        assert!(crate::submit::submit(&root, &project, &id).await.is_err());
        assert_eq!(load(&root, &id).unwrap().stage, Stage::Submitting);
        assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .is_err());
        assert!(crate::submit::submit(&root, &project, &id).await.is_err());
        let args = std::fs::read_to_string(project.join("attempted-args")).unwrap();
        assert!(args.contains("--app_version_num\n27\n"));
        assert_eq!(args.matches("--wu_name\n").count(), 1);
    }

    #[test]
    fn terminal_masks_preserve_unknown_zero_and_actual_no_output_dispositions() {
        for marker in [Some(&b"broken"[..]), Some(&b"0x0"[..])] {
            assert!(!TerminalFailure::from_marker(marker).retryable());
        }
        assert!(TerminalFailure::from_marker(None).no_output_files);
        assert!(TerminalFailure::from_marker(None).retryable());
        let terminal = TerminalFailure::from_marker(Some(b"0x2\n"));
        assert_eq!(terminal.error_mask, Some(2));
        assert!(!terminal.no_output_files);
        assert_eq!(terminal.marker_sha256, Some(boinc::sha256(b"0x2\n")));
    }
}
