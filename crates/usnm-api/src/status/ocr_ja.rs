//! Progress of the Japanese OCR job (#139): LoC ships no text for most
//! Japanese-language pages, so the job reads them itself and writes its
//! progress to `status/ocr-ja.json` in the reference store. How many of
//! those pages are searchable is in `published.ja.pages`.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use usnm_store::ObjectStore;

/// Where the job writes its progress.
pub const PATH: &str = "status/ocr-ja.json";

/// The job writes at least this often while it runs (every two minutes,
/// plus the time one issue takes).
const RUNNING_WITHIN: i64 = 15;

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
pub struct Count {
    pub pages: u64,
    pub issues: u64,
}

/// The file the job writes.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub updated_at: DateTime<Utc>,
    pub started_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub engine: Option<String>,
    pub targets: Count,
    pub done: Count,
    #[serde(default)]
    pub eta: Option<DateTime<Utc>>,
}

/// The status section.
#[derive(Debug, Clone, Serialize)]
pub struct OcrJa {
    /// The job wrote its progress recently and has pages left.
    pub running: bool,
    pub targets: Count,
    pub done: Count,
    pub percent: f64,
    pub engine: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub updated_at: DateTime<Utc>,
    /// Only while running.
    pub eta: Option<DateTime<Utc>>,
}

pub fn section(p: Progress, now: DateTime<Utc>) -> OcrJa {
    let done = Count {
        pages: p.done.pages.min(p.targets.pages),
        issues: p.done.issues.min(p.targets.issues),
    };
    let running =
        done.pages < p.targets.pages && now - p.updated_at < Duration::minutes(RUNNING_WITHIN);
    let percent = if p.targets.pages == 0 {
        100.0
    } else {
        (done.pages as f64 * 1000.0 / p.targets.pages as f64).floor() / 10.0
    };
    OcrJa {
        running,
        targets: p.targets,
        done,
        percent,
        engine: p.engine,
        started_at: p.started_at,
        updated_at: p.updated_at,
        eta: p.eta.filter(|_| running),
    }
}

/// The job's progress, or `None` without a reference store or before the
/// job's first run.
pub async fn read(store: Option<Arc<dyn ObjectStore>>) -> Result<Option<Progress>, String> {
    let Some(store) = store else {
        return Ok(None);
    };
    let Some(bytes) = store.get(PATH).await.map_err(|e| e.to_string())? else {
        return Ok(None);
    };
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|e| format!("{PATH}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn progress(done: u64, minutes_ago: i64, now: DateTime<Utc>) -> Progress {
        Progress {
            updated_at: now - Duration::minutes(minutes_ago),
            started_at: Some(now - Duration::hours(3)),
            engine: Some("ndlocr-lite x".into()),
            targets: Count {
                pages: 9000,
                issues: 1000,
            },
            done: Count {
                pages: done,
                issues: done / 9,
            },
            eta: Some(now + Duration::hours(5)),
        }
    }

    #[test]
    fn running_while_recent_and_unfinished() {
        let now = Utc::now();
        let s = section(progress(3000, 2, now), now);
        assert!(s.running);
        assert_eq!(s.percent, 33.3);
        assert!(s.eta.is_some());
        // Silent for too long: stopped, and the estimate is dropped.
        let s = section(progress(3000, 40, now), now);
        assert!(!s.running);
        assert_eq!(s.eta, None);
        // Finished.
        let s = section(progress(9000, 1, now), now);
        assert!(!s.running);
        assert_eq!(s.percent, 100.0);
    }

    #[test]
    fn percent_never_rounds_up_to_done() {
        let now = Utc::now();
        assert_eq!(section(progress(8999, 1, now), now).percent, 99.9);
    }

    #[tokio::test]
    async fn reads_the_jobs_file() {
        let dir = tempfile::tempdir().unwrap();
        let store: Arc<dyn ObjectStore> = Arc::new(usnm_store::LocalStore::new(dir.path()));
        assert!(read(Some(store.clone())).await.unwrap().is_none());
        assert!(read(None).await.unwrap().is_none());
        // As jaocr.py writes it.
        store
            .put(
                PATH,
                br#"{"schema": 1, "updated_at": "2026-10-05T02:21:39.149271+00:00",
                     "started_at": "2026-10-05T02:07:00+00:00", "engine": "ndlocr-lite 636d1cf",
                     "targets": {"pages": 11000, "issues": 1500},
                     "done": {"pages": 1200, "issues": 160}, "eta": null}"#
                    .to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let p = read(Some(store)).await.unwrap().unwrap();
        assert_eq!(
            p.done,
            Count {
                pages: 1200,
                issues: 160
            }
        );
        assert_eq!(p.eta, None);
    }
}
