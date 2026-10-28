//! Deliberate, journalled retry of a settled terminal single-page workunit.
use crate::{atomic, dir, load, read, save, LedgerRow, Lock, Record, Stage, TerminalFailure};
use anyhow::{ensure, Context};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::path::Path;
use usnm_ocr_contract::boinc;
use usnm_ocr_contract::message::{Cause, DeadLetter};
use usnm_ocr_local::boinc::Snapshot;
use usnm_state::state::State;
use usnm_store::queue::{Queue, BULK, DEADLETTER, PRIORITY};
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
                usnm_ocr_local::boinc::next_attempt(
                    &previous,
                    next.queued_at,
                    &format!("boinc-{}", &record.job.id[..12])
                )? == next
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
    pub handoff: &'a dyn ObjectStore,
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
    if previous.retry.is_none() && previous.ledger.is_some() {
        let key = previous.job.message.key()?;
        let bytes = opts
            .handoff
            .get(&crate::claim::path(&key.doc_id()))
            .await?
            .context("missing parent claim; reconcile its queue delivery")?;
        let claim = crate::claim::Claim::load(&bytes)?;
        ensure!(
            claim.message()? == previous.job.message
                && claim.ledger == LedgerRow::from_queued(&previous.queued()?)
                && claim.ended.as_deref() == Some("dead_lettered"),
            "parent delivery is not conclusively deleted; reconcile its claim"
        );
    }
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
    ensure!(
        previous.terminal.as_ref() == Some(&terminal)
            && previous.error.as_deref() == Some(crate::terminal_reason(
                marker.as_deref(), previous.job.engine.timeout_secs
            ).as_str()),
        "terminal proof is missing or changed; reconcile by collecting the actual terminal disposition"
    );
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
        let next = usnm_ocr_local::boinc::next_attempt(
            &previous_row,
            now,
            &format!("boinc-{}", &job.id[..12]),
        )?;
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
        child
    };
    if matches!(record.stage, Stage::Prepared | Stage::Reserved) {
        let scan = read(&previous_dir.join("page"), boinc::MAX_SCAN)?;
        ensure!(
            scan.len() as u64 == record.job.scan.bytes
                && boinc::sha256(&scan) == record.job.scan.sha256,
            "original cached scan changed"
        );
        let expected = record.queued()?;
        usnm_ocr_local::boinc::retry(
            state,
            curated,
            &snapshot,
            &previous_row,
            &expected,
            &previous.job.message,
            |next| {
                ensure!(
                    record.stage == Stage::Prepared,
                    "reserved retry lost its reservation; reconcile"
                );
                record.job.message.queued_at =
                    next.queued_at.to_rfc3339_opts(SecondsFormat::Micros, true);
                record.stamp = next.queued_at.format("%Y%m%dT%H%M%S%6fZ").to_string();
                record.ledger = Some(LedgerRow::from_queued(next));
                validate(&record)?;
                atomic(&child_dir.join("page"), &scan)?;
                atomic(
                    &child_dir.join("request.json"),
                    &serde_json::to_vec_pretty(&record.job)?,
                )?;
                save(opts.root, &record)
            },
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

/// Retry reservations have no new queue claim to release. Publish their own
/// terminal event, never end or delete the parent's (possibly newer) claim.
pub async fn dead_letter(
    record: &Record,
    handoff: &dyn ObjectStore,
    queue: &dyn Queue,
) -> anyhow::Result<crate::claim::DeadLettered> {
    record.job.validate()?;
    validate(record)?;
    ensure!(
        record.retry.is_some() && record.stage == Stage::Failed && record.terminal.is_some(),
        "retry has no collected terminal failure"
    );
    let error = record.error.as_deref().context("missing terminal error")?;
    let from = match record.job.message.reason {
        usnm_ocr_contract::message::Reason::User => PRIORITY,
        usnm_ocr_contract::message::Reason::Bulk => BULK,
    };
    let mut letter = DeadLetter::new(
        from,
        &record.job.id,
        Some(&record.job.message),
        Cause::Permanent,
        None,
        None,
        error,
        1,
        "usnm-boinc",
    );
    letter.validate()?;
    let path = format!("dispatch/jobs/{}/retry-deadletter.json", record.job.id);
    if !handoff
        .put_new(&path, serde_json::to_vec(&letter)?, "application/json")
        .await?
    {
        let saved: DeadLetter = serde_json::from_slice(
            &handoff
                .get(&path)
                .await?
                .context("retry dead letter vanished")?,
        )?;
        letter.at = saved.at.clone();
        ensure!(saved == letter, "retry dead letter changed");
    }
    letter.validate()?;
    let text = letter.encode();
    let digest = boinc::sha256(text.as_bytes()).into_bytes();
    let sent = format!("{path}.sent");
    if let Some(receipt) = handoff.get(&sent).await? {
        ensure!(receipt == digest, "retry dead-letter receipt changed");
        return Ok(crate::claim::DeadLettered::Already);
    }
    // Like claim dead letters, lost send acknowledgement can duplicate an event.
    // The durable payload retains its identity and the native retry-time fence.
    queue.send(DEADLETTER, &text).await?;
    if !handoff.put_new(&sent, digest.clone(), "text/plain").await? {
        ensure!(
            handoff.get(&sent).await?.as_ref() == Some(&digest),
            "retry dead-letter receipt differs"
        );
    }
    Ok(crate::claim::DeadLettered::Done)
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
        let terminal_error = crate::terminal_reason(Some(b"0x2\n"), job.engine.timeout_secs);
        let parent = Record {
            job,
            plan: "reviewed-retry".into(),
            stage: Stage::Failed,
            stamp: "prior".into(),
            error: Some(terminal_error),
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
                message_sha256: Some(boinc::sha256(
                    &serde_json::to_vec(&parent.job.message).unwrap(),
                )),
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
            handoff: curated.as_ref(),
        };
        for stage in [Stage::Submitted, Stage::Submitting] {
            let mut nonterminal = parent.clone();
            nonterminal.stage = stage;
            save(&root, &nonterminal).unwrap();
            assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
                .await
                .is_err());
        }
        let mut missing_proof = parent.clone();
        missing_proof.terminal = None;
        save(&root, &missing_proof).unwrap();
        assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .is_err());
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
            let before = std::fs::read(parent_path.join("record.json")).unwrap();
            assert!(crate::collect(
                &root,
                &spool,
                &parent.job.id,
                curated.as_ref(),
                reference.as_ref()
            )
            .await
            .is_err());
            assert_eq!(
                std::fs::read(parent_path.join("record.json")).unwrap(),
                before
            );
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
        let mut queue_parent = parent.clone();
        queue_parent.ledger = Some(LedgerRow::from_queued(&previous));
        save(&root, &queue_parent).unwrap();
        let claim_key = crate::claim::path(&previous.doc_id);
        assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .is_err());
        for ended in [None, Some("dead_letter_lease_lost"), Some("lease_lost")] {
            let claim = crate::claim::Claim {
                v: crate::claim::VERSION,
                queue: BULK.into(),
                message_id: "parent".into(),
                pop_receipt: "old-receipt".into(),
                dequeue_count: 1,
                text: parent.job.message.encode(),
                claimed_at: at.to_rfc3339(),
                lease_until: at.to_rfc3339(),
                ledger: LedgerRow::from_queued(&previous),
                ended: ended.map(str::to_owned),
            };
            curated
                .put(
                    &claim_key,
                    serde_json::to_vec(&claim).unwrap(),
                    "application/json",
                )
                .await
                .unwrap();
            assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
                .await
                .is_err());
        }
        save(&root, &parent).unwrap();
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
        assert_eq!(load(&root, &id).unwrap().stage, Stage::Reserved);
        assert!(!project.join("attempted-args").exists());
        interrupted.stage = Stage::Submitting;
        save(&root, &interrupted).unwrap();
        assert!(prepare(&opts, &state, curated.as_ref(), reference.as_ref())
            .await
            .is_err());
        assert!(crate::submit::submit(&root, &project, &id).await.is_err());
        assert!(!project.join("attempted-args").exists());
        atomic(
            &spool.join(format!("{}_error", child.job.workunit())),
            b"0x4\n",
        )
        .unwrap();
        assert!(
            crate::collect(&root, &spool, &id, curated.as_ref(), reference.as_ref())
                .await
                .is_err()
        );
        let failed = load(&root, &id).unwrap();
        let handoff = usnm_store::open(temp.path().join("handoff").to_str().unwrap()).unwrap();
        let queue = usnm_store::queue::LocalQueue::new(temp.path().join("queue"));
        assert_eq!(
            dead_letter(&failed, handoff.as_ref(), &queue)
                .await
                .unwrap(),
            crate::claim::DeadLettered::Done
        );
        assert_eq!(
            dead_letter(&failed, handoff.as_ref(), &queue)
                .await
                .unwrap(),
            crate::claim::DeadLettered::Already
        );
        assert_eq!(queue.approximate_count(DEADLETTER).await.unwrap(), 1);
        let event = queue
            .receive(DEADLETTER, std::time::Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let event = usnm_ocr_contract::message::decode_dead_letter(&event.text).unwrap();
        assert!(event.error.contains("error mask 0x4 "));
        assert_eq!(event.message, Some(child.job.message.clone()));
        assert!(handoff
            .get(&crate::claim::path(&previous.doc_id))
            .await
            .unwrap()
            .is_none());
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
