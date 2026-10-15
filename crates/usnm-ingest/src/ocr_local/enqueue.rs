//! `usnm-ingest ocr-enqueue` and `ocr-requeue` (12 §12.5 Step 8): queue
//! chosen pages for the pilot before the seeder exists, and queue dead-
//! lettered pages again by their cause. The operator has no queue data
//! role, so both run as executions of the ingest job, as `id-usnm-ingest`.
//!
//! Each writes a ledger row for every message before it puts it, since a
//! message with no ledger row would have its part refused as not asked
//! for. If a put fails, the pages after it get a `not_sent` outcome, so
//! they aren't outstanding and can be queued again; the page whose put
//! failed may have gone out and stays outstanding (`--include-outstanding`,
//! or the seeder's timeout, Step 10). Outside a dry run both hold the
//! `ocr-sync` lock, as the seeder does, so its window count never races
//! them.

use super::ledger::{Outcome, NOT_SENT};
use super::sync::Lease;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use anyhow::{bail, Context};
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use tokio::time::Instant;
use usnm_core::ids::PageKey;
use usnm_ocr_contract::message::{DeadLetter, OcrMessage, Reason};
use usnm_state::state::{RunBatch, State};
use usnm_store::queue::{Queue, BULK, PRIORITY};
use usnm_store::ObjectStore;

use super::ledger::{self, Ledger, Queued, DEAD_LETTERED};
use super::{run_id, ROOT};
use crate::ocr_damage::{self, StoredScore};

/// The most pages one `ocr-enqueue` or `ocr-requeue` queues.
pub const MAX_PAGES: usize = 10_000;

#[cfg(test)]
mod cache_tests {
    use super::*;

    #[derive(Debug)]
    struct StalledStore;

    #[async_trait::async_trait]
    impl ObjectStore for StalledStore {
        async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
            std::future::pending().await
        }
        async fn put_new(
            &self,
            _: &str,
            _: Vec<u8>,
            _: &str,
        ) -> Result<bool, usnm_store::StoreError> {
            panic!("deadline during lookup must not write queued rows");
        }
        async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), usnm_store::StoreError> {
            panic!("deadline during lookup must not write");
        }
        async fn list(&self, _: &str) -> Result<Vec<String>, usnm_store::StoreError> {
            std::future::pending().await
        }
    }

    #[tokio::test]
    async fn deadline_during_lookup_pauses_without_creating_outstanding_rows() {
        let state = State::new(Arc::new(usnm_state::docs::MemoryDocs::default()));
        state.set_current_version("v1").await.unwrap();
        let queue: Arc<dyn Queue> = Arc::new(usnm_store::queue::LocalQueue::new(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/deadline-unused"),
        ));
        let report = enqueue_with_policy(
            &state,
            &StalledStore,
            &StalledStore,
            &queue,
            &EnqueueOptions {
                pages: vec!["sn83045462_1890-05-03_ed-1_seq-7".into()],
                reason: Reason::User,
                plan: "hit-results".into(),
                batch: None,
                include_outstanding: false,
                dry_run: false,
            },
            None,
            QueuePolicy {
                deadline: Some(Instant::now() + std::time::Duration::from_millis(10)),
                ..QueuePolicy::default()
            },
        )
        .await
        .unwrap();
        assert!(report.paused);
        assert_eq!(report.queued, 0);
        assert!(report.ledger.is_none());
    }

    #[derive(Debug, Default)]
    struct CountingStore {
        objects: std::sync::Mutex<HashMap<String, Vec<u8>>>,
        reads: std::sync::Mutex<HashMap<String, usize>>,
        stall_b: std::sync::atomic::AtomicBool,
    }

    #[async_trait::async_trait]
    impl ObjectStore for CountingStore {
        async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
            *self.reads.lock().unwrap().entry(path.into()).or_default() += 1;
            if path == "b" && self.stall_b.load(std::sync::atomic::Ordering::SeqCst) {
                std::future::pending::<()>().await;
            }
            Ok(self.objects.lock().unwrap().get(path).cloned())
        }
        async fn put_new(
            &self,
            path: &str,
            body: Vec<u8>,
            _: &str,
        ) -> Result<bool, usnm_store::StoreError> {
            let mut objects = self.objects.lock().unwrap();
            if objects.contains_key(path) {
                return Ok(false);
            }
            objects.insert(path.into(), body);
            Ok(true)
        }
        async fn put(
            &self,
            path: &str,
            body: Vec<u8>,
            _: &str,
        ) -> Result<(), usnm_store::StoreError> {
            self.objects.lock().unwrap().insert(path.into(), body);
            Ok(())
        }
        async fn list(&self, _: &str) -> Result<Vec<String>, usnm_store::StoreError> {
            Ok(Vec::new())
        }
    }

    #[tokio::test]
    async fn tables_are_scanned_once_and_partial_lookup_progress_survives_deadline() {
        let day = NaiveDate::from_ymd_opt(1890, 5, 3).unwrap();
        let mut rows = Vec::new();
        for seq in 1..=200 {
            let key = PageKey::new("sn83045462", day, 1, seq).unwrap();
            let row = crate::curated::CuratedRow::from_ocr(
                key,
                "the newspaper and the people",
                "batch",
                1,
                "loc",
                Utc::now(),
            );
            rows.push(ocr_damage::score_row(&row, &["eng"]));
        }
        let store = CountingStore::default();
        rows[0].batch = "nbu_a".into();
        store
            .put("a", ocr_damage::encode_table(&rows[..1]).unwrap(), "x")
            .await
            .unwrap();
        for row in &mut rows {
            row.batch = "nbu_b".into();
        }
        store
            .put("b", ocr_damage::encode_table(&rows).unwrap(), "x")
            .await
            .unwrap();
        let batches: Vec<RunBatch> = ["a", "b"]
            .into_iter()
            .map(|batch| RunBatch {
                batch: batch.into(),
                curated: usnm_state::state::Curated {
                    version: 1,
                    parts: Vec::new(),
                    counts: String::new(),
                    pages: 200,
                    ok_pages: 200,
                    lccns: vec!["sn83045462".into()],
                    source_sha256: String::new(),
                    first: String::new(),
                    last: String::new(),
                },
            })
            .collect();
        store
            .put(
                "v1/batches.json",
                serde_json::to_vec(&batches).unwrap(),
                "x",
            )
            .await
            .unwrap();
        store
            .put(
                "v1/titles.json",
                serde_json::to_vec(&serde_json::json!([
                    {"lccn":"sn83045462","languages":["eng"]}
                ]))
                .unwrap(),
                "x",
            )
            .await
            .unwrap();
        store
            .put(
                &ocr_damage::versions_path("v1"),
                serde_json::to_vec(&serde_json::json!({
                    "metric_version": usnm_core::damage::VERSION,
                    "index_version": "v1",
                    "batches": [
                        {"batch":"a","version":1,"table":"a","summary":""},
                        {"batch":"b","version":1,"table":"b","summary":""}
                    ]
                }))
                .unwrap(),
                "x",
            )
            .await
            .unwrap();
        let state = State::new(Arc::new(usnm_state::docs::MemoryDocs::default()));
        state.set_current_version("v1").await.unwrap();
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../target")
            .join(format!("test-request-checkpoint-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let queue: Arc<dyn Queue> = Arc::new(usnm_store::queue::LocalQueue::new(&root));
        let options = EnqueueOptions {
            pages: rows[..10].iter().map(|row| row.doc_id.clone()).collect(),
            reason: Reason::User,
            plan: "hit-results".into(),
            batch: None,
            include_outstanding: false,
            dry_run: false,
        };
        let policy = QueuePolicy {
            request_day: Some(day),
            deadline: Some(Instant::now() + std::time::Duration::from_millis(100)),
            ..QueuePolicy::default()
        };
        store
            .stall_b
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let paused = enqueue_with_policy(&state, &store, &store, &queue, &options, None, policy)
            .await
            .unwrap();
        assert!(paused.paused);
        assert_eq!(paused.queued, 0);
        let checkpoint = format!("{ROOT}/requests/{day}/lookup.json");
        let saved = store.get(&checkpoint).await.unwrap().unwrap();
        let cache: ScoreCache = serde_json::from_slice(&saved).unwrap();
        assert_eq!(cache.next_batch, 1);
        assert_eq!(
            cache.scores.len(),
            1,
            "resolved candidate was persisted before timeout"
        );
        store
            .stall_b
            .store(false, std::sync::atomic::Ordering::SeqCst);
        let resumed = enqueue_with_policy(
            &state,
            &store,
            &store,
            &queue,
            &options,
            None,
            QueuePolicy {
                deadline: None,
                ..policy
            },
        )
        .await
        .unwrap();
        assert_eq!(
            resumed.queued, 10,
            "previously resolved candidates survive the deadline: {resumed:?}"
        );
        for row in &rows[..10] {
            let message = queue
                .receive(PRIORITY, std::time::Duration::from_secs(60))
                .await
                .unwrap()
                .unwrap();
            let (_, key) = usnm_ocr_contract::message::decode(&message.text).unwrap();
            assert_eq!(
                key.doc_id(),
                row.doc_id,
                "original shuffled enqueue order is preserved"
            );
        }
        let cache: ScoreCache =
            serde_json::from_slice(&store.get(&checkpoint).await.unwrap().unwrap()).unwrap();
        assert_eq!(
            cache.scores.len(),
            10,
            "unrequested scores are never retained"
        );
        assert_eq!(
            cache.scores[&rows[0].doc_id].batch, "nbu_a",
            "first published batch wins"
        );
        assert_eq!(cache.next_batch, 2);
        let reads = store.reads.lock().unwrap();
        assert_eq!(
            reads["a"], 1,
            "completed table is not reread after resuming"
        );
        assert_eq!(
            reads["b"], 2,
            "one canceled read and one completed read for all nine pages"
        );
        drop(reads);
        let _ = std::fs::remove_dir_all(root);
    }
}

pub fn queue_for(reason: Reason) -> &'static str {
    match reason {
        Reason::User => PRIORITY,
        Reason::Bulk => BULK,
    }
}

/// What a run queued, or would have.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize)]
pub struct QueueReport {
    pub queued: usize,
    /// Pages left out, by why.
    pub skipped: BTreeMap<String, usize>,
    pub ledger: Option<String>,
    pub paused: bool,
}

pub(super) struct SendContext<'a> {
    pub(super) lease: Option<&'a Lease>,
    pub(super) run: &'a str,
    pub(super) now: DateTime<Utc>,
    pub(super) private_pages: bool,
    pub(super) deadline: Option<Instant>,
}

pub(super) struct HitRequestBatch {
    pub pages: Vec<String>,
    pub day: chrono::NaiveDate,
    pub max_queued: usize,
    pub deadline: Option<Instant>,
}

pub(super) fn skip(r: &mut QueueReport, why: &str, doc_id: &str) {
    tracing::info!(doc_id, why, "page not queued");
    *r.skipped.entry(why.to_owned()).or_default() += 1;
}

fn skip_page(r: &mut QueueReport, why: &str, doc_id: &str, private: bool) {
    if private {
        tracing::info!(why, "user-requested page not queued");
        *r.skipped.entry(why.to_owned()).or_default() += 1;
    } else {
        skip(r, why, doc_id);
    }
}

/// The message language for a page: its detected language in ISO 639-3,
/// or its title's first language for `und` (the seeder's rule, Step 10).
pub fn message_language(detected: Option<&str>, title_languages: &[String]) -> Option<String> {
    let iso = |marc: &str| usnm_core::lang::marc_to_iso3(marc).map(str::to_owned);
    detected
        .filter(|l| *l != usnm_core::damage::UNKNOWN)
        .and_then(iso)
        .or_else(|| title_languages.first().and_then(|l| iso(l)))
}

/// Record pages whose ledger rows are written but whose messages were
/// never put, so they aren't outstanding; returns how many.
async fn record_not_sent(
    curated: &dyn ObjectStore,
    run: &str,
    rest: &[(Queued, OcrMessage)],
) -> usize {
    let at = Utc::now();
    let rows: Vec<Outcome> = rest
        .iter()
        .map(|(q, _)| Outcome {
            doc_id: q.doc_id.clone(),
            outcome: NOT_SENT.to_owned(),
            cause: None,
            kind: None,
            source: run.to_owned(),
            at,
            run: run.to_owned(),
        })
        .collect();
    if let Err(w) = ledger::write_outcomes(curated, &rows, at, run).await {
        tracing::error!(
            error = %format!("{w:#}"),
            pages = rows.len(),
            "could not record the pages not sent; they stay outstanding until they time out"
        );
    }
    rows.len()
}

/// Send messages after writing their ledger rows, checking the lock (when
/// held) before the write and before each put.
pub(super) async fn send_all(
    curated: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    context: SendContext<'_>,
    batch: Vec<(Queued, OcrMessage)>,
    report: &mut QueueReport,
) -> anyhow::Result<()> {
    if context.deadline.is_some_and(|d| Instant::now() >= d) {
        report.paused = true;
        return Ok(());
    }
    if let Some(l) = context.lease {
        l.check()?;
    }
    let rows: Vec<Queued> = batch.iter().map(|(q, _)| q.clone()).collect();
    report.ledger = ledger::write_queued(curated, &rows, context.now, context.run).await?;
    for (i, (q, m)) in batch.iter().enumerate() {
        if context.deadline.is_some_and(|d| Instant::now() >= d) {
            record_not_sent(curated, context.run, &batch[i..]).await;
            report.paused = true;
            return Ok(());
        }
        if let Some(Err(e)) = context.lease.map(Lease::check) {
            let n = record_not_sent(curated, context.run, &batch[i..]).await;
            return Err(e.context(format!("{n} pages recorded as not sent")));
        }
        let encoded = m.encode();
        let send = queue.send(queue_for(m.reason), &encoded);
        let send = if let Some(deadline) = context.deadline {
            match tokio::time::timeout_at(deadline, send).await {
                Ok(result) => result,
                Err(_) => {
                    // A timed-out send is ambiguous; only later messages are
                    // confirmed unsent and may safely release their cap charge.
                    record_not_sent(curated, context.run, &batch[i + 1..]).await;
                    report.paused = true;
                    return Ok(());
                }
            }
        } else {
            send.await
        };
        if let Err(e) = send {
            // The pages after this one never went out: not outstanding.
            let n = record_not_sent(curated, context.run, &batch[i + 1..]).await;
            let page = if context.private_pages {
                "a user-requested page"
            } else {
                q.doc_id.as_str()
            };
            return Err(e.context(format!(
                "queueing {page} ({n} pages after it are recorded as not sent; this one's ledger row \
                 is written and may have gone out: --include-outstanding queues it again)"
            )));
        }
        report.queued += 1;
    }
    Ok(())
}

/// Find a page's copy in the published version: the first batch holding
/// its title whose page table (or curated text) has the page.
async fn find_page(
    curated: &dyn ObjectStore,
    batches: &[RunBatch],
    tables: &Option<HashMap<String, String>>,
    cache: &mut ScoreCache,
    titles: &HashMap<String, Vec<String>>,
    key: &PageKey,
    only: Option<&str>,
) -> anyhow::Result<Option<StoredScore>> {
    let doc_id = key.doc_id();
    while !cache.scores.contains_key(&doc_id) && cache.next_batch < batches.len() {
        let b = &batches[cache.next_batch];
        let wanted: HashSet<String> = cache
            .wanted
            .iter()
            .filter(|id| !cache.scores.contains_key(*id))
            .filter(|id| {
                PageKey::from_doc_id(id).is_ok_and(|key| b.curated.lccns.contains(&key.lccn))
            })
            .cloned()
            .collect();
        let found = if only.is_some_and(|o| o != b.batch) || wanted.is_empty() {
            HashMap::new()
        } else {
            match tables.as_ref().and_then(|t| t.get(&b.batch)) {
                Some(path) => {
                    let bytes = curated
                        .get(path)
                        .await?
                        .with_context(|| format!("damage table {path} not found"))?;
                    ocr_damage::read_scores(Bytes::from(bytes), Some(&wanted))?
                }
                None => super::sync::score_from_curated(curated, b, titles, &wanted).await?,
            }
        };
        cache.scores.extend(found);
        cache.next_batch += 1;
        cache.save(curated).await?;
    }
    Ok(cache.scores.get(&doc_id).cloned())
}

#[derive(Serialize, Deserialize)]
struct ScoreCache {
    version: String,
    wanted: HashSet<String>,
    next_batch: usize,
    scores: HashMap<String, StoredScore>,
    #[serde(skip)]
    checkpoint: Option<String>,
}

impl ScoreCache {
    fn new(pages: impl IntoIterator<Item = String>) -> Self {
        Self {
            version: String::new(),
            wanted: pages.into_iter().collect(),
            next_batch: 0,
            scores: HashMap::new(),
            checkpoint: None,
        }
    }

    async fn save(&self, curated: &dyn ObjectStore) -> anyhow::Result<()> {
        if let Some(path) = &self.checkpoint {
            curated
                .put(path, serde_json::to_vec(self)?, "application/json")
                .await?;
        }
        Ok(())
    }
}

/// `ocr-enqueue`'s options.
#[derive(Debug, Clone)]
pub struct EnqueueOptions {
    /// Page doc_ids (`sn83045462_1890-05-03_ed-1_seq-7`).
    pub pages: Vec<String>,
    pub reason: Reason,
    /// Recorded in the ledger as the plan.
    pub plan: String,
    /// Only look for the pages in this base batch.
    pub batch: Option<String>,
    /// Queue pages the ledger has outstanding too: to recover from a run
    /// that wrote its ledger rows but failed to put some messages.
    pub include_outstanding: bool,
    pub dry_run: bool,
}

/// Take the `ocr-sync` lock, unless it is a dry run.
async fn take(state: &State, owner: &str, dry_run: bool) -> anyhow::Result<Option<Lease>> {
    Ok(if dry_run {
        None
    } else {
        Some(Lease::take(state, owner).await?)
    })
}

/// Give the lock back, if held, and return the run's result.
async fn release<T>(
    state: &State,
    owner: &str,
    lease: Option<Lease>,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    let Some(lease) = lease else {
        return result;
    };
    drop(lease);
    let unlocked = state.unlock(super::LOCK, owner).await;
    let out = result?;
    unlocked?;
    Ok(out)
}

/// Queue chosen pages.
pub async fn enqueue(
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    opts: &EnqueueOptions,
) -> anyhow::Result<QueueReport> {
    let owner = crate::owner_id();
    let lease = take(state, &owner, opts.dry_run).await?;
    let result = enqueue_locked(state, curated, reference, queue, opts, lease.as_ref()).await;
    release(state, &owner, lease, result).await
}

async fn enqueue_locked(
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    opts: &EnqueueOptions,
    lease: Option<&Lease>,
) -> anyhow::Result<QueueReport> {
    enqueue_with_policy(
        state,
        curated,
        reference,
        queue,
        opts,
        lease,
        QueuePolicy::default(),
    )
    .await
}

#[derive(Debug, Clone, Copy, Default)]
struct QueuePolicy {
    message_queued_at: Option<DateTime<Utc>>,
    request_day: Option<NaiveDate>,
    min_damage: Option<f64>,
    max_queued: Option<usize>,
    private_pages: bool,
    deadline: Option<Instant>,
}

/// Queue pages from a closed daily hit-request file while `ocr-sync` holds
/// the shared lock. Only page ids enter this path; logs omit those ids.
pub(super) async fn enqueue_hit_requests(
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    lease: &Lease,
    requests: HitRequestBatch,
) -> anyhow::Result<QueueReport> {
    let opts = EnqueueOptions {
        pages: requests.pages,
        reason: Reason::User,
        plan: "hit-results".into(),
        batch: None,
        include_outstanding: false,
        dry_run: false,
    };
    enqueue_with_policy(
        state,
        curated,
        reference,
        queue,
        &opts,
        Some(lease),
        QueuePolicy {
            message_queued_at: requests.day.and_hms_opt(0, 0, 0).map(|t| t.and_utc()),
            request_day: Some(requests.day),
            min_damage: Some(0.1),
            max_queued: Some(requests.max_queued),
            private_pages: true,
            deadline: requests.deadline,
        },
    )
    .await
}

async fn enqueue_with_policy(
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    opts: &EnqueueOptions,
    lease: Option<&Lease>,
    policy: QueuePolicy,
) -> anyhow::Result<QueueReport> {
    if opts.pages.len() > MAX_PAGES {
        bail!(
            "{} pages; queue at most {MAX_PAGES} at a time",
            opts.pages.len()
        );
    }
    if !usnm_store::is_safe_segment(&opts.plan) {
        bail!("--plan must be letters, digits, `.`, `_` or `-`");
    }
    let mut report = QueueReport::default();
    let prepare = async {
        let version = state
            .current_version()
            .await?
            .context("no published version to find the pages in")?;
        let batches = ocr_damage::version_batches(reference, &version, &[]).await?;
        let titles = ocr_damage::title_languages(reference, &version).await?;
        let tables = match curated.get(&ocr_damage::versions_path(&version)).await? {
            Some(b) => {
                let list: ocr_damage::VersionList = serde_json::from_slice(&b)?;
                Some(
                    list.batches
                        .into_iter()
                        .map(|b| (b.batch, b.table))
                        .collect(),
                )
            }
            None => None,
        };
        let ledger = ledger::load(curated).await?;
        let mut cache = ScoreCache::new(opts.pages.iter().cloned());
        if let Some(day) = policy.request_day {
            let path = format!("{ROOT}/requests/{day}/lookup.json");
            if let Some(bytes) = curated.get(&path).await? {
                let saved: ScoreCache =
                    serde_json::from_slice(&bytes).context("reading request lookup checkpoint")?;
                if saved.version == version && saved.wanted == cache.wanted {
                    anyhow::ensure!(
                        saved.scores.len() <= MAX_PAGES && saved.next_batch <= batches.len(),
                        "invalid request lookup checkpoint"
                    );
                    cache = saved;
                }
            }
            cache.checkpoint = Some(path);
        }
        cache.version = version;
        Ok::<_, anyhow::Error>((batches, titles, tables, ledger, cache))
    };
    let (batches, titles, tables, ledger, mut table_cache) = if let Some(deadline) = policy.deadline
    {
        match tokio::time::timeout_at(deadline, prepare).await {
            Ok(prepared) => prepared?,
            Err(_) => {
                report.paused = true;
                return Ok(report);
            }
        }
    } else {
        prepare.await?
    };
    let now = Utc::now();
    let message_queued_at = policy.message_queued_at.unwrap_or(now);
    let run = run_id(now);
    let mut out = Vec::new();
    let mut seen = HashSet::new();
    for doc_id in &opts.pages {
        if policy.deadline.is_some_and(|d| Instant::now() >= d) {
            report.paused = true;
            return Ok(report);
        }
        if !seen.insert(doc_id.clone()) {
            continue;
        }
        let Ok(key) = PageKey::from_doc_id(doc_id) else {
            skip_page(&mut report, "bad_doc_id", doc_id, policy.private_pages);
            continue;
        };
        if !opts.include_outstanding && ledger.outstanding(doc_id).is_some() {
            skip_page(&mut report, "outstanding", doc_id, policy.private_pages);
            continue;
        }
        if policy.private_pages && ledger.imported(doc_id) {
            skip_page(&mut report, "already_ocr", doc_id, true);
            continue;
        }
        let langs = titles.get(&key.lccn).cloned().unwrap_or_default();
        if langs.iter().any(|l| l == "jpn") {
            skip_page(&mut report, "japanese_title", doc_id, policy.private_pages);
            continue;
        }
        let lookup = find_page(
            curated,
            &batches,
            &tables,
            &mut table_cache,
            &titles,
            &key,
            opts.batch.as_deref(),
        );
        let found = if let Some(deadline) = policy.deadline {
            match tokio::time::timeout_at(deadline, lookup).await {
                Ok(found) => found?,
                Err(_) => {
                    report.paused = true;
                    return Ok(report);
                }
            }
        } else {
            lookup.await?
        };
        let Some(found) = found else {
            skip_page(&mut report, "not_published", doc_id, policy.private_pages);
            continue;
        };
        if policy
            .min_damage
            .is_some_and(|min| !found.damage_rate.is_some_and(|damage| damage >= min))
        {
            skip_page(&mut report, "not_damaged", doc_id, policy.private_pages);
            continue;
        }
        let Some(language) = message_language(found.lang.as_deref(), &langs) else {
            skip_page(&mut report, "no_language", doc_id, policy.private_pages);
            continue;
        };
        let msg = OcrMessage {
            v: 1,
            page: key.to_string(),
            batch: found.batch.clone(),
            language: language.clone(),
            reason: opts.reason,
            score: found.damage_rate.filter(|s| (0.0..=1.0).contains(s)),
            queued_at: message_queued_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        };
        if let Err(e) = msg.validate() {
            skip_page(&mut report, e.kind(), doc_id, policy.private_pages);
            continue;
        }
        let attempt = ledger
            .attempts
            .get(&(doc_id.clone(), opts.plan.clone()))
            .copied()
            .unwrap_or(0)
            + 1;
        out.push((
            Queued {
                doc_id: doc_id.clone(),
                batch: found.batch,
                batch_version: found.batch_version,
                reason: opts.reason.as_str().to_owned(),
                language,
                plan: opts.plan.clone(),
                attempt,
                request_day: policy.request_day,
                queued_at: now,
            },
            msg,
        ));
        if policy.max_queued.is_some_and(|max| out.len() >= max) {
            break;
        }
    }
    if opts.dry_run {
        report.queued = out.len();
        tracing::info!(would_queue = out.len(), skipped = ?report.skipped, "ocr-enqueue dry run");
        return Ok(report);
    }
    if !out.is_empty() {
        send_all(
            curated,
            queue,
            SendContext {
                lease,
                run: &run,
                now,
                private_pages: policy.private_pages,
                deadline: policy.deadline,
            },
            out,
            &mut report,
        )
        .await?;
    }
    tracing::info!(queued = report.queued, skipped = ?report.skipped, ledger = ?report.ledger, "ocr-enqueue finished");
    Ok(report)
}

/// `ocr-requeue`'s options.
#[derive(Debug, Clone)]
pub struct RequeueOptions {
    /// `dequeue_count`, `invalid`, `permanent` or `unroutable`.
    pub cause: String,
    pub kind: Option<String>,
    /// Only dead letters drained on or after this day (UTC).
    pub since: Option<NaiveDate>,
    pub plan: String,
    pub max_pages: usize,
    pub dry_run: bool,
}

/// Dead letters `ocr-sync` drained, newest last.
pub async fn read_dead_letters(
    curated: &dyn ObjectStore,
    since: Option<NaiveDate>,
) -> anyhow::Result<Vec<DeadLetter>> {
    let mut out = Vec::new();
    for path in curated.list(&format!("{ROOT}/deadletter")).await? {
        let day = path.split('/').nth(2).unwrap_or_default();
        if let (Some(since), Ok(d)) = (since, NaiveDate::parse_from_str(day, "%Y-%m-%d")) {
            if d < since {
                continue;
            }
        }
        let Some(body) = curated.get(&path).await? else {
            continue;
        };
        for line in String::from_utf8_lossy(&body).lines() {
            if let Ok(d) = serde_json::from_str::<DeadLetter>(line) {
                // Written from checked fields; checked again all the same.
                if d.validate().is_ok() {
                    out.push(d);
                }
            }
        }
    }
    Ok(out)
}

/// Queue dead-lettered pages again, by cause and kind.
pub async fn requeue(
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    opts: &RequeueOptions,
) -> anyhow::Result<QueueReport> {
    let owner = crate::owner_id();
    let lease = take(state, &owner, opts.dry_run).await?;
    let result = requeue_locked(state, curated, reference, queue, opts, lease.as_ref()).await;
    release(state, &owner, lease, result).await
}

async fn requeue_locked(
    state: &State,
    curated: &dyn ObjectStore,
    reference: &dyn ObjectStore,
    queue: &Arc<dyn Queue>,
    opts: &RequeueOptions,
    lease: Option<&Lease>,
) -> anyhow::Result<QueueReport> {
    const CAUSES: [&str; 4] = ["dequeue_count", "invalid", "permanent", "unroutable"];
    if !CAUSES.contains(&opts.cause.as_str()) {
        bail!("--cause must be one of {}", CAUSES.join(", "));
    }
    if !usnm_store::is_safe_segment(&opts.plan) {
        bail!("--plan must be letters, digits, `.`, `_` or `-`");
    }
    let max = opts.max_pages.min(MAX_PAGES);
    let version = state
        .current_version()
        .await?
        .context("no published version")?;
    let published: HashMap<String, u16> = ocr_damage::version_batches(reference, &version, &[])
        .await?
        .into_iter()
        .map(|b| (b.batch, b.curated.version))
        .collect();
    let ledger: Ledger = ledger::load(curated).await?;
    let letters = read_dead_letters(curated, opts.since).await?;
    let now = Utc::now();
    let run = run_id(now);
    let mut report = QueueReport::default();
    let mut latest: HashMap<String, OcrMessage> = HashMap::new();
    for d in letters {
        let cause = d.cause.map(|c| c.as_str());
        if cause != Some(opts.cause.as_str()) || (opts.kind.is_some() && d.kind != opts.kind) {
            continue;
        }
        let Some(m) = d.message else { continue };
        let Ok(key) = m.key() else { continue };
        latest.insert(key.doc_id(), m);
    }
    let mut out = Vec::new();
    let mut ids: Vec<String> = latest.keys().cloned().collect();
    ids.sort();
    for doc_id in ids {
        if out.len() >= max {
            skip(&mut report, "over_max_pages", &doc_id);
            continue;
        }
        let old = &latest[&doc_id];
        // Only pages whose latest result is still the dead letter.
        let last = ledger.outcomes.get(&doc_id);
        let queued_since = ledger
            .queued
            .get(&doc_id)
            .is_some_and(|q| last.is_none_or(|o| q.queued_at > o.at));
        if queued_since {
            skip(&mut report, "queued_since", &doc_id);
            continue;
        }
        if last.is_some_and(|o| o.outcome != DEAD_LETTERED) {
            skip(&mut report, "imported", &doc_id);
            continue;
        }
        // The page's latest dead letter must be the one asked for: one
        // requeued for `permanent` that then failed as `invalid` isn't
        // `permanent` any more.
        let matches = last.is_none_or(|o| {
            o.cause.as_deref() == Some(opts.cause.as_str())
                && (opts.kind.is_none() || o.kind == opts.kind)
        });
        if !matches {
            skip(&mut report, "latest_cause_differs", &doc_id);
            continue;
        }
        let Some(&batch_version) = published.get(&old.batch) else {
            skip(&mut report, "not_published", &doc_id);
            continue;
        };
        let msg = OcrMessage {
            queued_at: now.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            ..old.clone()
        };
        if let Err(e) = msg.validate() {
            skip(&mut report, e.kind(), &doc_id);
            continue;
        }
        let attempt = ledger
            .attempts
            .get(&(doc_id.clone(), opts.plan.clone()))
            .copied()
            .unwrap_or(0)
            + 1;
        out.push((
            Queued {
                doc_id: doc_id.clone(),
                batch: msg.batch.clone(),
                batch_version,
                reason: msg.reason.as_str().to_owned(),
                language: msg.language.clone(),
                plan: opts.plan.clone(),
                attempt,
                request_day: None,
                queued_at: now,
            },
            msg,
        ));
    }
    if opts.dry_run {
        report.queued = out.len();
        tracing::info!(would_queue = out.len(), skipped = ?report.skipped, "ocr-requeue dry run");
        return Ok(report);
    }
    if !out.is_empty() {
        send_all(
            curated,
            queue,
            SendContext {
                lease,
                run: &run,
                now,
                private_pages: false,
                deadline: None,
            },
            out,
            &mut report,
        )
        .await?;
    }
    tracing::info!(queued = report.queued, skipped = ?report.skipped, ledger = ?report.ledger, "ocr-requeue finished");
    Ok(report)
}
