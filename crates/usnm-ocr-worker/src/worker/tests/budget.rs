//! A page's budget and the stops: pages abandoned without output, and the
//! planned stop that drains.

use super::super::*;
use super::{engine_runs, msg, worker, FakeEngine};
use crate::queue::LocalQueue;
use async_trait::async_trait;

#[derive(Debug)]
struct SlowEngine;

#[async_trait]
impl Engine for SlowEngine {
    fn source(&self) -> &str {
        "usnm-slow"
    }
    fn can_read(&self, _: &str) -> bool {
        true
    }
    async fn ocr(&self, _: &Path, _: &str, _: &Path) -> Result<EngineOutput, EngineError> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        unreachable!()
    }
}

fn slow_worker(
    dir: &Path,
    visibility: Duration,
) -> (Arc<Worker>, Arc<LocalQueue>, Arc<dyn ObjectStore>) {
    let (w, queue, out) = worker(dir);
    let w = Arc::new(Worker {
        engine: Arc::new(SlowEngine),
        cfg: Config {
            once: false,
            visibility,
            shutdown_grace: Duration::from_millis(50),
            ..w.cfg.clone()
        },
        queue: w.queue.clone(),
        out: w.out.clone(),
        scans: w.scans.clone(),
        metrics: w.metrics.clone(),
        tracker: w.tracker.clone(),
    });
    (w, queue, out)
}

#[tokio::test]
async fn a_page_over_its_budget_is_abandoned_without_output() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = slow_worker(dir.path(), Duration::from_secs(2));
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-7", "deu"))
        .await
        .unwrap();
    let m = queue
        .receive(BULK, w.cfg.visibility)
        .await
        .unwrap()
        .unwrap();
    let t0 = Instant::now();
    let runs = engine_runs(async {
        tokio::time::timeout(Duration::from_secs(10), w.handle(m, t0))
            .await
            .unwrap();
    })
    .await;
    // The engine was called and nothing was written.
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        (runs[0]["status"].as_str(), runs[0]["error_kind"].as_str()),
        ("abandoned", "budget")
    );
    assert_eq!(runs[0]["ocr_engine"], "");
    assert!(t0.elapsed() >= Duration::from_millis(1600));
    assert!(
        t0.elapsed() < Duration::from_secs(2),
        "gave up before its visibility ended"
    );
    assert!(out.list("pages").await.unwrap().is_empty());
    assert!(out.list("alto").await.unwrap().is_empty());
    // Not deleted: it reappears once its visibility runs out.
    tokio::time::sleep(Duration::from_millis(500)).await;
    let again = queue
        .receive(BULK, w.cfg.visibility)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(again.dequeue_count, 2);
}

#[tokio::test]
async fn a_stop_abandons_unfinished_pages_after_the_grace() {
    let dir = tempfile::tempdir().unwrap();
    let (w, queue, out) = slow_worker(dir.path(), Duration::from_secs(2));
    queue
        .send(BULK, &msg("sn83045462/1890-05-03/ed-1/seq-7", "deu"))
        .await
        .unwrap();
    let (tx, rx) = watch::channel(false);
    let tracker = w.tracker.clone();
    let t0 = Instant::now();
    let runs = engine_runs(async {
        let run = tokio::spawn(w.run(rx));
        tokio::time::sleep(Duration::from_millis(300)).await;
        tx.send_replace(true);
        tokio::time::timeout(Duration::from_secs(10), run)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    })
    .await;
    assert!(
        t0.elapsed() < Duration::from_secs(1),
        "stopped after the grace"
    );
    // The read cut short is reported once, in the page's span, with
    // no page text.
    assert_eq!(runs.len(), 1, "{runs:?}");
    assert_eq!(
        (runs[0]["status"].as_str(), runs[0]["error_kind"].as_str()),
        ("abandoned", "shutdown")
    );
    assert_eq!(runs[0]["doc_id"], "sn83045462_1890-05-03_ed-1_seq-7");
    assert_eq!(runs[0]["span"], "ocr_page");
    assert!(runs[0].keys().all(|k| !k.contains("text")), "{:?}", runs[0]);
    // The tracker ends the dropped page as abandoned at shutdown.
    let st = tracker.status();
    assert!(st.in_flight.is_empty());
    assert_eq!(st.since_start.abandoned, 1);
    assert_eq!(st.recent[0].error_kind.as_deref(), Some("shutdown"));
    assert!(out.list("pages").await.unwrap().is_empty());
    // Hidden until its visibility runs out, then back for another node.
    assert_eq!(
        queue.receive(BULK, Duration::from_secs(600)).await.unwrap(),
        None
    );
    tokio::time::sleep(Duration::from_secs(2)).await;
    let m = queue
        .receive(BULK, Duration::from_secs(600))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(m.dequeue_count, 2);
}

#[derive(Debug)]
struct DelayedEngine;

#[async_trait]
impl Engine for DelayedEngine {
    fn source(&self) -> &str {
        "usnm-tesseract"
    }

    fn can_read(&self, language: &str) -> bool {
        language != "haw"
    }

    async fn ocr(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
    ) -> Result<EngineOutput, EngineError> {
        tokio::time::sleep(Duration::from_millis(200)).await;
        FakeEngine.ocr(image, language, work).await
    }
}

#[tokio::test]
async fn a_planned_stop_drains_in_flight_pages_and_stops_intake() {
    let dir = tempfile::tempdir().unwrap();
    let (base, queue, out) = worker(dir.path());
    let w = Arc::new(Worker {
        engine: Arc::new(DelayedEngine),
        cfg: Config {
            concurrency: 1,
            shutdown_grace: Duration::from_millis(50),
            drain_on_stop: true,
            page_timeout: Duration::from_secs(1),
            ..base.cfg.clone()
        },
        queue: base.queue.clone(),
        out: base.out.clone(),
        scans: base.scans.clone(),
        metrics: base.metrics.clone(),
        tracker: base.tracker.clone(),
    });
    for seq in [7, 8] {
        queue
            .send(
                BULK,
                &msg(&format!("sn83045462/1890-05-03/ed-1/seq-{seq}"), "deu"),
            )
            .await
            .unwrap();
    }

    let (tx, rx) = watch::channel(false);
    let run = tokio::spawn(w.clone().run(rx));
    tokio::time::timeout(Duration::from_secs(2), async {
        while w.tracker.status().in_flight.is_empty() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    let started = Instant::now();
    tx.send_replace(true);
    tokio::time::timeout(Duration::from_secs(2), run)
        .await
        .unwrap()
        .unwrap()
        .unwrap();

    assert!(started.elapsed() >= Duration::from_millis(150));
    assert_eq!(w.tracker.status().since_start.done, 1);
    assert_eq!(out.list("pages").await.unwrap().len(), 1);
    let pending = queue
        .receive(BULK, Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(pending.dequeue_count, 1);
}
