//! Opt-in, local execution observations. This is not acceptance or catalog state.

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;

use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use serde::Serialize;
use usnm_ocr_contract::message::OcrMessage;
use usnm_ocr_contract::runs::RunRecord;

use crate::engine::{EngineError, EngineOutput, ReadInfo};
use crate::loc::Scan;

const MAX_BYTES: usize = 4 << 20;
// The worker tries the original scan, then at most one JPEG fallback.
const MAX_READS: usize = 2;

#[derive(Serialize)]
struct Observation {
    image_sha256: String,
    image_format: &'static str,
    engine_started_at: Option<DateTime<Utc>>,
    engine_finished_at: Option<DateTime<Utc>>,
    seconds_wall: Option<f64>,
    status: &'static str,
    error_kind: Option<&'static str>,
    ocr_engine: Option<String>,
    /// Whether engine fields describe a plan, an assigned instance or a result.
    info_source: Option<&'static str>,
    info: Option<ReadInfo>,
    #[serde(skip)]
    started: Option<Instant>,
}

#[derive(Serialize)]
struct Snapshot {
    metadata_schema: u32,
    run_id: String,
    doc_id: String,
    node: String,
    worker_version: String,
    ocr_source: String,
    language: String,
    batch: String,
    reason: String,
    damage_before: Option<f64>,
    queued_at: Option<DateTime<Utc>>,
    dpi: u32,
    dpi_source: &'static str,
    execution_config: serde_json::Value,
    worker_config: serde_json::Value,
    started_at: DateTime<Utc>,
    finished_at: Option<DateTime<Utc>>,
    seconds_wall: Option<f64>,
    /// Page execution, not route selection, acceptance, import or indexing.
    status: &'static str,
    error_kind: Option<&'static str>,
    observations: Vec<Observation>,
    /// Exactly runs::to_json_line's object, including its schema version.
    run_record: Option<serde_json::Value>,
}

#[derive(Debug)]
pub(crate) struct Capture {
    root: PathBuf,
    started: Instant,
    state: Mutex<Snapshot>,
}

impl std::fmt::Debug for Snapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Snapshot")
            .field("run_id", &self.run_id)
            .finish()
    }
}

impl Capture {
    pub(crate) fn new(
        root: &Path,
        run_id: &str,
        doc_id: &str,
        cfg: &crate::worker::Config,
        engine: &dyn crate::engine::Engine,
        message: &OcrMessage,
    ) -> Self {
        Self {
            root: root.into(),
            started: Instant::now(),
            state: Mutex::new(Snapshot {
                metadata_schema: 1,
                run_id: run_id.into(),
                doc_id: doc_id.into(),
                node: cfg.node_id.clone(),
                worker_version: crate::stored::worker_version(),
                ocr_source: engine.source().into(),
                language: message.language.clone(),
                batch: message.batch.clone(),
                reason: message.reason.as_str().into(),
                damage_before: message.score,
                queued_at: DateTime::parse_from_rfc3339(&message.queued_at)
                    .ok()
                    .map(|t| t.with_timezone(&Utc)),
                dpi: engine.dpi(),
                dpi_source: "assumed",
                execution_config: engine.execution_config(),
                worker_config: serde_json::json!({
                    "concurrency": cfg.concurrency,
                    "visibility_seconds": cfg.visibility.as_secs_f64(),
                    "abandon_at": cfg.abandon_at,
                    "page_timeout_seconds": cfg.page_timeout.as_secs_f64(),
                }),
                started_at: Utc::now(),
                finished_at: None,
                seconds_wall: None,
                status: "running",
                error_kind: None,
                observations: Vec::with_capacity(MAX_READS),
                run_record: None,
            }),
        }
    }

    // Poisoning must not hide lost metadata or panic while dropping a cancelled page.
    fn state(&self) -> std::sync::MutexGuard<'_, Snapshot> {
        self.state.lock().unwrap_or_else(|e| {
            tracing::error!("run metadata lock poisoned; recovering observations");
            e.into_inner()
        })
    }

    pub(crate) fn input(&self, scan: &Scan) {
        let mut s = self.state();
        if s.observations.len() == MAX_READS {
            tracing::error!(run_id = %s.run_id, "run metadata read limit exceeded");
            return;
        }
        s.observations.push(Observation {
            image_sha256: crate::prefetch::sha256(&scan.bytes),
            image_format: scan.format.name(),
            engine_started_at: None,
            engine_finished_at: None,
            seconds_wall: None,
            status: "not_started",
            error_kind: None,
            ocr_engine: None,
            info_source: None,
            info: None,
            started: None,
        });
    }

    pub(crate) fn start(&self, planned: Option<(String, ReadInfo)>) {
        let mut s = self.state();
        if let Some(o) = s.observations.last_mut() {
            o.engine_started_at = Some(Utc::now());
            o.started = Some(Instant::now());
            o.status = "running";
            if let Some((tag, info)) = planned {
                o.ocr_engine = Some(tag);
                o.info = Some(info);
                o.info_source = Some("planned");
            }
        }
    }

    pub(crate) fn instance(&self, info: ReadInfo) {
        if let Some(o) = self.state().observations.last_mut() {
            o.info = Some(info);
            o.info_source = Some("instance");
        }
    }

    pub(crate) fn ended(&self, result: &Result<EngineOutput, EngineError>) {
        if let Some(o) = self.state().observations.last_mut() {
            o.engine_finished_at = Some(Utc::now());
            o.seconds_wall = o.started.take().map(|t| t.elapsed().as_secs_f64());
            match result {
                Ok(out) => {
                    o.status = "ok";
                    o.ocr_engine = Some(out.engine.clone());
                    o.info = Some(out.read.clone());
                    o.info_source = Some("result");
                }
                Err(EngineError::Unsupported(_)) => {
                    o.status = "unsupported";
                    o.error_kind = Some("no_model");
                }
                Err(EngineError::Failed(e)) => {
                    let (status, kind) = if e.is::<crate::worker::ScratchError>() {
                        ("failed", "scratch")
                    } else {
                        crate::engine::read_failure(e)
                    };
                    o.status = status;
                    o.error_kind = Some(kind);
                }
            }
        }
    }

    pub(crate) fn record(&self, record: &RunRecord) {
        let result = (|| -> anyhow::Result<serde_json::Value> {
            // Bound allocation by the existing JSON-line helper as well as the file.
            serde_json::to_writer(
                Limited {
                    inner: io::sink(),
                    remaining: MAX_BYTES / 2,
                },
                record,
            )?;
            let line = usnm_ocr_contract::runs::to_json_line(record)?;
            Ok(serde_json::from_str(&line)?)
        })();
        match result {
            Ok(value) => self.state().run_record = Some(value),
            Err(e) => tracing::error!(error = %e, "run metadata record serialization failed"),
        }
    }

    pub(crate) fn finish(&self, status: &'static str, error_kind: Option<&'static str>) {
        let mut s = self.state();
        s.status = status;
        s.error_kind = error_kind;
        s.finished_at = Some(Utc::now());
        s.seconds_wall = Some(self.started.elapsed().as_secs_f64());
        for o in &mut s.observations {
            if o.status == "running" {
                // Dropping a future observes cancellation, not the engine's actual exit.
                o.status = "abandoned";
                o.error_kind = error_kind;
                o.seconds_wall = o.started.take().map(|t| t.elapsed().as_secs_f64());
            }
        }
        if let Err(e) = publish(&self.root, &s) {
            // Like the status journal, this must not replace the page's original error.
            tracing::error!(
                run_id = %s.run_id, error = %format!("{e:#}"),
                "run metadata publication failed; local observation was not confirmed durable"
            );
        }
    }
}

/// Check the optional destination before the queue loop starts.
pub(crate) fn prepare(root: &Path) -> anyhow::Result<()> {
    ensure_directory(root)?;
    let probe = tempfile::Builder::new()
        .prefix(".probe-")
        .tempfile_in(root)?;
    probe.as_file().sync_all()?;
    probe.close()?;
    File::open(root)?.sync_all()?;
    Ok(())
}

fn ensure_directory(root: &Path) -> anyhow::Result<()> {
    ensure!(
        !root.as_os_str().is_empty(),
        "run metadata directory is empty"
    );
    let mut missing = Vec::new();
    let mut dir = root;
    while !dir.exists() {
        missing.push(dir);
        dir = parent(dir);
    }
    std::fs::create_dir_all(root).context("create run metadata directory")?;
    // Persist newly created directory names, including nested destinations.
    for dir in missing.into_iter().rev() {
        File::open(parent(dir))?.sync_all()?;
    }
    Ok(())
}

fn parent(path: &Path) -> &Path {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
}

struct Limited<W> {
    inner: W,
    remaining: usize,
}

impl<W: Write> Write for Limited<W> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.remaining {
            return Err(io::Error::other(
                "run metadata serialization byte limit exceeded",
            ));
        }
        let n = self.inner.write(bytes)?;
        self.remaining -= n;
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

fn publish(root: &Path, snapshot: &Snapshot) -> anyhow::Result<()> {
    ensure_directory(root)?;
    let name = crate::prefetch::sha256(snapshot.run_id.as_bytes());
    let path = root.join(format!("{name}.json"));
    let mut pending = tempfile::Builder::new()
        .prefix(".pending-")
        .tempfile_in(root)?;
    {
        let mut writer = Limited {
            inner: pending.as_file_mut(),
            remaining: MAX_BYTES,
        };
        serde_json::to_writer(&mut writer, snapshot)?;
        writer.write_all(b"\n")?;
    }
    pending.as_file().sync_all()?;
    // Create-only publication: concurrent publishers and replay never overwrite a run.
    pending.persist_noclobber(&path).map_err(|e| e.error)?;
    File::open(root)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tracing_subscriber::layer::SubscriberExt;

    fn snapshot() -> Snapshot {
        Snapshot {
            metadata_schema: 1,
            run_id: "node1.20261016T200000000000Z".into(),
            doc_id: "sn83045462_1890-05-03_ed-1_seq-7".into(),
            node: "node1".into(),
            worker_version: crate::stored::worker_version(),
            ocr_source: "usnm-tesseract".into(),
            language: "deu".into(),
            batch: "nbu_gage".into(),
            reason: "bulk".into(),
            damage_before: None,
            queued_at: None,
            dpi: 300,
            dpi_source: "assumed",
            execution_config: serde_json::Value::Null,
            worker_config: serde_json::json!({}),
            started_at: Utc::now(),
            finished_at: Some(Utc::now()),
            seconds_wall: Some(0.1),
            status: "failed",
            error_kind: Some("fetch"),
            observations: Vec::new(),
            run_record: None,
        }
    }

    #[test]
    fn concurrent_publication_is_atomic_create_only_and_replay_cannot_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("nested").join("metadata");
        prepare(&root).unwrap();
        // A crash before publication can leave a private staging file.
        std::fs::write(root.join(".pending-crashed"), b"{partial").unwrap();
        let snapshot = snapshot();
        let successes = AtomicUsize::new(0);
        std::thread::scope(|scope| {
            for _ in 0..8 {
                scope.spawn(|| {
                    if publish(&root, &snapshot).is_ok() {
                        successes.fetch_add(1, Ordering::Relaxed);
                    }
                });
            }
        });
        assert_eq!(successes.load(Ordering::Relaxed), 1);
        let path = root.join(format!(
            "{}.json",
            crate::prefetch::sha256(snapshot.run_id.as_bytes())
        ));
        let original = std::fs::read(&path).unwrap();
        let mut conflicting = snapshot;
        conflicting.status = "ok";
        assert!(publish(&root, &conflicting).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), original);
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&original).unwrap()["status"],
            "failed"
        );
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 2);
    }

    #[test]
    fn oversized_records_are_not_published_and_staging_files_are_removed() {
        let dir = tempfile::tempdir().unwrap();
        let mut snapshot = snapshot();
        snapshot.ocr_source = "x".repeat(MAX_BYTES);
        assert!(publish(dir.path(), &snapshot).is_err());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        assert!(prepare(Path::new("")).is_err());
    }

    #[test]
    fn publication_failure_is_explicitly_logged() {
        struct Errors(std::sync::Arc<AtomicUsize>);
        impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Errors {
            fn on_event(
                &self,
                event: &tracing::Event<'_>,
                _: tracing_subscriber::layer::Context<'_, S>,
            ) {
                if event.metadata().level() == &tracing::Level::ERROR {
                    self.0.fetch_add(1, Ordering::Relaxed);
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("not-a-directory");
        std::fs::write(&root, b"file").unwrap();
        assert!(prepare(&root).is_err());
        let capture = Capture {
            root,
            started: Instant::now(),
            state: Mutex::new(snapshot()),
        };
        let errors = std::sync::Arc::new(AtomicUsize::new(0));
        let subscriber = tracing_subscriber::registry().with(Errors(errors.clone()));
        tracing::subscriber::with_default(subscriber, || {
            capture.finish("failed", Some("fetch"));
        });
        assert_eq!(errors.load(Ordering::Relaxed), 1);
    }
}
