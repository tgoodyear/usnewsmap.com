//! `GET /v1/versions`: every index version the pipeline has built, newest
//! first, with what built each one (#161), for comparing versions.
//!
//! The versions come from the pipeline state's `index_runs` (the reading
//! `/v1/status` already makes, so this costs no extra reads). Each has its
//! build record: `recorded` when the release wrote one, or `reconstructed`
//! from `ops/index-history.json` for versions released before it did
//! (`scripts/reconstruct-index-history.py`; its shape is
//! `ops/index-history.schema.json`, which `scripts/check-index-history.py`
//! checks in CI).

use std::collections::BTreeMap;
use std::sync::OnceLock;

use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use usnm_state::state::RunStatus;
use usnm_state::summary::Summary;

/// Bump when the document's shape changes incompatibly.
pub const SCHEMA: u32 = 1;

/// Build records worked out for versions released before releases recorded
/// them, by version.
const HISTORY: &str = include_str!("../../../ops/index-history.json");

fn history() -> &'static BTreeMap<String, Value> {
    static H: OnceLock<BTreeMap<String, Value>> = OnceLock::new();
    H.get_or_init(|| serde_json::from_str(HISTORY).expect("ops/index-history.json is valid JSON"))
}

/// The reconstructed record of `run`: the history's entry for its version,
/// only if it is the same run. Version names are only unique within one
/// environment's pipeline state, so a staging run that happens to share a
/// name with a production one must not get production's record; the run's
/// start time, to the nanosecond, tells them apart.
fn reconstructed(run: &usnm_state::summary::RunSummary) -> Option<&'static Value> {
    let b = history().get(&run.index_version)?;
    let started: DateTime<Utc> = b["run_started_at"].as_str()?.parse().ok()?;
    (started == run.started_at).then_some(b)
}

const NOT_CONFIGURED: &str = "This server is not connected to the pipeline state.";

#[derive(Debug, Serialize)]
pub struct Versions {
    pub schema: u32,
    pub generated_at: DateTime<Utc>,
    pub available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<&'static str>,
    /// When the pipeline state was read.
    pub read_at: Option<DateTime<Utc>>,
    /// The version this server is serving.
    pub serving: String,
    pub versions: Vec<Version>,
}

#[derive(Debug, Serialize)]
pub struct Version {
    pub index_version: String,
    /// This server serves it.
    pub serving: bool,
    pub full: bool,
    pub status: RunStatus,
    pub indexes: Vec<String>,
    pub new_index: String,
    pub batches: Option<u64>,
    pub docs: u64,
    pub pages: u64,
    pub duplicate_pages: u64,
    pub started_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub previous_version: Option<String>,
    /// What built it; `null` when neither recorded nor reconstructed.
    pub build: Option<Value>,
    /// `recorded`, `reconstructed`, or `null` with no build.
    pub build_source: Option<&'static str>,
}

pub fn assemble(
    now: DateTime<Utc>,
    serving: &str,
    reading: Option<(DateTime<Utc>, &Summary)>,
) -> Versions {
    let Some((read_at, summary)) = reading else {
        return Versions {
            schema: SCHEMA,
            generated_at: now,
            available: false,
            reason: Some(NOT_CONFIGURED),
            read_at: None,
            serving: serving.to_owned(),
            versions: Vec::new(),
        };
    };
    let mut versions: Vec<Version> = summary
        .runs
        .iter()
        .map(|r| {
            let (build, build_source) = match (&r.build, reconstructed(r)) {
                (Some(b), _) => (Some(b.clone()), Some("recorded")),
                (None, Some(b)) => (Some(b.clone()), Some("reconstructed")),
                (None, None) => (None, None),
            };
            Version {
                index_version: r.index_version.clone(),
                serving: r.index_version == serving,
                full: r.full,
                status: r.status,
                indexes: r.indexes.clone(),
                new_index: r.new_index.clone(),
                batches: r.batches(),
                docs: r.docs,
                pages: r.pages,
                duplicate_pages: r.duplicate_pages,
                started_at: r.started_at,
                published_at: r.published_at,
                previous_version: r.previous_version.clone(),
                build,
                build_source,
            }
        })
        .collect();
    versions.sort_by_key(|v| std::cmp::Reverse(v.started_at));
    Versions {
        schema: SCHEMA,
        generated_at: now,
        available: true,
        reason: None,
        read_at: Some(read_at),
        serving: serving.to_owned(),
        versions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn summary(runs: Value) -> Summary {
        Summary {
            runs: serde_json::from_value(runs).unwrap(),
            ..Summary::default()
        }
    }

    #[test]
    fn the_history_file_is_valid_and_reconstructed() {
        assert!(!history().is_empty());
        for (v, b) in history() {
            assert!(v.starts_with("pages-v"), "{v}");
            assert_eq!(b["commit"].as_str().map(str::len), Some(40), "{v}");
            assert!(b["reconstructed"].is_string(), "{v}");
            assert!(b["templates"]["pages"]["sha256"].is_string(), "{v}");
            let started = b["run_started_at"].as_str().unwrap_or_default();
            assert!(started.parse::<DateTime<Utc>>().is_ok(), "{v}: {started}");
        }
    }

    #[test]
    fn recorded_wins_then_reconstructed_newest_first() {
        let now = Utc::now();
        let s = summary(json!([
            {"index_version": "pages-v20260929-1", "full": true, "status": "published",
             "started_at": "2026-09-29T00:06:01.225497468Z", "published_at": "2026-09-29T00:42:59Z"},
            {"index_version": "pages-v20991231-1", "full": false, "status": "published",
             "started_at": "2099-12-31T00:00:00Z", "previous_version": "pages-v20260929-1",
             "build": {"commit": "abc", "templates": {}}},
            {"index_version": "pages-v20200101-1", "full": true, "status": "failed",
             "started_at": "2020-01-01T00:00:00Z"}
        ]));
        let v = assemble(now, "pages-v20991231-1", Some((now, &s)));
        assert!(v.available);
        let order: Vec<_> = v
            .versions
            .iter()
            .map(|x| x.index_version.as_str())
            .collect();
        assert_eq!(
            order,
            [
                "pages-v20991231-1",
                "pages-v20260929-1",
                "pages-v20200101-1"
            ]
        );
        assert_eq!(
            v.versions
                .iter()
                .map(|x| x.build_source)
                .collect::<Vec<_>>(),
            [Some("recorded"), Some("reconstructed"), None]
        );
        assert!(v.versions[0].serving && !v.versions[1].serving);
        assert_eq!(v.versions[0].build.as_ref().unwrap()["commit"], "abc");
        assert_eq!(
            v.versions[1].build.as_ref().unwrap()["commit"],
            history()["pages-v20260929-1"]["commit"]
        );
    }

    #[test]
    fn another_environments_run_with_the_same_name_gets_no_record() {
        let now = Utc::now();
        let s = summary(json!([
            {"index_version": "pages-v20260929-1", "full": true, "status": "published",
             "started_at": "2026-09-29T07:00:00Z"}
        ]));
        let v = assemble(now, "pages-v20260929-1", Some((now, &s)));
        assert_eq!(v.versions[0].build_source, None);
        assert!(v.versions[0].build.is_none());
    }

    #[test]
    fn without_pipeline_state() {
        let v = assemble(Utc::now(), "fixture-v1", None);
        assert!(!v.available && v.versions.is_empty());
        assert_eq!(v.reason, Some(NOT_CONFIGURED));
    }
}
