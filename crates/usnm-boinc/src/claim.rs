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
    collections::{BTreeSet, HashMap, HashSet},
    path::Path,
    time::Duration,
};
use usnm_ingest::ocr_local::{boinc::Snapshot, ledger::Ledger};
use usnm_ocr_contract::{
    fetched,
    message::{self, OcrMessage},
    prefetch,
};
use usnm_ocr_worker::engine;
use usnm_store::{
    queue::{LeaseLost, Queue, Received, PRIORITY},
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
    /// `finished`, `released` or `lease_lost` once the message is gone.
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
    /// The pages of the cohort's prefetch manifest that are on `fetched`, by (batch, doc
    /// id), with the batch version the manifest has: `export` builds a job only from a
    /// manifest row, so a page outside it is left for the nodes rather than failing the
    /// export ([`manifest_pages`]). `None` skips the check.
    pub manifest: Option<&'a ManifestPages>,
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

/// A prefetch manifest's pages, by (batch, doc id), with their batch versions.
pub type ManifestPages = HashMap<(String, String), u16>;

/// The pages of a prefetch manifest (`manifest.parquet`) that are on `fetched`. Only
/// those are kept, so a manifest of millions of pages costs as much as the fetched list.
pub fn manifest_pages(
    bytes: bytes::Bytes,
    fetched: &fetched::List,
) -> anyhow::Result<ManifestPages> {
    let mut pages = HashMap::new();
    prefetch::decode_where(bytes, |row| {
        if fetched.contains(&row.batch, &row.doc_id) {
            pages.insert((row.batch.clone(), row.doc_id.clone()), row.batch_version);
        }
        false
    })?;
    Ok(pages)
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
    if let Some(manifest) = src.manifest {
        match manifest.get(&(msg.batch.clone(), doc_id.clone())) {
            None => return Verdict::Skip("not_in_manifest"),
            Some(version) if *version != row.batch_version => {
                return Verdict::Skip("manifest_batch_version")
            }
            Some(_) => {}
        }
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

/// The claim file for `job`'s page in a local directory (the hand-off job
/// downloads the selected pages' claims there before `export`).
pub fn read_local(dir: &Path, doc_id: &str) -> anyhow::Result<Claim> {
    let bytes = crate::read(&dir.join(format!("{doc_id}.json")), 64 * 1024)
        .with_context(|| format!("no claim for {doc_id}"))?;
    Claim::load(&bytes)
}
