//! The `ocr_engine_run` event of a read that writes nothing (13 §13.8.3).

use super::super::*;
use super::{
    engine_runs, msg, one_page, with_engine, worker, BadAltoEngine, ColdInstanceEngine,
    FailingEngine, FakeScans, NoParts,
};
use async_trait::async_trait;

#[tokio::test]
async fn a_read_that_writes_nothing_sends_an_engine_run_event() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = worker(dir.path());
    let failing = with_engine(&w, Arc::new(FailingEngine));
    let runs = engine_runs(one_page(
        &failing,
        &queue,
        "sn83045462/1890-05-03/ed-1/seq-7",
        "deu",
    ))
    .await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let e = &runs[0];
    assert_eq!(e["level"], "WARN");
    assert_eq!(e["status"], "timeout");
    assert_eq!(e["error_kind"], "engine_timeout");
    assert_eq!(e["doc_id"], "sn83045462_1890-05-03_ed-1_seq-7");
    assert_eq!(e["node"], "node1");
    assert!(e["run_id"].starts_with("node1."), "{e:?}");
    // The engine's side, from `describe` before the read.
    assert_eq!(e["family"], "tesseract");
    assert_eq!(e["engine_version"], "5.5.0");
    assert_eq!(e["profile"], "frak2021");
    assert_eq!(e["engine_id"], "tesseract-5.5");
    assert_eq!(e["adapter"], "usnm-ocr-adapter-tesseract 0.1.0 (rust)");
    assert_eq!(e["route_id"], "minimal");
    assert_eq!(e["routing_version"], "1");
    assert_eq!(e["contract"], "0");
    assert_eq!(e["selected"], "false");
    assert_eq!(e["oom"], "false");
    assert_eq!(e["models_loaded"], "at_start");
    assert!(e["seconds_wall"].parse::<f64>().unwrap() >= 0.0);
    assert!(out.list("runs").await.unwrap().is_empty());
    drop(failing);

    // A JP2 crash with status 1 has no JPEG fallback. (A fresh queue:
    // the failed page is back already, its visibility being zero.)
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, _) = worker(dir.path());
    let failing = with_engine(&w, Arc::new(FailingEngine));
    let runs = engine_runs(one_page(
        &failing,
        &queue,
        "sn83045462/1890-05-03/ed-1/seq-95",
        "deu",
    ))
    .await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        (runs[0]["status"].as_str(), runs[0]["error_kind"].as_str()),
        ("failed", "engine_crash")
    );

    // No read, no event: no model here, or a page written.
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = worker(dir.path());
    let runs = engine_runs(async {
        one_page(&w, &queue, "sn83045462/1890-05-03/ed-1/seq-9", "deu").await;
        one_page(&w, &queue, "sn83045462/1890-05-03/ed-1/seq-8", "haw").await;
    })
    .await;
    assert!(runs.is_empty(), "{runs:?}");
    assert_eq!(out.list("runs").await.unwrap().len(), 1);
}

/// Scans to evict, whose eviction never ends.
#[derive(Debug, Default)]
struct StuckEvict(FakeScans);

#[async_trait]
impl ScanSource for StuckEvict {
    async fn scan(&self, key: &PageKey, batch: &str) -> Result<Scan, FetchError> {
        let mut s = self.0.scan(key, batch).await?;
        s.evict = Some(s.url.clone());
        Ok(s)
    }
    async fn jpeg(&self, key: &PageKey, batch: &str, base: &str) -> Result<Scan, FetchError> {
        self.0.jpeg(key, batch, base).await
    }
    async fn evict(&self, _url: &str) {
        std::future::pending::<()>().await
    }
}

/// Once its runs record is uploaded a read has its record: a failure
/// after it (the part's upload, a stop while the scan is evicted)
/// sends no `ocr_engine_run` of its own.
#[tokio::test]
async fn a_read_with_its_runs_record_sends_no_failure_event() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = worker(dir.path());
    let no_parts = Arc::new(Worker {
        out: Arc::new(NoParts(out.clone())),
        cfg: w.cfg.clone(),
        engine: w.engine.clone(),
        queue: w.queue.clone(),
        scans: w.scans.clone(),
        metrics: w.metrics.clone(),
        tracker: w.tracker.clone(),
    });
    let runs = engine_runs(one_page(
        &no_parts,
        &queue,
        "sn83045462/1890-05-03/ed-1/seq-7",
        "deu",
    ))
    .await;
    assert!(runs.is_empty(), "{runs:?}");
    assert_eq!(out.list("runs").await.unwrap().len(), 1);
    assert!(out.list("pages").await.unwrap().is_empty());
    // The page failed and is back for another try.
    let again = queue.receive(BULK, Duration::ZERO).await.unwrap().unwrap();
    assert_eq!(again.dequeue_count, 2);

    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = worker(dir.path());
    let w = Arc::new(Worker {
        scans: Arc::new(StuckEvict::default()),
        cfg: Config {
            once: false,
            visibility: Duration::from_secs(60),
            shutdown_grace: Duration::from_millis(50),
            ..w.cfg.clone()
        },
        out: w.out.clone(),
        engine: w.engine.clone(),
        queue: w.queue.clone(),
        metrics: w.metrics.clone(),
        tracker: w.tracker.clone(),
    });
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-7", "deu"))
        .await
        .unwrap();
    let (tx, rx) = watch::channel(false);
    let runs = engine_runs(async {
        let run = tokio::spawn(w.run(rx));
        // Stopped while the uploaded page's scan is evicted.
        let t0 = Instant::now();
        while out.list("pages").await.unwrap().is_empty() {
            assert!(t0.elapsed() < Duration::from_secs(10), "no part");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    })
    .await;
    assert!(runs.is_empty(), "{runs:?}");
    assert_eq!(out.list("runs").await.unwrap().len(), 1);
}

/// A failed read reports the instance it got, not `describe`'s guess.
#[tokio::test]
async fn a_failed_read_reports_its_instance() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, _) = worker(dir.path());
    let cold = with_engine(&w, Arc::new(ColdInstanceEngine));
    let runs = engine_runs(one_page(
        &cold,
        &queue,
        "sn83045462/1890-05-03/ed-1/seq-7",
        "deu",
    ))
    .await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(runs[0]["cold_start"], "true");
    assert_eq!(runs[0]["model_load_seconds"], "2.5");
    // The tag is still the planned one.
    assert_eq!(runs[0]["profile"], "frak2021");
    assert_eq!(runs[0]["error_kind"], "engine_timeout");
}

#[tokio::test]
async fn a_convert_failure_sends_its_engine_run_without_text() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, _out) = worker(dir.path());
    let bad = with_engine(&w, Arc::new(BadAltoEngine));
    let runs = engine_runs(one_page(
        &bad,
        &queue,
        "sn83045462/1890-05-03/ed-1/seq-7",
        "deu",
    ))
    .await;
    assert_eq!(runs.len(), 1, "{runs:?}");
    let e = &runs[0];
    assert_eq!(
        (e["status"].as_str(), e["error_kind"].as_str()),
        ("failed", "convert")
    );
    // The tag the read had, in its older form.
    assert_eq!(e["ocr_engine"], "tesseract-5.5.1/deu_latf+deu");
    assert_eq!(e["profile"], "deu_latf+deu");
    assert!(!format!("{e:?}").contains("secret"), "no page text: {e:?}");
}
