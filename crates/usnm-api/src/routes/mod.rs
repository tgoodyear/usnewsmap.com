mod aggregate;
mod coverage;
mod hits;
mod meta;

pub use aggregate::aggregate;
pub use coverage::coverage;
pub use hits::hits;
pub use meta::{meta, places, readyz};

use std::future::Future;
use std::sync::Arc;

use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::error::ApiError;
use crate::version::{self, Pinning};
use crate::AppState;

/// Serve `compute()` through the response cache, with version-aware headers.
pub(crate) async fn cached<F, T>(
    state: &AppState,
    key: String,
    pinning: &Pinning,
    serving: &str,
    path: &str,
    canonical: &str,
    compute: F,
) -> Result<Response, ApiError>
where
    F: Future<Output = Result<T, ApiError>>,
    T: Serialize,
{
    let body = state
        .cache
        .try_get_with(key, async {
            let value = compute.await?;
            serde_json::to_vec(&value)
                .map(Arc::new)
                .map_err(|e| ApiError::Backend(e.to_string()))
        })
        .await
        .map_err(|e: Arc<ApiError>| Arc::try_unwrap(e).unwrap_or_else(|e| e.as_ref().clone()))?;
    let mut resp = body.as_ref().clone().into_response();
    let headers = resp.headers_mut();
    headers.extend(version::cache_headers(
        pinning, serving, path, canonical, &body,
    ));
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Ok(resp)
}

pub(crate) async fn with_timeout<T>(
    state: &AppState,
    fut: impl Future<Output = Result<T, usnm_search::SearchError>>,
) -> Result<T, ApiError> {
    match tokio::time::timeout(state.config.search_timeout, fut).await {
        Ok(r) => r.map_err(ApiError::from),
        Err(_) => Err(ApiError::Timeout),
    }
}

/// The mount prefix of a request path, e.g. `/api/v1` for `/api/v1/aggregate`.
pub(crate) fn mount_prefix<'a>(path: &'a str, endpoint: &str) -> &'a str {
    path.strip_suffix(endpoint).unwrap_or("/v1")
}

pub(crate) fn uses_fuzzy(node: &usnm_core::query::Node) -> bool {
    use usnm_core::query::Node;
    match node {
        Node::Term(t) => t.fuzzy > 0,
        Node::Phrase { .. } => false,
        Node::And(c) | Node::Or(c) => c.iter().any(uses_fuzzy),
        Node::Not(n) => uses_fuzzy(n),
    }
}
