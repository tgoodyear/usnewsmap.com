//! The worker's status on the LAN, for the dashboard (`usnm-ocrdash`,
//! 12 §12.2.5): what it is reading now, what it has finished, and a
//! journal of finished pages that survives a restart.
//!
//! - `GET /status`: [`WorkerStatus`].
//! - `GET /journal?from=<offset>`: the journal's lines from a byte offset,
//!   at most 1 MiB and whole lines only. Each line is a [`Completion`].
//!
//! Only `--status-allow` addresses are answered; the node's nftables rule
//! (`install-worker-node.sh`) says the same. Nothing here holds page text:
//! a page is its doc_id, with its language, queue, engine tag, times,
//! outcome and scan source.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use axum::extract::{ConnectInfo, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use serde::Deserialize;
pub use usnm_ocr_contract::status::{Completion, InFlight, Outcomes, WorkerStatus, RECENT_MAX};

use crate::worker::ScanSource;

/// The most `GET /journal` returns at once.
pub const JOURNAL_CHUNK: u64 = 1024 * 1024;

/// What the worker is, for the status.
#[derive(Debug, Clone, Default)]
pub struct About {
    pub node_id: String,
    pub version: String,
    pub concurrency: u32,
    pub engine: String,
    pub scan_cache_url: Option<String>,
}

#[derive(Debug, Default)]
struct Inner {
    in_flight: HashMap<String, InFlight>,
    /// Finished pages over the last day, oldest first.
    day: VecDeque<Completion>,
    since_start: Outcomes,
    recent: VecDeque<Completion>,
    /// Language → the engine tag's profile (or the tag) of its last page
    /// done.
    routes: BTreeMap<String, String>,
    /// Where each page in flight got its scan: `cache` or `loc`.
    sources: HashMap<String, &'static str>,
}

/// The worker's record of its pages. Cheap: a lock and a few counters per
/// page.
pub struct Tracker {
    about: About,
    started_at: DateTime<Utc>,
    inner: Mutex<Inner>,
    journal: Option<(PathBuf, Mutex<std::fs::File>)>,
    scans: Option<Arc<dyn ScanSource>>,
}

impl std::fmt::Debug for Tracker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Tracker")
            .field("node_id", &self.about.node_id)
            .finish_non_exhaustive()
    }
}

/// The short form of an engine tag for the routes: its `profile=`, else
/// the tag.
fn profile(tag: &str) -> String {
    tag.split_whitespace()
        .find_map(|w| w.strip_prefix("profile="))
        .unwrap_or(tag)
        .to_owned()
}

impl Tracker {
    /// A tracker; with `journal`, finished pages are appended to that file.
    pub fn new(
        about: About,
        journal: Option<PathBuf>,
        scans: Option<Arc<dyn ScanSource>>,
    ) -> anyhow::Result<Self> {
        let journal = match journal {
            Some(p) => {
                let f = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&p)
                    .map_err(|e| anyhow::anyhow!("opening the journal {}: {e}", p.display()))?;
                Some((p, Mutex::new(f)))
            }
            None => None,
        };
        Ok(Tracker {
            about,
            started_at: Utc::now(),
            inner: Mutex::new(Inner::default()),
            journal,
            scans,
        })
    }

    /// A tracker that keeps counts but no journal and serves nothing (tests,
    /// and a worker run without `--status-listen`).
    pub fn quiet() -> Self {
        Tracker {
            about: About::default(),
            started_at: Utc::now(),
            inner: Mutex::new(Inner::default()),
            journal: None,
            scans: None,
        }
    }

    pub fn begin(&self, doc_id: &str, language: &str, queue: &str) {
        self.inner.lock().expect("tracker lock").in_flight.insert(
            doc_id.to_owned(),
            InFlight {
                doc_id: doc_id.to_owned(),
                language: language.to_owned(),
                queue: queue.to_owned(),
                started_at: Utc::now(),
                stage: "fetch".into(),
            },
        );
    }

    pub fn stage(&self, doc_id: &str, stage: &str) {
        if let Some(p) = self
            .inner
            .lock()
            .expect("tracker lock")
            .in_flight
            .get_mut(doc_id)
        {
            p.stage = stage.to_owned();
        }
    }

    /// Where a page's scan came from.
    pub fn scanned(&self, doc_id: &str, from_cache: bool) {
        self.inner
            .lock()
            .expect("tracker lock")
            .sources
            .insert(doc_id.to_owned(), if from_cache { "cache" } else { "loc" });
    }

    /// A page finished: off the in-flight list, into the counts and the
    /// journal. A line that fails [`Completion::check`] is counted but not
    /// written.
    pub fn end(&self, mut c: Completion) {
        c.finished_at = Utc::now();
        {
            let mut i = self.inner.lock().expect("tracker lock");
            if let Some(p) = i.in_flight.remove(&c.doc_id) {
                c.started_at = p.started_at;
            }
            if let Some(src) = i.sources.remove(&c.doc_id) {
                if c.scan.is_empty() {
                    c.scan = src.to_owned();
                }
            }
            c.secs = ((c.finished_at - c.started_at).num_milliseconds().max(0) as f64) / 1000.0;
            i.since_start.add(&c);
            if c.outcome == "done" && !c.engine.is_empty() {
                let p = profile(&c.engine);
                i.routes.insert(c.language.clone(), p);
            }
            i.day.push_back(c.clone());
            let cut = c.finished_at - Duration::days(1);
            while i.day.front().is_some_and(|d| d.finished_at < cut) {
                i.day.pop_front();
            }
            i.recent.push_front(c.clone());
            i.recent.truncate(RECENT_MAX);
        }
        if let Some((path, f)) = &self.journal {
            if c.check().is_err() {
                tracing::warn!(
                    doc_id = c.doc_id,
                    "a journal line that fails its check; not written"
                );
                return;
            }
            let mut line = match serde_json::to_vec(&c) {
                Ok(l) => l,
                Err(_) => return,
            };
            line.push(b'\n');
            if let Err(e) = f.lock().expect("journal lock").write_all(&line) {
                tracing::warn!(error = %e, path = %path.display(), "can't write the journal");
            }
        }
    }

    pub fn status(&self) -> WorkerStatus {
        let now = Utc::now();
        let i = self.inner.lock().expect("tracker lock");
        let mut hour = Outcomes::default();
        let mut day = Outcomes::default();
        let cut_day = now - Duration::days(1);
        let cut_hour = now - Duration::hours(1);
        for c in i.day.iter().filter(|c| c.finished_at >= cut_day) {
            day.add(c);
            if c.finished_at >= cut_hour {
                hour.add(c);
            }
        }
        let mut in_flight: Vec<InFlight> = i.in_flight.values().cloned().collect();
        in_flight.sort_by_key(|p| p.started_at);
        WorkerStatus {
            schema: usnm_ocr_contract::status::SCHEMA,
            node_id: self.about.node_id.clone(),
            version: self.about.version.clone(),
            started_at: self.started_at,
            now,
            concurrency: self.about.concurrency,
            engine: self.about.engine.clone(),
            engine_version: String::new(),
            models: Vec::new(),
            routes: i.routes.clone(),
            scan_cache_url: self.about.scan_cache_url.clone(),
            loc_hold_secs: self.scans.as_ref().and_then(|s| {
                s.held_until().map(|t| {
                    t.saturating_duration_since(tokio::time::Instant::now())
                        .as_secs()
                })
            }),
            in_flight,
            last_hour: hour,
            last_day: day,
            since_start: i.since_start,
            recent: i.recent.iter().cloned().collect(),
            journal_bytes: self
                .journal
                .as_ref()
                .and_then(|(p, _)| std::fs::metadata(p).ok())
                .map_or(0, |m| m.len()),
        }
    }

    /// The journal from `from`: whole lines, at most [`JOURNAL_CHUNK`].
    pub fn journal_from(&self, from: u64) -> std::io::Result<Vec<u8>> {
        let Some((path, _)) = &self.journal else {
            return Ok(Vec::new());
        };
        let mut f = std::fs::File::open(path)?;
        let len = f.metadata()?.len();
        if from >= len {
            return Ok(Vec::new());
        }
        f.seek(SeekFrom::Start(from))?;
        let mut buf = Vec::new();
        f.take((len - from).min(JOURNAL_CHUNK))
            .read_to_end(&mut buf)?;
        match buf.iter().rposition(|b| *b == b'\n') {
            Some(end) => buf.truncate(end + 1),
            None => buf.clear(),
        }
        Ok(buf)
    }
}

#[derive(Clone)]
struct Served {
    tracker: Arc<Tracker>,
    allow: Arc<Vec<IpAddr>>,
}

async fn allow_only(
    State(s): State<Served>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    req: Request,
    next: Next,
) -> Response {
    if s.allow.contains(&peer.ip().to_canonical()) {
        next.run(req).await
    } else {
        StatusCode::FORBIDDEN.into_response()
    }
}

#[derive(Deserialize)]
struct From {
    #[serde(default)]
    from: u64,
}

pub fn router(tracker: Arc<Tracker>, allow: Vec<IpAddr>) -> Router {
    let s = Served {
        tracker,
        allow: Arc::new(allow),
    };
    Router::new()
        .route(
            "/status",
            get(|State(s): State<Served>| async move { Json(s.tracker.status()) }),
        )
        .route(
            "/journal",
            get(
                |State(s): State<Served>, Query(q): Query<From>| async move {
                    let t = s.tracker.clone();
                    match tokio::task::spawn_blocking(move || t.journal_from(q.from)).await {
                        Ok(Ok(b)) => {
                            ([(header::CONTENT_TYPE, "application/x-ndjson")], b).into_response()
                        }
                        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
                    }
                },
            ),
        )
        .layer(middleware::from_fn_with_state(s.clone(), allow_only))
        .with_state(s)
}

/// Serve the status on `listen` to `allow` until `stop`.
pub async fn serve(
    listen: SocketAddr,
    tracker: Arc<Tracker>,
    allow: Vec<IpAddr>,
    mut stop: tokio::sync::watch::Receiver<bool>,
) -> anyhow::Result<()> {
    let listener = tokio::net::TcpListener::bind(listen).await?;
    tracing::info_span!("ocr_worker_start")
        .in_scope(|| tracing::info!(%listen, allow = ?allow, "status listening"));
    axum::serve(
        listener,
        router(tracker, allow).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = stop.wait_for(|s| *s).await;
    })
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::connect_info::MockConnectInfo;
    use axum::http::Request as HttpRequest;
    use tower::ServiceExt;

    fn page(doc: &str, outcome: &str, scan: &str) -> Completion {
        Completion {
            doc_id: doc.into(),
            language: "deu".into(),
            engine: "tesseract/5.5.0 profile=frak2021 image=sha256:0123456789ab".into(),
            queue: "ocr-bulk".into(),
            reason: "bulk".into(),
            outcome: outcome.into(),
            scan: scan.into(),
            uploaded: outcome == "done",
            ..Completion::default()
        }
    }

    async fn get(tracker: Arc<Tracker>, from: &str, uri: &str) -> (StatusCode, Vec<u8>) {
        let peer: SocketAddr = format!("{from}:5000").parse().unwrap();
        let r = router(tracker, vec!["192.168.0.105".parse().unwrap()])
            .layer(MockConnectInfo(peer))
            .oneshot(HttpRequest::get(uri).body(Body::empty()).unwrap())
            .await
            .unwrap();
        let s = r.status();
        (
            s,
            axum::body::to_bytes(r.into_body(), 1 << 22)
                .await
                .unwrap()
                .to_vec(),
        )
    }

    #[tokio::test]
    async fn tracks_pages_and_serves_its_journal_to_the_dashboard_only() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal.jsonl");
        let about = About {
            node_id: "usnm-ocr-node1".into(),
            concurrency: 4,
            engine: "usnm-tesseract".into(),
            ..About::default()
        };
        let t = Arc::new(Tracker::new(about.clone(), Some(journal.clone()), None).unwrap());
        t.begin("sn1_1890-01-01_ed-1_seq-1", "deu", "ocr-bulk");
        t.begin("sn1_1890-01-01_ed-1_seq-2", "deu", "ocr-bulk");
        t.stage("sn1_1890-01-01_ed-1_seq-2", "ocr");
        t.end(page("sn1_1890-01-01_ed-1_seq-1", "done", "cache"));
        let s = t.status();
        assert_eq!(s.in_flight.len(), 1);
        assert_eq!(s.in_flight[0].stage, "ocr");
        assert_eq!(s.last_hour.done, 1);
        assert_eq!(s.last_hour.from_cache, 1);
        assert_eq!(s.routes["deu"], "frak2021");
        assert_eq!(s.recent.len(), 1);
        assert!(s.journal_bytes > 0);
        t.end(page("sn1_1890-01-01_ed-1_seq-2", "failed", "loc"));

        let (code, body) = get(t.clone(), "192.168.0.105", "/journal?from=0").await;
        assert_eq!(code, StatusCode::OK);
        let lines: Vec<Completion> = String::from_utf8(body.clone())
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[1].outcome, "failed");
        let (_, rest) = get(
            t.clone(),
            "192.168.0.105",
            &format!("/journal?from={}", body.len()),
        )
        .await;
        assert!(rest.is_empty());
        let (code, _) = get(t.clone(), "192.168.0.50", "/status").await;
        assert_eq!(code, StatusCode::FORBIDDEN);
        let (code, body) = get(t.clone(), "192.168.0.105", "/status").await;
        assert_eq!(code, StatusCode::OK);
        let s: WorkerStatus = serde_json::from_slice(&body).unwrap();
        assert_eq!(s.node_id, "usnm-ocr-node1");
        assert_eq!(s.since_start.failed, 1);

        // A restarted worker carries on with the same journal.
        drop(t);
        let t = Tracker::new(about, Some(journal), None).unwrap();
        t.end(page("sn1_1890-01-01_ed-1_seq-3", "done", "loc"));
        let all = String::from_utf8(t.journal_from(0).unwrap()).unwrap();
        assert_eq!(all.lines().count(), 3);
    }

    #[test]
    fn a_journal_read_stops_at_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let journal = dir.path().join("journal.jsonl");
        std::fs::write(&journal, "{\"a\":1}\n{\"b\":").unwrap();
        let t = Tracker::new(About::default(), Some(journal), None).unwrap();
        assert_eq!(t.journal_from(0).unwrap(), b"{\"a\":1}\n");
        assert!(t.journal_from(100).unwrap().is_empty());
        assert!(Tracker::quiet().journal_from(0).unwrap().is_empty());
    }
}
