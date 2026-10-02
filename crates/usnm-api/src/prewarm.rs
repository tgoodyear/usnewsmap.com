//! Cache warm-up before a version serves (06 §6.6).
//!
//! The first search against a cold index can take longer than the request
//! timeout, so before a replica serves a version (at start, and before a
//! reload swaps a new one in) it runs what the site's first visitors load:
//! the places layer, and each example search from the home page with the
//! coverage cube its response links to; then, with what is left of the
//! budget, the searches visitors made most often recently, read from the
//! search log (06 §6.8), each with its coverage cube. The requests go through the same
//! handlers as visitors' requests, pinned to the new version, so the search
//! engine's caches and the response caches (in-process, and the persistent
//! cache for slow ones) hold exactly the entries those visitors will ask for.
//!
//! Queries run one at a time (the searcher has one vCPU), each with
//! `config.prewarm_query_timeout` (the searcher's own limit per call still
//! applies), all within `config.prewarm_budget` before a publish, or
//! `config.prewarm_startup_budget` after a start. Nothing here fails: a
//! warm-up that times out or errors is logged and the version serves anyway.
//!
//! Searches from the log are logged by rank only (`search-log-3`), never by
//! their text, and their errors by kind only: an error message can quote
//! the query.

use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use axum::http::{StatusCode, Uri};
use axum::response::Response;
use serde::Deserialize;

use usnm_core::params::{RawParams, SearchRequest};

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
    /// The limit on a run: past the readiness cap a starting replica serves
    /// visitors while it warms, so its run stays short; a publish only
    /// delays the swap, so it can afford to warm every example.
    fn budget(self, config: &crate::config::Config) -> Duration {
        match self {
            Self::Startup => config.prewarm_startup_budget,
            Self::Publish => config.prewarm_budget,
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Publish => "publish",
        }
    }
}

/// Attempts per warm-up query when the backend fails.
const RETRY_ATTEMPTS: u32 = 6;
/// The longest pause between attempts.
const RETRY_PAUSE_MAX: Duration = Duration::from_secs(10);

/// What one warm-up run did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Report {
    pub queries: usize,
    pub ok: usize,
    pub timed_out: usize,
    pub failed: usize,
    /// Not run because the budget ran out.
    pub skipped: usize,
    /// Searches from the search log the run went on to (each with its
    /// coverage cube, which is counted in `queries` too).
    pub from_log: usize,
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
    /// One warm-up request, labelled `example` in the logs. A `private`
    /// one came from the search log: its error is logged by kind only.
    async fn query(
        &mut self,
        endpoint: Endpoint,
        example: &str,
        uri: &str,
        private: bool,
    ) -> Option<Response> {
        self.report.queries += 1;
        let started = Instant::now();
        let outcome = match uri.parse::<Uri>() {
            Err(_) if private => Outcome::Failed("invalid uri".to_owned()),
            Err(e) => Outcome::Failed(e.to_string()),
            Ok(uri) => self.call(endpoint, &uri, private).await,
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

    /// One query, retrying backend errors with a doubling pause while the
    /// run's budget allows. On the first prod start the searcher sidecar
    /// answered 500 for its first seconds and three warm-up queries failed
    /// within 5 ms each.
    async fn call(&self, endpoint: Endpoint, uri: &Uri, private: bool) -> Outcome {
        // One limit for the query across its attempts, so retries can't
        // stretch one example past `prewarm_query_timeout`.
        let deadline =
            (Instant::now() + self.state.config.prewarm_query_timeout).min(self.deadline);
        let mut pause = self.state.config.prewarm_retry_first;
        let mut attempt = 1;
        loop {
            let (outcome, retry) = self.call_once(endpoint, uri, deadline, private).await;
            let remaining = deadline.saturating_duration_since(Instant::now());
            if !retry || attempt >= RETRY_ATTEMPTS || remaining <= pause {
                return outcome;
            }
            tracing::info!(
                endpoint = endpoint.as_str(),
                attempt,
                pause_ms = pause.as_millis() as u64,
                "warm-up query hit a backend error; retrying"
            );
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(RETRY_PAUSE_MAX);
            attempt += 1;
        }
    }

    /// One attempt, and whether a failure is worth retrying (the backend,
    /// not the request, failed).
    async fn call_once(
        &self,
        endpoint: Endpoint,
        uri: &Uri,
        deadline: Instant,
        private: bool,
    ) -> (Outcome, bool) {
        let describe = |e: &ApiError| {
            if private {
                kind(e).to_owned()
            } else {
                format!("{e:?}")
            }
        };
        let now = Instant::now();
        if self.deadline <= now {
            return (Outcome::Skipped, false);
        }
        let limit = deadline.saturating_duration_since(now);
        if limit.is_zero() {
            return (Outcome::TimedOut, false);
        }
        let ctx = Ctx {
            snap: self.snap.clone(),
            timeout: limit,
            warm_up: true,
        };
        let state = self.state;
        let served = tokio::time::timeout(limit, async move {
            match endpoint {
                Endpoint::Places => routes::places_in(state, ctx, uri).await,
                Endpoint::Aggregate => routes::aggregate_in(state, ctx, uri, None).await,
                Endpoint::Coverage => routes::coverage_in(state, ctx, uri).await,
            }
        })
        .await;
        match served {
            Err(_) | Ok(Err(ApiError::Timeout)) => (Outcome::TimedOut, false),
            Ok(Err(e @ ApiError::Backend(_))) => (Outcome::Failed(describe(&e)), true),
            Ok(Err(e)) => (Outcome::Failed(describe(&e)), false),
            Ok(Ok(resp)) if resp.status() == StatusCode::OK => (Outcome::Ok(resp), false),
            Ok(Ok(resp)) => {
                let retry = resp.status().is_server_error();
                (Outcome::Failed(format!("status {}", resp.status())), retry)
            }
        }
    }
}

/// An error's kind, without its message.
fn kind(e: &ApiError) -> &'static str {
    match e {
        ApiError::Params(_) | ApiError::BadRequest(_) => "bad_parameter",
        ApiError::Unsupported(_) => "unsupported",
        ApiError::NotFound(_) => "not_found",
        ApiError::TooBroad(_) => "too_broad",
        ApiError::Timeout => "timeout",
        ApiError::Busy => "busy",
        ApiError::Backend(_) => "backend",
        ApiError::BackendRejected(_) => "backend_rejected",
        ApiError::RateLimited(_) => "rate_limited",
        ApiError::BadBeacon(_) | ApiError::TooLarge(_) | ApiError::MediaType(_) => "bad_request",
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

/// Limit on reading the search log for the warm-up.
const LOG_READ_LIMIT: Duration = Duration::from_secs(30);

/// The most frequent recent searches in the search log, as canonical
/// aggregate query strings, leaving out those in `skip` (the examples).
/// Empty without a search log, with `prewarm_top_searches` at 0, or when
/// reading takes longer than the time left.
async fn from_log(
    state: &AppState,
    snap: &Snapshot,
    skip: &[String],
    left: Duration,
) -> Vec<String> {
    let n = state.config.prewarm_top_searches;
    let Some(log) = state.search_log.as_deref().filter(|_| n > 0) else {
        return Vec::new();
    };
    let read = crate::searchlog::top_searches(
        log.store().as_ref(),
        chrono::Utc::now().date_naive(),
        state.config.prewarm_log_days,
        // Room for the examples it may contain.
        n + skip.len(),
        snap.refdata.bounds(),
    );
    match tokio::time::timeout(left.min(LOG_READ_LIMIT), read).await {
        Ok(top) => top
            .into_iter()
            .filter(|key| !skip.contains(key))
            .take(n)
            .collect(),
        Err(_) => {
            tracing::warn!("warm-up: reading the search log timed out; skipping its searches");
            Vec::new()
        }
    }
}

/// Warm the caches for `snap`'s version: the places layer, then each example
/// search and its coverage cube, as the web app requests them, then the most
/// frequent searches from the search log the same way while the budget
/// lasts. Logs one line for the run and records `api.prewarm_*`.
pub async fn run(state: &Arc<AppState>, snap: Arc<Snapshot>, trigger: Trigger) -> Report {
    let started = Instant::now();
    let version = snap.refdata.version().to_owned();
    let v: String = form_urlencoded::byte_serialize(version.as_bytes()).collect();
    let deadline = started + trigger.budget(&state.config);
    let mut run = Run {
        state,
        snap: snap.clone(),
        deadline,
        report: Report::default(),
    };
    run.query(Endpoint::Places, "", &format!("/v1/places?v={v}"), false)
        .await;
    for ex in examples() {
        let uri = format!("/v1/aggregate?{}&v={v}", ex.aggregate);
        let Some(resp) = run.query(Endpoint::Aggregate, &ex.id, &uri, false).await else {
            continue;
        };
        if let Some(coverage) = baseline_ref(resp).await {
            run.query(Endpoint::Coverage, &ex.id, &coverage, false)
                .await;
        }
    }
    let bounds = snap.refdata.bounds();
    let warmed: Vec<String> = examples()
        .iter()
        .filter_map(|ex| {
            let raw = RawParams::parse(&ex.aggregate).ok()?;
            SearchRequest::from_raw(&raw, bounds)
                .ok()
                .map(|r| r.canonical())
        })
        .collect();
    let left = deadline.saturating_duration_since(Instant::now());
    let top = if left.is_zero() {
        Vec::new()
    } else {
        from_log(state, &snap, &warmed, left).await
    };
    run.report.from_log = top.len();
    for (rank, key) in top.iter().enumerate() {
        let label = format!("search-log-{}", rank + 1);
        let uri = format!("/v1/aggregate?{key}&v={v}");
        let Some(resp) = run.query(Endpoint::Aggregate, &label, &uri, true).await else {
            continue;
        };
        if let Some(coverage) = baseline_ref(resp).await {
            run.query(Endpoint::Coverage, &label, &coverage, true).await;
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
        from_log = report.from_log,
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
