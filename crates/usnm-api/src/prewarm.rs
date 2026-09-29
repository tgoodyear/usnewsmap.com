//! Cache warm-up before a version serves (06 §6.6).
//!
//! The first search against a cold index can take longer than the request
//! timeout, so before a replica serves a version (at start, and before a
//! reload swaps a new one in) it runs what the site's first visitors load:
//! the places layer, and each example search from the home page with the
//! coverage cube its response links to. The requests go through the same
//! handlers as visitors' requests, pinned to the new version, so the search
//! engine's caches and the response caches (in-process, and the persistent
//! cache for slow ones) hold exactly the entries those visitors will ask for.
//!
//! Queries run one at a time (the searcher has one vCPU), each with
//! `config.prewarm_query_timeout` (the searcher's own limit per call still
//! applies), all within `config.prewarm_budget`. Nothing here fails: a
//! warm-up that times out or errors is logged and the version serves anyway.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::http::{StatusCode, Uri};
use axum::response::Response;
use serde::Deserialize;

use crate::error::ApiError;
use crate::routes::{self, Ctx};
use crate::{AppState, Snapshot};

/// A warm-up query slower than this gets its own log line.
const SLOW: Duration = Duration::from_secs(1);

/// Largest response body read back to find the coverage link.
const MAX_BODY: usize = 64 * 1024 * 1024;

/// The home page's example searches, shared with the web app.
const EXAMPLES_JSON: &str = include_str!("../../../web/src/examples.json");

#[derive(Debug, Deserialize)]
pub struct Example {
    pub id: String,
    /// The `/v1/aggregate` query string the web app sends for this example,
    /// without `v` (web/src/examples.test.ts checks it).
    pub aggregate: String,
}

static EXAMPLES: LazyLock<Vec<Example>> = LazyLock::new(|| {
    serde_json::from_str(EXAMPLES_JSON).unwrap_or_else(|e| {
        // Checked by this module's tests; never block serving over it.
        tracing::error!(error = %e, "web/src/examples.json is unreadable; warming places only");
        Vec::new()
    })
});

/// The example searches the warm-up runs.
pub fn examples() -> &'static [Example] {
    &EXAMPLES
}

/// Why a warm-up ran.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Trigger {
    /// The replica started.
    Startup,
    /// A new version is about to be swapped in.
    Publish,
}

impl Trigger {
    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Publish => "publish",
        }
    }
}

/// What one warm-up run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub queries: usize,
    pub ok: usize,
    pub timed_out: usize,
    pub failed: usize,
    /// Not run because the budget ran out.
    pub skipped: usize,
    pub elapsed: Duration,
}

enum Outcome {
    Ok(Response),
    TimedOut,
    Failed(String),
    Skipped,
}

impl Outcome {
    fn label(&self) -> &'static str {
        match self {
            Self::Ok(_) => "ok",
            Self::TimedOut => "timeout",
            Self::Failed(_) => "error",
            Self::Skipped => "skipped",
        }
    }
}

#[derive(Clone, Copy)]
enum Endpoint {
    Places,
    Aggregate,
    Coverage,
}

impl Endpoint {
    fn as_str(self) -> &'static str {
        match self {
            Self::Places => "places",
            Self::Aggregate => "aggregate",
            Self::Coverage => "coverage",
        }
    }
}

struct Run<'a> {
    state: &'a Arc<AppState>,
    snap: Arc<Snapshot>,
    deadline: Instant,
    report: Report,
}

impl Run<'_> {
    async fn query(&mut self, endpoint: Endpoint, example: &str, uri: &str) -> Option<Response> {
        self.report.queries += 1;
        let started = Instant::now();
        let outcome = match uri.parse::<Uri>() {
            Err(e) => Outcome::Failed(e.to_string()),
            Ok(uri) => self.call(endpoint, &uri).await,
        };
        let elapsed = started.elapsed();
        match &outcome {
            Outcome::Ok(_) => self.report.ok += 1,
            Outcome::TimedOut => self.report.timed_out += 1,
            Outcome::Failed(_) => self.report.failed += 1,
            Outcome::Skipped => self.report.skipped += 1,
        }
        let version = self.snap.refdata.version();
        let ms = elapsed.as_millis() as u64;
        match &outcome {
            Outcome::Failed(error) => tracing::warn!(
                version,
                endpoint = endpoint.as_str(),
                example,
                ms,
                error = %error,
                "warm-up query failed"
            ),
            Outcome::Skipped => {}
            _ if elapsed >= SLOW => tracing::info!(
                version,
                endpoint = endpoint.as_str(),
                example,
                outcome = outcome.label(),
                ms,
                "slow warm-up query"
            ),
            _ => {}
        }
        match outcome {
            Outcome::Ok(resp) => Some(resp),
            _ => None,
        }
    }

    async fn call(&self, endpoint: Endpoint, uri: &Uri) -> Outcome {
        let remaining = self.deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Outcome::Skipped;
        }
        let limit = self.state.config.prewarm_query_timeout.min(remaining);
        let ctx = Ctx {
            snap: self.snap.clone(),
            timeout: limit,
            warm_up: true,
        };
        let state = self.state;
        let served = tokio::time::timeout(limit, async move {
            match endpoint {
                Endpoint::Places => routes::places_in(state, ctx, uri).await,
                Endpoint::Aggregate => routes::aggregate_in(state, ctx, uri).await,
                Endpoint::Coverage => routes::coverage_in(state, ctx, uri).await,
            }
        })
        .await;
        match served {
            Err(_) | Ok(Err(ApiError::Timeout)) => Outcome::TimedOut,
            Ok(Err(e)) => Outcome::Failed(format!("{e:?}")),
            Ok(Ok(resp)) if resp.status() == StatusCode::OK => Outcome::Ok(resp),
            Ok(Ok(resp)) => Outcome::Failed(format!("status {}", resp.status())),
        }
    }
}

/// The coverage link (`cube.baseline_ref`) in an aggregate response, which
/// the web app fetches next.
async fn baseline_ref(resp: Response) -> Option<String> {
    let bytes = axum::body::to_bytes(resp.into_body(), MAX_BODY)
        .await
        .ok()?;
    let body: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    body["cube"]["baseline_ref"].as_str().map(str::to_owned)
}

/// Warm the caches for `snap`'s version: the places layer, then each example
/// search and its coverage cube, as the web app requests them. Logs one line
/// for the run and records `api.prewarm_*`.
pub async fn run(state: &Arc<AppState>, snap: Arc<Snapshot>, trigger: Trigger) -> Report {
    let started = Instant::now();
    let version = snap.refdata.version().to_owned();
    let v: String = form_urlencoded::byte_serialize(version.as_bytes()).collect();
    let mut run = Run {
        state,
        snap,
        deadline: started + state.config.prewarm_budget,
        report: Report::default(),
    };
    run.query(Endpoint::Places, "", &format!("/v1/places?v={v}"))
        .await;
    for ex in examples() {
        let uri = format!("/v1/aggregate?{}&v={v}", ex.aggregate);
        let Some(resp) = run.query(Endpoint::Aggregate, &ex.id, &uri).await else {
            continue;
        };
        if let Some(coverage) = baseline_ref(resp).await {
            run.query(Endpoint::Coverage, &ex.id, &coverage).await;
        }
    }
    let mut report = run.report;
    report.elapsed = started.elapsed();
    tracing::info!(
        trigger = trigger.as_str(),
        version,
        queries = report.queries,
        ok = report.ok,
        timed_out = report.timed_out,
        failed = report.failed,
        skipped = report.skipped,
        ms = report.elapsed.as_millis() as u64,
        "warm-up finished"
    );
    state.metrics.prewarm(trigger.as_str(), &report);
    report
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use usnm_core::params::{RawParams, SearchRequest};

    #[test]
    fn examples_are_valid_aggregate_queries() {
        let ex = examples();
        assert!(!ex.is_empty(), "web/src/examples.json lists no examples");
        let bounds = (
            NaiveDate::from_ymd_opt(1770, 1, 1).unwrap(),
            NaiveDate::from_ymd_opt(1963, 12, 31).unwrap(),
        );
        for e in ex {
            let raw = RawParams::parse(&e.aggregate).unwrap();
            raw.reject_unknown(&[]).unwrap();
            assert!(raw.get("v").is_none(), "{}: `v` is added per version", e.id);
            SearchRequest::from_raw(&raw, bounds).unwrap_or_else(|err| panic!("{}: {err}", e.id));
        }
    }
}
