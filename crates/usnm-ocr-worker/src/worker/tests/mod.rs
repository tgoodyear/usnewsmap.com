//! The tests of [`super`], in four groups, with the fakes they share.

use super::*;
use crate::message::Reason;
use crate::queue::LocalQueue;
use async_trait::async_trait;

mod budget;
mod engine_run_events;
mod pipeline;
mod run_metadata;

/// Counts scan requests; a hold, when set, is reported as loc.gov's.
/// Pages 95 to 97 come as JP2s (see [`FakeEngine`]).
#[derive(Debug, Default)]
pub(super) struct FakeScans {
    requests: std::sync::atomic::AtomicUsize,
    jpeg_requests: std::sync::atomic::AtomicUsize,
    held_until: std::sync::Mutex<Option<tokio::time::Instant>>,
}

#[async_trait]
impl ScanSource for FakeScans {
    fn held_until(&self) -> Option<tokio::time::Instant> {
        *self.held_until.lock().unwrap()
    }

    async fn scan(&self, key: &PageKey, _batch: &str) -> Result<Scan, FetchError> {
        self.requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if key.seq == 99 {
            return Err(FetchError::Permanent("no image".into()));
        }
        if key.seq == 98 {
            return Err(FetchError::Transient("HTTP 500".into()));
        }
        let iiif_base = format!(
            "https://tile.loc.gov/image-services/iiif/service:ndnp:x:{}",
            key.seq
        );
        if (95..=97).contains(&key.seq) {
            let body: &'static [u8] = match key.seq {
                95 => b"jp2 exit 1",
                _ => b"jp2 exit 136",
            };
            return Ok(Scan {
                url: format!(
                    "https://tile.loc.gov/storage-services/service/x/{}.jp2",
                    key.seq
                ),
                iiif_base,
                format: crate::loc::ScanFormat::Jp2,
                bytes: bytes::Bytes::from_static(body),
                md5: Some("0123".into()),
                download: std::time::Duration::ZERO,
                from_cache: false,
                evict: None,
            });
        }
        Ok(Scan {
            url: format!("{iiif_base}/full/full/0/default.jpg"),
            iiif_base,
            format: crate::loc::ScanFormat::Jpeg,
            bytes: bytes::Bytes::from_static(b"jpeg"),
            md5: None,
            download: std::time::Duration::ZERO,
            from_cache: false,
            evict: None,
        })
    }

    /// Page 96's JPEG crashes the engine too.
    async fn jpeg(&self, key: &PageKey, _batch: &str, iiif_base: &str) -> Result<Scan, FetchError> {
        self.jpeg_requests
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(Scan {
            url: format!("{iiif_base}/full/full/0/default.jpg"),
            iiif_base: iiif_base.into(),
            format: crate::loc::ScanFormat::Jpeg,
            bytes: bytes::Bytes::from_static(if key.seq == 96 {
                b"jpeg exit 136"
            } else {
                b"jpeg"
            }),
            md5: None,
            download: std::time::Duration::ZERO,
            from_cache: false,
            evict: None,
        })
    }
}

/// An engine exit with `code`, as Tesseract's would be.
fn exited(code: i32) -> EngineError {
    use std::os::unix::process::ExitStatusExt;
    EngineError::Failed(anyhow::Error::new(crate::engine::Exited {
        status: std::process::ExitStatus::from_raw(code << 8),
        stderr: String::new(),
    }))
}

/// Reads `jpeg`; a scan whose bytes say `exit <code>` exits with it.
#[derive(Debug)]
pub(super) struct FakeEngine;

#[async_trait]
impl Engine for FakeEngine {
    fn source(&self) -> &str {
        "usnm-tesseract"
    }
    /// Every language but Hawaiian.
    fn can_read(&self, language: &str) -> bool {
        language != "haw"
    }
    async fn ocr(
        &self,
        image: &Path,
        language: &str,
        _: &Path,
    ) -> Result<EngineOutput, EngineError> {
        let bytes = std::fs::read(image).unwrap();
        if let Some(code) = std::str::from_utf8(&bytes)
            .unwrap()
            .split_once(" exit ")
            .map(|(_, c)| c.parse().unwrap())
        {
            return Err(exited(code));
        }
        assert_eq!(bytes, b"jpeg");
        if language == "haw" {
            return Err(EngineError::Unsupported(language.into()));
        }
        Ok(EngineOutput {
            text: "Die  Nebraska Staats-Zeitung\nerscheint  jeden\u{0007} Sonn-\nabend.\n".into(),
            alto: FAKE_ALTO.as_bytes().to_vec(),
            page: None,
            layout_from: "ALTO",
            engine: "tesseract-5.5.1/deu_latf+deu".into(),
            read: crate::engine::ReadInfo::local("tesseract-5.5.1"),
        })
    }
}

/// ALTO as Tesseract 5 writes it, with the fake engine's text.
const FAKE_ALTO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#" xmlns:xsi="http://www.w3.org/2001/XMLSchema-instance" xsi:schemaLocation="http://www.loc.gov/standards/alto/ns-v3# http://www.loc.gov/alto/v3/alto-3-0.xsd">
 <Description><MeasurementUnit>pixel</MeasurementUnit><sourceImageInformation><fileName>/tmp/page.jpg</fileName></sourceImageInformation>
  <OCRProcessing ID="OCR_0"><ocrProcessingStep><processingSoftware><softwareName>tesseract 5.5.1</softwareName></processingSoftware></ocrProcessingStep></OCRProcessing></Description>
 <Layout><Page WIDTH="1000" HEIGHT="800" PHYSICAL_IMG_NR="0" ID="page_0"><PrintSpace HPOS="0" VPOS="0" WIDTH="1000" HEIGHT="800">
  <ComposedBlock ID="cblock_0" HPOS="10" VPOS="10" WIDTH="500" HEIGHT="100"><TextBlock ID="block_0" HPOS="10" VPOS="10" WIDTH="500" HEIGHT="100">
   <TextLine ID="line_0" HPOS="10" VPOS="10" WIDTH="500" HEIGHT="30"><String ID="string_0" HPOS="10" VPOS="10" WIDTH="40" HEIGHT="30" WC="0.96" CONTENT="Die"/><SP WIDTH="10" VPOS="10" HPOS="50"/><String ID="string_1" HPOS="60" VPOS="10" WIDTH="150" HEIGHT="30" WC="0.95" CONTENT="Nebraska"/><SP WIDTH="10" VPOS="10" HPOS="210"/><String ID="string_2" HPOS="220" VPOS="10" WIDTH="290" HEIGHT="30" WC="0.9" CONTENT="Staats-Zeitung"/></TextLine>
   <TextLine ID="line_1" HPOS="10" VPOS="45" WIDTH="400" HEIGHT="30"><String ID="string_3" HPOS="10" VPOS="45" WIDTH="150" HEIGHT="30" WC="0.93" CONTENT="erscheint"/><SP WIDTH="10" VPOS="45" HPOS="160"/><String ID="string_4" HPOS="170" VPOS="45" WIDTH="100" HEIGHT="30" WC="0.91" CONTENT="jeden"/><SP WIDTH="10" VPOS="45" HPOS="270"/><String ID="string_5" HPOS="280" VPOS="45" WIDTH="100" HEIGHT="30" WC="0.88" CONTENT="Sonn-"/></TextLine>
   <TextLine ID="line_2" HPOS="10" VPOS="80" WIDTH="100" HEIGHT="30"><String ID="string_6" HPOS="10" VPOS="80" WIDTH="100" HEIGHT="30" WC="0.97" CONTENT="abend."/></TextLine>
  </TextBlock></ComposedBlock>
 </PrintSpace></Page></Layout>
</alto>
"#;

pub(super) fn msg(page: &str, language: &str) -> String {
    OcrMessage {
        v: 1,
        page: page.into(),
        batch: "nbu_gage".into(),
        language: language.into(),
        reason: Reason::Bulk,
        score: Some(0.62),
        queued_at: "2026-10-10T12:00:00Z".into(),
    }
    .encode()
}

pub(super) fn worker(dir: &Path) -> (Arc<Worker>, Arc<LocalQueue>, Arc<dyn ObjectStore>) {
    let (w, queue, out, _) = worker_with_scans(dir);
    (w, queue, out)
}

pub(super) fn worker_with_scans(
    dir: &Path,
) -> (
    Arc<Worker>,
    Arc<LocalQueue>,
    Arc<dyn ObjectStore>,
    Arc<FakeScans>,
) {
    let queue = Arc::new(LocalQueue::new(dir.join("q")));
    let out: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.join("out")));
    let scans = Arc::new(FakeScans::default());
    let w = Arc::new(Worker {
        queue: queue.clone(),
        out: out.clone(),
        scans: scans.clone(),
        engine: Arc::new(FakeEngine),
        cfg: Config {
            node_id: "node1".into(),
            concurrency: 2,
            once: true,
            ..Config::default()
        },
        metrics: Metrics::global(),
        tracker: Arc::new(Tracker::quiet()),
    });
    (w, queue, out, scans)
}

/// The `ocr_engine_run` events (13 §13.8.3) sent while `f` runs, as
/// their fields.
pub(super) async fn engine_runs<F: std::future::Future<Output = ()>>(
    f: F,
) -> Vec<std::collections::BTreeMap<String, String>> {
    use tracing_subscriber::layer::SubscriberExt;
    type Seen = Arc<std::sync::Mutex<Vec<std::collections::BTreeMap<String, String>>>>;
    struct Fields<'a>(&'a mut std::collections::BTreeMap<String, String>);
    impl tracing::field::Visit for Fields<'_> {
        fn record_str(&mut self, f: &tracing::field::Field, v: &str) {
            self.0.insert(f.name().into(), v.into());
        }
        fn record_debug(&mut self, f: &tracing::field::Field, v: &dyn std::fmt::Debug) {
            self.0.insert(f.name().into(), format!("{v:?}"));
        }
    }
    struct Capture(Seen);
    impl<S> tracing_subscriber::Layer<S> for Capture
    where
        S: tracing::Subscriber + for<'l> tracing_subscriber::registry::LookupSpan<'l>,
    {
        fn on_event(&self, e: &tracing::Event<'_>, ctx: tracing_subscriber::layer::Context<'_, S>) {
            if e.metadata().target() == crate::provenance::ENGINE_RUN {
                let mut m = std::collections::BTreeMap::new();
                e.record(&mut Fields(&mut m));
                m.insert("level".into(), e.metadata().level().to_string());
                if let Some(span) = ctx.event_span(e) {
                    m.insert("span".into(), span.name().into());
                }
                self.0.lock().unwrap().push(m);
            }
        }
    }
    let seen = Seen::default();
    let sub = tracing_subscriber::registry().with(Capture(seen.clone()));
    let _guard = tracing::subscriber::set_default(sub);
    f.await;
    let v = seen.lock().unwrap().clone();
    v
}

/// Fails each read as its scan's bytes say, after describing it.
#[derive(Debug)]
pub(super) struct FailingEngine;

#[async_trait]
impl Engine for FailingEngine {
    fn source(&self) -> &str {
        "usnm-tesseract"
    }
    fn can_read(&self, language: &str) -> bool {
        language != "haw"
    }
    fn describe(&self, _: &str) -> Option<(String, crate::engine::ReadInfo)> {
        Some((
            "tesseract/5.5.0 profile=frak2021 image=local models=local route=minimal@r1 contract=0"
                .into(),
            crate::engine::ReadInfo {
                adapter: "usnm-ocr-adapter-tesseract 0.1.0 (rust)".into(),
                models_loaded: "at_start",
                cold_start: false,
                ..crate::engine::ReadInfo::local("tesseract-5.5")
            },
        ))
    }
    async fn ocr(&self, image: &Path, _: &str, _: &Path) -> Result<EngineOutput, EngineError> {
        let bytes = std::fs::read(image).unwrap();
        if bytes.starts_with(b"jp2 exit") {
            return Err(exited(1));
        }
        // The IIIF JPEG: the adapter's timeout.
        Err(EngineError::Failed(crate::engine::ReadFailed::error(
            "timeout",
            "engine_timeout",
            "no result from the engine in 16s".into(),
        )))
    }
}

/// [`FailingEngine`], whose read gets a cold instance though
/// `describe` guessed a warm one.
#[derive(Debug)]
pub(super) struct ColdInstanceEngine;

#[async_trait]
impl Engine for ColdInstanceEngine {
    fn source(&self) -> &str {
        FailingEngine.source()
    }
    fn can_read(&self, language: &str) -> bool {
        FailingEngine.can_read(language)
    }
    fn describe(&self, language: &str) -> Option<(String, crate::engine::ReadInfo)> {
        FailingEngine.describe(language)
    }
    async fn ocr(&self, image: &Path, l: &str, w: &Path) -> Result<EngineOutput, EngineError> {
        FailingEngine.ocr(image, l, w).await
    }
    async fn read_traced(
        &self,
        image: &Path,
        language: &str,
        work: &Path,
        _: &str,
        on_instance: &(dyn Fn(crate::engine::ReadInfo) + Send + Sync),
    ) -> Result<EngineOutput, EngineError> {
        let (_, info) = FailingEngine.describe(language).unwrap();
        on_instance(crate::engine::ReadInfo {
            cold_start: true,
            model_load_seconds: Some(2.5),
            ..info
        });
        FailingEngine.ocr(image, language, work).await
    }
}

/// Reads into ALTO that doesn't convert.
#[derive(Debug)]
pub(super) struct BadAltoEngine;

#[async_trait]
impl Engine for BadAltoEngine {
    fn source(&self) -> &str {
        "usnm-tesseract"
    }
    fn can_read(&self, _: &str) -> bool {
        true
    }
    async fn ocr(&self, _: &Path, _: &str, _: &Path) -> Result<EngineOutput, EngineError> {
        Ok(EngineOutput {
            text: "secret page text".into(),
            alto: b"<not alto".to_vec(),
            page: None,
            layout_from: "ALTO",
            engine: "tesseract-5.5.1/deu_latf+deu".into(),
            read: crate::engine::ReadInfo::local("tesseract-5.5.1"),
        })
    }
}

pub(super) fn with_engine(w: &Arc<Worker>, engine: Arc<dyn Engine>) -> Arc<Worker> {
    Arc::new(Worker {
        engine,
        cfg: w.cfg.clone(),
        queue: w.queue.clone(),
        out: w.out.clone(),
        scans: w.scans.clone(),
        metrics: w.metrics.clone(),
        tracker: w.tracker.clone(),
    })
}

pub(super) async fn one_page(w: &Arc<Worker>, queue: &LocalQueue, page: &str, language: &str) {
    queue.send(BULK, &msg(page, language)).await.unwrap();
    let m = queue.receive(BULK, Duration::ZERO).await.unwrap().unwrap();
    w.handle(m, Instant::now()).await;
}

/// A store whose overlay-part uploads fail.
#[derive(Debug)]
pub(super) struct NoParts(Arc<dyn ObjectStore>);

#[async_trait]
impl ObjectStore for NoParts {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.0.get(path).await
    }
    async fn put_new(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<bool, StoreError> {
        if path.starts_with("pages/") {
            return Err(StoreError::Io("the store went away".into()));
        }
        self.0.put_new(path, body, ct).await
    }
    async fn put(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<(), StoreError> {
        self.0.put(path, body, ct).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.0.list(prefix).await
    }
}
