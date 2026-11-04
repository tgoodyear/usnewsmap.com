//! BOINC as a second consumer of the OCR exchange's work queues (#296).
//!
//! The on-prem nodes take pages from `ocr-priority` and `ocr-bulk` themselves.
//! This module lets the BOINC hand-off take pages from the same queues, so a
//! page has one place in line whichever pool reads it:
//!
//! 1. [`pull`] (the hand-off job's `claim` step) receives messages from the
//!    queues it is asked for, with a lease as long as a BOINC task can take
//!    plus a day to collect it ([`LEASE`]). A message it can use becomes a
//!    [`Claim`] file; one it can't use goes back where it was (a copy sent,
//!    the original deleted) for a node to read, and one whose page has been
//!    answered already is deleted.
//! 2. `usnm-boinc export --claims` turns the claim into a job whose message
//!    is the queue's own, and checks, without writing, that the page's ledger
//!    row is still the outstanding one ([`ocr_local::boinc::adopt`]).
//! 3. After `collect` publishes the page, [`finish`] deletes the message.
//!    A job that fails, or is never collected, leaves the message hidden
//!    until the lease runs out; then it is visible to the nodes again. A
//!    claim is therefore at least once, like a node's.
//!
//! A message here is the same page-id-only message the nodes read; nothing
//! about a query or a person is added. The claim file holds the message's pop
//! receipt, which can only delete or release that one message, in the
//! private `boinc-handoff` container.
use crate::LedgerRow;
use anyhow::{bail, ensure, Context};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeSet, HashSet},
    path::Path,
    time::Duration,
};
use usnm_ocr_contract::{
    fetched,
    message::{self, Cause, DeadLetter, OcrMessage},
};
use usnm_ocr_local::{boinc::Snapshot, ledger::Ledger};
use usnm_ocr_worker::engine;
use usnm_store::{
    queue::{LeaseLost, Queue, Received, DEADLETTER, PRIORITY},
    ObjectStore,
};

pub mod cli;

/// How long a claimed message stays hidden: the workunits' `delay_bound`
/// (three days) and a day to collect. Azure's longest hide is seven days.
pub const LEASE: Duration = Duration::from_secs(4 * 24 * 3600);
/// Messages one `pull` peeks at on a queue before taking any.
const PEEK: u32 = 32;
/// The claim file's version.
pub const VERSION: u32 = 1;

/// The container path of a page's claim.
pub fn path(doc_id: &str) -> String {
    format!("claims/{doc_id}.json")
}

/// A queue message taken for BOINC, and the ledger row it must be imported
/// against. Written to the hand-off container as `claims/<doc_id>.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Claim {
    pub v: u32,
    pub queue: String,
    pub message_id: String,
    pub pop_receipt: String,
    pub dequeue_count: u32,
    /// The queue text as received.
    pub text: String,
    pub claimed_at: String,
    pub lease_until: String,
    pub ledger: LedgerRow,
    /// `finished`, `released`, `lease_lost` or `dead_lettered` once the message is gone;
    /// `dead_letter_lease_lost` while a dead-lettered page's message is back on its queue.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ended: Option<String>,
}

impl Claim {
    pub fn message(&self) -> anyhow::Result<OcrMessage> {
        Ok(message::decode(&self.text)?.0)
    }

    fn received(&self) -> Received {
        Received {
            queue: self.queue.clone(),
            id: self.message_id.clone(),
            pop_receipt: self.pop_receipt.clone(),
            dequeue_count: self.dequeue_count,
            text: self.text.clone(),
        }
    }

    pub fn load(bytes: &[u8]) -> anyhow::Result<Self> {
        let claim: Self = serde_json::from_slice(bytes)?;
        ensure!(claim.v == VERSION, "unsupported claim version {}", claim.v);
        Ok(claim)
    }
}

/// What `pull` may use, read once for the whole run.
pub struct Sources<'a> {
    pub queue: &'a dyn Queue,
    pub snapshot: &'a Snapshot,
    pub ledger: &'a Ledger,
    /// The newest fetched-scans list for the manifest the cohort uses: BOINC
    /// volunteers never fetch from LoC, so a page without a cached scan stays
    /// with the nodes.
    pub fetched: &'a fetched::List,
    /// The Tesseract models the worker image holds.
    pub installed: &'a BTreeSet<String>,
    /// The source languages BOINC results can be trusted for (the gold calibration's
    /// scope, #866). A page in any other language would be held back at collect and
    /// stay hidden for the lease, so it stays with the nodes. `None` takes any
    /// routable language.
    pub languages: Option<&'a BTreeSet<String>>,
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The queues to take from, in order. `ocr-bulk` alone by default:
    /// a page someone was shown should not wait for a volunteer.
    pub queues: Vec<String>,
    /// Most claims to make, across all the queues: filled in `queues` order.
    pub max: usize,
    /// Most messages to receive from each queue, claimed or not. Each queue has its
    /// own budget, so a queue of unusable messages can't use up the next one's (#1057).
    pub max_receives: usize,
    pub lease: Duration,
}

#[derive(Debug, Default)]
pub struct Report {
    pub claims: Vec<Claim>,
    pub received: usize,
    /// Messages sent back for the nodes: not usable here.
    pub released: usize,
    /// Messages deleted because their page was answered already.
    pub stale: usize,
    /// The same counts for each queue read, in the order read.
    pub queues: Vec<QueueReport>,
}

/// What `pull` did on one queue.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueueReport {
    pub queue: String,
    pub claimed: usize,
    pub received: usize,
    pub released: usize,
    pub stale: usize,
}

#[derive(Debug, PartialEq)]
enum Verdict {
    Use(LedgerRow),
    /// The page has an answer already: delete the message.
    Stale,
    /// Not for BOINC: leave it for a node.
    Skip(&'static str),
}

fn judge(text: &str, src: &Sources<'_>) -> Verdict {
    let Ok((msg, key)) = message::decode(text) else {
        return Verdict::Skip("invalid");
    };
    let doc_id = key.doc_id();
    if !src.ledger.queued.contains_key(&doc_id) {
        // Queued some other way (a test command): ocr-sync could not import
        // a page the ledger doesn't know either, so it is not ours to take.
        return Verdict::Skip("no_ledger_row");
    }
    let Some(row) = src.ledger.outstanding(&doc_id) else {
        return Verdict::Stale;
    };
    if row.retry_run.is_some() {
        return Verdict::Skip("deliberate_retry");
    }
    if row.retry_message_at.is_some_and(|expected| {
        !chrono::DateTime::parse_from_rfc3339(&msg.queued_at)
            .is_ok_and(|at| at.timestamp_micros() == expected.timestamp_micros())
    }) {
        return Verdict::Stale;
    }
    if row.batch != msg.batch || row.reason != msg.reason.as_str() || row.language != msg.language {
        return Verdict::Skip("ledger_mismatch");
    }
    if src.snapshot.batch_version(&msg.batch) != Some(row.batch_version) {
        return Verdict::Skip("batch_not_published");
    }
    if !src.snapshot.titles.contains_key(&key.lccn) || src.snapshot.is_japanese(&key.lccn) {
        return Verdict::Skip("title");
    }
    if engine::route(&msg.language, src.installed).is_none() {
        return Verdict::Skip("unroutable");
    }
    if src.languages.is_some_and(|l| !l.contains(&msg.language)) {
        return Verdict::Skip("untrusted_language");
    }
    if !src.fetched.contains(&msg.batch, &doc_id) {
        return Verdict::Skip("no_cached_scan");
    }
    Verdict::Use(LedgerRow::from_queued(row))
}

/// Take up to `opts.max` usable messages off the queues, in `opts.queues` order:
/// a later queue fills what the earlier ones left. Bulk queues are peeked
/// first, so one whose front holds nothing usable is not touched. Priority
/// queues scan even past an unusable front. Each queue is read for at most
/// `max_receives` messages of its own, so one queue's unusable messages never
/// use up another's budget.
/// Claim files are written to `out`; the caller puts them in the container
/// before anything else, because the pop receipts exist nowhere else.
///
/// The report is filled in as it goes, so a caller that gets an error can
/// still say what was taken before it: those messages are hidden for the
/// lease, and their claim files are already in `out`.
pub async fn pull(
    src: &Sources<'_>,
    opts: &Options,
    out: &Path,
    report: &mut Report,
) -> anyhow::Result<()> {
    ensure!(
        (1..=100).contains(&opts.max) && opts.max_receives >= opts.max,
        "claim 1 to 100 pages, from at least that many receives"
    );
    ensure!(
        opts.lease <= Duration::from_secs(7 * 24 * 3600),
        "a queue message can be hidden for seven days at most"
    );
    std::fs::create_dir_all(out)?;
    let mut claimed = HashSet::new();
    for queue in &opts.queues {
        if report.claims.len() >= opts.max {
            break;
        }
        report.queues.push(QueueReport {
            queue: queue.clone(),
            ..QueueReport::default()
        });
        match src.queue.peek(queue, PEEK).await {
            Ok(front)
                if queue != PRIORITY
                    && !front
                        .iter()
                        .any(|t| matches!(judge(t, src), Verdict::Use(_) | Verdict::Stale)) =>
            {
                continue;
            }
            // A queue that can't peek is read anyway.
            Ok(_) | Err(_) => {}
        }
        let mut sent_back = HashSet::new();
        let mut received = 0;
        while report.claims.len() < opts.max && received < opts.max_receives {
            let Some(msg) = src.queue.receive(queue, opts.lease).await? else {
                break;
            };
            received += 1;
            report.tally(Event::Received);
            if sent_back.contains(&msg.text) {
                // The queue is short enough that we met a copy we sent back. Put it
                // back again, rather than leave it hidden for the lease, and stop.
                send_back(src.queue, &msg).await?;
                report.tally(Event::Released);
                break;
            }
            let doc = message::decode(&msg.text).ok().map(|(_, key)| key.doc_id());
            match judge(&msg.text, src) {
                Verdict::Use(_) if doc.as_ref().is_some_and(|d| claimed.contains(d)) => {
                    // A second delivery of a page this pull already holds: the first claim
                    // covers it, and writing a second would replace the first's pop receipt.
                    src.queue.delete(&msg).await?;
                    report.tally(Event::Stale);
                }
                Verdict::Use(ledger) => {
                    claimed.extend(doc.clone());
                    let now = Utc::now();
                    let claim = Claim {
                        v: VERSION,
                        queue: msg.queue.clone(),
                        message_id: msg.id.clone(),
                        pop_receipt: msg.pop_receipt.clone(),
                        dequeue_count: msg.dequeue_count,
                        text: msg.text.clone(),
                        claimed_at: now.to_rfc3339_opts(SecondsFormat::Micros, true),
                        lease_until: (now + chrono::Duration::from_std(opts.lease)?)
                            .to_rfc3339_opts(SecondsFormat::Micros, true),
                        ledger,
                        ended: None,
                    };
                    let doc_id = claim.message()?.key()?.doc_id();
                    crate::atomic(
                        &out.join(format!("{doc_id}.json")),
                        &serde_json::to_vec_pretty(&claim)?,
                    )?;
                    report.claims.push(claim);
                    report.tally(Event::Claimed);
                }
                Verdict::Stale => {
                    src.queue.delete(&msg).await?;
                    report.tally(Event::Stale);
                }
                Verdict::Skip(_) => {
                    send_back(src.queue, &msg).await?;
                    sent_back.insert(msg.text.clone());
                    report.tally(Event::Released);
                }
            }
        }
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Event {
    Received,
    Claimed,
    Released,
    Stale,
}

impl Report {
    /// Count `event` on the queue being read and in the run's totals.
    fn tally(&mut self, event: Event) {
        let queue = self.queues.last_mut().expect("a queue is being read");
        match event {
            Event::Received => {
                self.received += 1;
                queue.received += 1;
            }
            Event::Claimed => queue.claimed += 1,
            Event::Released => {
                self.released += 1;
                queue.released += 1;
            }
            Event::Stale => {
                self.stale += 1;
                queue.stale += 1;
            }
        }
    }
}

/// Put a message back for other receivers ([`Queue::release`]: a copy goes to
/// the back of the queue first, then the original goes; Azure and pgmq alike).
async fn send_back(queue: &dyn Queue, msg: &Received) -> anyhow::Result<()> {
    match queue.release(msg).await {
        Ok(()) => Ok(()),
        // Someone else holds or deleted the original; the copy stands.
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => Ok(()),
        Err(e) => Err(e),
    }
}

/// What `finish` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    /// The job did not come off the queue (an explicit cohort).
    NotQueueFed,
    NoClaim,
    /// The claim was ended already.
    Already,
    Deleted,
    /// The lease had run out and the message was read again by someone else.
    LeaseLost,
}

async fn load(handoff: &dyn ObjectStore, doc_id: &str) -> anyhow::Result<Option<Claim>> {
    handoff
        .get(&path(doc_id))
        .await?
        .map(|b| Claim::load(&b))
        .transpose()
}

async fn save(handoff: &dyn ObjectStore, doc_id: &str, claim: &Claim) -> anyhow::Result<()> {
    handoff
        .put(
            &path(doc_id),
            serde_json::to_vec_pretty(claim)?,
            "application/json",
        )
        .await?;
    Ok(())
}

/// After `collect` has published a queue-fed job's page, delete its message.
/// The claim is read back and must be this job's message; a failed delete
/// leaves the message to come back, and the ledger's import makes a repeat
/// harmless.
pub async fn finish(
    handoff: &dyn ObjectStore,
    queue: &dyn Queue,
    record: &crate::Record,
) -> anyhow::Result<Ended> {
    if record.ledger.is_none() {
        return Ok(Ended::NotQueueFed);
    }
    ensure!(
        record.stage == crate::Stage::Published,
        "job {} is {:?}, not published; its message stays hidden",
        record.job.id,
        record.stage
    );
    let doc_id = record.job.message.key()?.doc_id();
    let Some(mut claim) = load(handoff, &doc_id).await? else {
        return Ok(Ended::NoClaim);
    };
    if claim.ended.is_some() {
        return Ok(Ended::Already);
    }
    let m = claim.message()?;
    let j = &record.job.message;
    ensure!(
        m.page == j.page
            && m.batch == j.batch
            && m.reason == j.reason
            && m.language == j.language
            && m.queued_at == j.queued_at,
        "the claim for {doc_id} is not job {}'s message",
        record.job.id
    );
    let ended = match queue.delete(&claim.received()).await {
        Ok(()) => Ended::Deleted,
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => Ended::LeaseLost,
        Err(e) => return Err(e),
    };
    claim.ended = Some(
        match ended {
            Ended::Deleted => "finished",
            _ => "lease_lost",
        }
        .into(),
    );
    save(handoff, &doc_id, &claim).await?;
    Ok(ended)
}

/// Give a claimed page back to the queue, for the nodes: its scan turned
/// out not to be on the scan server after all, or the cohort was abandoned.
pub async fn release(
    handoff: &dyn ObjectStore,
    queue: &dyn Queue,
    doc_id: &str,
) -> anyhow::Result<bool> {
    let Some(mut claim) = load(handoff, doc_id).await? else {
        bail!("no claim for {doc_id}");
    };
    if claim.ended.is_some() {
        return Ok(false);
    }
    send_back(queue, &claim.received()).await?;
    claim.ended = Some("released".into());
    save(handoff, doc_id, &claim).await?;
    Ok(true)
}

/// What `dead_letter` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadLettered {
    /// A dead letter went to `ocr-deadletter` and the message was deleted.
    Done,
    /// The claim was ended already: nothing to do.
    Already,
    /// The dead letter went, but the message had come back to its queue (the lease ran out):
    /// it is there twice now, and an operator decides which stays.
    LeaseLost,
}

/// A page whose BOINC workunit failed for good (#1154): put its message on `ocr-deadletter`
/// as a v2 `permanent` dead letter, then delete it with the claim's pop receipt, so the page
/// is not taken again. A release would not do: the copy is the same job, with the same
/// workunit name, which BOINC already has. The dead letter goes first, so a failed delete
/// leaves the page on both queues rather than on neither.
pub async fn dead_letter(
    handoff: &dyn ObjectStore,
    queue: &dyn Queue,
    doc_id: &str,
    error: &str,
    failed: Option<&OcrMessage>,
) -> anyhow::Result<DeadLettered> {
    let Some(mut claim) = load(handoff, doc_id).await? else {
        bail!("no claim for {doc_id}");
    };
    // The claim must be the failed job's message: a page claimed again after a failure has
    // a newer claim, which this must not take off its queue.
    if let Some(m) = failed {
        let c = claim.message()?;
        ensure!(
            c.page == m.page
                && c.batch == m.batch
                && c.reason == m.reason
                && c.language == m.language
                && c.queued_at == m.queued_at,
            "the claim for {doc_id} is not the failed job's message"
        );
    }
    match claim.ended.as_deref() {
        Some("dead_lettered") => return Ok(DeadLettered::Already),
        // The dead letter went but its message had come back: still the operator's to settle.
        Some("dead_letter_lease_lost") => return Ok(DeadLettered::LeaseLost),
        // Released or lost: the message isn't held, so it can't be taken off its queue here.
        Some(other) => {
            bail!("the claim for {doc_id} ended {other}; its message isn't held to dead-letter")
        }
        None => {}
    }
    let msg = claim.message()?;
    let letter = DeadLetter::new(
        &claim.queue,
        &claim.message_id,
        Some(&msg),
        Cause::Permanent,
        None,
        None,
        error,
        claim.dequeue_count,
        "usnm-boinc",
    );
    letter
        .validate()
        .map_err(|e| anyhow::anyhow!("{e} for {doc_id}"))?;
    queue.send(DEADLETTER, &letter.encode()).await?;
    let done = match queue.delete(&claim.received()).await {
        Ok(()) => DeadLettered::Done,
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => DeadLettered::LeaseLost,
        Err(e) => return Err(e),
    };
    claim.ended = Some(
        match done {
            DeadLettered::LeaseLost => "dead_letter_lease_lost",
            _ => "dead_lettered",
        }
        .into(),
    );
    save(handoff, doc_id, &claim).await?;
    Ok(done)
}

/// What [`dead_letter_superseding`] found, and what it did once confirmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Superseding {
    pub doc_id: String,
    /// The newer claim's queue, message id and ledger attempt.
    pub queue: String,
    pub message_id: String,
    pub attempt: u32,
    /// The newer message's `queued_at`.
    pub queued_at: String,
    /// The sha256 of the newer claim's queue text, as `claim pull`'s summary and the
    /// controller's state name it: the operator's confirmation must give it.
    pub message_sha256: String,
    /// `None` for a dry run: nothing was sent or deleted.
    pub done: Option<DeadLettered>,
}

/// The receipt of a superseding dead letter, at
/// `dispatch/jobs/<failed job>/superseding/<message digest>.json`, the digest being the
/// sha256 of the message's JSON as the ledger records its dead letter; `.sent` beside it holds
/// the sha256 of the dead letter's text once it went.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SupersedingReceipt {
    v: u32,
    failed_job: String,
    doc_id: String,
    message_sha256: String,
    letter: DeadLetter,
}

/// A page whose job was dead-lettered and whose newer message was claimed again (#1417): the
/// enqueuer queued the next attempt with a new `queued_at`, `claim pull` overwrote the old
/// claim with it, and the controller stops on a page claimed twice. [`dead_letter`] refuses
/// the newer claim, as it should for the controller's own step. This is the operator's: it
/// dead-letters the newer claim as `permanent`, naming the failed job, then deletes its
/// message, so the page isn't claimed again and the controller's tick goes on.
///
/// It checks that the claim is the failed job's page (page, batch and language), newer than
/// the failed job's message, still live, and the page's outstanding ledger row; and that the
/// ledger's latest outcome for the page is the failed job's own `permanent` dead letter, or
/// the one this sent for an attempt queued after it. With
/// no `confirm` it changes nothing (a dry run). `confirm` must be the newer claim's
/// `message_sha256`. The dead letter is written as a receipt first and sent once, so a rerun
/// neither sends it twice nor takes another message off its queue.
pub async fn dead_letter_superseding(
    handoff: &dyn ObjectStore,
    queue: &dyn Queue,
    failed: &crate::Record,
    ledger: &Ledger,
    confirm: Option<&str>,
) -> anyhow::Result<Superseding> {
    failed.job.validate()?;
    crate::retry::validate(failed)?;
    let id = &failed.job.id;
    ensure!(
        failed.stage == crate::Stage::Failed && failed.ledger.is_some() && failed.retry.is_none(),
        "job {id} is not a failed queue-fed job"
    );
    let old = &failed.job.message;
    let doc_id = old.key()?.doc_id();
    let Some(mut claim) = load(handoff, &doc_id).await? else {
        bail!("no claim for {doc_id}");
    };
    let new = claim.message()?;
    ensure!(
        new.page == old.page && new.batch == old.batch && new.language == old.language,
        "the claim for {doc_id} is not job {id}'s page"
    );
    let at = |m: &OcrMessage| chrono::DateTime::parse_from_rfc3339(&m.queued_at);
    ensure!(
        at(&new)? > at(old)?,
        "the claim for {doc_id} doesn't supersede job {id}: it is that job's message or an older one"
    );
    let message_sha256 = usnm_ocr_contract::boinc::sha256(claim.text.as_bytes());
    if let Some(confirm) = confirm {
        ensure!(
            confirm == message_sha256,
            "the confirmation names message {confirm}, but the claim for {doc_id} is {message_sha256}"
        );
    }
    let mut report = Superseding {
        doc_id: doc_id.clone(),
        queue: claim.queue.clone(),
        message_id: claim.message_id.clone(),
        attempt: claim.ledger.attempt,
        queued_at: new.queued_at.clone(),
        message_sha256: message_sha256.clone(),
        done: None,
    };
    // Receipts are named by the message's digest as the ledger records its dead letter, so a
    // later attempt can find the one this sent for the attempt before it.
    let digest = |m: &OcrMessage| -> anyhow::Result<String> {
        Ok(usnm_ocr_contract::boinc::sha256(&serde_json::to_vec(m)?))
    };
    let receipt_path = |digest: &str| format!("dispatch/jobs/{id}/superseding/{digest}.json");
    let path = receipt_path(&digest(&new)?);
    let sent = format!("{path}.sent");
    let was_sent = handoff.get(&sent).await?;
    match claim.ended.as_deref() {
        None => {}
        Some(ended @ ("dead_lettered" | "dead_letter_lease_lost")) if was_sent.is_some() => {
            report.done = Some(if ended == "dead_lettered" {
                DeadLettered::Already
            } else {
                DeadLettered::LeaseLost
            });
            return Ok(report);
        }
        Some(other) => {
            bail!("the claim for {doc_id} ended {other}; its message isn't held to dead-letter")
        }
    }
    // Once its dead letter went, the ledger may have settled the newer message too; only the
    // delete is left, and the receipt holds what was checked.
    if was_sent.is_none() {
        // The latest outcome is the failed job's own dead letter, or the one this sent for an
        // attempt queued after it, so each later attempt can be settled the same way.
        let settled = ledger
            .outcomes
            .get(&doc_id)
            .filter(|o| {
                o.outcome == usnm_ocr_local::ledger::DEAD_LETTERED
                    && o.cause.as_deref() == Some("permanent")
            })
            .and_then(|o| o.message_sha256.as_deref());
        let failed_digest = digest(old)?;
        let ours = match settled {
            Some(s) if s == failed_digest => true,
            Some(s) if usnm_ocr_contract::boinc::is_sha256(s) => {
                handoff.exists(&format!("{}.sent", receipt_path(s))).await?
            }
            _ => false,
        };
        ensure!(
            ours,
            "job {id} is not dead-lettered: the ledger's latest outcome for {doc_id} isn't its permanent dead letter or one this sent for a later attempt"
        );
        ensure!(
            ledger
                .outstanding(&doc_id)
                .is_some_and(|row| LedgerRow::from_queued(row) == claim.ledger),
            "the claim for {doc_id} isn't the page's outstanding ledger row; leave it to `claim pull`"
        );
    }
    if confirm.is_none() {
        return Ok(report);
    }
    let error = format!(
        "superseded failed BOINC job {}: {}",
        &id[..12],
        failed
            .error
            .as_deref()
            .unwrap_or("BOINC workunit failed for good")
    );
    let receipt = SupersedingReceipt {
        v: 1,
        failed_job: id.clone(),
        doc_id: doc_id.clone(),
        message_sha256: message_sha256.clone(),
        letter: DeadLetter::new(
            &claim.queue,
            &claim.message_id,
            Some(&new),
            Cause::Permanent,
            None,
            None,
            &error,
            claim.dequeue_count,
            "usnm-boinc",
        ),
    };
    receipt
        .letter
        .validate()
        .map_err(|e| anyhow::anyhow!("{e} for {doc_id}"))?;
    let letter = if handoff
        .put_new(
            &path,
            serde_json::to_vec_pretty(&receipt)?,
            "application/json",
        )
        .await?
    {
        receipt.letter
    } else {
        let saved: SupersedingReceipt = serde_json::from_slice(
            &handoff
                .get(&path)
                .await?
                .context("superseding receipt vanished")?,
        )?;
        ensure!(
            saved.v == receipt.v
                && saved.failed_job == receipt.failed_job
                && saved.doc_id == receipt.doc_id
                && saved.message_sha256 == receipt.message_sha256
                && saved.letter.message == receipt.letter.message,
            "{path} names another message"
        );
        saved.letter
    };
    let text = letter.encode();
    let digest = usnm_ocr_contract::boinc::sha256(text.as_bytes()).into_bytes();
    if let Some(receipt) = was_sent {
        ensure!(receipt == digest, "{sent} changed");
    } else {
        queue.send(DEADLETTER, &text).await?;
        if !handoff.put_new(&sent, digest.clone(), "text/plain").await? {
            ensure!(
                handoff.get(&sent).await?.as_ref() == Some(&digest),
                "{sent} differs"
            );
        }
    }
    let done = match queue.delete(&claim.received()).await {
        Ok(()) => DeadLettered::Done,
        Err(e) if e.downcast_ref::<LeaseLost>().is_some() => DeadLettered::LeaseLost,
        Err(e) => return Err(e),
    };
    claim.ended = Some(
        match done {
            DeadLettered::LeaseLost => "dead_letter_lease_lost",
            _ => "dead_lettered",
        }
        .into(),
    );
    save(handoff, &doc_id, &claim).await?;
    report.done = Some(done);
    Ok(report)
}

/// The claim file for `job`'s page in a local directory (the hand-off job
/// downloads the selected pages' claims there before `export`).
pub fn read_local(dir: &Path, doc_id: &str) -> anyhow::Result<Claim> {
    let bytes = crate::read(&dir.join(format!("{doc_id}.json")), 64 * 1024)
        .with_context(|| format!("no claim for {doc_id}"))?;
    Claim::load(&bytes)
}
