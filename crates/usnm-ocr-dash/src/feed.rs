//! Read the monitor's aggregate-only output; retrieval never renews evidence age.
use std::io::Read;
use std::io::Write;
use std::path::Path;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

const CHECKS: [&str; 7] = [
    "qwen_refill_window",
    "usnm_ocr_no_work",
    "usnm_qwen_no_work",
    "cpu_shortage",
    "gpu_shortage",
    "controller_tick",
    "collection",
];
/// Opt-in checks (#1462, #1698): each appears at most once, and no other code is accepted.
/// Deploy a dashboard that knows a code before an evaluator config turns it on.
const OPTIONAL_CHECKS: [&str; 5] = [
    "feeder_allocation",
    "controller_cap",
    "ownership_conflict",
    "controller_progress",
    "tick_trend",
];
const MAX_BYTES: u64 = 65536;
const MAX_AGE: i64 = 180;
pub const MAX_SOURCE_BYTES: usize = 8 * 1024 * 1024;

pub fn source_kind(name: &str) -> Option<&'static str> {
    match name {
        "server" => Some("server"),
        "controller" => Some("controller"),
        "backlog" => Some("backlog"),
        _ if name.starts_with("client-")
            && name.len() <= 60
            && name.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-') =>
        {
            Some("client")
        }
        _ => None,
    }
}

/// Private source envelopes are never exposed by the public view or a GET route.
pub fn save_source(root: &Path, name: &str, body: &[u8], now: DateTime<Utc>) -> anyhow::Result<()> {
    let kind = source_kind(name).ok_or_else(|| anyhow::anyhow!("unknown source"))?;
    anyhow::ensure!(body.len() <= MAX_SOURCE_BYTES, "source size limit");
    let value: serde_json::Value = serde_json::from_slice(body)?;
    let at = value["observed_at"]
        .as_i64()
        .ok_or_else(|| anyhow::anyhow!("missing time"))?;
    anyhow::ensure!(
        value["schema_version"] == 1
            && value["source"] == kind
            && (0..=MAX_AGE).contains(&(now.timestamp() - at)),
        "invalid source envelope"
    );
    std::fs::create_dir_all(root)?;
    let target = root.join(format!("{name}.json"));
    // The caller serializes writes. Reject an older delivery from the same source.
    if let Ok(old) = std::fs::read(&target) {
        if let Ok(old) = serde_json::from_slice::<serde_json::Value>(&old) {
            anyhow::ensure!(
                old["observed_at"]
                    .as_i64()
                    .is_none_or(|old| old <= at || old > now.timestamp()),
                "old delivery"
            );
        }
    }
    let pending = root.join(format!("{name}.new"));
    let result = (|| -> anyhow::Result<()> {
        let mut file = std::fs::File::create(&pending)?;
        file.write_all(body)?;
        file.sync_all()?;
        std::fs::rename(&pending, &target)?;
        std::fs::File::open(root)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(pending);
    }
    result
}

#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Healthy,
    Pending,
    Firing,
    Unknown,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Check {
    pub code: String,
    pub state: State,
    pub duration_s: u64,
    pub threshold_s: u64,
    pub remaining_s: Option<i64>,
    pub backlog: Option<u64>,
    pub last_workunit_at: Option<u64>,
    pub idle_slots: Option<u64>,
    pub eligible_results: Option<u64>,
    pub reason: Option<String>,
    pub seconds: Option<f64>,
    pub app_ids: Option<Vec<u64>>,
    /// controller_tick (#1698): runs in a row that failed with the same error; sticky at 3.
    pub same_error_in_a_row: Option<u64>,
    pub sticky: Option<bool>,
    /// controller_cap: the cap nearest its limit, as a percentage.
    pub cap: Option<String>,
    pub percent: Option<f64>,
    pub ownership_conflicts: Option<u64>,
    /// The source an unknown check waits on, such as "source:server".
    pub cause: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Document {
    schema_version: u32,
    event: String,
    observed_at: i64,
    checks: Vec<Check>,
}

#[derive(Debug, Serialize)]
pub struct FeedView {
    pub observed_at: Option<i64>,
    pub stale: bool,
    pub error: Option<&'static str>,
    pub checks: Vec<Check>,
}

impl Default for FeedView {
    fn default() -> Self {
        Self {
            observed_at: None,
            stale: true,
            error: Some("BOINC feed monitor is not configured"),
            checks: Vec::new(),
        }
    }
}

fn parse(bytes: &[u8], now: DateTime<Utc>) -> anyhow::Result<FeedView> {
    anyhow::ensure!(bytes.len() <= MAX_BYTES as usize, "size limit");
    let doc: Document = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        doc.schema_version == 1
            && doc.event == "boinc_feed_health"
            && doc.checks.iter().all(|c| CHECKS
                .iter()
                .chain(&OPTIONAL_CHECKS)
                .any(|code| c.code == *code))
            && CHECKS.iter().chain(&OPTIONAL_CHECKS).all(|code| {
                let count = doc.checks.iter().filter(|c| c.code == *code).count();
                count == 1 || count == 0 && OPTIONAL_CHECKS.contains(code)
            }),
        "invalid feed contract"
    );
    let stale = !(0..=MAX_AGE).contains(&(now.timestamp() - doc.observed_at));
    let mut checks = doc.checks;
    if stale {
        for check in &mut checks {
            check.state = State::Unknown;
        }
    }
    Ok(FeedView {
        observed_at: Some(doc.observed_at),
        stale: stale || checks.iter().any(|c| c.state == State::Unknown),
        error: stale.then_some("BOINC feed evidence is stale or future-dated"),
        checks,
    })
}

pub fn read(path: Option<&Path>, now: DateTime<Utc>) -> FeedView {
    let Some(path) = path else {
        return FeedView::default();
    };
    let result = (|| {
        let mut bytes = Vec::new();
        std::fs::File::open(path)?
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)?;
        parse(&bytes, now)
    })();
    result.unwrap_or_else(|_: anyhow::Error| FeedView {
        error: Some("BOINC feed evidence is unavailable or invalid"),
        ..FeedView::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn document(at: i64) -> serde_json::Value {
        json!({
            "schema_version": 1, "event": "boinc_feed_health", "observed_at": at,
            "checks": CHECKS.iter().map(|code| json!({
                "code": code, "state": "healthy", "duration_s": 0, "threshold_s": 0
            })).collect::<Vec<_>>()
        })
    }

    #[test]
    fn unknown_and_stale_never_become_healthy() {
        let now = Utc::now();
        for age in [181, -1] {
            let value = document(now.timestamp() - age);
            let view = parse(&serde_json::to_vec(&value).unwrap(), now).unwrap();
            assert!(view.stale);
            assert!(view.checks.iter().all(|c| c.state == State::Unknown));
        }
        let mut value = document(now.timestamp());
        value["checks"][0]["state"] = json!("unknown");
        assert!(
            parse(&serde_json::to_vec(&value).unwrap(), now)
                .unwrap()
                .stale
        );
        value["checks"][0]["state"] = json!("firing");
        let view = parse(&serde_json::to_vec(&value).unwrap(), now).unwrap();
        assert!(!view.stale);
        assert_eq!(view.checks[0].state, State::Firing);
    }

    #[test]
    fn partial_duplicate_and_malformed_contracts_fail_closed() {
        let now = Utc::now();
        let optional = json!({
            "code": "feeder_allocation", "state": "pending", "duration_s": 60,
            "threshold_s": 600, "reason": "registry_changed", "app_ids": [3, 7]
        });
        let signal = json!({
            "code": "controller_cap", "state": "firing", "duration_s": 0,
            "threshold_s": 0, "cap": "dispatch_journals", "percent": 96.1
        });
        for edit in [
            "missing",
            "duplicate",
            "state",
            "unknown_code",
            "duplicate_optional",
            "duplicate_signal",
            "optional_replaces_required",
        ] {
            let mut value = document(now.timestamp());
            let checks = value["checks"].as_array_mut().unwrap();
            match edit {
                "missing" => {
                    checks.pop();
                }
                "duplicate" => checks[0]["code"] = json!(CHECKS[1]),
                "unknown_code" => checks.push(json!({
                    "code": "other", "state": "healthy", "duration_s": 0, "threshold_s": 0
                })),
                "duplicate_optional" => checks.extend([optional.clone(), optional.clone()]),
                "duplicate_signal" => checks.extend([signal.clone(), signal.clone()]),
                "optional_replaces_required" => checks[0] = optional.clone(),
                _ => checks[0]["state"] = json!("fine"),
            }
            assert!(parse(&serde_json::to_vec(&value).unwrap(), now).is_err());
        }
        let mut value = document(now.timestamp());
        value["checks"].as_array_mut().unwrap().push(optional);
        let view = parse(&serde_json::to_vec(&value).unwrap(), now).unwrap();
        assert_eq!(view.checks.len(), CHECKS.len() + 1);
        assert_eq!(view.checks[CHECKS.len()].app_ids, Some(vec![3, 7]));
        // #1698: the controller's signal checks, and a cause on an unknown check.
        let mut value = document(now.timestamp());
        let checks = value["checks"].as_array_mut().unwrap();
        checks[0]["state"] = json!("unknown");
        checks[0]["cause"] = json!("source:server");
        checks.push(signal);
        for code in ["ownership_conflict", "controller_progress", "tick_trend"] {
            checks
                .push(json!({"code": code, "state": "healthy", "duration_s": 0, "threshold_s": 0}));
        }
        let view = parse(&serde_json::to_vec(&value).unwrap(), now).unwrap();
        assert_eq!(view.checks.len(), CHECKS.len() + 4);
        assert_eq!(view.checks[0].cause.as_deref(), Some("source:server"));
        assert_eq!(view.checks[CHECKS.len()].percent, Some(96.1));
        assert!(read(Some(Path::new("/no-such-feed-file")), now).stale);
    }

    #[test]
    fn source_delivery_preserves_time_and_rejects_replay_or_wrong_identity() {
        let root = std::env::temp_dir().join(format!("usnm-feed-sources-{}", std::process::id()));
        let now = Utc::now();
        let mut value = json!({"schema_version":1,"source":"server","observed_at":now.timestamp()});
        save_source(&root, "server", &serde_json::to_vec(&value).unwrap(), now).unwrap();
        value["observed_at"] = json!(now.timestamp() - 1);
        assert!(save_source(&root, "server", &serde_json::to_vec(&value).unwrap(), now).is_err());
        let rolled_back = now - chrono::Duration::seconds(1);
        save_source(
            &root,
            "server",
            &serde_json::to_vec(&value).unwrap(),
            rolled_back,
        )
        .unwrap();
        assert!(save_source(
            &root,
            "client-own",
            &serde_json::to_vec(&value).unwrap(),
            now
        )
        .is_err());
        assert!(save_source(&root, "../escape", b"{}", now).is_err());
        std::fs::remove_dir_all(root).unwrap();
    }
}
