//! `POST /v1/search-report`: a search Cloudflare answered (06 §6.5), which
//! never reached [`super::aggregate`] to be counted in the search log (06
//! §6.8). The body holds the request's query string, and the search goes
//! through the handler a search takes, with the same admission checks. So it
//! adds nothing a `GET /v1/aggregate` couldn't, and only a search that gets
//! its results counts. The text is never logged.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::beacon::read_body;
use super::{aggregate_in, Ctx};
use crate::error::ApiError;
use crate::searchlog::{self, Admission};
use crate::AppState;

/// Largest body accepted, in bytes: a query string as long as URLs get.
const MAX_BODY: usize = 8192;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    query: String,
}

pub async fn search_report(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match read_body(&parts.headers, body, "the search", MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(e) => return e.into_response(),
    };
    let uri = serde_json::from_slice::<Report>(&bytes)
        .ok()
        .and_then(|r| format!("/v1/aggregate?{}", r.query).parse::<Uri>().ok());
    let Some(uri) = uri else {
        return ApiError::BadBeacon("the body must be a JSON object with query".to_owned())
            .into_response();
    };
    record(&state, &parts.headers, &uri).await;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn record(state: &Arc<AppState>, headers: &HeaderMap, uri: &Uri) {
    let Some(log) = state.search_log.as_deref() else {
        return;
    };
    let admission = searchlog::admit(headers, &state.config.site_host);
    if let Admission::Excluded(reason) = admission {
        log.excluded(reason);
        return;
    }
    // A search that fails isn't counted, as for a visitor's own request.
    let ctx = Ctx::serving(state);
    let _ = aggregate_in(state, ctx, uri, Some((log, admission))).await;
}
