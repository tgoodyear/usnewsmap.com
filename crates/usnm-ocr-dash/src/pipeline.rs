//! Bounded, read-only corpus-catalog evidence, independent of worker totals.

use std::collections::HashSet;
use std::io::Read as _;
use std::path::PathBuf;

use anyhow::{ensure, Context};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub const MAX_BYTES: usize = 1024 * 1024;
pub const MAX_COUNT: u64 = 1_000_000_000_000;
/// The longest age budget a stage may claim: 30 days.
pub const MAX_STAGE_BUDGET_SECS: u64 = 2_592_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Snapshot {
    pub schema_version: u8,
    #[serde(deserialize_with = "optional_utc")]
    pub observed_at: Option<DateTime<Utc>>,
    #[serde(deserialize_with = "utc")]
    pub generated_at: DateTime<Utc>,
    pub producer: String,
    pub coverage: Vec<String>,
    pub stages: Vec<Stage>,
    pub runs: Runs,
    pub artifacts: Artifacts,
    pub issues: Vec<Issue>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Stage {
    pub id: String,
    pub label: String,
    #[serde(deserialize_with = "Option::deserialize")]
    pub count: Option<u64>,
    pub state: StageState,
    pub detail: String,
    /// Missing uses the conservative snapshot evidence time; explicit null is unknown.
    #[serde(
        default,
        deserialize_with = "stage_utc",
        skip_serializing_if = "Option::is_none"
    )]
    pub observed_at: Option<Option<DateTime<Utc>>>,
    /// Fed only by sources that will never report again: it keeps its own age and is
    /// flagged, but doesn't make the panel stale (14: Overall evidence age).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub historical: bool,
    /// This stage's age budget, for evidence slower than the snapshot's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_secs: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum StageState {
    Observed,
    Unknown,
    Blocked,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Runs {
    pub total: u64,
    pub pages: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Artifacts {
    pub locations: u64,
    pub verified: u64,
    pub missing: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Issue {
    pub code: String,
    pub detail: String,
}

fn parse_utc(s: &str) -> Result<DateTime<Utc>, String> {
    let t = DateTime::parse_from_rfc3339(s).map_err(|_| "expected RFC3339 UTC timestamp")?;
    if t.offset().local_minus_utc() != 0 || s.ends_with("-00:00") {
        return Err("expected UTC timestamp (Z or +00:00)".into());
    }
    Ok(t.with_timezone(&Utc))
}

fn utc<'de, D: serde::Deserializer<'de>>(d: D) -> Result<DateTime<Utc>, D::Error> {
    parse_utc(&String::deserialize(d)?).map_err(serde::de::Error::custom)
}

fn optional_utc<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<DateTime<Utc>>, D::Error> {
    Option::<String>::deserialize(d)?
        .map(|s| parse_utc(&s).map_err(serde::de::Error::custom))
        .transpose()
}

fn stage_utc<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<DateTime<Utc>>>, D::Error> {
    optional_utc(d).map(Some)
}

fn text(s: &str, max: usize, empty: bool) -> anyhow::Result<()> {
    ensure!(
        (empty || !s.trim().is_empty())
            && s.chars().count() <= max
            && !s.chars().any(char::is_control),
        "invalid metadata string (empty, control character or over {max} characters)"
    );
    Ok(())
}

pub fn parse(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<Snapshot> {
    ensure!(bytes.len() <= MAX_BYTES, "snapshot exceeds 1 MiB");
    let s: Snapshot = serde_json::from_slice(bytes).context("malformed pipeline snapshot")?;
    ensure!(s.schema_version == 1, "unsupported schema_version");
    ensure!(s.producer == "corpus-catalog", "unexpected producer");
    ensure!(s.coverage.len() <= 32, "too many coverage entries");
    ensure!(s.stages.len() <= 64, "too many stages");
    ensure!(s.issues.len() <= 128, "too many issues");
    for t in std::iter::once(s.generated_at).chain(s.observed_at) {
        ensure!(
            t - now <= chrono::Duration::seconds(60),
            "future pipeline timestamp"
        );
    }
    if let Some(t) = s.observed_at {
        ensure!(
            t - s.generated_at <= chrono::Duration::seconds(60),
            "observed_at is after generated_at"
        );
    }
    for c in &s.coverage {
        text(c, 128, false)?;
    }
    let mut ids = HashSet::new();
    for stage in &s.stages {
        if let Some(Some(t)) = stage.observed_at {
            ensure!(
                t - now <= chrono::Duration::seconds(60),
                "future stage timestamp"
            );
            ensure!(
                t - s.generated_at <= chrono::Duration::seconds(60),
                "stage observed_at is after generated_at"
            );
        }
        text(&stage.id, 256, false)?;
        text(&stage.label, 128, false)?;
        text(&stage.detail, 512, true)?;
        ensure!(ids.insert(&stage.id), "duplicate stage id");
        ensure!(
            stage.count.is_none_or(|n| n <= MAX_COUNT),
            "stage count too large"
        );
        ensure!(
            stage.state != StageState::Unknown || stage.count.is_none(),
            "unknown stage must have null count"
        );
        ensure!(
            !stage.historical || matches!(stage.observed_at, Some(Some(_))),
            "historical stage needs a source timestamp"
        );
        ensure!(
            stage
                .max_age_secs
                .is_none_or(|n| (1..=MAX_STAGE_BUDGET_SECS).contains(&n)),
            "stage age budget out of range"
        );
    }
    for issue in &s.issues {
        text(&issue.code, 64, false)?;
        text(&issue.detail, 512, true)?;
    }
    ensure!(
        [
            s.runs.total,
            s.runs.pages,
            s.artifacts.locations,
            s.artifacts.verified,
            s.artifacts.missing
        ]
        .iter()
        .all(|&n| n <= MAX_COUNT),
        "count too large"
    );
    ensure!(
        s.artifacts.verified + s.artifacts.missing <= s.artifacts.locations,
        "verified + missing exceeds artifact locations"
    );
    Ok(s)
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineView {
    pub retrieved_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub age_secs: Option<i64>,
    pub stale: bool,
    pub stage_freshness: Vec<StageFreshness>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<Snapshot>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StageFreshness {
    pub id: String,
    pub age_secs: Option<i64>,
    pub stale: bool,
    pub historical: bool,
}

impl Default for PipelineView {
    fn default() -> Self {
        Self {
            retrieved_at: None,
            last_error: Some("pipeline snapshot not configured".into()),
            age_secs: None,
            stale: true,
            stage_freshness: Vec::new(),
            snapshot: None,
        }
    }
}

pub struct Pipeline {
    path: Option<PathBuf>,
    max_age_secs: u64,
    last: PipelineView,
}

impl Pipeline {
    pub fn new(path: Option<PathBuf>, max_age_secs: u64) -> Self {
        let mut last = PipelineView::default();
        if path.is_some() {
            last.last_error = Some("pipeline snapshot not yet read".into());
        }
        Self {
            path,
            max_age_secs,
            last,
        }
    }
}

impl Pipeline {
    pub fn poll(&mut self, now: DateTime<Utc>) {
        let Some(path) = &self.path else { return };
        let result = (|| {
            let metadata =
                std::fs::metadata(path).context("pipeline snapshot missing/unreadable")?;
            ensure!(
                metadata.is_file(),
                "pipeline snapshot must be a regular file"
            );
            ensure!(metadata.len() <= MAX_BYTES as u64, "snapshot exceeds 1 MiB");
            let mut bytes = Vec::new();
            std::fs::File::open(path)
                .context("pipeline snapshot unreadable")?
                .take(MAX_BYTES as u64 + 1)
                .read_to_end(&mut bytes)
                .context("pipeline snapshot read failed")?;
            parse(&bytes, now)
        })();
        match result {
            Ok(snapshot) => {
                self.last.snapshot = Some(snapshot);
                self.last.retrieved_at = Some(now);
                self.last.last_error = None;
            }
            // Do not include paths or input content in the served error.
            Err(e) => self.last.last_error = Some(e.to_string()),
        }
    }

    pub fn view(&self, now: DateTime<Utc>) -> PipelineView {
        let mut v = self.last.clone();
        v.age_secs = v
            .snapshot
            .as_ref()
            .and_then(|s| s.observed_at)
            .map(|t| (now - t).num_seconds().max(0));
        // The overall age is the oldest non-historical stage's, so it can't be held to a tighter
        // budget than that stage's own. Each stage is checked against its own below.
        let budget = v
            .snapshot
            .iter()
            .flat_map(|s| &s.stages)
            .filter(|stage| !stage.historical)
            .filter_map(|stage| stage.max_age_secs)
            .max()
            .unwrap_or(0)
            .max(self.max_age_secs);
        let unavailable = match v.age_secs {
            None => Some("source evidence time unknown; snapshot unavailable"),
            Some(age) if age as u64 > budget => Some("source evidence is stale"),
            _ => None,
        };
        v.stale = v.last_error.is_some() || unavailable.is_some();
        if v.last_error.is_none() {
            v.last_error = unavailable.map(str::to_owned);
        }
        if let Some(s) = &v.snapshot {
            v.stage_freshness = s
                .stages
                .iter()
                .map(|stage| {
                    let age = stage
                        .observed_at
                        .unwrap_or(s.observed_at)
                        .map(|t| (now - t).num_seconds().max(0));
                    StageFreshness {
                        id: stage.id.clone(),
                        age_secs: age,
                        stale: self.last.last_error.is_some()
                            || age.is_none_or(|a| {
                                a as u64 > stage.max_age_secs.unwrap_or(self.max_age_secs)
                            }),
                        historical: stage.historical,
                    }
                })
                .collect();
            if s.stages
                .iter()
                .zip(&v.stage_freshness)
                .any(|(stage, freshness)| freshness.stale && !stage.historical)
            {
                v.stale = true;
                if v.last_error.is_none() {
                    v.last_error = Some("stage source evidence is stale or unknown".into());
                }
            }
        }
        v
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use chrono::Duration;
    use serde_json::{json, Value};

    pub(crate) fn fixture(now: DateTime<Utc>) -> Value {
        json!({
            "schema_version": 1,
            "observed_at": now.to_rfc3339(),
            "generated_at": now.to_rfc3339(),
            "producer": "corpus-catalog",
            "coverage": ["local metadata"],
            "stages": [
                {"id":"scan","label":"Scans","count":1234,"state":"observed","detail":"metadata only"},
                {"id":"search","label":"Search","count":null,"state":"unknown","detail":"not measured"}
            ],
            "runs":{"total":2,"pages":100},
            "artifacts":{"locations":5,"verified":2,"missing":1},
            "issues":[{"code":"partial","detail":"local coverage only"}]
        })
    }

    fn check(v: &Value, now: DateTime<Utc>) -> anyhow::Result<Snapshot> {
        parse(&serde_json::to_vec(v).unwrap(), now)
    }

    #[test]
    fn strict_schema_and_bounds() {
        let now = Utc::now();
        let good = fixture(now);
        assert!(check(&good, now).is_ok());
        for (pointer, bad) in [
            ("/schema_version", json!(2)),
            ("/producer", json!("other")),
            ("/stages/0/count", json!(-1)),
            ("/stages/0/count", json!(1.5)),
            ("/stages/0/count", json!(MAX_COUNT + 1)),
            ("/stages/0/state", json!("ready")),
            ("/stages/1/count", json!(0)),
            ("/stages/0/id", json!("")),
            ("/stages/0/label", json!("x".repeat(129))),
            ("/stages/0/detail", json!("x".repeat(513))),
            ("/coverage", json!(["x".repeat(129)])),
            ("/coverage", json!(vec!["x"; 33])),
            ("/stages", json!(vec![good["stages"][0].clone(); 65])),
            ("/issues", json!(vec![good["issues"][0].clone(); 129])),
            ("/runs/pages", json!(MAX_COUNT + 1)),
            ("/artifacts/locations", json!(MAX_COUNT + 1)),
            ("/artifacts/missing", json!(4)),
            ("/issues/0/code", json!("bad\ncode")),
            ("/observed_at", json!("2026-10-16T12:00:00-04:00")),
            ("/generated_at", json!("not a timestamp")),
        ] {
            let mut v = good.clone();
            *v.pointer_mut(pointer).unwrap() = bad;
            assert!(check(&v, now).is_err(), "{pointer}: {v}");
        }
        for field in [
            "observed_at",
            "generated_at",
            "coverage",
            "stages",
            "issues",
        ] {
            let mut v = good.clone();
            v.as_object_mut().unwrap().remove(field);
            assert!(check(&v, now).is_err(), "{field}");
        }
        let mut v = good.clone();
        v["stages"][0].as_object_mut().unwrap().remove("count");
        assert!(check(&v, now).is_err());
        v = good.clone();
        v["ocr_text"] = json!("not permitted");
        assert!(check(&v, now).is_err());
        v = good.clone();
        v["stages"][0]["storage_uri"] = json!("not permitted");
        assert!(check(&v, now).is_err());
        v = good.clone();
        v["stages"][1]["id"] = json!("scan");
        assert!(check(&v, now).is_err());
        assert!(parse(b"not json", now).is_err());
        assert!(parse(&vec![b' '; MAX_BYTES + 1], now).is_err());
    }

    #[test]
    fn missing_malformed_oversized_future_retain_valid_and_recover() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        p.poll(now);
        let v = p.view(now);
        assert!(v.stale && v.snapshot.is_none());
        assert!(v.last_error.unwrap().contains("missing/unreadable"));
        let good = serde_json::to_vec(&fixture(now)).unwrap();
        std::fs::write(&path, &good).unwrap();
        p.poll(now);
        assert!(!p.view(now).stale);
        let mut future = fixture(now);
        future["generated_at"] = json!((now + Duration::seconds(120)).to_rfc3339());
        for bad in [
            b"{".to_vec(),
            vec![b' '; MAX_BYTES + 1],
            serde_json::to_vec(&future).unwrap(),
        ] {
            std::fs::write(&path, bad).unwrap();
            p.poll(now + Duration::seconds(1));
            let v = p.view(now + Duration::seconds(1));
            assert!(v.stale && v.last_error.is_some());
            assert_eq!(v.snapshot.unwrap().runs.total, 2);
            assert_eq!(v.retrieved_at, Some(now));
        }
        std::fs::remove_file(&path).unwrap();
        p.poll(now);
        assert!(p.view(now).snapshot.is_some());
        std::fs::write(&path, good).unwrap();
        p.poll(now + Duration::seconds(2));
        assert!(!p.view(now + Duration::seconds(2)).stale);
    }

    #[test]
    fn freshness_uses_source_evidence_not_generation_or_retrieval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        let mut v = fixture(now);
        v["observed_at"] = json!((now - Duration::seconds(901)).to_rfc3339());
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        p.poll(now);
        assert!(p.view(now).stale);
        assert_eq!(p.view(now).age_secs, Some(901));
        v["observed_at"] = Value::Null;
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        p.poll(now);
        assert!(p.view(now).stale);
        assert_eq!(p.view(now).age_secs, None);
        v["observed_at"] = json!(now.to_rfc3339());
        std::fs::write(&path, serde_json::to_vec(&v).unwrap()).unwrap();
        p.poll(now);
        assert!(!p.view(now + Duration::seconds(900)).stale);
        assert!(p.view(now + Duration::seconds(901)).stale);
        for field in ["observed_at", "generated_at"] {
            let mut v = fixture(now);
            v[field] = json!((now + Duration::seconds(61)).to_rfc3339());
            assert!(check(&v, now).is_err());
        }
        assert!(Pipeline::new(None, 900).view(now).stale);
    }

    #[test]
    fn fresh_stage_cannot_hide_stale_or_unknown_stage_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 60);
        let mut source = fixture(now);
        source["stages"][0]["observed_at"] = json!((now - Duration::seconds(61)).to_rfc3339());
        source["stages"][1]["observed_at"] = json!(now.to_rfc3339());
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        p.poll(now);
        let view = p.view(now);
        assert!(view.stale);
        assert_eq!(view.age_secs, Some(0));
        assert_eq!(view.stage_freshness[0].age_secs, Some(61));
        assert!(view.stage_freshness[0].stale);
        assert!(!view.stage_freshness[1].stale);
        source["stages"][0]["observed_at"] = Value::Null;
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        p.poll(now);
        assert!(p.view(now).stage_freshness[0].stale);
        assert_eq!(p.view(now).stage_freshness[0].age_secs, None);
        source["stages"][0]["observed_at"] = json!((now + Duration::seconds(61)).to_rfc3339());
        assert!(check(&source, now).is_err());
        source["stages"][0]["observed_at"] = json!("invalid");
        assert!(check(&source, now).is_err());
    }

    /// A snapshot like the observer's: a fresh stage and one more, written to `path`.
    fn poll_with(
        p: &mut Pipeline,
        path: &std::path::Path,
        now: DateTime<Utc>,
        oldest: Duration,
        second: Value,
    ) {
        let mut source = fixture(now);
        source["observed_at"] = json!((now - oldest).to_rfc3339());
        source["stages"][0]["observed_at"] = json!((now - oldest).to_rfc3339());
        source["stages"][1] = second;
        std::fs::write(path, serde_json::to_vec(&source).unwrap()).unwrap();
        p.poll(now);
    }

    #[test]
    fn historical_stage_keeps_its_age_but_does_not_make_the_panel_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        poll_with(
            &mut p,
            &path,
            now,
            Duration::seconds(60),
            json!({"id":"accepted","label":"Accepted","count":1,"state":"observed","detail":"",
                   "observed_at":(now - Duration::days(8)).to_rfc3339(),"historical":true}),
        );
        let view = p.view(now);
        assert!(!view.stale, "{:?}", view.last_error);
        assert_eq!(view.age_secs, Some(60));
        let accepted = &view.stage_freshness[1];
        assert_eq!(accepted.age_secs, Some(8 * 86400));
        assert!(accepted.stale && accepted.historical);
    }

    #[test]
    fn the_same_old_stage_without_the_mark_still_makes_the_panel_stale() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        poll_with(
            &mut p,
            &path,
            now,
            Duration::seconds(60),
            json!({"id":"accepted","label":"Accepted","count":1,"state":"observed","detail":"",
                   "observed_at":(now - Duration::days(8)).to_rfc3339()}),
        );
        let view = p.view(now);
        assert!(view.stale);
        assert_eq!(
            view.last_error.as_deref(),
            Some("stage source evidence is stale or unknown")
        );
    }

    #[test]
    fn unknown_stage_still_makes_the_panel_stale_beside_historical_ones() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        poll_with(
            &mut p,
            &path,
            now,
            Duration::seconds(60),
            json!({"id":"queued","label":"Queued","count":null,"state":"unknown","detail":"",
                   "observed_at":null}),
        );
        assert!(p.view(now).stale);
    }

    #[test]
    fn a_stage_inheriting_the_snapshot_time_keeps_the_default_budget() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        let mut source = fixture(now);
        source["observed_at"] = json!((now - Duration::seconds(3600)).to_rfc3339());
        source["stages"][1] = json!({
            "id":"imported","label":"Imported","count":71,"state":"observed","detail":"",
            "observed_at":(now - Duration::seconds(3600)).to_rfc3339(),"max_age_secs":10800
        });
        std::fs::write(&path, serde_json::to_vec(&source).unwrap()).unwrap();
        p.poll(now);
        let view = p.view(now);
        assert!(view.stage_freshness[0].stale);
        assert!(view.stale);
    }

    #[test]
    fn a_stage_budget_replaces_the_default_for_that_stage_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("pipeline.json");
        let now = Utc::now();
        let mut p = Pipeline::new(Some(path.clone()), 900);
        let hourly = |age: i64| {
            json!({"id":"imported","label":"Imported","count":71,"state":"observed","detail":"",
                   "observed_at":(now - Duration::seconds(age)).to_rfc3339(),
                   "max_age_secs":10800})
        };
        poll_with(&mut p, &path, now, Duration::seconds(7200), hourly(7200));
        // The first stage has no budget of its own, so at this age it is stale.
        assert!(p.view(now).stale);
        assert!(p.view(now).stage_freshness[0].stale);
        assert!(!p.view(now).stage_freshness[1].stale);
        poll_with(&mut p, &path, now, Duration::seconds(60), hourly(7200));
        let view = p.view(now);
        assert!(!view.stale, "{:?}", view.last_error);
        assert_eq!(view.age_secs, Some(60));
        poll_with(&mut p, &path, now, Duration::seconds(60), hourly(10801));
        assert!(p.view(now).stale);
        assert!(p.view(now).stage_freshness[1].stale);
    }

    #[test]
    fn historical_and_budget_fields_are_checked() {
        let now = Utc::now();
        let mut v = fixture(now);
        assert!(check(&v, now).is_ok());
        v["stages"][1]["historical"] = json!(true);
        assert!(check(&v, now).is_err(), "no timestamp");
        v = fixture(now);
        v["stages"][0]["historical"] = json!(true);
        assert!(check(&v, now).is_err(), "inherited timestamp");
        v["stages"][0]["observed_at"] = json!(now.to_rfc3339());
        assert!(check(&v, now).is_ok());
        v["stages"][0]["historical"] = json!("yes");
        assert!(check(&v, now).is_err());
        for budget in [json!(0), json!(MAX_STAGE_BUDGET_SECS + 1), json!(-1)] {
            let mut v = fixture(now);
            v["stages"][0]["max_age_secs"] = budget;
            assert!(check(&v, now).is_err());
        }
    }
}
