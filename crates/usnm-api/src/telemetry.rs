//! Request telemetry and the API's metrics (08 §8.1.2, 09 §9.2).
//!
//! Every request except the health probes gets one `request` span, which
//! the Application Insights exporter turns into a request (`AppRequests`):
//! method, the matched route template (`/v1/aggregate`), status code and
//! duration, failed only on a 5xx. It also gets one INFO log line with the
//! same fields, outside the span so it goes to the console (and Log
//! Analytics) only. Neither ever carries the raw path or query string: query
//! strings hold search text (09 §9.4.2).
//!
//! The metrics go to `AppMetrics` when telemetry is on; otherwise the
//! instruments come from the no-op global meter and record nothing.

use std::sync::{Arc, OnceLock, Weak};
use std::time::{Duration, Instant};

use axum::extract::{MatchedPath, Request, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::Response;
use opentelemetry::metrics::{Counter, Histogram, Meter, ObservableGauge};
use opentelemetry::KeyValue;
use tracing::field::Empty;
use tracing::Instrument;

use crate::error::ApiError;
use crate::AppState;

/// The cloud role the API reports as.
pub const SERVICE: usnm_telemetry::Service = usnm_telemetry::Service {
    name: "usnm-api",
    version: env!("CARGO_PKG_VERSION"),
};

/// Container Apps probes: counted, but neither traced nor logged.
const PROBES: [&str; 2] = ["/healthz", "/readyz"];

/// Route label for a request no API route matched: the site's files...
pub const SITE_ROUTE: &str = "(site)";
/// ...or an unknown path under `/v1` or `/api/v1` (and CORS preflights).
pub const NO_ROUTE: &str = "(no route)";

/// The API's instruments.
pub struct Metrics {
    /// Responses by `route` and `status_class` (`2xx`…), probes included.
    requests: Counter<u64>,
    /// Time to the response head, by `route`.
    duration: Histogram<f64>,
    /// Search backend time for one response, by `endpoint` (aggregate,
    /// hits) and `outcome` (ok, too_broad, timeout, error).
    backend: Histogram<f64>,
    /// Response cache lookups by `layer` (memory, blob) and `result` (hit, miss).
    cache: Counter<u64>,
    /// Requests refused as the client's fault, by `reason`.
    rejected: Counter<u64>,
    /// Reference-data reloads by `outcome` (published, failed).
    reloads: Counter<u64>,
    index_version: OnceLock<ObservableGauge<u64>>,
}

impl Metrics {
    pub fn new(meter: &Meter) -> Self {
        Self {
            requests: meter
                .u64_counter("api.requests")
                .with_description("Responses by route and status class")
                .build(),
            duration: meter
                .f64_histogram("api.request_duration_seconds")
                .with_unit("s")
                .with_description("Time to the response head, by route")
                .build(),
            backend: meter
                .f64_histogram("api.backend_duration_seconds")
                .with_unit("s")
                .with_description("Search backend time for one response, by endpoint and outcome")
                .build(),
            cache: meter
                .u64_counter("api.cache_lookups")
                .with_description("Response cache lookups by layer (memory, blob) and result")
                .build(),
            rejected: meter
                .u64_counter("api.rejected_queries")
                .with_description("Requests refused as the client's fault, by reason")
                .build(),
            reloads: meter
                .u64_counter("api.reference_reloads")
                .with_description("Reference-data reloads by outcome (published, failed)")
                .build(),
            index_version: OnceLock::new(),
        }
    }

    /// From the global meter provider (`usnm_telemetry::init` installs it).
    pub fn global() -> Self {
        Self::new(&opentelemetry::global::meter(SERVICE.name))
    }

    fn request(&self, route: &str, status: StatusCode, elapsed: Duration) {
        let route = KeyValue::new("route", route.to_owned());
        let class = KeyValue::new("status_class", format!("{}xx", status.as_u16() / 100));
        self.requests.add(1, &[route.clone(), class]);
        self.duration.record(elapsed.as_secs_f64(), &[route]);
    }

    pub(crate) fn backend<T>(
        &self,
        endpoint: &'static str,
        elapsed: Duration,
        r: &Result<T, ApiError>,
    ) {
        let outcome = match r {
            Ok(_) => "ok",
            Err(ApiError::TooBroad(_)) => "too_broad",
            Err(ApiError::Timeout) => "timeout",
            Err(_) => "error",
        };
        self.backend.record(
            elapsed.as_secs_f64(),
            &[
                KeyValue::new("endpoint", endpoint),
                KeyValue::new("outcome", outcome),
            ],
        );
    }

    pub(crate) fn cache(&self, layer: &'static str, hit: bool) {
        self.cache.add(
            1,
            &[
                KeyValue::new("layer", layer),
                KeyValue::new("result", if hit { "hit" } else { "miss" }),
            ],
        );
    }

    pub(crate) fn reload(&self, outcome: &'static str) {
        self.reloads.add(1, &[KeyValue::new("outcome", outcome)]);
    }
}

/// Report the serving index version as the gauge `api.index_version`: value
/// 1, with the version in the `index_version` attribute. Observed at each
/// export, so it always names the version being served. Once per state.
pub fn observe_index_version(state: &Arc<AppState>, meter: &Meter) {
    let weak: Weak<AppState> = Arc::downgrade(state);
    state.metrics.index_version.get_or_init(|| {
        meter
            .u64_observable_gauge("api.index_version")
            .with_description("1, with the index version being served as an attribute")
            .with_callback(move |observer| {
                if let Some(state) = weak.upgrade() {
                    let version = state.snapshot.load().refdata.version().to_owned();
                    observer.observe(1, &[KeyValue::new("index_version", version)]);
                }
            })
            .build()
    });
}

/// Why a request was refused as the client's fault, for `api.rejected_queries`.
/// [`ApiError`] attaches it to its response.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Rejection(pub &'static str);

/// The index version a response was computed from, attached by the search
/// endpoints' response cache for the request span.
#[derive(Debug, Clone)]
pub(crate) struct ServedVersion(pub String);

/// The route a request matched, written by [`record_route`] for [`track`].
#[derive(Clone, Default)]
struct RouteSlot(Arc<OnceLock<String>>);

/// Outermost middleware: the request span, the log line and the request
/// metrics.
pub(crate) async fn track(
    State(state): State<Arc<AppState>>,
    mut req: Request,
    next: Next,
) -> Response {
    let started = Instant::now();
    let path = req.uri().path();
    if let Some(probe) = PROBES.iter().find(|p| **p == path) {
        let resp = next.run(req).await;
        state
            .metrics
            .request(probe, resp.status(), started.elapsed());
        return resp;
    }
    let api = is_api_path(path);
    let method = req.method().clone();
    let slot = RouteSlot::default();
    req.extensions_mut().insert(slot.clone());
    // Fields in OpenTelemetry's HTTP conventions, which the exporter maps to
    // the request's name, result code and success. No url.* fields: the raw
    // path and query stay out.
    let span = tracing::info_span!(
        "request",
        otel.name = %method,
        otel.kind = "server",
        otel.status_code = Empty,
        http.request.method = %method,
        http.route = Empty,
        http.response.status_code = Empty,
        usnm.index_version = Empty,
    );
    let resp = next.run(req).instrument(span.clone()).await;
    let elapsed = started.elapsed();
    let status = resp.status();
    let route = match slot.0.get() {
        Some(r) => r.as_str(),
        None => {
            let r = if api { NO_ROUTE } else { SITE_ROUTE };
            span.record("http.route", r);
            span.record("otel.name", format!("{method} {r}"));
            r
        }
    };
    // The version the response came from; the one serving now for responses
    // that don't say (a reload may have swapped it in since).
    match resp.extensions().get::<ServedVersion>() {
        Some(ServedVersion(v)) => span.record("usnm.index_version", v.as_str()),
        None => span.record(
            "usnm.index_version",
            state.snapshot.load().refdata.version(),
        ),
    };
    span.record("http.response.status_code", status.as_u16());
    // Client errors (4xx) are the client's outcome, not a failure (09 §9.1).
    span.record(
        "otel.status_code",
        if status.as_u16() >= 500 {
            "ERROR"
        } else {
            "OK"
        },
    );
    drop(span);
    tracing::info!(
        parent: None,
        method = %method,
        route,
        status = status.as_u16(),
        ms = elapsed.as_millis() as u64,
        "request"
    );
    state.metrics.request(route, status, elapsed);
    if let Some(Rejection(reason)) = resp.extensions().get::<Rejection>() {
        state
            .metrics
            .rejected
            .add(1, &[KeyValue::new("reason", *reason)]);
    }
    resp
}

/// Route middleware: tells [`track`] and the request span which route
/// template matched.
pub(crate) async fn record_route(req: Request, next: Next) -> Response {
    if let Some(matched) = req.extensions().get::<MatchedPath>() {
        let route = matched.as_str();
        if let Some(slot) = req.extensions().get::<RouteSlot>() {
            let _ = slot.0.set(route.to_owned());
        }
        let span = tracing::Span::current();
        span.record("http.route", route);
        span.record("otel.name", format!("{} {route}", req.method()));
    }
    next.run(req).await
}

fn is_api_path(path: &str) -> bool {
    ["/v1", "/api/v1"].iter().any(|p| {
        path.strip_prefix(p)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn api_paths() {
        assert!(is_api_path("/v1"));
        assert!(is_api_path("/v1/nope"));
        assert!(is_api_path("/api/v1/aggregate"));
        assert!(!is_api_path("/v10"));
        assert!(!is_api_path("/"));
        assert!(!is_api_path("/assets/app.js"));
    }
}
