//! Progress and summary of the OCR quality audit (`jaocr.py quality`,
//! docs/operations.md "OCR quality audit"): it samples the curated pages,
//! detects each page's language and scores its OCR, and writes its progress,
//! then a summary, to `status/ocr-quality.json` in the reference store.

use std::sync::Arc;

use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use usnm_store::ObjectStore;

/// Where the audit writes its progress.
pub const PATH: &str = "status/ocr-quality.json";

/// The audit writes every 50 batches, about every two minutes; silent for
/// longer than this, a run without `finished_at` has stopped.
const RUNNING_WITHIN: i64 = 15;

#[derive(Debug, Clone, Copy, Default, PartialEq, Deserialize, Serialize)]
pub struct Batches {
    pub done: u64,
    pub total: u64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Agreement {
    /// Sampled pages whose detected language isn't their title's first catalog language.
    pub differs_share: f64,
    pub mixed_share: f64,
    /// The same, among pages of titles that list more than one language.
    pub multilingual_differs_share: f64,
    pub und_share: f64,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Language {
    pub language: String,
    pub pages: u64,
    pub function_share_median: Option<f64>,
    pub damage_rate_median: Option<f64>,
    /// Pages with a damage rate above 0.1.
    pub damaged_share: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct Summary {
    pub agreement: Agreement,
    pub languages: Vec<Language>,
}

/// The file the audit writes.
#[derive(Debug, Clone, Deserialize)]
pub struct Progress {
    pub metric: String,
    pub version: String,
    pub sample_pct: f64,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    #[serde(default)]
    pub finished_at: Option<DateTime<Utc>>,
    pub batches: Batches,
    pub pages_sampled: u64,
    #[serde(default)]
    pub summary: Option<Summary>,
}

/// The status section.
#[derive(Debug, Clone, Serialize)]
pub struct OcrQuality {
    /// Unfinished and reported recently.
    pub running: bool,
    /// Unfinished and silent for longer than a run reports.
    pub stopped: bool,
    pub metric: String,
    /// The published version it sampled.
    pub version: String,
    pub sample_pct: f64,
    pub batches: Batches,
    /// Of the batches, to one decimal, never rounded up to 100.
    pub percent: f64,
    pub pages_sampled: u64,
    pub started_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    /// Only once finished.
    pub summary: Option<Summary>,
}

pub fn section(p: Progress, now: DateTime<Utc>) -> OcrQuality {
    let done = p.batches.done.min(p.batches.total);
    let finished = p.finished_at.is_some();
    let recent = now - p.updated_at < Duration::minutes(RUNNING_WITHIN);
    let percent = if finished || p.batches.total == 0 {
        100.0
    } else {
        (done as f64 * 1000.0 / p.batches.total as f64).floor() / 10.0
    };
    OcrQuality {
        running: !finished && recent,
        stopped: !finished && !recent,
        metric: p.metric,
        version: p.version,
        sample_pct: p.sample_pct,
        batches: Batches {
            done,
            total: p.batches.total,
        },
        percent,
        pages_sampled: p.pages_sampled,
        started_at: p.started_at,
        updated_at: p.updated_at,
        finished_at: p.finished_at,
        summary: p.summary.filter(|_| finished),
    }
}

/// The audit's progress, or `None` without a reference store or before its
/// first run.
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
    use serde_json::json;

    fn progress(done: u64, minutes_ago: i64, finished: bool, now: DateTime<Utc>) -> Progress {
        let mut v = json!({
            "batches": {"done": done, "total": 2989},
            "finished_at": null,
            "metric": "v2",
            "pages_sampled": done * 160,
            "sample_pct": 2.0,
            "started_at": (now - Duration::hours(1)).to_rfc3339(),
            "summary": null,
            "updated_at": (now - Duration::minutes(minutes_ago)).to_rfc3339(),
            "version": "pages-v20261003-1"
        });
        if finished {
            v["finished_at"] = json!((now - Duration::minutes(minutes_ago)).to_rfc3339());
            v["summary"] = json!({
                "agreement": {"differs_share": 0.031, "mixed_share": 0.004,
                              "multilingual_differs_share": 0.41, "und_share": 0.012},
                "languages": [{"damage_rate_median": 0.02, "damaged_share": 0.04,
                               "function_share_median": 0.46, "language": "eng", "pages": 430000}]
            });
        }
        serde_json::from_value(v).unwrap()
    }

    #[test]
    fn running_stopped_and_finished() {
        let now = Utc::now();
        let s = section(progress(1200, 2, false, now), now);
        assert!(s.running && !s.stopped);
        assert_eq!(s.percent, 40.1);
        assert!(s.summary.is_none());
        let s = section(progress(1200, 40, false, now), now);
        assert!(!s.running && s.stopped);
        let s = section(progress(2989, 400, true, now), now);
        assert!(!s.running && !s.stopped);
        assert_eq!(s.percent, 100.0);
        let sum = s.summary.unwrap();
        assert_eq!(sum.languages[0].language, "eng");
        assert_eq!(sum.agreement.multilingual_differs_share, 0.41);
    }
}
