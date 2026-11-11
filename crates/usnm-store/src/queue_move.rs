//! Move messages from one work queue to another (#1367): Azure Storage Queues to pgmq at the
//! on-prem cutover.
//!
//! Each message is received with a visibility timeout, its exact text sent to the target, and
//! deleted from the source only after the send has returned (pgmq's send is one committed
//! statement). A crash between the two leaves the message in both queues: delivery is at least
//! once. Every body is hashed with SHA-256 so duplicates can be found: the report counts bodies
//! seen more than once in a run, and its digest is the sum of every moved body's hash modulo
//! 2^256, which does not depend on order and adds up across runs, so it can be compared with
//! the same sum over the target's messages.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::queue::{LeaseLost, Queue};

/// The SHA-256 of a message's text.
pub fn body_sha256(text: &str) -> [u8; 32] {
    Sha256::digest(text.as_bytes()).into()
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// A multiset of body hashes: their sum modulo 2^256, and how often each was seen.
#[derive(Debug, Default, Clone)]
pub struct BodyDigest {
    sum: [u8; 32],
    seen: HashMap<[u8; 32], u64>,
}

impl BodyDigest {
    pub fn add(&mut self, hash: [u8; 32]) {
        let mut carry = 0u16;
        for i in (0..32).rev() {
            let s = u16::from(self.sum[i]) + u16::from(hash[i]) + carry;
            self.sum[i] = (s & 0xff) as u8;
            carry = s >> 8;
        }
        *self.seen.entry(hash).or_default() += 1;
    }

    /// The sum, as 64 hex digits.
    pub fn sum_hex(&self) -> String {
        hex(&self.sum)
    }

    /// Distinct bodies.
    pub fn distinct(&self) -> u64 {
        self.seen.len() as u64
    }

    /// Bodies seen more than once, counting each extra copy.
    pub fn duplicates(&self) -> u64 {
        self.seen.values().map(|n| n - 1).sum()
    }
}

/// What happened to one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    /// Sent to the target and deleted from the source.
    Moved,
    /// Sent to the target, but the source's lease had gone by the delete (the visibility
    /// timeout ran out, or another receiver took it): it will be moved again, a duplicate.
    SentLeaseLost,
    /// Sent to the target, but the source's delete failed (a timeout or a server error): the
    /// move stops here, and the message may be sent again by the next run.
    SentDeleteFailed,
    /// The send returned an error. It may still have committed (a connection lost before the
    /// reply), so the message is logged; it is not counted as sent, and the source keeps it.
    SendFailed,
    /// Read with a peek (dry run); nothing sent or deleted.
    Peeked,
}

/// One message's line in the hash log.
#[derive(Debug, Clone, Serialize)]
pub struct Record {
    pub source_id: Option<String>,
    pub dequeue_count: Option<u32>,
    pub sha256: String,
    pub outcome: Outcome,
}

/// How to move.
#[derive(Debug, Clone)]
pub struct Options {
    /// Stop after this many messages received.
    pub max: Option<u64>,
    /// How long a received message stays hidden at the source.
    pub visibility: Duration,
    /// Messages in flight at once.
    pub concurrency: usize,
}

/// What a move (or a dry run) did.
#[derive(Debug, Clone, Default, Serialize)]
pub struct Report {
    pub dry_run: bool,
    pub received: u64,
    pub moved: u64,
    /// Sent but not deleted from the source: these will arrive at the target again.
    pub sent_lease_lost: u64,
    /// Received with a dequeue count above one: some receiver had taken them before.
    pub previously_received: u64,
    pub peeked: u64,
    pub distinct_bodies: u64,
    pub duplicate_bodies: u64,
    /// Sum of SHA-256(body) over the messages counted, modulo 2^256, in hex.
    pub body_digest: String,
    pub source_count_before: Option<u64>,
    /// Approximate messages left at the source.
    pub source_count_after: Option<u64>,
    pub target_count_before: Option<u64>,
    pub target_count_after: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Default)]
struct Tally {
    received: u64,
    moved: u64,
    sent_lease_lost: u64,
    previously_received: u64,
    digest: BodyDigest,
}

async fn count(q: &dyn Queue, name: &str, progress: &(dyn Fn(&str) + Sync)) -> Option<u64> {
    match q.approximate_count(name).await {
        Ok(n) => Some(n),
        Err(e) => {
            progress(&format!("could not count {name}: {e:#}"));
            None
        }
    }
}

/// Move messages from `from_queue` on `from` to `to_queue` on `to`. An error stops the move;
/// the report then carries it, with the counts so far.
pub async fn move_messages(
    from: &dyn Queue,
    from_queue: &str,
    to: &dyn Queue,
    to_queue: &str,
    opts: &Options,
    progress: &(dyn Fn(&str) + Sync),
    record: &(dyn Fn(&Record) + Sync),
) -> Report {
    let source_count_before = count(from, from_queue, progress).await;
    let target_count_before = count(to, to_queue, progress).await;
    let tally = Mutex::new(Tally::default());
    let taken = AtomicU64::new(0);
    let stop = AtomicBool::new(false);
    let first_error: Mutex<Option<String>> = Mutex::new(None);
    let worker = || async {
        let result: anyhow::Result<()> = async {
            loop {
                if stop.load(Ordering::SeqCst) {
                    return Ok(());
                }
                if let Some(max) = opts.max {
                    if taken.fetch_add(1, Ordering::SeqCst) >= max {
                        return Ok(());
                    }
                }
                let Some(msg) = from.receive(from_queue, opts.visibility).await? else {
                    return Ok(());
                };
                let hash = body_sha256(&msg.text);
                {
                    let mut t = tally.lock().expect("tally");
                    t.received += 1;
                    if msg.dequeue_count > 1 {
                        t.previously_received += 1;
                    }
                }
                if let Err(e) = to.send(to_queue, &msg.text).await {
                    record(&Record {
                        source_id: Some(msg.id.clone()),
                        dequeue_count: Some(msg.dequeue_count),
                        sha256: hex(&hash),
                        outcome: Outcome::SendFailed,
                    });
                    return Err(e.context(format!("sending {} to {to_queue}", msg.id)));
                }
                let outcome = match from.delete(&msg).await {
                    Ok(()) => Outcome::Moved,
                    Err(e) if e.downcast_ref::<LeaseLost>().is_some() => Outcome::SentLeaseLost,
                    Err(e) => {
                        // Sent but not deleted: it may come again, so it is counted and logged
                        // like a lost lease before the error stops the move.
                        {
                            let mut t = tally.lock().expect("tally");
                            t.sent_lease_lost += 1;
                            t.digest.add(hash);
                        }
                        record(&Record {
                            source_id: Some(msg.id.clone()),
                            dequeue_count: Some(msg.dequeue_count),
                            sha256: hex(&hash),
                            outcome: Outcome::SentDeleteFailed,
                        });
                        return Err(e.context(format!("deleting {} from {from_queue}", msg.id)));
                    }
                };
                let done = {
                    let mut t = tally.lock().expect("tally");
                    match outcome {
                        Outcome::Moved => t.moved += 1,
                        _ => t.sent_lease_lost += 1,
                    }
                    t.digest.add(hash);
                    t.moved + t.sent_lease_lost
                };
                record(&Record {
                    source_id: Some(msg.id.clone()),
                    dequeue_count: Some(msg.dequeue_count),
                    sha256: hex(&hash),
                    outcome,
                });
                if done % 500 == 0 {
                    progress(&format!("{from_queue} -> {to_queue}: {done} sent"));
                }
            }
        }
        .await;
        if let Err(e) = result {
            stop.store(true, Ordering::SeqCst);
            first_error
                .lock()
                .expect("error")
                .get_or_insert_with(|| format!("{e:#}"));
        }
    };
    futures::future::join_all((0..opts.concurrency.max(1)).map(|_| worker())).await;
    let t = tally.into_inner().expect("tally");
    let source_count_after = count(from, from_queue, progress).await;
    let mut error = first_error.into_inner().expect("error");
    // An empty receive means nothing is visible, not that nothing is left: messages hidden
    // under another receiver's lease stay behind. Unless --max stopped the move, a source that
    // still counts messages makes it incomplete.
    let stopped_at_max = opts.max.is_some_and(|max| t.received >= max);
    if error.is_none() && !stopped_at_max {
        match source_count_after {
            Some(0) => {}
            Some(n) => {
                error = Some(format!(
                    "{from_queue} still holds about {n} messages (hidden under other leases, or \
                     sent again); run again once they are visible"
                ))
            }
            None => error = Some(format!("could not count {from_queue} after the move")),
        }
    }
    let report = Report {
        dry_run: false,
        received: t.received,
        moved: t.moved,
        sent_lease_lost: t.sent_lease_lost,
        previously_received: t.previously_received,
        peeked: 0,
        distinct_bodies: t.digest.distinct(),
        duplicate_bodies: t.digest.duplicates(),
        body_digest: t.digest.sum_hex(),
        source_count_before,
        source_count_after,
        target_count_before,
        target_count_after: count(to, to_queue, progress).await,
        error,
    };
    progress(&format!(
        "{from_queue} -> {to_queue}: {} moved, {} sent with the lease lost",
        report.moved, report.sent_lease_lost
    ));
    report
}

/// A dry run: count both queues and hash up to `n` (1 to 32) messages at the front of the
/// source with a peek, which neither takes nor hides them.
pub async fn peek_messages(
    from: &dyn Queue,
    from_queue: &str,
    to: &dyn Queue,
    to_queue: &str,
    n: u32,
    progress: &(dyn Fn(&str) + Sync),
    record: &(dyn Fn(&Record) + Sync),
) -> Report {
    let mut report = Report {
        dry_run: true,
        source_count_before: count(from, from_queue, progress).await,
        target_count_before: count(to, to_queue, progress).await,
        ..Report::default()
    };
    let mut digest = BodyDigest::default();
    match from.peek(from_queue, n.clamp(1, 32)).await {
        Ok(texts) => {
            for text in texts {
                let hash = body_sha256(&text);
                digest.add(hash);
                report.peeked += 1;
                record(&Record {
                    source_id: None,
                    dequeue_count: None,
                    sha256: hex(&hash),
                    outcome: Outcome::Peeked,
                });
            }
        }
        Err(e) => report.error = Some(format!("{e:#}")),
    }
    report.distinct_bodies = digest.distinct();
    report.duplicate_bodies = digest.duplicates();
    report.body_digest = digest.sum_hex();
    report.source_count_after = report.source_count_before;
    report.target_count_after = report.target_count_before;
    progress(&format!("{from_queue}: {} peeked", report.peeked));
    report
}

/// An Azure queue URL (`https://{account}.queue.core.windows.net/{queue}`) as the account's
/// endpoint and the queue's name.
pub fn split_azure_queue(url: &str) -> anyhow::Result<(String, String)> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("queue URL `{url}`"))?;
    ensure!(
        parsed.query().is_none() && parsed.fragment().is_none(),
        "the queue URL must not carry a query (SAS tokens are not used)"
    );
    let segments: Vec<&str> = parsed
        .path_segments()
        .map(|s| s.filter(|p| !p.is_empty()).collect())
        .unwrap_or_default();
    let [queue] = segments.as_slice() else {
        bail!("the queue URL names one queue: https://<account>.queue.core.windows.net/<queue>");
    };
    Ok((parsed.origin().ascii_serialization(), (*queue).to_owned()))
}

/// A pgmq location with its queue (`postgres://…/usnm_queue?user=…&queue=ocr-bulk`) as the
/// location without `queue` and the queue's name.
pub fn split_pgmq_queue(location: &str) -> anyhow::Result<(String, String)> {
    let mut url = reqwest::Url::parse(location).context("pgmq location")?;
    ensure!(
        matches!(url.scheme(), "postgres" | "postgresql"),
        "the target is a postgres:// location"
    );
    let mut queue = None;
    let mut rest = Vec::new();
    for (k, v) in url.query_pairs() {
        if k == "queue" {
            ensure!(queue.is_none(), "`queue` is given twice");
            queue = Some(v.into_owned());
        } else {
            rest.push((k.into_owned(), v.into_owned()));
        }
    }
    let queue = queue.context("name the pgmq queue with ?queue=<name>, e.g. queue=ocr-bulk")?;
    crate::pgmq::pg_name(&queue)?;
    url.set_query(None);
    if !rest.is_empty() {
        url.query_pairs_mut().extend_pairs(rest);
    }
    crate::pgmq::Location::parse(url.as_str())?;
    Ok((url.to_string(), queue))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::queue::LocalQueue;

    fn quiet(_: &str) {}
    fn no_record(_: &Record) {}

    fn opts(max: Option<u64>, concurrency: usize) -> Options {
        Options {
            max,
            visibility: Duration::from_secs(60),
            concurrency,
        }
    }

    async fn filled(n: usize) -> (tempfile::TempDir, LocalQueue, Vec<String>) {
        let dir = tempfile::tempdir().unwrap();
        let q = LocalQueue::new(dir.path().join("src"));
        let mut texts = Vec::new();
        for i in 0..n {
            let t = format!("{{\"doc_id\":\"p{i}\",\"z\":1,\"a\":\"ü\"}}");
            q.send("ocr-bulk", &t).await.unwrap();
            texts.push(t);
        }
        (dir, q, texts)
    }

    async fn drain(q: &LocalQueue, name: &str) -> Vec<String> {
        let mut out = Vec::new();
        while let Some(m) = q.receive(name, Duration::from_secs(60)).await.unwrap() {
            out.push(m.text.clone());
            q.delete(&m).await.unwrap();
        }
        out.sort();
        out
    }

    #[tokio::test]
    async fn moves_identical_bodies_and_empties_the_source() {
        let (dir, from, mut texts) = filled(23).await;
        let to = LocalQueue::new(dir.path().join("dst"));
        let records = Mutex::new(Vec::new());
        let log = |r: &Record| records.lock().unwrap().push(r.clone());
        let r = move_messages(
            &from,
            "ocr-bulk",
            &to,
            "ocr-bulk",
            &opts(None, 4),
            &quiet,
            &log,
        )
        .await;
        assert_eq!(r.error, None);
        assert_eq!((r.received, r.moved, r.sent_lease_lost), (23, 23, 0));
        assert_eq!((r.distinct_bodies, r.duplicate_bodies), (23, 0));
        assert_eq!(
            (r.source_count_before, r.source_count_after),
            (Some(23), Some(0))
        );
        assert_eq!(
            (r.target_count_before, r.target_count_after),
            (Some(0), Some(23))
        );
        texts.sort();
        assert_eq!(drain(&to, "ocr-bulk").await, texts);
        // The digest is the order-free sum of the bodies' hashes.
        let mut expect = BodyDigest::default();
        for t in texts.iter().rev() {
            expect.add(body_sha256(t));
        }
        assert_eq!(r.body_digest, expect.sum_hex());
        let records = records.lock().unwrap();
        assert_eq!(records.len(), 23);
        assert!(records.iter().all(|r| r.outcome == Outcome::Moved));
    }

    #[tokio::test]
    async fn max_stops_early_and_digests_add_up_across_runs() {
        let (dir, from, _) = filled(10).await;
        let to = LocalQueue::new(dir.path().join("dst"));
        let first = move_messages(
            &from,
            "ocr-bulk",
            &to,
            "ocr-bulk",
            &opts(Some(4), 3),
            &quiet,
            &no_record,
        )
        .await;
        assert_eq!(first.moved, 4);
        assert_eq!(from.approximate_count("ocr-bulk").await.unwrap(), 6);
        let rest = move_messages(
            &from,
            "ocr-bulk",
            &to,
            "ocr-bulk",
            &opts(None, 2),
            &quiet,
            &no_record,
        )
        .await;
        assert_eq!(rest.moved, 6);
        // One run over everything gives the sum of the two.
        let (dir2, all_from, _) = filled(10).await;
        let all_to = LocalQueue::new(dir2.path().join("dst"));
        let all = move_messages(
            &all_from,
            "ocr-bulk",
            &all_to,
            "ocr-bulk",
            &opts(None, 1),
            &quiet,
            &no_record,
        )
        .await;
        let mut sum = BodyDigest::default();
        for d in [&first.body_digest, &rest.body_digest] {
            let bytes: Vec<u8> = (0..32)
                .map(|i| u8::from_str_radix(&d[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            sum.add(bytes.try_into().unwrap());
        }
        assert_eq!(sum.sum_hex(), all.body_digest);
    }

    #[tokio::test]
    async fn duplicate_bodies_are_counted() {
        let dir = tempfile::tempdir().unwrap();
        let from = LocalQueue::new(dir.path().join("src"));
        for t in ["a", "b", "a", "a"] {
            from.send("ocr-priority", t).await.unwrap();
        }
        let to = LocalQueue::new(dir.path().join("dst"));
        let r = move_messages(
            &from,
            "ocr-priority",
            &to,
            "ocr-priority",
            &opts(None, 1),
            &quiet,
            &no_record,
        )
        .await;
        assert_eq!((r.moved, r.distinct_bodies, r.duplicate_bodies), (4, 2, 2));
    }

    /// A target whose sends fail: nothing is deleted from the source.
    #[derive(Debug)]
    struct Broken;

    #[async_trait::async_trait]
    impl Queue for Broken {
        async fn receive(
            &self,
            _: &str,
            _: Duration,
        ) -> anyhow::Result<Option<crate::queue::Received>> {
            Ok(None)
        }
        async fn delete(&self, _: &crate::queue::Received) -> anyhow::Result<()> {
            Ok(())
        }
        async fn send(&self, _: &str, _: &str) -> anyhow::Result<()> {
            bail!("connection refused")
        }
        async fn approximate_count(&self, _: &str) -> anyhow::Result<u64> {
            bail!("no count")
        }
    }

    #[tokio::test]
    async fn a_failed_send_deletes_nothing_and_stops() {
        let (_dir, from, _) = filled(5).await;
        let records = Mutex::new(Vec::new());
        let log = |r: &Record| records.lock().unwrap().push(r.clone());
        let r = move_messages(
            &from,
            "ocr-bulk",
            &Broken,
            "ocr-bulk",
            &opts(None, 1),
            &quiet,
            &log,
        )
        .await;
        assert!(
            r.error.as_deref().unwrap().contains("connection refused"),
            "{r:?}"
        );
        assert_eq!(r.moved, 0);
        assert_eq!(r.target_count_before, None);
        // Received messages stay at the source (hidden until their timeout).
        assert_eq!(from.approximate_count("ocr-bulk").await.unwrap(), 5);
        // The attempt is logged in case the send committed before its reply was lost.
        let records = records.into_inner().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, Outcome::SendFailed);
        assert!(records[0].source_id.is_some());
    }

    #[tokio::test]
    async fn hidden_messages_left_behind_make_the_move_incomplete() {
        let (dir, from, _) = filled(3).await;
        // Another receiver holds one under a long lease.
        let held = from
            .receive("ocr-bulk", Duration::from_secs(3600))
            .await
            .unwrap()
            .unwrap();
        let to = LocalQueue::new(dir.path().join("dst"));
        let r = move_messages(
            &from,
            "ocr-bulk",
            &to,
            "ocr-bulk",
            &opts(None, 1),
            &quiet,
            &no_record,
        )
        .await;
        assert_eq!(r.moved, 2);
        assert_eq!(r.source_count_after, Some(1));
        assert!(
            r.error.as_deref().unwrap().contains("still holds about 1"),
            "{r:?}"
        );
        // A move stopped by --max is partial on purpose.
        from.delete(&held).await.unwrap();
        for t in ["x", "y"] {
            from.send("ocr-bulk", t).await.unwrap();
        }
        let r = move_messages(
            &from,
            "ocr-bulk",
            &to,
            "ocr-bulk",
            &opts(Some(1), 1),
            &quiet,
            &no_record,
        )
        .await;
        assert_eq!((r.moved, r.error), (1, None));
    }

    /// A source whose deletes fail after the send: the record still names the message.
    #[derive(Debug)]
    struct NoDelete(LocalQueue);

    #[async_trait::async_trait]
    impl Queue for NoDelete {
        async fn receive(
            &self,
            q: &str,
            v: Duration,
        ) -> anyhow::Result<Option<crate::queue::Received>> {
            self.0.receive(q, v).await
        }
        async fn delete(&self, _: &crate::queue::Received) -> anyhow::Result<()> {
            bail!("HTTP 500")
        }
        async fn send(&self, q: &str, t: &str) -> anyhow::Result<()> {
            self.0.send(q, t).await
        }
        async fn approximate_count(&self, q: &str) -> anyhow::Result<u64> {
            self.0.approximate_count(q).await
        }
    }

    #[tokio::test]
    async fn a_failed_delete_after_the_send_is_logged() {
        let (dir, from, _) = filled(1).await;
        let from = NoDelete(from);
        let to = LocalQueue::new(dir.path().join("dst"));
        let records = Mutex::new(Vec::new());
        let log = |r: &Record| records.lock().unwrap().push(r.clone());
        let r = move_messages(
            &from,
            "ocr-bulk",
            &to,
            "ocr-bulk",
            &opts(None, 1),
            &quiet,
            &log,
        )
        .await;
        assert!(r.error.as_deref().unwrap().contains("HTTP 500"), "{r:?}");
        assert_eq!((r.moved, r.sent_lease_lost), (0, 1));
        let records = records.into_inner().unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].outcome, Outcome::SentDeleteFailed);
        assert!(records[0].source_id.is_some());
        assert_eq!(to.approximate_count("ocr-bulk").await.unwrap(), 1);
    }

    #[tokio::test]
    async fn a_dry_run_peeks_without_taking() {
        let (dir, from, _) = filled(40).await;
        let to = LocalQueue::new(dir.path().join("dst"));
        let r = peek_messages(&from, "ocr-bulk", &to, "ocr-bulk", 100, &quiet, &no_record).await;
        assert!(r.dry_run);
        assert_eq!((r.peeked, r.received, r.moved), (32, 0, 0));
        assert_eq!(r.source_count_before, Some(40));
        assert_eq!(from.approximate_count("ocr-bulk").await.unwrap(), 40);
        assert_eq!(to.approximate_count("ocr-bulk").await.unwrap(), 0);
        // Nothing is hidden either: all 40 can be received right away.
        assert_eq!(drain(&from, "ocr-bulk").await.len(), 40);
    }

    #[test]
    fn splits_the_queue_urls() {
        assert_eq!(
            split_azure_queue("https://stusnm.queue.core.windows.net/ocr-bulk").unwrap(),
            (
                "https://stusnm.queue.core.windows.net".to_owned(),
                "ocr-bulk".to_owned()
            )
        );
        for bad in [
            "https://stusnm.queue.core.windows.net",
            "https://stusnm.queue.core.windows.net/a/b",
            "https://stusnm.queue.core.windows.net/ocr-bulk?sv=2020&sig=x",
        ] {
            assert!(split_azure_queue(bad).is_err(), "{bad}");
        }
        let (loc, queue) = split_pgmq_queue(
            "postgres://usnm-queue.example.test/usnm_queue?user=ingest_producer&queue=ocr-bulk&sslcert=/c/a.crt&sslkey=/c/a.key&sslrootcert=/c/ca.pem",
        )
        .unwrap();
        assert_eq!(queue, "ocr-bulk");
        assert!(
            !loc.contains("queue=") && loc.contains("sslrootcert="),
            "{loc}"
        );
        crate::pgmq::Location::parse(&loc).unwrap();
        for bad in [
            "postgres://h/usnm_queue?user=u&sslcert=a&sslkey=b&sslrootcert=c",
            "postgres://h/usnm_queue?user=u&sslcert=a&sslkey=b&sslrootcert=c&queue=Bad%20Name",
            "postgres://h/usnm_queue?user=u&sslcert=a&sslkey=b&sslrootcert=c&queue=a&queue=b",
            "postgres://u:pw@h/usnm_queue?sslcert=a&sslkey=b&sslrootcert=c&queue=ocr-bulk",
            "https://h/usnm_queue?queue=ocr-bulk",
        ] {
            assert!(split_pgmq_queue(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_digest_sum_wraps() {
        let mut d = BodyDigest::default();
        d.add([0xff; 32]);
        d.add({
            let mut one = [0u8; 32];
            one[31] = 1;
            one
        });
        assert_eq!(d.sum_hex(), "0".repeat(64));
    }

    /// From a local queue into pgmq's `ocr_deadletter` on a real database (CI's queue job):
    /// bodies arrive byte for byte.
    #[tokio::test]
    async fn queue_move_into_pgmq_live() {
        let location = std::env::var("USNM_TEST_PGMQ").ok();
        assert!(
            location.is_some() || std::env::var("USNM_TEST_PGMQ_REQUIRED").is_err(),
            "USNM_TEST_PGMQ_REQUIRED is set but USNM_TEST_PGMQ isn't"
        );
        let Some(location) = location else {
            return;
        };
        let dir = tempfile::tempdir().unwrap();
        let from = LocalQueue::new(dir.path());
        let tag = format!("queue-move test {}", crate::queue::random_token());
        let texts: Vec<String> = (0..5)
            .map(|i| format!("{{\"z\":{i},\"a\":\"{tag} ü\"}}"))
            .collect();
        for t in &texts {
            from.send("ocr-deadletter", t).await.unwrap();
        }
        let to = crate::pgmq::PgmqQueue::new(&location).unwrap();
        let r = move_messages(
            &from,
            "ocr-deadletter",
            &to,
            "ocr-deadletter",
            &opts(None, 2),
            &quiet,
            &no_record,
        )
        .await;
        assert_eq!(r.error, None);
        assert_eq!(r.moved, 5);
        assert_eq!(from.approximate_count("ocr-deadletter").await.unwrap(), 0);
        // Read the target's rows directly rather than receiving: a receive would hide other
        // live tests' messages on the same queue, and theirs could take these.
        let (config, tls, _) = crate::pgmq::connection_options(&location).unwrap();
        let (client, connection) = config.connect(tls).await.unwrap();
        let task = tokio::spawn(connection);
        let pattern = format!("%{tag}%");
        let rows = client
            .query(
                "SELECT message FROM pgmq.q_ocr_deadletter WHERE message::text LIKE $1",
                &[&pattern],
            )
            .await
            .unwrap();
        let mut got: Vec<String> = rows
            .iter()
            .map(|r| {
                r.get::<_, serde_json::Value>(0)
                    .as_str()
                    .unwrap()
                    .to_owned()
            })
            .collect();
        got.sort();
        let mut want = texts.clone();
        want.sort();
        assert_eq!(got, want);
        client
            .execute(
                "DELETE FROM pgmq.q_ocr_deadletter WHERE message::text LIKE $1",
                &[&pattern],
            )
            .await
            .unwrap();
        task.abort();
    }
}
