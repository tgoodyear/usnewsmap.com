//! `ocr-sync`, `ocr-enqueue` and `ocr-requeue` against a local exchange:
//! directories for `ocr-out`, `ocr-quarantine`, the curated and reference
//! stores, a directory queue, and in-memory pipeline state.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use chrono::{DateTime, NaiveDate, Utc};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::message::{Cause, DeadLetter, OcrMessage, Reason};
use usnm_overlay::OverlayRow;
use usnm_state::docs::MemoryDocs;
use usnm_state::state::{Curated, RunBatch, State};
use usnm_store::queue::Received;
use usnm_store::queue::{LocalQueue, Queue, BULK, DEADLETTER, PRIORITY};
use usnm_store::ObjectStore;

use super::checks::{self, Reject};
use super::enqueue::{self, EnqueueOptions, RequeueOptions};
use super::ledger::{self, Queued};
use super::quarantine::{self, Record};
use super::sync::{self, ImportRecord, Options, Status, Stores};
use super::{pages, report, ROOT, STATUS_PATH};
use usnm_overlay::curated::{CuratedRow, PartWriter};

const LCCN: &str = "sn83045462";
const JA_LCCN: &str = "sn00000001";
const BATCH: &str = "nbu_gage";
const NODE: &str = "node1";

const GERMAN: &str = "Der Stadtrath hat am Dienstag beschlossen, dass die neue Schule auf dem Grundstück \
    gebaut werden soll, welches die Behörde im vorigen Jahre gekauft hat. Es ist noch nicht bekannt, wann \
    die Arbeiten beginnen werden, aber man hofft, dass es bald sein wird und dass die Kinder im nächsten \
    Jahre in das neue Haus ziehen können. ";

const ALTO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#">
  <Description><MeasurementUnit>pixel</MeasurementUnit></Description>
  <Layout><Page ID="p1" WIDTH="5000" HEIGHT="7000"><PrintSpace>
    <TextBlock ID="b1"><TextLine ID="l1"><String CONTENT="Der"/></TextLine></TextBlock>
  </PrintSpace></Page></Layout>
</alto>
"#;

fn day() -> NaiveDate {
    NaiveDate::from_ymd_opt(1890, 5, 3).unwrap()
}

fn key(seq: u16) -> PageKey {
    PageKey::new(LCCN, day(), 1, seq).unwrap()
}

/// LoC's text, badly read: the German head words one letter off.
fn damaged() -> String {
    GERMAN
        .replace("die ", "dic ")
        .replace("der ", "dcr ")
        .replace("und ", "nnd ")
        .replace("das ", "bas ")
        .replace("dass ", "baß ")
        .repeat(4)
}

struct Fx {
    _dir: tempfile::TempDir,
    state: State,
    stores: Stores,
    out: Arc<dyn ObjectStore>,
    requests: Arc<dyn ObjectStore>,
    queue: Arc<dyn Queue>,
}

#[derive(Debug)]
struct FailFirstQueue {
    inner: Arc<dyn Queue>,
    failed: std::sync::atomic::AtomicBool,
}

#[derive(Debug)]
struct FailSecondQueue {
    inner: Arc<dyn Queue>,
    attempts: std::sync::atomic::AtomicUsize,
}

#[async_trait]
impl Queue for FailSecondQueue {
    async fn receive(&self, queue: &str, visibility: Duration) -> anyhow::Result<Option<Received>> {
        self.inner.receive(queue, visibility).await
    }

    async fn delete(&self, msg: &Received) -> anyhow::Result<()> {
        self.inner.delete(msg).await
    }

    async fn send(&self, queue: &str, text: &str) -> anyhow::Result<()> {
        if self
            .attempts
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            == 1
        {
            anyhow::bail!("injected second-send failure");
        }
        self.inner.send(queue, text).await
    }

    async fn approximate_count(&self, queue: &str) -> anyhow::Result<u64> {
        self.inner.approximate_count(queue).await
    }
}

#[derive(Debug)]
struct FailWriteStore {
    inner: Arc<dyn ObjectStore>,
    path: &'static str,
    empty_only: bool,
    failed: std::sync::atomic::AtomicBool,
}

impl FailWriteStore {
    fn check(&self, path: &str, body: &[u8]) -> Result<(), usnm_store::StoreError> {
        if path == self.path
            && (!self.empty_only || body.is_empty())
            && !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            return Err(usnm_store::StoreError::Io(
                "injected completion failure".into(),
            ));
        }
        Ok(())
    }
}

#[async_trait]
impl ObjectStore for FailWriteStore {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        self.inner.get(path).await
    }

    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        kind: &str,
    ) -> Result<bool, usnm_store::StoreError> {
        self.check(path, &body)?;
        self.inner.put_new(path, body, kind).await
    }

    async fn put(
        &self,
        path: &str,
        body: Vec<u8>,
        kind: &str,
    ) -> Result<(), usnm_store::StoreError> {
        self.check(path, &body)?;
        self.inner.put(path, body, kind).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        self.inner.list(prefix).await
    }

    async fn list_info(
        &self,
        prefix: &str,
    ) -> Result<Vec<usnm_store::ObjectInfo>, usnm_store::StoreError> {
        self.inner.list_info(prefix).await
    }

    async fn get_if_match(
        &self,
        path: &str,
        etag: &str,
        max: u64,
    ) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        self.inner.get_if_match(path, etag, max).await
    }
}

#[derive(Debug)]
struct StallAfterFirstQueue {
    inner: Arc<dyn Queue>,
    sent: std::sync::atomic::AtomicBool,
}

#[async_trait]
impl Queue for StallAfterFirstQueue {
    async fn receive(&self, queue: &str, visibility: Duration) -> anyhow::Result<Option<Received>> {
        self.inner.receive(queue, visibility).await
    }

    async fn delete(&self, msg: &Received) -> anyhow::Result<()> {
        self.inner.delete(msg).await
    }

    async fn send(&self, queue: &str, text: &str) -> anyhow::Result<()> {
        if self.sent.swap(true, std::sync::atomic::Ordering::SeqCst) {
            std::future::pending::<()>().await;
        }
        self.inner.send(queue, text).await
    }

    async fn approximate_count(&self, queue: &str) -> anyhow::Result<u64> {
        self.inner.approximate_count(queue).await
    }
}

#[async_trait]
impl Queue for FailFirstQueue {
    async fn receive(&self, queue: &str, visibility: Duration) -> anyhow::Result<Option<Received>> {
        self.inner.receive(queue, visibility).await
    }

    async fn delete(&self, msg: &Received) -> anyhow::Result<()> {
        self.inner.delete(msg).await
    }

    async fn send(&self, queue: &str, text: &str) -> anyhow::Result<()> {
        if !self.failed.swap(true, std::sync::atomic::Ordering::SeqCst) {
            anyhow::bail!("injected first-send failure");
        }
        self.inner.send(queue, text).await
    }

    async fn approximate_count(&self, queue: &str) -> anyhow::Result<u64> {
        self.inner.approximate_count(queue).await
    }
}

fn open(dir: &std::path::Path, sub: &str) -> Arc<dyn ObjectStore> {
    usnm_store::open(dir.join(sub).to_str().unwrap()).unwrap()
}

/// Version `v1` with `nbu_gage` (a German title, 9 pages of damaged LoC
/// text) and `dlc_japan` (a Japanese title); `with_tables` runs the damage
/// pass so LoC's scores come from Step 6's tables.
async fn fixture(with_tables: bool) -> Fx {
    fixture_with_batch(with_tables, BATCH).await
}

async fn fixture_with_batch(with_tables: bool, batch: &str) -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let curated = open(dir.path(), "curated");
    let reference = open(dir.path(), "reference");
    let out = open(dir.path(), "out");
    let quarantine = open(dir.path(), "quarantine");
    let requests = open(dir.path(), "requests");
    let queue: Arc<dyn Queue> = Arc::new(LocalQueue::new(dir.path().join("queue")));
    let at = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
    let mut batches = Vec::new();
    for (batch, lccn) in [(batch, LCCN), ("dlc_japan", JA_LCCN)] {
        let mut w = PartWriter::new();
        for seq in 1..=9 {
            let k = PageKey::new(lccn, day(), 1, seq).unwrap();
            assert!(w
                .push(CuratedRow::from_ocr(k, &damaged(), batch, 1, "loc", at))
                .unwrap()
                .is_none());
        }
        let part = if batch.len() + "_ver01".len() <= 128 {
            format!("pages/{batch}_ver01/part-0.parquet")
        } else {
            format!("pages/{batch}/ver01/part-0.parquet")
        };
        curated
            .put_new(&part, w.finish_part().unwrap().unwrap(), "x")
            .await
            .unwrap();
        batches.push(RunBatch {
            batch: batch.into(),
            curated: Curated {
                version: 1,
                parts: vec![part],
                counts: String::new(),
                pages: 9,
                ok_pages: 9,
                lccns: vec![lccn.into()],
                source_sha256: String::new(),
                first: String::new(),
                last: String::new(),
            },
        });
    }
    reference
        .put_new(
            "v1/batches.json",
            serde_json::to_vec(&batches).unwrap(),
            "x",
        )
        .await
        .unwrap();
    reference
        .put_new(
            "v1/titles.json",
            serde_json::to_vec(&serde_json::json!([
                {"lccn": LCCN, "languages": ["ger"]},
                {"lccn": JA_LCCN, "languages": ["jpn", "eng"]},
            ]))
            .unwrap(),
            "x",
        )
        .await
        .unwrap();
    if with_tables {
        crate::ocr_damage::full_pass(
            Arc::clone(&curated),
            reference.as_ref(),
            &crate::ocr_damage::Options {
                index_version: "v1".into(),
                parallel: 1,
                ..Default::default()
            },
        )
        .await
        .unwrap();
    }
    let state = State::new(Arc::new(MemoryDocs::default()));
    state.set_current_version("v1").await.unwrap();
    Fx {
        _dir: dir,
        state,
        stores: Stores {
            curated,
            reference,
            out: Arc::clone(&out),
            quarantine,
            requests: Some(Arc::clone(&requests)),
            queue: Arc::clone(&queue),
        },
        out,
        requests,
        queue,
    }
}

fn queued(doc_id: &str, batch: &str, reason: &str) -> Queued {
    Queued {
        doc_id: doc_id.into(),
        batch: batch.into(),
        batch_version: 1,
        reason: reason.into(),
        language: "deu".into(),
        plan: "test".into(),
        attempt: 1,
        request_day: None,
        retry_message_at: None,
        retry_run: None,
        queued_at: Utc::now() - chrono::Duration::hours(1),
    }
}

impl Fx {
    async fn ask(&self, rows: &[Queued]) {
        ledger::write_queued(
            self.stores.curated.as_ref(),
            rows,
            Utc::now(),
            &super::run_id(Utc::now()),
        )
        .await
        .unwrap();
    }

    /// What a worker writes for a page: its ALTO, then its part.
    async fn node_writes(&self, row: &OverlayRow, alto: &str, stamp: &str) -> (String, String) {
        let k = &row.key;
        let alto_path = format!("alto/{}.{NODE}.{stamp}.xml", k.doc_id());
        let part_path = format!(
            "pages/{}_{}_ed-{}.{NODE}.{stamp}.parquet",
            k.lccn,
            k.date.format("%Y-%m-%d"),
            k.edition
        );
        assert!(self
            .out
            .put_new(&alto_path, alto.as_bytes().to_vec(), "x")
            .await
            .unwrap());
        assert!(self
            .out
            .put_new(
                &part_path,
                usnm_overlay::write_part(std::slice::from_ref(row)).unwrap(),
                "x"
            )
            .await
            .unwrap());
        (alto_path, part_path)
    }

    async fn sync(&self) -> Status {
        sync::run(&self.state, "test", &self.stores, &Options::default())
            .await
            .unwrap()
    }

    async fn out_paths(&self) -> Vec<String> {
        let mut v = self.out.list("pages").await.unwrap();
        v.extend(self.out.list("alto").await.unwrap());
        v
    }

    async fn records(&self) -> Vec<Record> {
        let mut out = Vec::new();
        for p in self.stores.quarantine.list(quarantine::TOP).await.unwrap() {
            if p.ends_with(".reason.json") {
                let b = self.stores.quarantine.get(&p).await.unwrap().unwrap();
                out.push(serde_json::from_slice(&b).unwrap());
            }
        }
        out
    }
}

fn row(seq: u16, text: &str) -> OverlayRow {
    OverlayRow {
        key: key(seq),
        batch: BATCH.into(),
        ocr_source: "usnm-tesseract".into(),
        ocr_engine: "tesseract-5.5.1/deu_latf+deu".into(),
        loc_text: "bulk".into(),
        text: text.into(),
        image_url: "https://tile.loc.gov/image-services/iiif/service:ndnp:nbu:batch_nbu_gage_ver01:data:sn83045462:0001".into(),
        ocred_at: Utc::now(),
    }
}

fn stamp(n: u32) -> String {
    format!("20261011T120000{n:06}Z")
}

#[tokio::test]
async fn imports_a_page_it_asked_for() {
    for with_tables in [true, false] {
        let fx = fixture(with_tables).await;
        let doc = key(3).doc_id();
        fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
        let text = GERMAN.repeat(4);
        let (alto_src, part_src) = fx.node_writes(&row(3, &text), ALTO, &stamp(1)).await;

        let status = fx.sync().await;
        assert_eq!(status.last_run.imported, 1, "{:?}", status.last_run);
        assert!(status.last_run.rejected.is_empty());
        assert!(fx.out_paths().await.is_empty(), "ocr-out emptied");

        let c = &fx.stores.curated;
        let alto = c
            .get(&format!("{ROOT}/alto/{doc}.{NODE}.{}.xml", stamp(1)))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(alto, ALTO.as_bytes());
        let parts = c.list(&format!("{ROOT}/pages/{LCCN}")).await.unwrap();
        assert_eq!(parts.len(), 1);
        let bytes = bytes::Bytes::from(c.get(&parts[0]).await.unwrap().unwrap());
        let stored = pages::read_scores(bytes.clone()).unwrap();
        assert_eq!(stored.len(), 1);
        let s = &stored[0];
        assert_eq!(s.doc_id, doc);
        assert_eq!(s.ocr_reason, "bulk");
        assert_eq!(s.ours.lang.as_deref(), Some("ger"));
        assert_eq!(s.loc.lang.as_deref(), Some("ger"));
        let (ours, loc) = (s.ours.damage_rate.unwrap(), s.loc.damage_rate.unwrap());
        assert!(ours < loc, "ours {ours} < LoC's {loc}");
        // The overlay columns read back as a part; ocred_at is the server's.
        let overlay = usnm_overlay::read_part(bytes).unwrap();
        assert_eq!(overlay[0].text.as_deref(), Some(text.as_str()));
        assert_eq!(overlay[0].loc_text, "bulk");

        let records = c.list(&format!("{ROOT}/import")).await.unwrap();
        assert_eq!(records.len(), 1);
        let rec: ImportRecord =
            serde_json::from_slice(&c.get(&records[0]).await.unwrap().unwrap()).unwrap();
        let mut srcs: Vec<&str> = rec.sources.iter().map(|s| s.path.as_str()).collect();
        srcs.sort();
        assert_eq!(srcs, [alto_src.as_str(), part_src.as_str()]);
        let l = ledger::load(c.as_ref()).await.unwrap();
        assert!(l.imported(&doc) && l.outstanding(&doc).is_none());

        let st: Status =
            serde_json::from_slice(&fx.stores.reference.get(STATUS_PATH).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(
            (st.totals.imported, st.parts_left, st.outstanding),
            (1, 0, 0)
        );

        // The same page again is no longer asked for.
        fx.node_writes(&row(3, &text), ALTO, &stamp(2)).await;
        let again = fx.sync().await;
        assert_eq!(again.last_run.imported, 0);
        assert_eq!(again.last_run.rejected.get("not_requested"), Some(&1));
        assert_eq!(again.totals.imported, 1);
    }
}

#[tokio::test]
async fn loc_reuse_fixture_preserves_baseline_conflicts_missing_coverage_and_rollback() {
    let fx = fixture(false).await;
    let curated = fx.stores.curated.as_ref();
    let baseline_path = format!("pages/{BATCH}_ver01/part-0.parquet");
    let baseline = curated.get(&baseline_path).await.unwrap().unwrap();
    let doc = key(3).doc_id();
    let missing = key(4).doc_id();
    fx.ask(&[queued(&doc, BATCH, "bulk"), queued(&missing, BATCH, "bulk")])
        .await;
    let text = GERMAN.repeat(4);
    let (alto_source, part_source) = fx.node_writes(&row(3, &text), ALTO, &stamp(1)).await;
    let source_alto_bytes = fx.out.get(&alto_source).await.unwrap().unwrap();
    let source_part_bytes = fx.out.get(&part_source).await.unwrap().unwrap();
    assert_eq!(fx.sync().await.last_run.imported, 1);
    let parts = curated.list(&format!("{ROOT}/pages")).await.unwrap();
    let records = curated.list(&format!("{ROOT}/import")).await.unwrap();
    let imported_bytes = curated.get(&parts[0]).await.unwrap().unwrap();
    let record_bytes = curated.get(&records[0]).await.unwrap().unwrap();
    let record: ImportRecord = serde_json::from_slice(&record_bytes).unwrap();
    let listing = super::replace::Listing::list(curated).await.unwrap();
    let loaded = listing
        .load(curated, [LCCN].into_iter(), &[])
        .await
        .unwrap();
    assert_eq!(loaded.pages[&doc].text, text.trim_end());
    assert!(!loaded.pages.contains_key(&missing));
    assert!(ledger::load(curated)
        .await
        .unwrap()
        .outstanding(&missing)
        .is_some());

    for (path, body) in [
        (&alto_source, source_alto_bytes),
        (&part_source, source_part_bytes),
    ] {
        assert!(fx.out.put_new(path, body, "x").await.unwrap());
    }
    let replay = fx.sync().await;
    assert_eq!(replay.last_run.imported, 0);
    assert_eq!(replay.last_run.rejected.get("not_requested"), Some(&1));
    assert_eq!(curated.list(&format!("{ROOT}/pages")).await.unwrap(), parts);
    assert_eq!(
        curated.get(&parts[0]).await.unwrap().unwrap(),
        imported_bytes
    );
    assert_eq!(
        curated.get(&records[0]).await.unwrap().unwrap(),
        record_bytes
    );
    let mut successful_imports = Vec::new();
    let mut quarantine_imports = 0;
    for path in curated.list(&format!("{ROOT}/import")).await.unwrap() {
        let receipt: ImportRecord =
            serde_json::from_slice(&curated.get(&path).await.unwrap().unwrap()).unwrap();
        if receipt
            .sources
            .iter()
            .any(|source| source.outcome == "imported")
        {
            successful_imports.push(path);
        } else {
            assert!(receipt.pages.is_empty());
            assert!(receipt
                .sources
                .iter()
                .all(|source| source.outcome == "quarantined"));
            quarantine_imports += 1;
        }
    }
    assert_eq!(successful_imports, records);
    assert_eq!(quarantine_imports, 1);
    let replayed = super::replace::Listing::list(curated)
        .await
        .unwrap()
        .load(curated, [LCCN].into_iter(), &[])
        .await
        .unwrap();
    assert_eq!(replayed.pages[&doc].text, text.trim_end());
    // A second reading cannot bypass the normal eligibility boundary.
    let conflict = "A conflicting synthetic reading";
    let (_, conflict_path) = fx.node_writes(&row(3, conflict), ALTO, &stamp(2)).await;
    let status = fx.sync().await;
    assert_eq!(status.last_run.imported, 0);
    assert_eq!(status.last_run.rejected.get("not_requested"), Some(&1));
    assert!(fx
        .records()
        .await
        .iter()
        .any(|r| r.source == conflict_path && r.reason == "not_requested"));
    let unchanged = super::replace::Listing::list(curated)
        .await
        .unwrap()
        .load(curated, [LCCN].into_iter(), &[])
        .await
        .unwrap();
    assert_eq!(unchanged.pages[&doc].text, text.trim_end());

    let revoked_path = format!("{ROOT}/revoked/synthetic-loc-reuse.json");
    assert!(curated
        .put_new(
            &revoked_path,
            serde_json::to_vec(&serde_json::json!({
                "run": record.run,
                "reason": "Synthetic rollback rehearsal, not a LoC acceptance",
                "reference": "#811"
            }))
            .unwrap(),
            "application/json",
        )
        .await
        .unwrap());
    let revoked = super::replace::revocations(curated).await.unwrap();
    let rollback = listing
        .load(curated, [LCCN].into_iter(), &revoked)
        .await
        .unwrap();
    assert!(rollback.pages.is_empty());
    assert!(rollback.revoked.contains(&doc));
    assert_eq!(
        curated.get(&baseline_path).await.unwrap().unwrap(),
        baseline
    );
    assert_eq!(
        curated.get(&parts[0]).await.unwrap().unwrap(),
        imported_bytes
    );
    assert_eq!(
        curated.get(&records[0]).await.unwrap().unwrap(),
        record_bytes
    );
    assert!(ledger::load(curated)
        .await
        .unwrap()
        .outstanding(&missing)
        .is_some());
}

#[tokio::test]
async fn loc_reuse_fixture_rejects_unknown_provenance_and_malformed_alto() {
    for (source, alto, reason) in [
        ("loc", ALTO, "engine"),
        ("usnm-tesseract", "<html/>", "alto_not_alto"),
    ] {
        let fx = fixture(false).await;
        let curated = fx.stores.curated.as_ref();
        let baseline_path = format!("pages/{BATCH}_ver01/part-0.parquet");
        let baseline = curated.get(&baseline_path).await.unwrap().unwrap();
        let doc = key(3).doc_id();
        fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
        let mut candidate = row(3, &GERMAN.repeat(4));
        candidate.ocr_source = source.into();
        if source == "loc" {
            candidate.ocr_engine = "unknown".into();
        }
        fx.node_writes(&candidate, alto, &stamp(1)).await;
        let status = fx.sync().await;
        assert_eq!(status.last_run.imported, 0, "{reason}");
        assert_eq!(status.last_run.rejected.get(reason), Some(&1));
        assert!(curated
            .list(&format!("{ROOT}/pages"))
            .await
            .unwrap()
            .is_empty());
        assert!(ledger::load(curated)
            .await
            .unwrap()
            .outstanding(&doc)
            .is_some());
        assert_eq!(
            curated.get(&baseline_path).await.unwrap().unwrap(),
            baseline
        );
    }
}

/// One fixture per reject reason: a change to a good page's output.
#[tokio::test]
async fn each_reject_goes_to_quarantine_with_its_reason() {
    type Case = (
        &'static str,
        Box<dyn Fn(&mut OverlayRow, &mut String, &mut Queued)>,
    );
    let text = GERMAN.repeat(4);
    let cases: Vec<Case> = vec![
        (
            "not_requested",
            Box::new(|_, _, q| q.doc_id = "other".into()),
        ),
        (
            "batch_mismatch",
            Box::new(|_, _, q| q.batch = "nbu_other".into()),
        ),
        (
            "reason_mismatch",
            Box::new(|_, _, q| q.reason = "user".into()),
        ),
        ("text_control", Box::new(|r, _, _| r.text.push('\u{7}'))),
        (
            "engine",
            Box::new(|r, _, _| r.ocr_source = "usnm-other".into()),
        ),
        (
            "engine_tag",
            Box::new(|r, _, _| {
                r.ocr_engine = "tesseract/5.5.3 profile=deu image=local contract=1".into()
            }),
        ),
        (
            "reason",
            Box::new(|r, _, q| {
                r.loc_text = "search".into();
                q.reason = "search".into();
            }),
        ),
        (
            "batch_unpublished",
            Box::new(|r, _, q| {
                r.batch = "nbu_other".into();
                q.batch = "nbu_other".into();
            }),
        ),
        (
            "title_unknown",
            Box::new(|r, _, q| {
                r.key = PageKey::new("sn99999999", day(), 1, 3).unwrap();
                q.doc_id = r.key.doc_id();
            }),
        ),
        (
            "title_japanese",
            Box::new(|r, _, q| {
                r.key = PageKey::new(JA_LCCN, day(), 1, 3).unwrap();
                r.batch = "dlc_japan".into();
                q.doc_id = r.key.doc_id();
                q.batch = "dlc_japan".into();
            }),
        ),
        (
            "image_url",
            Box::new(|r, _, _| r.image_url = "https://evil.example/x".into()),
        ),
        (
            "alto_doctype",
            Box::new(|_, a, _| *a = a.replace("<alto ", "<!DOCTYPE alto>\n<alto ")),
        ),
        (
            "alto_no_page",
            Box::new(|_, a, _| *a = a.replace("WIDTH=\"5000\"", "")),
        ),
        ("alto_not_alto", Box::new(|_, a, _| *a = "<html/>".into())),
    ];
    for (want, change) in cases {
        let fx = fixture(false).await;
        let mut r = row(3, &text);
        let mut alto = ALTO.to_owned();
        let mut q = queued(&key(3).doc_id(), BATCH, "bulk");
        change(&mut r, &mut alto, &mut q);
        fx.ask(&[q]).await;
        let (alto_src, part_src) = fx.node_writes(&r, &alto, &stamp(1)).await;
        let status = fx.sync().await;
        assert_eq!(status.last_run.imported, 0, "{want}");
        assert_eq!(
            status.last_run.rejected.get(want),
            Some(&1),
            "{want}: {:?}",
            status.last_run.rejected
        );
        assert!(fx.out_paths().await.is_empty(), "{want}: ocr-out emptied");
        let records = fx.records().await;
        let mut sources: Vec<(&str, &str)> = records
            .iter()
            .map(|r| (r.source.as_str(), r.reason.as_str()))
            .collect();
        sources.sort();
        assert_eq!(
            sources,
            [(alto_src.as_str(), want), (part_src.as_str(), want)],
            "{want}"
        );
        // Records hold no text.
        for rec in &records {
            let json = serde_json::to_string(rec).unwrap();
            assert!(!json.contains("Stadtrath"), "{want}");
        }
        assert!(fx
            .stores
            .curated
            .list(&format!("{ROOT}/pages"))
            .await
            .unwrap()
            .is_empty());
    }
}

#[tokio::test]
async fn rejects_bad_names_sizes_and_parts() {
    let fx = fixture(false).await;
    let doc = key(3).doc_id();
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    // A name outside the pattern.
    fx.out
        .put_new("pages/whatever.parquet", b"x".to_vec(), "x")
        .await
        .unwrap();
    // Too large to read.
    fx.out
        .put_new(
            &format!("pages/{LCCN}_1890-05-03_ed-1.{NODE}.{}.parquet", stamp(1)),
            vec![0; checks::MAX_PART_BYTES as usize + 1],
            "x",
        )
        .await
        .unwrap();
    // Not Parquet.
    fx.out
        .put_new(
            &format!("pages/{LCCN}_1890-05-03_ed-1.{NODE}.{}.parquet", stamp(2)),
            b"not parquet".to_vec(),
            "x",
        )
        .await
        .unwrap();
    // Two rows.
    let two = usnm_overlay::write_part(&[row(3, "a"), row(4, "b")]).unwrap();
    fx.out
        .put_new(
            &format!("pages/{LCCN}_1890-05-03_ed-1.{NODE}.{}.parquet", stamp(3)),
            two,
            "x",
        )
        .await
        .unwrap();
    // A row from another issue than its name.
    let other = OverlayRow {
        key: PageKey::new(LCCN, day(), 2, 3).unwrap(),
        ..row(3, "x")
    };
    fx.out
        .put_new(
            &format!("pages/{LCCN}_1890-05-03_ed-1.{NODE}.{}.parquet", stamp(4)),
            usnm_overlay::write_part(&[other]).unwrap(),
            "x",
        )
        .await
        .unwrap();
    // No ALTO.
    fx.out
        .put_new(
            &format!("pages/{LCCN}_1890-05-03_ed-1.{NODE}.{}.parquet", stamp(5)),
            usnm_overlay::write_part(&[row(3, &GERMAN.repeat(4))]).unwrap(),
            "x",
        )
        .await
        .unwrap();
    let status = fx.sync().await;
    let r = &status.last_run.rejected;
    for code in [
        "name",
        "part_too_large",
        "part_unreadable",
        "rows",
        "issue_mismatch",
        "alto_missing",
    ] {
        assert_eq!(r.get(code), Some(&1), "{code}: {r:?}");
    }
    assert!(fx.out_paths().await.is_empty());
    // The oversized part's record has no content.
    let big = fx
        .records()
        .await
        .into_iter()
        .find(|r| r.reason == "part_too_large")
        .unwrap();
    assert_eq!(big.content, None);
}

#[tokio::test]
async fn a_run_stopped_before_its_deletes_is_finished_without_importing_again() {
    let fx = fixture(true).await;
    let doc = key(3).doc_id();
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    let (alto_src, part_src) = fx
        .node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    // A run that wrote its import record and stopped: its sources are still
    // in ocr-out.
    let listed = {
        let mut v = fx.out.list_info("pages").await.unwrap();
        v.extend(fx.out.list_info("alto").await.unwrap());
        v
    };
    let record = ImportRecord {
        run: super::run_id(Utc::now()),
        at: Utc::now(),
        index_version: "v1".into(),
        sources: listed
            .iter()
            .map(|o| sync::Consumed {
                path: o.path.clone(),
                etag: o.etag.clone(),
                outcome: "imported".into(),
                reason: None,
            })
            .collect(),
        pages: vec![],
        outcomes: None,
    };
    fx.stores
        .curated
        .put_new(
            &super::import_path(&record.run),
            serde_json::to_vec(&record).unwrap(),
            "x",
        )
        .await
        .unwrap();
    let status = fx.sync().await;
    assert_eq!((status.last_run.cleaned, status.last_run.imported), (2, 0));
    assert!(fx.out_paths().await.is_empty());
    assert!(!fx.out.exists(&alto_src).await.unwrap() && !fx.out.exists(&part_src).await.unwrap());
    // A source written again since that record is new content: checked again.
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    let status = fx.sync().await;
    assert_eq!(status.last_run.imported, 1);
}

#[tokio::test]
async fn deliberate_retry_requires_settlement_replays_and_keeps_original_request_day() {
    let fx = fixture(false).await;
    let mut previous = queued(&key(3).doc_id(), BATCH, "user");
    previous.plan = "hit-results".into();
    previous.queued_at = DateTime::from_timestamp(Utc::now().timestamp() - 86400, 0).unwrap();
    let message = OcrMessage {
        v: 1,
        page: key(3).to_string(),
        batch: BATCH.into(),
        language: "deu".into(),
        reason: Reason::User,
        score: None,
        queued_at: previous.queued_at.to_rfc3339(),
    };
    fx.ask(std::slice::from_ref(&previous)).await;
    let snapshot = super::boinc::Snapshot::load(&fx.state, fx.stores.reference.as_ref())
        .await
        .unwrap();
    let mut next = super::boinc::next_attempt(
        &previous,
        previous.queued_at + chrono::Duration::seconds(30),
        "boinc-0123456789ab",
    )
    .unwrap();
    assert!(super::boinc::retry(
        &fx.state,
        fx.stores.curated.as_ref(),
        &snapshot,
        &previous,
        &next,
        &message,
        |_| Ok(())
    )
    .await
    .is_err());
    let at = previous.queued_at + chrono::Duration::minutes(1);
    ledger::write_outcomes(
        fx.stores.curated.as_ref(),
        &[ledger::Outcome {
            doc_id: previous.doc_id.clone(),
            outcome: ledger::DEAD_LETTERED.into(),
            cause: Some("permanent".into()),
            kind: None,
            source: "terminal-dead-letter".into(),
            at,
            run: "prior-failure".into(),
            message_sha256: Some(usnm_ocr_contract::boinc::sha256(
                &serde_json::to_vec(&message).unwrap(),
            )),
        }],
        at,
        "prior-failure",
    )
    .await
    .unwrap();
    let mut wrong_message = message.clone();
    wrong_message.queued_at = Utc::now().to_rfc3339();
    assert!(super::boinc::retry(
        &fx.state,
        fx.stores.curated.as_ref(),
        &snapshot,
        &previous,
        &next,
        &wrong_message,
        |_| panic!("mismatched parent must not prepare a journal")
    )
    .await
    .is_err());
    let prepared = std::cell::Cell::new(0);
    for _ in 0..2 {
        next = super::boinc::retry(
            &fx.state,
            fx.stores.curated.as_ref(),
            &snapshot,
            &previous,
            &next,
            &message,
            |reserved| {
                assert!(reserved.queued_at > at);
                assert_eq!(reserved.retry_message_at, Some(reserved.queued_at));
                prepared.set(prepared.get() + 1);
                Ok(())
            },
        )
        .await
        .unwrap();
    }
    assert_eq!(prepared.get(), 1);
    let loaded = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(loaded.outstanding(&previous.doc_id), Some(&next));
    assert_eq!(next.attempt, 2);
    assert_eq!(next.request_day, Some(previous.queued_at.date_naive()));
    assert_eq!(
        loaded.user_requests_for_day(previous.queued_at.date_naive()),
        1
    );
    assert_eq!(loaded.user_requests_for_day(next.queued_at.date_naive()), 0);
    let stale = super::boinc::next_attempt(
        &previous,
        Utc::now() + chrono::Duration::seconds(1),
        "boinc-0123456789ab",
    )
    .unwrap();
    assert!(super::boinc::retry(
        &fx.state,
        fx.stores.curated.as_ref(),
        &snapshot,
        &previous,
        &stale,
        &message,
        |_| Ok(())
    )
    .await
    .is_err());
    for (offset, outcome) in [(1, ledger::IMPORTED), (2, ledger::DEAD_LETTERED)] {
        let at = next.queued_at + chrono::Duration::seconds(offset);
        ledger::write_outcomes(
            fx.stores.curated.as_ref(),
            &[ledger::Outcome {
                doc_id: next.doc_id.clone(),
                outcome: outcome.into(),
                cause: Some("permanent".into()),
                kind: None,
                source: "later".into(),
                at,
                run: format!("later-{offset}"),
                message_sha256: None,
            }],
            at,
            &format!("later-{offset}"),
        )
        .await
        .unwrap();
    }
    assert!(super::boinc::retry(
        &fx.state,
        fx.stores.curated.as_ref(),
        &snapshot,
        &previous,
        &next,
        &message,
        |_| panic!("import history must not prepare a journal")
    )
    .await
    .is_err());
}

#[test]
fn legacy_ledger_without_retry_timestamp_remains_readable() {
    let row = queued(&key(3).doc_id(), BATCH, "bulk");
    let encoded = ledger::encode_queued(std::slice::from_ref(&row)).unwrap();
    let batch = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        bytes::Bytes::from(encoded),
    )
    .unwrap()
    .build()
    .unwrap()
    .next()
    .unwrap()
    .unwrap();
    let columns: Vec<_> = (0..batch.num_columns())
        .filter(|&i| batch.schema().field(i).name() != "retry_message_at")
        .collect();
    let legacy = batch.project(&columns).unwrap();
    let mut writer =
        parquet::arrow::ArrowWriter::try_new(Vec::new(), legacy.schema(), None).unwrap();
    writer.write(&legacy).unwrap();
    let loaded = ledger::decode_queued(bytes::Bytes::from(writer.into_inner().unwrap())).unwrap();
    assert_eq!(loaded[0].retry_message_at, None);
    assert_eq!(loaded[0].doc_id, row.doc_id);
}

#[tokio::test]
async fn stale_dead_letter_is_archived_without_settling_a_new_attempt() {
    let fx = fixture(false).await;
    let mut previous = queued(&key(3).doc_id(), BATCH, "bulk");
    previous.queued_at = DateTime::from_timestamp(Utc::now().timestamp() - 3600, 0).unwrap();
    let next = super::boinc::next_attempt(&previous, Utc::now(), "boinc-0123456789ab").unwrap();
    fx.ask(std::slice::from_ref(&next)).await;
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    let stale_output = fx.sync().await;
    assert_eq!(
        stale_output.last_run.rejected.get("retry_attempt"),
        Some(&1)
    );
    assert_eq!(
        ledger::load(fx.stores.curated.as_ref())
            .await
            .unwrap()
            .outstanding(&next.doc_id),
        Some(&next)
    );
    let message = OcrMessage {
        v: 1,
        page: key(3).to_string(),
        batch: BATCH.into(),
        language: "deu".into(),
        reason: Reason::Bulk,
        score: None,
        queued_at: previous.queued_at.to_rfc3339(),
    };
    let dead = DeadLetter::new(
        BULK,
        "old-attempt",
        Some(&message),
        Cause::Permanent,
        None,
        None,
        "BOINC error mask 0x2",
        1,
        NODE,
    );
    fx.queue.send(DEADLETTER, &dead.encode()).await.unwrap();
    let status = fx.sync().await;
    let loaded = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(loaded.outstanding(&next.doc_id), Some(&next));
    assert_eq!(
        status.last_run.dead_letters_rejected.get("stale_attempt"),
        Some(&1)
    );
    assert_eq!(fx.queue.approximate_count(DEADLETTER).await.unwrap(), 0);
    assert_eq!(
        fx.stores
            .curated
            .list(&format!("{ROOT}/deadletter"))
            .await
            .unwrap()
            .len(),
        1
    );
    let current = OcrMessage {
        queued_at: next.retry_message_at.unwrap().to_rfc3339(),
        ..message
    };
    let dead = DeadLetter::new(
        BULK,
        "current-attempt",
        Some(&current),
        Cause::Permanent,
        None,
        None,
        "BOINC error mask 0x4",
        1,
        NODE,
    );
    fx.queue.send(DEADLETTER, &dead.encode()).await.unwrap();
    let status = fx.sync().await;
    assert!(status.last_run.dead_letters_rejected.is_empty());
    let loaded = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert!(loaded.outstanding(&next.doc_id).is_none());
    let mut stale_message = current.clone();
    stale_message.language = "fra".into();
    stale_message.queued_at = previous.queued_at.to_rfc3339();
    let stale = DeadLetter::new(
        BULK,
        "late-parent",
        Some(&stale_message),
        Cause::Permanent,
        None,
        None,
        "old failure",
        1,
        NODE,
    );
    fx.queue.send(DEADLETTER, &stale.encode()).await.unwrap();
    fx.sync().await;
    let report = enqueue::requeue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &RequeueOptions {
            cause: "permanent".into(),
            kind: None,
            since: None,
            plan: "recovery".into(),
            max_pages: 1,
            dry_run: false,
        },
    )
    .await
    .unwrap();
    assert_eq!(report.queued, 1);
    let loaded = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    let ordinary = loaded.queued[&next.doc_id].clone();
    assert_eq!(ordinary.language, current.language);
    assert!(ordinary.retry_message_at.is_some());
    assert!(ordinary.retry_run.is_none());
    fx.queue.send(DEADLETTER, &dead.encode()).await.unwrap();
    let status = fx.sync().await;
    assert_eq!(
        status.last_run.dead_letters_rejected.get("stale_attempt"),
        Some(&1)
    );
    assert_eq!(
        ledger::load(fx.stores.curated.as_ref())
            .await
            .unwrap()
            .outstanding(&next.doc_id),
        Some(&ordinary)
    );
}

#[tokio::test]
async fn drains_dead_letters_and_quarantines_bad_ones() {
    let fx = fixture(false).await;
    let msg = OcrMessage {
        v: 1,
        page: key(5).to_string(),
        batch: BATCH.into(),
        language: "deu".into(),
        reason: Reason::Bulk,
        score: Some(0.6),
        queued_at: "2026-10-10T12:00:00Z".into(),
    };
    fx.ask(&[queued(&key(5).doc_id(), BATCH, "bulk")]).await;
    // One page per cause.
    for (seq, cause) in [
        (5, Cause::Permanent),
        (6, Cause::Unroutable),
        (7, Cause::DequeueCount),
    ] {
        let engines = (cause == Cause::Unroutable).then(|| vec!["tesseract-5.5".to_owned()]);
        let msg = OcrMessage {
            page: key(seq).to_string(),
            ..msg.clone()
        };
        let d = DeadLetter::new(
            "ocr-bulk",
            "m1",
            Some(&msg),
            cause,
            None,
            engines,
            "no image",
            6,
            NODE,
        );
        fx.queue.send(DEADLETTER, &d.encode()).await.unwrap();
    }
    // A dead letter with a field the schema doesn't have.
    let bad = r#"{"v":1,"from":"ocr-bulk","message_id":"m2","message":null,"error":"x","dequeue_count":6,"node":"n","at":"2026-10-10T12:00:00Z","q":"cross of gold"}"#;
    fx.queue.send(DEADLETTER, bad).await.unwrap();
    let status = fx.sync().await;
    let d = &status.last_run.dead_letters;
    assert_eq!(
        (
            d.get("permanent"),
            d.get("unroutable"),
            d.get("dequeue_count")
        ),
        (Some(&1), Some(&1), Some(&1))
    );
    assert_eq!(
        status
            .last_run
            .dead_letters_rejected
            .get("deadletter_bad_json"),
        Some(&1)
    );
    assert_eq!(fx.queue.approximate_count(DEADLETTER).await.unwrap(), 0);
    let files = fx
        .stores
        .curated
        .list(&format!("{ROOT}/deadletter"))
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    let body = fx.stores.curated.get(&files[0]).await.unwrap().unwrap();
    let text = String::from_utf8(body).unwrap();
    assert_eq!(text.lines().count(), 3);
    assert!(
        !text.contains("cross of gold"),
        "rejects never reach the lake"
    );
    let rec = fx.records().await;
    assert_eq!(rec.len(), 1);
    assert_eq!(
        (rec[0].kind.as_str(), rec[0].reason.as_str()),
        ("deadletter", "deadletter_bad_json")
    );
    // The page's latest outcome is the last dead letter drained.
    let l = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert!(l.outcomes.contains_key(&key(5).doc_id()));

    // ocr-requeue: the permanent one, once.
    let opts = RequeueOptions {
        cause: "permanent".into(),
        kind: None,
        since: None,
        plan: "requeue".into(),
        max_pages: 10,
        dry_run: false,
    };
    // Make sure the outcome is newer than the queued row (same-second runs).
    let r = enqueue::requeue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(r.queued, 1, "{r:?}");
    let m = fx
        .queue
        .receive(BULK, std::time::Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let (back, k) = usnm_ocr_contract::message::decode(&m.text).unwrap();
    assert_eq!((k, back.batch.as_str()), (key(5), BATCH));
    // Queued again since: not twice.
    let again = enqueue::requeue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(again.queued, 0);
    // The requeued page then dead-letters for another cause: no longer
    // `permanent`, so not queued again under that cause.
    ledger::write_outcomes(
        fx.stores.curated.as_ref(),
        &[ledger::Outcome {
            doc_id: key(5).doc_id(),
            outcome: ledger::DEAD_LETTERED.into(),
            cause: Some("dequeue_count".into()),
            kind: None,
            source: "m9".into(),
            at: Utc::now() + chrono::Duration::seconds(5),
            run: "later".into(),
            message_sha256: None,
        }],
        Utc::now(),
        "later",
    )
    .await
    .unwrap();
    let third = enqueue::requeue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(
        third.skipped.get("latest_cause_differs"),
        Some(&1),
        "{third:?}"
    );
}

#[tokio::test]
async fn replays_from_quarantine_after_a_fix() {
    let fx = fixture(false).await;
    let doc = key(3).doc_id();
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    let first = fx.sync().await;
    assert_eq!(first.last_run.rejected.get("not_requested"), Some(&1));
    // The fix: the ledger row that was missing.
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    let opts = Options {
        from_quarantine: Some("not_requested".into()),
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.imported, 1, "{:?}", status.last_run);
    assert!(fx.records().await.is_empty(), "quarantine emptied");
}

#[tokio::test]
async fn real_loc_batches_seed_enqueue_and_sync_with_original_case() {
    for batch in [
        "curiv_joshuaTree",
        "lu_Cypher",
        "me_Bissonnette",
        "vi_White",
        "wvu_Quote",
        &format!("nbu_{}", "A".repeat(101)),
        &format!("nbu_{}", "A".repeat(124)),
    ] {
        let fx = fixture_with_batch(true, batch).await;
        let status = sync::run(
            &fx.state,
            "test",
            &fx.stores,
            &Options {
                seed: super::seed::Options {
                    plan: "mixed-case".into(),
                    batches: vec![format!("{batch}_ver01")],
                    window: 1,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(status.seed.unwrap().queued, 1, "{batch}");
        let seeded = fx
            .queue
            .receive(BULK, Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let seeded = usnm_ocr_contract::message::decode(&seeded.text).unwrap().0;
        assert_eq!(seeded.batch, batch);

        let report = enqueue::enqueue(
            &fx.state,
            fx.stores.curated.as_ref(),
            fx.stores.reference.as_ref(),
            &fx.queue,
            &EnqueueOptions {
                pages: vec![key(9).doc_id()],
                reason: Reason::User,
                plan: "mixed-case".into(),
                batch: Some(batch.into()),
                include_outstanding: false,
                dry_run: false,
            },
        )
        .await
        .unwrap();
        assert_eq!(report.queued, 1, "{batch}: {report:?}");
        let received = fx
            .queue
            .receive(PRIORITY, Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let msg = usnm_ocr_contract::message::decode(&received.text)
            .unwrap()
            .0;
        assert_eq!(msg.batch, batch);
        let ledger = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
        assert_eq!(ledger.queued[&key(9).doc_id()].batch, batch);

        for v in [1, 2] {
            let mut dead = DeadLetter::new(
                PRIORITY,
                &format!("mixed-case-{v}"),
                Some(&msg),
                Cause::Permanent,
                None,
                None,
                "no image",
                6,
                NODE,
            );
            dead.v = v;
            dead.cause = (v == 2).then_some(Cause::Permanent);
            fx.queue.send(DEADLETTER, &dead.encode()).await.unwrap();
        }
        let status = fx.sync().await;
        assert!(status.last_run.dead_letters_rejected.is_empty(), "{batch}");
        assert_eq!(status.last_run.dead_letters.values().sum::<u64>(), 2);
        let files = fx
            .stores
            .curated
            .list(&format!("{ROOT}/deadletter"))
            .await
            .unwrap();
        let body = fx.stores.curated.get(&files[0]).await.unwrap().unwrap();
        let text = String::from_utf8(body).unwrap();
        for line in text.lines() {
            let dead: DeadLetter = serde_json::from_str(line).unwrap();
            assert_eq!(dead.message.unwrap().batch, batch);
        }
    }
}

#[tokio::test]
async fn fetched_source_preserves_mixed_case_and_full_length_batches() {
    use usnm_ocr_contract::{fetched, prefetch};

    for batch in [
        "curiv_joshuaTree",
        "lu_Cypher",
        "me_Bissonnette",
        "vi_White",
        "wvu_Quote",
        &format!("nbu_{}", "A".repeat(61)),
        &format!("nbu_{}", "A".repeat(101)),
        &format!("nbu_{}", "A".repeat(124)),
    ] {
        let fx = fixture_with_batch(true, batch).await;
        let excluded = format!("excluded_{}", "B".repeat(119));
        let rows = [
            prefetch::ManifestRow::new(&key(1), batch, 1, Some("deu".into()), "detected", 0.5),
            prefetch::ManifestRow::new(&key(2), &excluded, 1, Some("deu".into()), "detected", 0.5),
        ];
        let path = format!("{}/manifest.parquet", crate::ocr_prefetch::dir("v1", None));
        fx.stores
            .curated
            .put_new(&path, prefetch::encode(&rows).unwrap(), "x")
            .await
            .unwrap();
        let pages = [fetched::Fetched {
            batch: batch.into(),
            doc_id: key(1).doc_id(),
        }]
        .into_iter()
        .collect();
        fx.out
            .put_new(
                &fetched::path("v1", NODE, "20261012T010500Z"),
                fetched::encode("v1", &pages).unwrap(),
                "x",
            )
            .await
            .unwrap();
        let status = sync::run(
            &fx.state,
            "test",
            &fx.stores,
            &Options {
                seed: super::seed::Options {
                    plan: "mixed-case-fetched".into(),
                    batches: vec![batch.into()],
                    window: 1,
                    source: super::seed::Source::Fetched,
                    ..Default::default()
                },
                ..Default::default()
            },
        )
        .await
        .unwrap();
        assert_eq!(status.seed.unwrap().queued, 1, "{batch}");
        let received = fx
            .queue
            .receive(BULK, Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let msg = usnm_ocr_contract::message::decode(&received.text)
            .unwrap()
            .0;
        assert_eq!(msg.batch, batch);
        assert_eq!(msg.page, key(1).to_string());
    }
}

#[tokio::test]
async fn enqueue_finds_the_page_and_writes_the_ledger_first() {
    let fx = fixture(true).await;
    let doc = key(4).doc_id();
    let opts = EnqueueOptions {
        pages: vec![
            doc.clone(),
            PageKey::new(JA_LCCN, day(), 1, 1).unwrap().doc_id(),
            "junk".into(),
        ],
        reason: Reason::User,
        plan: "pilot-test".into(),
        batch: None,
        include_outstanding: false,
        dry_run: false,
    };
    let r = enqueue::enqueue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(r.queued, 1, "{r:?}");
    assert_eq!(r.skipped.get("japanese_title"), Some(&1));
    assert_eq!(r.skipped.get("bad_doc_id"), Some(&1));
    let m = fx
        .queue
        .receive(PRIORITY, std::time::Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let (msg, k) = usnm_ocr_contract::message::decode(&m.text).unwrap();
    assert_eq!(k, key(4));
    assert_eq!((msg.batch.as_str(), msg.language.as_str()), (BATCH, "deu"));
    assert!(msg.score.is_some_and(|s| s > 0.1));
    let l = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    let q = l.outstanding(&doc).unwrap();
    assert_eq!(
        (q.plan.as_str(), q.attempt, q.reason.as_str()),
        ("pilot-test", 1, "user")
    );
    // Outstanding now: not queued twice.
    let again = enqueue::enqueue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &opts,
    )
    .await
    .unwrap();
    assert_eq!(again.skipped.get("outstanding"), Some(&1));
    // A send that failed after the ledger was written: queued again on request.
    let retry = enqueue::enqueue(
        &fx.state,
        fx.stores.curated.as_ref(),
        fx.stores.reference.as_ref(),
        &fx.queue,
        &EnqueueOptions {
            include_outstanding: true,
            ..opts.clone()
        },
    )
    .await
    .unwrap();
    assert_eq!(retry.queued, 1);
    let l = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(l.outstanding(&doc).unwrap().attempt, 2);
}

#[test]
fn part_names() {
    let n = checks::parse_part_name(&format!(
        "pages/{LCCN}_1890-05-03_ed-2.nuc-1.{}.parquet",
        stamp(7)
    ))
    .unwrap();
    assert_eq!(
        (n.lccn.as_str(), n.edition, n.node.as_str()),
        (LCCN, 2, "nuc-1")
    );
    assert_eq!(n.date, day());
    for bad in [
        format!("pages/{LCCN}_1890-05-03_ed-2.nuc1.20261011.parquet"),
        format!("pages/{LCCN}_1890-5-3_ed-2.nuc1.{}.parquet", stamp(1)),
        format!("pages/{LCCN}_1890-05-03_ed-02.nuc1.{}.parquet", stamp(1)),
        format!("pages/{LCCN}_1890-05-03_ed-2.nuc_1.{}.parquet", stamp(1)),
        format!(
            "pages/{LCCN}_1890-05-03_ed-2.{}.{}.parquet",
            "n".repeat(33),
            stamp(1)
        ),
        format!("alto/{LCCN}_1890-05-03_ed-2.nuc1.{}.parquet", stamp(1)),
        format!("pages/SN1_1890-05-03_ed-2.nuc1.{}.parquet", stamp(1)),
    ] {
        assert!(checks::parse_part_name(&bad).is_none(), "{bad}");
    }
    assert_eq!(
        Reject::DeadLetter("v1 has no cause").code(),
        "deadletter_v1_has_no_cause"
    );
}

#[test]
fn rule_v1_on_stored_scores() {
    let s = |lang: &str, rate: f64, head: u32| pages::Scores {
        lang: Some(lang.into()),
        decision: Some("detected".into()),
        damage_rate: Some(rate),
        function_share: None,
        head: Some(head),
    };
    assert_eq!(
        report::verdict(&s("ger", 0.1, 50), &s("ger", 0.4, 40)),
        Ok(())
    );
    assert_eq!(
        report::verdict(&s("ger", 0.38, 50), &s("ger", 0.4, 40)),
        Err("margin")
    );
    assert_eq!(
        report::verdict(&s("ger", 0.1, 30), &s("ger", 0.4, 40)),
        Err("head")
    );
    assert_eq!(
        report::verdict(&s("eng", 0.1, 50), &s("ger", 0.4, 40)),
        Err("language_differs")
    );
    let und = pages::Scores {
        lang: Some("und".into()),
        ..Default::default()
    };
    assert_eq!(report::verdict(&s("ger", 0.1, 50), &und), Ok(()));
    assert_eq!(
        report::verdict(&und, &s("ger", 0.4, 40)),
        Err("ours_undecided")
    );
    assert_eq!(
        report::engine_key(
            "tesseract/5.5.3 profile=deu_latf+deu image=sha256:3f9a route=x@r1 contract=1"
        ),
        "tesseract/5.5.3 deu_latf+deu"
    );
    assert_eq!(
        report::engine_key("tesseract-5.5.1/deu"),
        "tesseract-5.5.1/deu"
    );
}

#[tokio::test]
async fn report_counts_the_imported_pages() {
    let fx = fixture(true).await;
    let doc = key(3).doc_id();
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    fx.sync().await;
    let r = report::run(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!((r.pages.considered, r.pages.replaced), (1, 1), "{r:?}");
    assert_eq!(
        fx.stores
            .curated
            .list(&format!("{ROOT}/report"))
            .await
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn every_import_gets_loc_scores_from_curated_text() {
    // No damage tables: LoC's scores come from the curated text, for each
    // import's own pages.
    let fx = fixture(false).await;
    fx.ask(&[
        queued(&key(3).doc_id(), BATCH, "bulk"),
        queued(&key(4).doc_id(), BATCH, "bulk"),
    ])
    .await;
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    fx.node_writes(&row(4, &GERMAN.repeat(4)), ALTO, &stamp(2))
        .await;
    let opts = Options {
        chunk: 1,
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.imported, 2);
    let c = &fx.stores.curated;
    let mut rows = Vec::new();
    for p in c.list(&format!("{ROOT}/pages/{LCCN}")).await.unwrap() {
        let b = bytes::Bytes::from(c.get(&p).await.unwrap().unwrap());
        rows.extend(pages::read_scores(b).unwrap());
    }
    assert_eq!(rows.len(), 2);
    assert!(rows.iter().all(|r| r.loc.damage_rate.is_some()), "{rows:?}");
}

#[tokio::test]
async fn an_alto_already_imported_with_other_bytes_stops_the_run() {
    let fx = fixture(false).await;
    let doc = key(3).doc_id();
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    // An earlier run's copy, then the node rewrote the source.
    fx.stores
        .curated
        .put_new(
            &super::alto_path(&doc, NODE, &stamp(1)),
            b"<alto/>".to_vec(),
            "x",
        )
        .await
        .unwrap();
    let err = sync::run(&fx.state, "test", &fx.stores, &Options::default())
        .await
        .unwrap_err();
    assert!(format!("{err:#}").contains("other bytes"), "{err:#}");
    // Nothing was deleted.
    assert_eq!(fx.out_paths().await.len(), 2);
}

#[tokio::test]
async fn hit_requests_queue_damaged_pages_with_a_daily_cap_and_complete_the_day() {
    let fx = fixture(true).await;
    let request_day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
    let mut body = Vec::new();
    for doc_id in [
        key(1).doc_id(),
        key(2).doc_id(),
        key(3).doc_id(),
        key(1).doc_id(),
        PageKey::new(JA_LCCN, day(), 1, 1).unwrap().doc_id(),
    ] {
        serde_json::to_writer(&mut body, &serde_json::json!({"v": 1, "doc_id": doc_id})).unwrap();
        body.push(b'\n');
    }
    fx.requests
        .put_new("days/2026-10-14.jsonl", body, "application/x-ndjson")
        .await
        .unwrap();

    let opts = Options {
        user_daily_max: 2,
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.user_requests_queued, 2);
    assert_eq!(status.outstanding, 2);
    assert_eq!(
        fx.requests.get("days/2026-10-14.jsonl").await.unwrap(),
        Some(Vec::new())
    );
    assert!(fx
        .requests
        .exists("completed/2026-10-14.json")
        .await
        .unwrap());
    assert_eq!(
        fx.stores
            .curated
            .get("ocr-local/requests/2026-10-14/lookup.json")
            .await
            .unwrap(),
        Some(Vec::new()),
        "normal completion clears the bounded daily ID list and resolved scores"
    );
    let ledger = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(ledger.user_requests_for_day(request_day), 2);
    for expected in [key(1), key(2)] {
        let message = fx
            .queue
            .receive(PRIORITY, std::time::Duration::from_secs(60))
            .await
            .unwrap()
            .unwrap();
        let (message, page) = usnm_ocr_contract::message::decode(&message.text).unwrap();
        assert_eq!(page, expected);
        assert_eq!(message.reason, Reason::User);
        assert!(message.score.is_some_and(|score| score >= 0.1));
        assert_eq!(message.queued_at, "2026-10-14T00:00:00Z");
        let queued = ledger.queued.get(&expected.doc_id()).unwrap();
        assert_eq!(queued.request_day, Some(request_day));
        assert!(queued.queued_at > request_day.and_hms_opt(0, 0, 0).unwrap().and_utc());
    }
    assert!(fx
        .queue
        .receive(PRIORITY, std::time::Duration::from_millis(1))
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn request_retry_skips_charged_dead_letters_and_counts_partial_successes() {
    let fx = fixture(true).await;
    let mut body = Vec::new();
    for seq in 1..=3 {
        serde_json::to_writer(
            &mut body,
            &serde_json::json!({"v":1,"doc_id":key(seq).doc_id()}),
        )
        .unwrap();
        body.push(b'\n');
    }
    fx.requests
        .put_new("days/2026-10-14.jsonl", body, "application/x-ndjson")
        .await
        .unwrap();
    let stores = Stores {
        queue: Arc::new(FailSecondQueue {
            inner: Arc::clone(&fx.queue),
            attempts: std::sync::atomic::AtomicUsize::new(0),
        }),
        ..fx.stores.clone()
    };
    let opts = Options {
        user_daily_max: 3,
        ..Options::default()
    };
    let error = sync::run(&fx.state, "test", &stores, &opts)
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("injected second-send failure"));
    let status: Status =
        serde_json::from_slice(&fx.stores.reference.get(STATUS_PATH).await.unwrap().unwrap())
            .unwrap();
    assert_eq!(status.last_run.user_requests_queued, 1);
    assert_eq!(status.totals.user_requests_queued, 1);
    let ledger = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    let day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
    assert_eq!(ledger.user_requests_for_day(day), 2);
    let charged = ledger.charged_user_requests(day);
    assert!(charged.contains(key(1).doc_id().as_str()));
    assert!(charged.contains(key(2).doc_id().as_str()));
    assert!(!charged.contains(key(3).doc_id().as_str()));

    let sent = fx
        .queue
        .receive(PRIORITY, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let (message, page) = usnm_ocr_contract::message::decode(&sent.text).unwrap();
    assert_eq!(page, key(1));
    fx.queue.delete(&sent).await.unwrap();
    let letter = DeadLetter::new(
        PRIORITY,
        &sent.id,
        Some(&message),
        Cause::Permanent,
        None,
        None,
        "no image",
        1,
        NODE,
    );
    fx.queue.send(DEADLETTER, &letter.encode()).await.unwrap();
    let resumed = sync::run(&fx.state, "test", &stores, &opts).await.unwrap();
    assert_eq!(resumed.last_run.user_requests_queued, 1);
    assert_eq!(resumed.totals.user_requests_queued, 2);
    assert_eq!(resumed.last_run.dead_letters.get("permanent"), Some(&1));
    let next = fx
        .queue
        .receive(PRIORITY, Duration::from_secs(60))
        .await
        .unwrap()
        .unwrap();
    let (_, page) = usnm_ocr_contract::message::decode(&next.text).unwrap();
    assert_eq!(
        page,
        key(3),
        "terminal A stays charged; confirmed-unsent C gets the remaining slot"
    );
    assert_eq!(fx.queue.approximate_count(PRIORITY).await.unwrap(), 1);
    assert!(fx
        .requests
        .exists("completed/2026-10-14.json")
        .await
        .unwrap());
    let final_status = sync::run(&fx.state, "test", &stores, &opts).await.unwrap();
    assert_eq!(final_status.totals.user_requests_queued, 2);
}

#[tokio::test]
async fn request_completion_failures_preserve_success_counts_and_cleanup_on_retry() {
    for path in [
        "completed/2026-10-14.json",
        "days/2026-10-14.jsonl",
        "ocr-local/requests/2026-10-14/lookup.json",
    ] {
        let fx = fixture(true).await;
        fx.requests
            .put_new(
                "days/2026-10-14.jsonl",
                serde_json::to_vec(&serde_json::json!({"v":1,"doc_id":key(1).doc_id()})).unwrap(),
                "application/x-ndjson",
            )
            .await
            .unwrap();
        let mut stores = fx.stores.clone();
        let checkpoint = path.starts_with("ocr-local/");
        let failing: Arc<dyn ObjectStore> = Arc::new(FailWriteStore {
            inner: Arc::clone(if checkpoint {
                &fx.stores.curated
            } else {
                &fx.requests
            }),
            path,
            empty_only: checkpoint,
            failed: std::sync::atomic::AtomicBool::new(false),
        });
        if checkpoint {
            stores.curated = failing;
        } else {
            stores.requests = Some(failing);
        }
        let error = sync::run(&fx.state, "test", &stores, &Options::default())
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("injected completion failure"));
        let published: Status =
            serde_json::from_slice(&fx.stores.reference.get(STATUS_PATH).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(published.last_run.user_requests_queued, 1, "{path}");
        assert_eq!(published.totals.user_requests_queued, 1, "{path}");
        let resumed = sync::run(&fx.state, "test", &stores, &Options::default())
            .await
            .unwrap();
        assert_eq!(resumed.last_run.user_requests_queued, 0);
        assert_eq!(resumed.totals.user_requests_queued, 1);
        assert_eq!(fx.queue.approximate_count(PRIORITY).await.unwrap(), 1);
        assert_eq!(
            fx.stores
                .curated
                .get("ocr-local/requests/2026-10-14/lookup.json")
                .await
                .unwrap(),
            Some(Vec::new())
        );
        assert_eq!(
            fx.requests.get("days/2026-10-14.jsonl").await.unwrap(),
            Some(Vec::new())
        );
    }
}

#[tokio::test]
async fn failed_request_sends_keep_the_day_for_unsent_retry_and_charge_only_sent_pages() {
    let fx = fixture(true).await;
    let request_day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
    let mut body = Vec::new();
    for doc_id in [key(1).doc_id(), key(2).doc_id(), key(3).doc_id()] {
        serde_json::to_writer(&mut body, &serde_json::json!({"v": 1, "doc_id": doc_id})).unwrap();
        body.push(b'\n');
    }
    fx.requests
        .put_new("days/2026-10-14.jsonl", body, "application/x-ndjson")
        .await
        .unwrap();
    let flaky: Arc<dyn Queue> = Arc::new(FailFirstQueue {
        inner: Arc::clone(&fx.queue),
        failed: std::sync::atomic::AtomicBool::new(false),
    });
    let stores = Stores {
        queue: Arc::clone(&flaky),
        ..fx.stores.clone()
    };
    let opts = Options {
        user_daily_max: 3,
        ..Options::default()
    };

    let first = sync::run(&fx.state, "test", &stores, &opts).await;
    assert!(first.is_err());
    assert!(fx.requests.exists("days/2026-10-14.jsonl").await.unwrap());
    assert!(!fx
        .requests
        .exists("completed/2026-10-14.json")
        .await
        .unwrap());
    let ledger = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(ledger.user_requests_for_day(request_day), 1);

    let second = sync::run(&fx.state, "test", &stores, &opts).await.unwrap();
    assert_eq!(second.last_run.user_requests_queued, 2);
    assert_eq!(
        fx.requests.get("days/2026-10-14.jsonl").await.unwrap(),
        Some(Vec::new())
    );
    assert!(fx
        .requests
        .exists("completed/2026-10-14.json")
        .await
        .unwrap());
    let ledger = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(ledger.user_requests_for_day(request_day), 3);
    assert_eq!(
        fx.queue.approximate_count(PRIORITY).await.unwrap(),
        2,
        "the ambiguous first send remains charged but is not duplicated"
    );
}

#[tokio::test]
async fn completed_request_days_are_tombstoned_without_spending_the_next_days_slot() {
    let fx = fixture(true).await;
    let day = NaiveDate::from_ymd_opt(2026, 10, 14).unwrap();
    for completed_day in ["2026-10-13", "2026-10-14"] {
        fx.stores
            .curated
            .put(
                &format!("ocr-local/requests/{completed_day}/lookup.json"),
                b"previous daily IDs and scores".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
    }
    fx.requests
        .put_new("days/2026-10-13.jsonl", Vec::new(), "application/x-ndjson")
        .await
        .unwrap();
    fx.requests
        .put_new(
            "completed/2026-10-13.json",
            br#"{"v":1}"#.to_vec(),
            "application/json",
        )
        .await
        .unwrap();
    let body = serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "doc_id": key(1).doc_id()
    }))
    .unwrap();
    fx.requests
        .put_new("days/2026-10-14.jsonl", body, "application/x-ndjson")
        .await
        .unwrap();
    fx.requests
        .put_new(
            "completed/2026-10-14.json",
            br#"{"v":1}"#.to_vec(),
            "application/json",
        )
        .await
        .unwrap();

    fx.requests
        .put_new(
            "days/2026-10-15.jsonl",
            serde_json::to_vec(&serde_json::json!({"v": 1, "doc_id":key(2).doc_id()})).unwrap(),
            "application/x-ndjson",
        )
        .await
        .unwrap();
    let status = fx.sync().await;
    assert_eq!(status.last_run.user_requests_queued, 1);
    for completed_day in ["2026-10-13", "2026-10-14"] {
        assert_eq!(
            fx.stores
                .curated
                .get(&format!("ocr-local/requests/{completed_day}/lookup.json"))
                .await
                .unwrap(),
            Some(Vec::new()),
            "cleanup clears checkpoints even when the day payload is already empty"
        );
    }
    assert_eq!(
        fx.requests.get("days/2026-10-14.jsonl").await.unwrap(),
        Some(Vec::new())
    );
    assert!(!fx
        .requests
        .put_new(
            "days/2026-10-14.jsonl",
            b"stale aggregate from a concurrent API close".to_vec(),
            "application/x-ndjson"
        )
        .await
        .unwrap());
    assert_eq!(
        fx.queue.approximate_count(PRIORITY).await.unwrap(),
        1,
        "the durable marker prevents a consumed day from replaying"
    );
    assert_eq!(
        ledger::load(fx.stores.curated.as_ref())
            .await
            .unwrap()
            .user_requests_for_day(day),
        0
    );
}

#[tokio::test]
async fn request_deadline_marks_unattempted_pages_unsent_and_recovers_next_run() {
    let fx = fixture(true).await;
    let mut body = Vec::new();
    for seq in 1..=3 {
        serde_json::to_writer(
            &mut body,
            &serde_json::json!({"v":1,"doc_id":key(seq).doc_id()}),
        )
        .unwrap();
        body.push(b'\n');
    }
    fx.requests
        .put_new("days/2026-10-14.jsonl", body, "application/x-ndjson")
        .await
        .unwrap();
    let stores = Stores {
        queue: Arc::new(StallAfterFirstQueue {
            inner: Arc::clone(&fx.queue),
            sent: std::sync::atomic::AtomicBool::new(false),
        }),
        ..fx.stores.clone()
    };
    let options = Options {
        user_daily_max: 3,
        deadline: Some(tokio::time::Instant::now() + Duration::from_secs(1)),
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &stores, &options)
        .await
        .unwrap();
    assert_eq!(status.last_run.user_requests_queued, 1);
    assert_eq!(
        status.outstanding, 2,
        "successful and ambiguous sends stay outstanding"
    );
    assert!(!fx
        .requests
        .exists("completed/2026-10-14.json")
        .await
        .unwrap());
    assert!(!fx
        .requests
        .get("days/2026-10-14.jsonl")
        .await
        .unwrap()
        .unwrap()
        .is_empty());
    let before = ledger::load(fx.stores.curated.as_ref()).await.unwrap();
    assert_eq!(before.outcomes[&key(3).doc_id()].outcome, ledger::NOT_SENT);
    assert_eq!(
        before.user_requests_for_day(NaiveDate::from_ymd_opt(2026, 10, 14).unwrap()),
        2
    );

    let resumed = sync::run(
        &fx.state,
        "test",
        &fx.stores,
        &Options {
            user_daily_max: 3,
            ..Options::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(resumed.last_run.user_requests_queued, 1);
    assert_eq!(resumed.outstanding, 3);
    assert_eq!(fx.queue.approximate_count(PRIORITY).await.unwrap(), 2);
    assert_eq!(
        fx.requests.get("days/2026-10-14.jsonl").await.unwrap(),
        Some(Vec::new())
    );
}

#[tokio::test]
async fn request_failure_publishes_completed_import_counts_before_returning_the_error() {
    let fx = fixture(true).await;
    fx.ask(&[queued(&key(3).doc_id(), BATCH, "bulk")]).await;
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    fx.requests
        .put_new(
            "days/2026-10-14.jsonl",
            serde_json::to_vec(&serde_json::json!({"v":1,"doc_id":key(1).doc_id()})).unwrap(),
            "application/x-ndjson",
        )
        .await
        .unwrap();
    let stores = Stores {
        queue: Arc::new(FailFirstQueue {
            inner: Arc::clone(&fx.queue),
            failed: std::sync::atomic::AtomicBool::new(false),
        }),
        ..fx.stores.clone()
    };
    let err = sync::run(&fx.state, "test", &stores, &Options::default())
        .await
        .unwrap_err();
    assert!(
        format!("{err:#}").contains("injected first-send failure"),
        "{err:#}"
    );
    assert!(
        fx.out_paths().await.is_empty(),
        "successful import deleted its source"
    );
    let published: Status =
        serde_json::from_slice(&fx.stores.reference.get(STATUS_PATH).await.unwrap().unwrap())
            .unwrap();
    assert_eq!(published.last_run.imported, 1);
    assert_eq!(published.totals.imported, 1);
    assert_eq!(
        published.outstanding, 1,
        "ambiguous request send is reflected in status"
    );
    let resumed = fx.sync().await;
    assert_eq!(resumed.last_run.imported, 0);
    assert_eq!(
        resumed.totals.imported, 1,
        "completed import remains counted exactly once"
    );
}

/// The runs record a worker writes for `page` (13 §13.8.3).
fn runs_row(page: &OverlayRow, stamp: &str) -> usnm_ocr_contract::runs::RunRecord {
    usnm_ocr_contract::runs::RunRecord {
        run_id: format!("{NODE}.{stamp}"),
        doc_id: page.key.doc_id(),
        node: NODE.into(),
        worker_version: "0.1.0+1dcc5c3".into(),
        contract: 1,
        ocr_engine: page.ocr_engine.clone(),
        engine_id: "tesseract-5.5".into(),
        family: "tesseract".into(),
        engine_version: "5.5.3".into(),
        adapter: "usnm-ocr-adapter-tesseract 0.1.0 (rust)".into(),
        image_ref: "local".into(),
        image_digest: "sha256:3f9a".into(),
        models_digest: "sha256:8c2d".into(),
        models_json: "{}".into(),
        profile: "deu".into(),
        options_json: "{\"psm\":3}".into(),
        scale: 1.0,
        tiles: 0,
        route_id: "german-pre1920".into(),
        routing_version: 1,
        routing_digest: None,
        candidate_rank: 0,
        selection: Some("route".into()),
        selected: true,
        status: "ok".into(),
        error_kind: None,
        seconds_wall: 41.5,
        seconds_cpu: Some(40.25),
        peak_mem_mb: Some(512.0),
        instance_peak_mem_mb: None,
        oom: false,
        instance_restarts: 0,
        model_load_seconds: Some(0.5),
        models_loaded: "per_page".into(),
        cold_start: true,
        image_sha256: "02".into(),
        image_width: 5412,
        image_height: 7208,
        dpi: 300,
        dpi_source: "assumed".into(),
        granularity: "word".into(),
        reading_order: None,
        markup: "none".into(),
        blocks: Some(12),
        lines: Some(400),
        words: Some(6034),
        mean_conf: Some(0.71),
        text_chars: Some(30000),
        text_sha256: Some("ab".into()),
        engine_stats_json: "{}".into(),
        damage_before: Some(0.62),
        damage_after: None,
        language: "deu".into(),
        script: Some("Latf".into()),
        reason: "bulk".into(),
        queued_at: None,
        node_finished_at: "2026-10-20T14:04:31.123456Z".parse().unwrap(),
    }
}

impl Fx {
    /// What a worker writes for a page since #307: its ALTO, its runs
    /// record (`runs`'s bytes), then its part.
    async fn node_writes_with_runs(
        &self,
        row: &OverlayRow,
        runs: Vec<u8>,
        stamp: &str,
    ) -> (String, String, String) {
        let k = &row.key;
        let issue = format!("{}_{}_ed-{}", k.lccn, k.date.format("%Y-%m-%d"), k.edition);
        let alto_path = format!("alto/{}.{NODE}.{stamp}.xml", k.doc_id());
        let runs_path = format!("runs/{issue}.{NODE}.{stamp}.parquet");
        let part_path = format!("pages/{issue}.{NODE}.{stamp}.parquet");
        for (path, bytes) in [
            (&alto_path, ALTO.as_bytes().to_vec()),
            (&runs_path, runs),
            (
                &part_path,
                usnm_overlay::write_part(std::slice::from_ref(row)).unwrap(),
            ),
        ] {
            assert!(self.out.put_new(path, bytes, "x").await.unwrap());
        }
        (alto_path, runs_path, part_path)
    }
}

#[tokio::test]
async fn imports_a_page_with_its_runs_record() {
    let fx = fixture(false).await;
    let page = row(3, &GERMAN.repeat(4));
    let doc = page.key.doc_id();
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    let record = runs_row(&page, &stamp(1));
    let bytes = usnm_ocr_contract::runs::write(std::slice::from_ref(&record)).unwrap();
    let (alto_src, runs_src, part_src) = fx.node_writes_with_runs(&page, bytes, &stamp(1)).await;

    let status = fx.sync().await;
    assert_eq!(status.last_run.imported, 1, "{:?}", status.last_run);
    assert!(
        fx.out.list("runs").await.unwrap().is_empty(),
        "runs record consumed"
    );
    assert!(fx.out_paths().await.is_empty());

    // The curated copy: the checked rows, with the part's server time.
    let c = &fx.stores.curated;
    let dest = super::runs_path(&format!("{LCCN}_1890-05-03_ed-1"), NODE, &stamp(1));
    assert_eq!(
        c.list(&format!("{ROOT}/runs")).await.unwrap(),
        vec![dest.clone()]
    );
    let curated = c.get(&dest).await.unwrap().unwrap();
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
        bytes::Bytes::from(curated),
    )
    .unwrap();
    assert_eq!(
        reader.schema().fields(),
        usnm_ocr_contract::runs::curated_schema().fields()
    );
    let batch = reader.build().unwrap().next().unwrap().unwrap();
    assert_eq!(batch.num_rows(), 1);
    let parts = c.list(&format!("{ROOT}/pages/{LCCN}")).await.unwrap();
    let overlay =
        usnm_overlay::read_part(bytes::Bytes::from(c.get(&parts[0]).await.unwrap().unwrap()))
            .unwrap();
    let ocred_at = batch
        .column_by_name("ocred_at")
        .unwrap()
        .as_any()
        .downcast_ref::<arrow_array::TimestampMicrosecondArray>()
        .unwrap()
        .value(0);
    assert_eq!(ocred_at, overlay[0].ocred_at);

    let records = c.list(&format!("{ROOT}/import")).await.unwrap();
    let rec: ImportRecord =
        serde_json::from_slice(&c.get(&records[0]).await.unwrap().unwrap()).unwrap();
    let mut srcs: Vec<&str> = rec.sources.iter().map(|s| s.path.as_str()).collect();
    srcs.sort();
    assert_eq!(
        srcs,
        [alto_src.as_str(), part_src.as_str(), runs_src.as_str()]
    );
}

/// Check 7: each way a runs record can fail, and the record goes to
/// quarantine with its part and ALTO.
#[tokio::test]
async fn check_7_refuses_a_bad_runs_record() {
    type Case = (&'static str, Box<dyn Fn(&OverlayRow) -> Vec<u8>>);
    let write =
        |r: usnm_ocr_contract::runs::RunRecord| usnm_ocr_contract::runs::write(&[r]).unwrap();
    let cases: Vec<Case> = vec![
        ("runs_unreadable", Box::new(|_| b"not parquet".to_vec())),
        (
            "runs_too_large",
            Box::new(|_| vec![0u8; checks::MAX_PART_BYTES as usize + 1]),
        ),
        (
            "runs_mismatch",
            Box::new(move |p| {
                let mut r = runs_row(p, &stamp(1));
                r.run_id = format!("{NODE}.{}", stamp(9));
                write(r)
            }),
        ),
        (
            "runs_mismatch",
            Box::new(move |p| {
                let mut r = runs_row(p, &stamp(1));
                r.doc_id = key(4).doc_id();
                write(r)
            }),
        ),
        (
            "runs_mismatch",
            Box::new(move |p| {
                let mut r = runs_row(p, &stamp(1));
                r.node = "node2".into();
                write(r)
            }),
        ),
        (
            "runs_engine",
            Box::new(move |p| {
                let mut r = runs_row(p, &stamp(1));
                r.ocr_engine = "tesseract-5.4.0/deu".into();
                write(r)
            }),
        ),
    ];
    for (code, bad) in cases {
        let fx = fixture(false).await;
        let page = row(3, &GERMAN.repeat(4));
        fx.ask(&[queued(&page.key.doc_id(), BATCH, "bulk")]).await;
        let (alto_src, runs_src, part_src) =
            fx.node_writes_with_runs(&page, bad(&page), &stamp(1)).await;
        let status = fx.sync().await;
        assert_eq!(status.last_run.imported, 0, "{code}");
        assert_eq!(
            status.last_run.rejected.get(code),
            Some(&1),
            "{code}: {:?}",
            status.last_run
        );
        assert!(fx.out.list("runs").await.unwrap().is_empty(), "{code}");
        assert!(fx.out_paths().await.is_empty(), "{code}");
        let mut kept: Vec<(String, String, bool)> = fx
            .records()
            .await
            .into_iter()
            .map(|r| (r.kind, r.source, r.content.is_some()))
            .collect();
        kept.sort();
        let too_large = code == "runs_too_large";
        assert_eq!(
            kept,
            [
                (quarantine::ALTO.to_owned(), alto_src, true),
                (quarantine::PART.to_owned(), part_src, true),
                (quarantine::RUNS.to_owned(), runs_src, !too_large),
            ],
            "{code}"
        );
        assert!(fx
            .stores
            .curated
            .list(&format!("{ROOT}/runs"))
            .await
            .unwrap()
            .is_empty());
    }
}

/// A runs record quarantined with its part comes back with it.
#[tokio::test]
async fn a_replay_puts_the_runs_record_back_too() {
    let fx = fixture(false).await;
    let page = row(3, &GERMAN.repeat(4));
    let bytes = usnm_ocr_contract::runs::write(&[runs_row(&page, &stamp(1))]).unwrap();
    fx.node_writes_with_runs(&page, bytes, &stamp(1)).await;
    let first = fx.sync().await;
    assert_eq!(first.last_run.rejected.get("not_requested"), Some(&1));
    assert_eq!(fx.records().await.len(), 3);
    fx.ask(&[queued(&page.key.doc_id(), BATCH, "bulk")]).await;
    let opts = Options {
        from_quarantine: Some("not_requested".into()),
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.imported, 1, "{:?}", status.last_run);
    assert!(fx.records().await.is_empty());
    assert_eq!(
        fx.stores
            .curated
            .list(&format!("{ROOT}/runs"))
            .await
            .unwrap()
            .len(),
        1
    );
}

impl Fx {
    /// A page with a good runs record, quarantined as `not_requested`, and
    /// its ledger row then written: ready to replay.
    async fn quarantined_with_runs(&self) -> OverlayRow {
        let page = row(3, &GERMAN.repeat(4));
        let bytes = usnm_ocr_contract::runs::write(&[runs_row(&page, &stamp(1))]).unwrap();
        self.node_writes_with_runs(&page, bytes, &stamp(1)).await;
        let first = self.sync().await;
        assert_eq!(first.last_run.rejected.get("not_requested"), Some(&1));
        self.ask(&[queued(&page.key.doc_id(), BATCH, "bulk")]).await;
        page
    }

    async fn replay(
        &self,
        out: &dyn ObjectStore,
        reason: &str,
    ) -> anyhow::Result<quarantine::Replayed> {
        quarantine::replay(self.stores.quarantine.as_ref(), out, &self.queue, reason).await
    }

    async fn all_out(&self) -> Vec<String> {
        let mut v = self.out_paths().await;
        v.extend(self.out.list("runs").await.unwrap());
        v.sort();
        v
    }

    /// The quarantine record paths, by kind.
    async fn record_paths(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for p in self.stores.quarantine.list(quarantine::TOP).await.unwrap() {
            if p.ends_with(".reason.json") {
                let b = self.stores.quarantine.get(&p).await.unwrap().unwrap();
                let r: Record = serde_json::from_slice(&b).unwrap();
                out.push((r.kind, p));
            }
        }
        out
    }
}

/// A part's record names its ALTO and runs record.
#[tokio::test]
async fn a_quarantined_part_names_its_companions() {
    let fx = fixture(false).await;
    let page = row(3, &GERMAN.repeat(4));
    let bytes = usnm_ocr_contract::runs::write(&[runs_row(&page, &stamp(1))]).unwrap();
    let (alto_src, runs_src, _) = fx.node_writes_with_runs(&page, bytes, &stamp(1)).await;
    fx.sync().await;
    let part = fx
        .records()
        .await
        .into_iter()
        .find(|r| r.kind == quarantine::PART)
        .unwrap();
    assert_eq!(part.companions, [alto_src, runs_src]);
}

/// A runs record over the size limit isn't kept, so its part can never be
/// replayed: it would pass as one from a worker without runs records.
#[tokio::test]
async fn a_replay_keeps_a_part_whose_runs_record_was_too_large() {
    let fx = fixture(false).await;
    let page = row(3, &GERMAN.repeat(4));
    fx.ask(&[queued(&page.key.doc_id(), BATCH, "bulk")]).await;
    let big = vec![0u8; checks::MAX_PART_BYTES as usize + 1];
    fx.node_writes_with_runs(&page, big, &stamp(1)).await;
    let first = fx.sync().await;
    assert_eq!(first.last_run.rejected.get("runs_too_large"), Some(&1));

    let r = fx.replay(fx.out.as_ref(), "runs_too_large").await.unwrap();
    assert_eq!(r.incomplete, 1, "{r:?}");
    assert_eq!((r.parts, r.alto, r.runs), (0, 0, 0), "{r:?}");
    assert!(fx.all_out().await.is_empty(), "nothing put back");
    assert_eq!(fx.records().await.len(), 3, "all kept in quarantine");

    let opts = Options {
        from_quarantine: Some("runs_too_large".into()),
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.imported, 0, "{:?}", status.last_run);
    assert_eq!(fx.records().await.len(), 3);
}

/// A part whose runs record's quarantine entry is gone (a stop between the
/// part's record and the runs record's) stays in quarantine.
#[tokio::test]
async fn a_replay_keeps_a_part_whose_runs_record_is_missing() {
    let fx = fixture(false).await;
    fx.quarantined_with_runs().await;
    let q = &fx.stores.quarantine;
    let etags: std::collections::HashMap<String, String> = q
        .list_info(quarantine::TOP)
        .await
        .unwrap()
        .into_iter()
        .map(|i| (i.path, i.etag))
        .collect();
    for (kind, path) in fx.record_paths().await {
        if kind == quarantine::RUNS {
            let r: Record = serde_json::from_slice(&q.get(&path).await.unwrap().unwrap()).unwrap();
            for p in [r.content.unwrap(), path] {
                assert!(q.delete_if_match(&p, &etags[&p]).await.unwrap());
            }
        }
    }
    let r = fx.replay(fx.out.as_ref(), "not_requested").await.unwrap();
    assert_eq!(r.incomplete, 1, "{r:?}");
    assert_eq!(r.alto, 0, "the ALTO stays with its part: {r:?}");
    assert!(fx.all_out().await.is_empty());
    assert_eq!(fx.records().await.len(), 2);
}

/// `ocr-out` that refuses parts, to stop a replay midway.
#[derive(Debug)]
struct NoParts(Arc<dyn ObjectStore>);

#[async_trait::async_trait]
impl ObjectStore for NoParts {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        self.0.get(path).await
    }
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, usnm_store::StoreError> {
        if path.starts_with("pages/") {
            return Err(usnm_store::StoreError::Io("stopped".into()));
        }
        self.0.put_new(path, body, content_type).await
    }
    async fn put(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), usnm_store::StoreError> {
        self.0.put(path, body, content_type).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        self.0.list(prefix).await
    }
}

/// A replay stopped before the part leaves its ALTO and runs record in
/// `ocr-out` and every entry in quarantine; never a part without its runs
/// record. The next replay finishes it.
#[tokio::test]
async fn an_interrupted_replay_never_leaves_a_part_without_its_runs_record() {
    let fx = fixture(false).await;
    fx.quarantined_with_runs().await;
    let stopped = NoParts(Arc::clone(&fx.out));
    assert!(fx.replay(&stopped, "not_requested").await.is_err());
    let out = fx.all_out().await;
    assert!(out.iter().any(|p| p.starts_with("runs/")), "{out:?}");
    assert!(!out.iter().any(|p| p.starts_with("pages/")), "{out:?}");
    assert_eq!(fx.records().await.len(), 3);

    // A normal pass meanwhile imports nothing.
    assert_eq!(fx.sync().await.last_run.imported, 0);

    let opts = Options {
        from_quarantine: Some("not_requested".into()),
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.imported, 1, "{:?}", status.last_run);
    assert!(fx.records().await.is_empty());
    assert_eq!(
        fx.stores
            .curated
            .list(&format!("{ROOT}/runs"))
            .await
            .unwrap()
            .len(),
        1
    );
}

/// A part's record from before companions were named replays as before.
#[tokio::test]
async fn a_legacy_part_record_replays_on_its_own() {
    let fx = fixture(false).await;
    let doc = key(3).doc_id();
    fx.node_writes(&row(3, &GERMAN.repeat(4)), ALTO, &stamp(1))
        .await;
    fx.sync().await;
    let q = &fx.stores.quarantine;
    for (kind, path) in fx.record_paths().await {
        if kind == quarantine::PART {
            let mut v: serde_json::Value =
                serde_json::from_slice(&q.get(&path).await.unwrap().unwrap()).unwrap();
            assert!(v.as_object_mut().unwrap().remove("companions").is_some());
            q.put(&path, serde_json::to_vec(&v).unwrap(), "application/json")
                .await
                .unwrap();
        }
    }
    fx.ask(&[queued(&doc, BATCH, "bulk")]).await;
    let opts = Options {
        from_quarantine: Some("not_requested".into()),
        ..Options::default()
    };
    let status = sync::run(&fx.state, "test", &fx.stores, &opts)
        .await
        .unwrap();
    assert_eq!(status.last_run.imported, 1, "{:?}", status.last_run);
    assert!(fx.records().await.is_empty());
}
