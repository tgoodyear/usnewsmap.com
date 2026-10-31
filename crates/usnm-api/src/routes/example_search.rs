//! `POST /v1/example-search`: an example search Cloudflare answered (06
//! §6.5), which never reached [`super::aggregate`] to be counted in the search
//! log (06 §6.8). The body holds only an example's id, so it can't add text
//! to the log: the record is built from the API's own copy of the example,
//! through the handler a search takes, and the same admission applies.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use serde::Deserialize;

use super::beacon::read_body;
use super::{aggregate_in, Ctx};
use crate::error::ApiError;
use crate::prewarm::{examples, Example};
use crate::searchlog::{self, Admission};
use crate::AppState;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Report {
    example: String,
}

pub async fn example_search(State(state): State<Arc<AppState>>, req: Request) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = match read_body(&parts.headers, body, "the example").await {
        Ok(bytes) => bytes,
        Err(e) => return e.into_response(),
    };
    let report = serde_json::from_slice::<Report>(&bytes).ok();
    let Some(example) = report.and_then(|r| examples().iter().find(|e| e.id == r.example)) else {
        return ApiError::BadBeacon("the body must be a JSON object naming an example".to_owned())
            .into_response();
    };
    record(&state, &parts.headers, example).await;
    let mut resp = StatusCode::NO_CONTENT.into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    resp
}

async fn record(state: &Arc<AppState>, headers: &HeaderMap, example: &Example) {
    let Some(log) = state.search_log.as_deref() else {
        return;
    };
    let admission = searchlog::admit(headers, &state.config.site_host);
    if let Admission::Excluded(reason) = admission {
        log.excluded(reason);
        return;
    }
    let Ok(uri) = format!("/v1/aggregate?{}", example.aggregate).parse::<Uri>() else {
        return;
    };
    let ctx = Ctx::serving(state);
    if let Err(e) = aggregate_in(state, ctx, &uri, Some((log, admission))).await {
        tracing::debug!(error = %e, "an example reported from the edge wasn't counted");
    }
}
