//! A controller-side retry of a container `usnm_ocr` page that failed for good (#1938).
//!
//! `usnm-boinc retry` (#1159) needs a local `--root` of job journals and a `--spool` of
//! assimilator markers, the BOINC host's own copies. The controller has neither: its state is the
//! hand-off container, Cosmos and the lake, and a parent's journal may exist only in an archive or
//! an evidence copy. This runs there, with managed identity only (`usnm-boinc controller-retry`,
//! the hand-off step `retry`): it stages the parent's journal and page in scratch, proves the
//! page failed for good and has no result anywhere, then prepares the child with the same
//! [`retry::prepare`] the host's command uses, and writes the child under `dispatch/jobs/` for
//! the server to pull and submit. Nothing is submitted here.
use crate::retry::{self, ExpectedTerminal, Reason};
use crate::{atomic, dir, load, read, save, Record, Stage, TerminalFailure};
use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use clap::Args;
use serde::Deserialize;
use std::path::{Path, PathBuf};
use usnm_ocr_contract::boinc;
use usnm_state::state::State;
use usnm_store::ObjectStore;

/// Where the `wu-terminal` bridge step writes the BOINC database's answer, in the hand-off
/// container.
pub const WU_PROOF_PATH: &str = "scheduler/wu-terminal.json";
/// The oldest a database answer may be.
const MAX_PROOF_AGE_SECS: i64 = 3600;
/// How far ahead of this host's clock one may be.
const MAX_PROOF_SKEW_SECS: i64 = 300;

/// The BOINC database's rows for some workunits, from `BRIDGE_STEP=wu-terminal`. It reads only:
/// `workunit.error_mask` and `canonical_resultid`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WuProof {
    pub v: u32,
    pub nonce: String,
    pub checked_at: String,
    pub workunits: Vec<WuRow>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WuRow {
    pub name: String,
    pub id: u64,
    pub error_mask: u32,
    pub canonical_resultid: u64,
    pub assimilate_state: u32,
}

impl WuProof {
    /// The workunit's error mask, when the database says it failed for good: a mask other than 0
    /// and no canonical result. The answer must be this run's (`nonce`) and recent.
    pub fn terminal_mask(
        &self,
        nonce: &str,
        workunit: &str,
        now: DateTime<Utc>,
    ) -> anyhow::Result<u32> {
        ensure!(self.v == 1, "unknown BOINC database proof version");
        ensure!(
            self.nonce == nonce,
            "the BOINC database proof is for another run (nonce differs); run wu-terminal again"
        );
        let at = DateTime::parse_from_rfc3339(&self.checked_at)
            .context("BOINC database proof has no valid checked_at")?
            .with_timezone(&Utc);
        let age = (now - at).num_seconds();
        ensure!(
            (-MAX_PROOF_SKEW_SECS..=MAX_PROOF_AGE_SECS).contains(&age),
            "the BOINC database proof is {age} s old; run wu-terminal again"
        );
        let mut rows = self.workunits.iter().filter(|row| row.name == workunit);
        let row = rows
            .next()
            .with_context(|| format!("the BOINC database proof has no workunit {workunit}"))?;
        ensure!(rows.next().is_none(), "{workunit} is in the proof twice");
        ensure!(
            row.canonical_resultid == 0,
            "workunit {workunit} (id {}) has canonical result {} in the BOINC database",
            row.id,
            row.canonical_resultid
        );
        ensure!(
            row.error_mask != 0,
            "workunit {workunit} (id {}) has no error mask in the BOINC database: it did not fail for good",
            row.id
        );
        Ok(row.error_mask)
    }
}

/// What to retry and how.
pub struct Request<'a> {
    pub parent: &'a str,
    /// An archive or evidence copy of the parent's journal, when the live hand-off has none:
    /// a directory holding `jobs/<id>/record.json`, `dispatch/jobs/<id>/record.json`,
    /// `<id>/record.json` or `record.json` itself.
    pub journal_dir: Option<&'a Path>,
    /// The nonce `wu-terminal` was given.
    pub nonce: &'a str,
    pub image: &'a str,
    pub app_version_num: u32,
    pub timeout_secs: Option<u32>,
    pub reason: Reason,
    pub reviewed_reason: &'a str,
    pub target_hosts: &'a [u64],
    /// Check everything and reserve nothing.
    pub dry_run: bool,
}

/// The hand-off container, the lake and the state the retry reads and writes.
pub struct Stores<'a> {
    pub handoff: &'a dyn ObjectStore,
    pub curated: &'a dyn ObjectStore,
    pub reference: &'a dyn ObjectStore,
    /// The published outputs (`ocr-out`): a page with an ALTO there has a result.
    pub out: Out<'a>,
    pub state: &'a State,
}

/// The published outputs store, and how its `list` takes a prefix: Blob Storage matches names
/// that start with it, and a local store treats it as a directory.
pub struct Out<'a> {
    pub store: &'a dyn ObjectStore,
    pub by_name_prefix: bool,
}

impl Out<'_> {
    /// Whether an ALTO is published for the page (`alto/<doc id>.boinc-<job>.<stamp>.xml`).
    async fn has_alto(&self, doc_id: &str) -> anyhow::Result<bool> {
        let prefix = format!("alto/{doc_id}.");
        let names = if self.by_name_prefix {
            self.store.list(&prefix).await?
        } else {
            self.store.list("alto").await?
        };
        Ok(names.iter().any(|name| name.starts_with(&prefix)))
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum Outcome {
    /// Every check passed; nothing was reserved.
    Checked,
    /// The child, reserved and written to the hand-off container (`exported` is false when an
    /// earlier run had already).
    Prepared { child: String, exported: bool },
}

/// The parent's journal and page, where they are: the archive or evidence copy when given, else
/// the live hand-off container.
async fn parent_files(
    request: &Request<'_>,
    handoff: &dyn ObjectStore,
) -> anyhow::Result<(Vec<u8>, Option<Vec<u8>>)> {
    let id = request.parent;
    if let Some(base) = request.journal_dir {
        for candidate in [
            base.join("jobs").join(id),
            base.join("dispatch").join("jobs").join(id),
            base.join(id),
            base.to_path_buf(),
        ] {
            let record = candidate.join("record.json");
            if !record.is_file() {
                continue;
            }
            let bytes = read(&record, boinc::MAX_REQUEST * 2)?;
            let named: Record = serde_json::from_slice(&bytes)?;
            if named.job.id != id {
                continue;
            }
            let page = candidate.join("page");
            let page = if page.is_file() {
                Some(read(&page, boinc::MAX_SCAN)?)
            } else {
                None
            };
            return Ok((bytes, page));
        }
        anyhow::bail!(
            "no journal for {id} under {} (looked for jobs/{id}, dispatch/jobs/{id}, {id} and record.json)",
            base.display()
        );
    }
    let record = handoff
        .get(&format!("dispatch/jobs/{id}/record.json"))
        .await?
        .with_context(|| {
            format!("the live hand-off has no journal for {id}; give --journal-dir with an archive or evidence copy")
        })?;
    Ok((record, None))
}

/// The spool the server put in the container for the parent: its terminal marker, and any file
/// that would be a result.
struct Spool {
    error_marker: Option<Vec<u8>>,
    no_output_files: bool,
    outputs: Vec<String>,
}

async fn spool(handoff: &dyn ObjectStore, id: &str) -> anyhow::Result<Spool> {
    let workunit = format!("usnm_{id}");
    let mut found = Spool {
        error_marker: None,
        no_output_files: false,
        outputs: Vec::new(),
    };
    for (name, max) in crate::handoff::spool_names(&workunit) {
        let Some(bytes) = handoff.get(&format!("spool/{id}/{name}")).await? else {
            continue;
        };
        ensure!(bytes.len() <= max, "{name} is over its size limit");
        if name.ends_with("_error") {
            found.error_marker = Some(bytes);
        } else if name.ends_with("_no_output_files") {
            found.no_output_files = true;
        } else {
            found.outputs.push(name);
        }
    }
    Ok(found)
}

/// The retry's page deadline. A slow-class page runs out again with the deadline it ran out of,
/// so it needs a longer one; a page that crashed once is read once more with the same settings.
fn child_timeout(
    reason: Reason,
    parent: &boinc::Job,
    requested: Option<u32>,
) -> anyhow::Result<Option<u32>> {
    let parent_secs = parent.engine.timeout_secs;
    match (reason, requested) {
        (Reason::SlowClass, None) => anyhow::bail!(
            "a slow-class retry needs --timeout-secs, longer than the parent's {parent_secs} s"
        ),
        (Reason::SlowClass, Some(secs)) => {
            ensure!(
                (1..=crate::claim::MAX_TIMEOUT_SECS).contains(&secs),
                "a page deadline is 1 to {} seconds",
                crate::claim::MAX_TIMEOUT_SECS
            );
            ensure!(
                secs > parent_secs,
                "a slow-class retry needs a deadline longer than the parent's {parent_secs} s: the same one runs out again"
            );
            Ok(Some(secs))
        }
        (Reason::SegfaultOnce, Some(_)) => anyhow::bail!(
            "a segfault-once retry keeps the parent's settings; --timeout-secs is for slow-class"
        ),
        (Reason::SegfaultOnce, None) => {
            ensure!(
                parent.attempt == 0,
                "segfault-once retries a first attempt only; this parent is attempt {}",
                parent.attempt
            );
            Ok(None)
        }
    }
}

fn check_request(request: &Request<'_>) -> anyhow::Result<()> {
    ensure!(
        boinc::is_sha256(request.parent),
        "--job is a job id (64 hex)"
    );
    ensure!(
        request
            .image
            .strip_prefix("sha256:")
            .is_some_and(boinc::is_sha256),
        "--image is the released worker's digest, sha256:<64 hex>"
    );
    ensure!(
        (1..=i32::MAX as u32).contains(&request.app_version_num),
        "--app-version-num is the released usnm_ocr version"
    );
    ensure!(
        request.nonce.len() == 32
            && request
                .nonce
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
        "--nonce is the 32 hex the wu-terminal step was given"
    );
    ensure!(
        request.target_hosts.windows(2).all(|w| w[0] < w[1])
            && request.target_hosts.iter().all(|h| *h > 0)
            && request.target_hosts.len() <= 64,
        "target hosts are positive and listed once"
    );
    Ok(())
}

/// Every host must have a gold report for the child's image in the parent's scope, and none may
/// be held. One refusal refuses the retry: a page is never sent where it was not asked to go.
fn check_targets(
    trust: &crate::trust::Trust,
    job: &boinc::Job,
    image: &str,
    hosts: &[u64],
) -> anyhow::Result<()> {
    let mut probe = job.clone();
    probe.engine.image = image.into();
    for host in hosts {
        if let Some(reason) = trust.refuse_job(&probe, Some(*host)) {
            anyhow::bail!("host {host} is not golded for {image}: {reason}");
        }
    }
    Ok(())
}

/// Runs the retry of `request.parent`. See the module docs for what it checks.
pub async fn run(request: &Request<'_>, stores: &Stores<'_>) -> anyhow::Result<Outcome> {
    check_request(request)?;
    let id = request.parent;
    // A parent is retried once from here: its child is named in the hand-off, and a repeat finds
    // it. A failed child is retried from its own journal, with a fresh review naming it.
    let index = format!("dispatch/retries/{id}.json");
    if let Some(bytes) = stores.handoff.get(&index).await? {
        let saved: Index = serde_json::from_slice(&bytes)?;
        ensure!(boinc::is_sha256(&saved.child), "the retry index is damaged");
        return Ok(Outcome::Prepared {
            child: saved.child,
            exported: false,
        });
    }
    let (record_bytes, journal_page) = parent_files(request, stores.handoff).await?;
    let scratch = tempfile::tempdir()?;
    let root = scratch.path().join("root");
    let spool_dir = scratch.path().join("spool");
    std::fs::create_dir_all(&spool_dir)?;
    let parent_dir = dir(&root, id)?;
    atomic(&parent_dir.join("record.json"), &record_bytes)?;
    let mut parent = load(&root, id)?;
    ensure!(
        parent.bundle.is_none() && parent.job.lines.is_none(),
        "only a single-page container usnm_ocr job may be retried here"
    );
    ensure!(
        matches!(parent.stage, Stage::Failed | Stage::Submitted),
        "the parent's journal is {:?}: only a failed job (or a submitted one the database shows failed for good) is retried",
        parent.stage
    );
    let timeout = child_timeout(request.reason, &parent.job, request.timeout_secs)?;
    let page = match journal_page {
        Some(page) => page,
        None => stores
            .handoff
            .get(&format!("dispatch/jobs/{id}/page"))
            .await?
            .context("no page for the parent in the journal copy or the live hand-off")?,
    };
    ensure!(
        page.len() as u64 == parent.job.scan.bytes
            && boinc::sha256(&page) == parent.job.scan.sha256,
        "the parent's page differs from its request's scan"
    );
    atomic(&parent_dir.join("page"), &page)?;
    let doc_id = parent.job.message.key()?.doc_id();

    // No result anywhere: not a finished claim, no accepted or spooled output, no import in the
    // ledger, no published ALTO, and (below) no canonical result in the BOINC database.
    if let Some(bytes) = stores.handoff.get(&crate::claim::path(&doc_id)).await? {
        let claim = crate::claim::Claim::load(&bytes)?;
        ensure!(
            claim.ended.as_deref() != Some("finished"),
            "the claim for {doc_id} ended finished: the page was read; refusing to read it again"
        );
    }
    let accepted = crate::handoff::get(
        stores.handoff,
        &format!("dispatch/jobs/{id}/accepted"),
        &parent_dir.join("accepted"),
    )
    .await?;
    ensure!(
        accepted == 0,
        "the parent has accepted output in the hand-off: a canonical result needs reconciliation, not retry"
    );
    let spooled = spool(stores.handoff, id).await?;
    ensure!(
        spooled.outputs.is_empty(),
        "the parent's spool holds output ({}): a canonical result needs reconciliation, not retry",
        spooled.outputs.join(", ")
    );
    ensure!(
        !spooled.no_output_files,
        "the parent's marker is no-output-files; use usnm-boinc retry --expected-no-output-files"
    );
    ensure!(
        !usnm_ocr_local::ledger::load(stores.curated)
            .await?
            .imported(&doc_id),
        "{doc_id} was imported: it has a result"
    );
    ensure!(
        !stores.out.has_alto(&doc_id).await?,
        "{doc_id} has a published ALTO: it has a result"
    );

    // Terminal proof: the BOINC database's error mask and no canonical result. When the
    // assimilator's marker is in the hand-off it must say the same; otherwise the marker is the
    // mask the database gave, which is what the assimilator would have written.
    let proof_bytes = stores.handoff.get(WU_PROOF_PATH).await?.with_context(|| {
        format!("no {WU_PROOF_PATH} in the hand-off: run the wu-terminal bridge step first")
    })?;
    ensure!(
        proof_bytes.len() <= boinc::MAX_JSON,
        "the proof is too large"
    );
    let proof: WuProof = serde_json::from_slice(&proof_bytes)?;
    let workunit = parent.job.workunit();
    let mask = proof.terminal_mask(request.nonce, &workunit, Utc::now())?;
    let marker = match &spooled.error_marker {
        Some(real) => {
            ensure!(
                TerminalFailure::from_marker(Some(real)).error_mask == Some(mask),
                "the assimilator's marker and the BOINC database disagree on the error mask ({mask:#x})"
            );
            real.clone()
        }
        None => format!("{mask:#x}\n").into_bytes(),
    };
    let terminal = TerminalFailure::from_marker(Some(&marker));
    ensure!(
        terminal.retryable(),
        "unknown or zero error mask is not retryable"
    );
    let reason_text = crate::terminal_reason(Some(&marker), parent.job.engine.timeout_secs);
    if parent.terminal.is_none() {
        // A journal that predates terminal fields: the database's proof stands in for them, in
        // scratch only. The archive is never written.
        parent.terminal = Some(terminal.clone());
        parent.error = Some(reason_text);
        parent.stage = Stage::Failed;
        save(&root, &parent)?;
    } else {
        ensure!(
            parent.stage == Stage::Failed,
            "a submitted journal with terminal fields is inconsistent; reconcile it"
        );
    }
    atomic(&spool_dir.join(format!("{workunit}_error")), &marker)?;

    if !request.target_hosts.is_empty() {
        let trust = stores
            .handoff
            .get("gold/trust.json")
            .await?
            .context("no gold/trust.json in the hand-off: target hosts cannot be checked")?;
        let trust: crate::trust::Trust = serde_json::from_slice(&trust)?;
        check_targets(&trust, &parent.job, request.image, request.target_hosts)?;
    }
    // An earlier run that was interrupted after it reserved the child left the time it made it
    // at, so this one makes the same child (its ledger row is no longer the parent's, so the
    // checks of the parent's row are for a first run only). An intent whose time is in no ledger
    // row is a run that stopped before it reserved; this one replaces it.
    let intent_path = format!("dispatch/retries/{id}.intent.json");
    let saved = match stores.handoff.get(&intent_path).await? {
        Some(bytes) => Some(serde_json::from_slice::<Intent>(&bytes)?),
        None => None,
    };
    let want = Intent {
        queued_at: Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        request: fingerprint(request, timeout),
    };
    let stale = saved.is_some();
    let mut resumed = None;
    if let Some(saved) = saved {
        let at = DateTime::parse_from_rfc3339(&saved.queued_at)?.with_timezone(&Utc);
        let attempt = parent.queued()?.attempt;
        let reserved = usnm_ocr_local::ledger::load(stores.curated)
            .await?
            .queued
            .get(&doc_id)
            .is_some_and(|row| row.queued_at == at && row.attempt > attempt);
        if reserved {
            ensure!(
                saved.request == want.request,
                "an interrupted retry of {id} was begun with other settings; repeat it with those, or reconcile it"
            );
            resumed = Some(at);
        }
    }
    // The time the child is made at is final: after the parent's permanent outcome, which the
    // reservation would otherwise move it past, leaving the intent short of the ledger's row.
    let mut settled_at = None;
    if resumed.is_none() {
        settled_at = Some(preflight(&parent, stores).await?);
    }
    // The controller reads a retry's lineage from the live journals: the parent is there, failed.
    // Checked before anything is reserved, written after the child is.
    let parent_write = plan_parent(stores.handoff, &parent).await?;
    if request.dry_run {
        return Ok(Outcome::Checked);
    }
    let queued_at = match resumed {
        Some(at) => at,
        None => {
            let now = DateTime::parse_from_rfc3339(&want.queued_at)?.with_timezone(&Utc);
            let at = settled_at.map_or(now, |settled| {
                now.max(settled + chrono::Duration::microseconds(1))
            });
            let intent = Intent {
                queued_at: at.to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
                request: want.request,
            };
            let bytes = serde_json::to_vec(&intent)?;
            // A first intent is written only if none is there: of two runs that start together
            // (the step's lock prevents it; a direct run takes none) the second finds the first's
            // and stops. Replacing a stale one is for the holder of the lock.
            if stale {
                stores
                    .handoff
                    .put(&intent_path, bytes, "application/json")
                    .await?;
            } else if !stores
                .handoff
                .put_new(&intent_path, bytes, "application/json")
                .await?
            {
                anyhow::bail!(
                    "another retry of {id} wrote its intent first; run this one again to resume it"
                );
            }
            at
        }
    };

    let child = retry::prepare(
        &retry::Options {
            root: &root,
            spool: &spool_dir,
            previous_job: id,
            expected: ExpectedTerminal::Mask(mask),
            app_version_num: request.app_version_num,
            image: request.image,
            reviewed_reason: request.reviewed_reason,
            timeout_secs: timeout,
            note: Some((request.reason, request.target_hosts.to_vec())),
            now: Some(queued_at),
            native_host: None,
            handoff: stores.handoff,
        },
        stores.state,
        stores.curated,
        stores.reference,
    )
    .await?;
    // A resumed reservation does not rewrite the child's files; they are the parent's page and
    // the request the child's journal holds.
    let child_dir = dir(&root, &child)?;
    atomic(&child_dir.join("page"), &page)?;
    let child_record = load(&root, &child)?;
    atomic(
        &child_dir.join("request.json"),
        &serde_json::to_vec_pretty(&child_record.job)?,
    )?;
    write_parent(stores.handoff, id, parent_write).await?;
    // The server pulls the child from here. Its journal goes last, so one that is there has its
    // page and request; a journal already there is the server's to move on and is never put back.
    let prefix = format!("dispatch/jobs/{child}");
    let exported = export_child(stores.handoff, &child_dir, &prefix).await?;
    stores
        .handoff
        .put_new(
            &index,
            serde_json::to_vec(&Index {
                child: child.clone(),
            })?,
            "application/json",
        )
        .await?;
    Ok(Outcome::Prepared { child, exported })
}

/// What an interrupted retry needs to be made again as it was: the time its child was made at,
/// and what was asked (`dispatch/retries/<parent>.intent.json`, written before the reservation).
#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Intent {
    queued_at: String,
    request: String,
}

fn fingerprint(request: &Request<'_>, timeout: Option<u32>) -> String {
    boinc::sha256(
        serde_json::json!({
            "image": request.image,
            "app_version_num": request.app_version_num,
            "timeout_secs": timeout,
            "reason": request.reason.as_str(),
            "reviewed_reason": request.reviewed_reason,
            "target_hosts": request.target_hosts,
        })
        .to_string()
        .as_bytes(),
    )
}

/// The checks `retry::prepare` makes under the native lease, made without reserving, so a dry
/// run says what a real one would: a queue-fed parent's claim is conclusively dead-lettered, the
/// page's batch and title are still published, and the parent's row is the latest in the ledger
/// with a message-bound permanent failure. The reservation checks them again under the lease.
async fn preflight(parent: &Record, stores: &Stores<'_>) -> anyhow::Result<DateTime<Utc>> {
    let key = parent.job.message.key()?;
    let doc_id = key.doc_id();
    let previous = parent.queued()?;
    if parent.retry.is_none() && parent.ledger.is_some() {
        let bytes = stores
            .handoff
            .get(&crate::claim::path(&doc_id))
            .await?
            .context("missing parent claim; reconcile its queue delivery")?;
        let claim = crate::claim::Claim::load(&bytes)?;
        ensure!(
            claim.message()? == parent.job.message
                && claim.ledger == crate::LedgerRow::from_queued(&previous)
                && claim.ended.as_deref() == Some("dead_lettered"),
            "parent delivery is not conclusively deleted; reconcile its claim"
        );
    }
    let snapshot = usnm_ocr_local::boinc::Snapshot::load(stores.state, stores.reference).await?;
    ensure!(
        snapshot.batch_version(&previous.batch) == Some(previous.batch_version),
        "the page's batch version is not published"
    );
    ensure!(
        snapshot.titles.contains_key(&key.lccn) && !snapshot.is_japanese(&key.lccn),
        "the page's title is unavailable or uses the Japanese OCR pipeline"
    );
    let ledger = usnm_ocr_local::ledger::load(stores.curated).await?;
    ensure!(
        !ledger.imported(&doc_id),
        "{doc_id} was imported: it has a result"
    );
    ensure!(
        ledger.queued.get(&doc_id) == Some(&previous),
        "the parent's attempt is not the page's latest ledger row; reconcile"
    );
    let outcome = ledger
        .outcomes
        .get(&doc_id)
        .context("the parent's attempt has no settled outcome in the ledger")?;
    ensure!(
        outcome.outcome == usnm_ocr_local::ledger::DEAD_LETTERED
            && outcome.cause.as_deref() == Some("permanent")
            && outcome.at >= previous.queued_at
            && outcome.message_sha256.as_deref()
                == Some(boinc::sha256(&serde_json::to_vec(&parent.job.message)?).as_str()),
        "only a message-bound settled permanent failure may be retried"
    );
    Ok(outcome.at)
}

/// What the live hand-off needs for the parent's journal to be failed, with the proof the retry
/// stands on, so the controller finds a failed parent for the child.
enum ParentWrite {
    /// Already so.
    Nothing,
    /// There is no live journal: write this one.
    Create(Vec<u8>),
    /// The live journal is `submitted`, or a legacy `failed` with no terminal fields: this one is
    /// it with those, and nothing else of it changed.
    Replace(Vec<u8>),
}

/// Decides [`ParentWrite`], refusing any other live state: it is never overwritten. The hand-off
/// steps hold one lock, so nothing else writes the journal between this and [`write_parent`].
async fn plan_parent(handoff: &dyn ObjectStore, parent: &Record) -> anyhow::Result<ParentWrite> {
    let path = format!("dispatch/jobs/{}/record.json", parent.job.id);
    let Some(bytes) = handoff.get(&path).await? else {
        return Ok(ParentWrite::Create(serde_json::to_vec_pretty(parent)?));
    };
    let live: Record = serde_json::from_slice(&bytes)?;
    ensure!(
        live.job == parent.job && live.plan == parent.plan && live.bundle.is_none(),
        "the live journal for {} is another job's or a bundle's; reconcile it",
        parent.job.id
    );
    match (&live.stage, &live.terminal) {
        (Stage::Failed, Some(terminal)) => {
            ensure!(
                Some(terminal) == parent.terminal.as_ref(),
                "the live journal for {} has other terminal proof; reconcile it",
                parent.job.id
            );
            Ok(ParentWrite::Nothing)
        }
        (Stage::Failed | Stage::Submitted, None) => {
            let mut updated = live.clone();
            updated.stage = Stage::Failed;
            updated.terminal = parent.terminal.clone();
            updated.error = parent.error.clone();
            Ok(ParentWrite::Replace(serde_json::to_vec_pretty(&updated)?))
        }
        (stage, _) => anyhow::bail!(
            "the live journal for {} is {stage:?}; reconcile it, a retry does not overwrite it",
            parent.job.id
        ),
    }
}

async fn write_parent(
    handoff: &dyn ObjectStore,
    id: &str,
    write: ParentWrite,
) -> anyhow::Result<()> {
    let path = format!("dispatch/jobs/{id}/record.json");
    match write {
        ParentWrite::Nothing => {}
        ParentWrite::Create(bytes) => {
            if !handoff.put_new(&path, bytes, "application/json").await? {
                anyhow::bail!("the live journal for {id} appeared meanwhile; reconcile it");
            }
        }
        ParentWrite::Replace(bytes) => handoff.put(&path, bytes, "application/json").await?,
    }
    Ok(())
}

/// The child a parent's retry made, at `dispatch/retries/<parent>.json`.
#[derive(serde::Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Index {
    child: String,
}

/// Writes the child to the hand-off: its page and request, checked against the scratch copies
/// and written if missing or different (they are fixed by the child's identity), then its
/// journal, last and only if there is none. Returns whether it wrote the journal. A journal
/// that is there has its files, and an interrupted export is repaired, never overwritten.
/// Each write is tried five times; the reservation is made by now, so a child that cannot be
/// written has its journal printed to be restored by hand, as the hand-off job does for its
/// entries (a lost reservation's record cannot be made again).
async fn export_child(
    handoff: &dyn ObjectStore,
    child_dir: &Path,
    prefix: &str,
) -> anyhow::Result<bool> {
    let record_path = format!("{prefix}/record.json");
    let record = read(&child_dir.join("record.json"), boinc::MAX_REQUEST * 2)?;
    let result = async {
        for name in ["page", "request.json"] {
            let path = format!("{prefix}/{name}");
            let want = read(&child_dir.join(name), boinc::MAX_SCAN)?;
            if handoff.get(&path).await?.as_ref() != Some(&want) {
                put_retrying(handoff, &path, want).await?;
            }
        }
        if handoff.exists(&record_path).await? {
            return Ok(false);
        }
        put_retrying(handoff, &record_path, record.clone()).await?;
        Ok::<_, anyhow::Error>(true)
    }
    .await;
    if result.is_err() {
        eprintln!(
            "USNM-HANDOFF-RECORD {record_path} {}",
            String::from_utf8_lossy(&record).replace('\n', "")
        );
    }
    result.context("could not write the child to the hand-off; its journal is in the log")
}

async fn put_retrying(handoff: &dyn ObjectStore, path: &str, body: Vec<u8>) -> anyhow::Result<()> {
    let mut last = None;
    for attempt in 1..=5u64 {
        match handoff
            .put(path, body.clone(), "application/octet-stream")
            .await
        {
            Ok(()) => return Ok(()),
            Err(error) => {
                eprintln!("writing {path} failed (attempt {attempt}): {error:#}");
                last = Some(error);
                tokio::time::sleep(std::time::Duration::from_secs(attempt * 10)).await;
            }
        }
    }
    Err(anyhow::Error::from(last.unwrap()))
}

#[derive(Args)]
pub struct ControllerRetryArgs {
    /// The failed parent's job id.
    #[arg(long)]
    job: String,
    /// An archive or evidence copy of the parent's journal (and page), when the live hand-off
    /// has none.
    #[arg(long)]
    journal_dir: Option<PathBuf>,
    /// The nonce the `wu-terminal` step ran with (32 hex).
    #[arg(long)]
    nonce: String,
    /// The released worker's image digest, sha256:<64 hex>: the child's `engine.image`.
    #[arg(long)]
    image: String,
    /// The released `usnm_ocr` version that packages it.
    #[arg(long)]
    app_version_num: u32,
    /// The child's page deadline, 1 to 3600: required for slow-class, longer than the parent's;
    /// not allowed for segfault-once.
    #[arg(long, value_parser = clap::value_parser!(u32).range(1..=i64::from(crate::claim::MAX_TIMEOUT_SECS)))]
    timeout_secs: Option<u32>,
    /// `slow-class` or `segfault-once`: noted in the journal and the published ALTO.
    #[arg(long)]
    reason: Reason,
    #[arg(long)]
    reviewed_reason: String,
    /// A host the child may go to (`--target-host` at submit); repeat for more. Each must be
    /// golded for --image in gold/trust.json.
    #[arg(long = "target-host", value_parser = clap::value_parser!(u64).range(1..))]
    target_hosts: Vec<u64>,
    /// Check everything and reserve nothing.
    #[arg(long)]
    dry_run: bool,
    #[arg(long)]
    handoff_url: String,
    #[arg(long)]
    curated_url: String,
    #[arg(long)]
    reference_url: String,
    /// The published outputs; a page with an ALTO there is refused.
    #[arg(long)]
    out_url: String,
    #[arg(long, required_unless_present = "cosmos", conflicts_with = "cosmos")]
    state_file: Option<PathBuf>,
    #[arg(long)]
    cosmos: Option<String>,
}

pub async fn controller_retry(args: ControllerRetryArgs) -> anyhow::Result<()> {
    let docs: std::sync::Arc<dyn usnm_state::docs::DocStore> = if let Some(endpoint) = args.cosmos {
        usnm_state::docs::open_endpoint(&endpoint)?
    } else {
        std::sync::Arc::new(usnm_state::docs::FileDocs::open(
            args.state_file.context("--state-file required")?,
        )?)
    };
    let state = State::new(docs);
    let handoff = usnm_store::open(&args.handoff_url)?;
    let curated = usnm_store::open(&args.curated_url)?;
    let reference = usnm_store::open(&args.reference_url)?;
    let out = usnm_store::open(&args.out_url)?;
    let by_name_prefix =
        args.out_url.starts_with("https://") || args.out_url.starts_with("http://");
    let mut hosts = args.target_hosts;
    hosts.sort_unstable();
    hosts.dedup();
    let outcome = run(
        &Request {
            parent: &args.job,
            journal_dir: args.journal_dir.as_deref(),
            nonce: &args.nonce,
            image: &args.image,
            app_version_num: args.app_version_num,
            timeout_secs: args.timeout_secs,
            reason: args.reason,
            reviewed_reason: &args.reviewed_reason,
            target_hosts: &hosts,
            dry_run: args.dry_run,
        },
        &Stores {
            handoff: handoff.as_ref(),
            curated: curated.as_ref(),
            reference: reference.as_ref(),
            out: Out {
                store: out.as_ref(),
                by_name_prefix,
            },
            state: &state,
        },
    )
    .await?;
    match outcome {
        Outcome::Checked => eprintln!("dry run: every check passed; nothing reserved"),
        Outcome::Prepared { child, exported } => {
            if !exported {
                eprintln!("the child was already in the hand-off; left as it is");
            }
            println!("{child}");
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
