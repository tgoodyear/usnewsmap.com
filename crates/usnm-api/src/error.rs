//! RFC 9457 problem details (06 §6.3.5).

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use usnm_core::params::ParamError;
use usnm_search::SearchError;

use crate::telemetry::{ProblemType, Rejection};

/// Seconds a client is asked to wait after [`ApiError::Busy`].
pub const BUSY_RETRY_SECS: u64 = 5;

#[derive(Debug, Clone)]
pub enum ApiError {
    Params(ParamError),
    Unsupported(String),
    BadRequest(String),
    NotFound(String),
    TooBroad(String),
    /// A search ran past its computation limit (`USNM_COMPUTE_CAP_SECS`),
    /// or the backend gave up on it.
    Timeout,
    /// Every slot for search computations is taken and none freed up while
    /// the visitor waited (`USNM_COMPUTE_CONCURRENCY`).
    Busy,
    Backend(String),
    /// The search backend refused the request (a bug on our side, not an
    /// outage). Visitors see the same response as `Backend`; the warm-up
    /// doesn't retry it.
    BackendRejected(String),
    /// Too many requests from this client; retry after the given wait.
    RateLimited(std::time::Duration),
    /// A page view the web app sent (`/v1/beacon`) is malformed.
    BadBeacon(String),
    /// The request body is larger than the endpoint accepts.
    TooLarge(String),
    /// The request body's media type isn't one the endpoint accepts.
    MediaType(String),
}

#[derive(Serialize)]
struct Problem<'a> {
    #[serde(rename = "type")]
    kind: &'a str,
    title: &'a str,
    status: u16,
    detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    position: Option<usize>,
}

impl From<ParamError> for ApiError {
    fn from(e: ParamError) -> Self {
        Self::Params(e)
    }
}

impl From<SearchError> for ApiError {
    fn from(e: SearchError) -> Self {
        match e {
            SearchError::Unsupported(what) => Self::Unsupported(what),
            SearchError::Timeout => Self::Timeout,
            SearchError::Backend(msg) => Self::Backend(msg),
            SearchError::Rejected(msg) => Self::BackendRejected(msg),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let retry_after = match &self {
            ApiError::RateLimited(wait) => Some(wait.as_secs_f64().ceil().max(1.0) as u64),
            ApiError::Timeout | ApiError::Backend(_) | ApiError::BackendRejected(_) => Some(30),
            ApiError::Busy => Some(BUSY_RETRY_SECS),
            _ => None,
        };
        let rejection = match &self {
            ApiError::Params(ParamError::Query(_)) => Some("syntax"),
            ApiError::Params(_) | ApiError::BadRequest(_) => Some("bad_parameter"),
            ApiError::Unsupported(_) => Some("unsupported"),
            ApiError::TooBroad(_) => Some("too_broad"),
            ApiError::RateLimited(_) => Some("rate_limited"),
            ApiError::BadBeacon(_) | ApiError::TooLarge(_) | ApiError::MediaType(_) => {
                Some("bad_beacon")
            }
            ApiError::NotFound(_)
            | ApiError::Timeout
            | ApiError::Busy
            | ApiError::Backend(_)
            | ApiError::BackendRejected(_) => None,
        };
        let (status, kind, title, detail, hint, position) = match self {
            ApiError::Params(ParamError::Query(q)) => (
                StatusCode::BAD_REQUEST,
                "/errors/query-syntax",
                "Query syntax error",
                q.message,
                Some("Use quotes for phrases, OR between alternatives and - to exclude a word."),
                q.position,
            ),
            ApiError::Params(e) => (
                StatusCode::BAD_REQUEST,
                "/errors/bad-parameter",
                "Invalid parameter",
                e.to_string(),
                None,
                None,
            ),
            ApiError::BadRequest(msg) => (
                StatusCode::BAD_REQUEST,
                "/errors/bad-parameter",
                "Invalid parameter",
                msg,
                None,
                None,
            ),
            ApiError::Unsupported(what) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/errors/unsupported",
                "Not supported",
                format!("{what} is not available yet"),
                Some("Try the search without that option."),
                None,
            ),
            ApiError::TooBroad(msg) => (
                StatusCode::UNPROCESSABLE_ENTITY,
                "/errors/query-too-broad",
                "Query too broad",
                msg,
                Some("Narrow the date range, add filters, or choose a coarser time bucket."),
                None,
            ),
            ApiError::NotFound(msg) => (
                StatusCode::NOT_FOUND,
                "/errors/not-found",
                "Not found",
                msg,
                None,
                None,
            ),
            ApiError::Timeout => (
                StatusCode::SERVICE_UNAVAILABLE,
                "/errors/backend-timeout",
                "Search took too long",
                "The search did not finish in time.".to_owned(),
                Some("Narrow the date range or add filters, then try again."),
                None,
            ),
            ApiError::Busy => (
                StatusCode::SERVICE_UNAVAILABLE,
                "/errors/busy",
                "Too many large searches",
                "Too many large searches are running at the moment.".to_owned(),
                Some("Wait for the time in Retry-After, then try again."),
                None,
            ),
            ApiError::RateLimited(_) => (
                StatusCode::TOO_MANY_REQUESTS,
                "/errors/rate-limited",
                "Too many requests",
                "This client has sent too many requests.".to_owned(),
                Some("Wait for the time in Retry-After, then try again."),
                None,
            ),
            ApiError::BadBeacon(msg) => (
                StatusCode::BAD_REQUEST,
                "/errors/bad-beacon",
                "Invalid page view",
                msg,
                None,
                None,
            ),
            ApiError::TooLarge(msg) => (
                StatusCode::PAYLOAD_TOO_LARGE,
                "/errors/too-large",
                "Request body too large",
                msg,
                None,
                None,
            ),
            ApiError::MediaType(msg) => (
                StatusCode::UNSUPPORTED_MEDIA_TYPE,
                "/errors/unsupported-media-type",
                "Unsupported media type",
                msg,
                None,
                None,
            ),
            ApiError::Backend(msg) | ApiError::BackendRejected(msg) => {
                tracing::error!(error = %msg, "search backend error");
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    "/errors/backend",
                    "Search is unavailable",
                    "The search service is not responding.".to_owned(),
                    None,
                    None,
                )
            }
        };
        let body = Problem {
            kind,
            title,
            status: status.as_u16(),
            detail,
            hint,
            position,
        };
        let mut resp = (status, axum::Json(body)).into_response();
        let headers = resp.headers_mut();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/problem+json"),
        );
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if let Some(secs) = retry_after {
            headers.insert(header::RETRY_AFTER, HeaderValue::from(secs));
        }
        if let Some(reason) = rejection {
            resp.extensions_mut().insert(Rejection(reason));
        }
        resp.extensions_mut().insert(ProblemType(kind));
        resp
    }
}
