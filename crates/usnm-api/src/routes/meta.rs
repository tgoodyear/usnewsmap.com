use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use usnm_core::query;

use crate::AppState;

pub async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    match tokio::time::timeout(Duration::from_secs(2), state.backend.health()).await {
        Ok(Ok(())) => (StatusCode::OK, "ready").into_response(),
        Ok(Err(e)) => (StatusCode::SERVICE_UNAVAILABLE, e.to_string()).into_response(),
        Err(_) => (
            StatusCode::SERVICE_UNAVAILABLE,
            "search backend health check timed out",
        )
            .into_response(),
    }
}

pub async fn meta(State(state): State<Arc<AppState>>) -> Response {
    let rd = state.refdata.load();
    let c = &rd.current;
    let body = json!({
        "index_version": c.index_version,
        "indexes": c.indexes,
        "bounds": c.bounds,
        "published_at": c.published_at,
        "synthetic": c.synthetic,
        "places": rd.places.len(),
        "titles": rd.titles.len(),
        "capabilities": state.backend.capabilities(),
        "limits": {
            "max_query_chars": query::MAX_QUERY_CHARS,
            "max_terms": query::MAX_TERMS,
            "max_or_branches": query::MAX_OR_BRANCHES,
            "max_slop": query::MAX_SLOP,
            "max_fuzzy": query::MAX_FUZZY,
            "min_prefix_chars": query::MIN_PREFIX_CHARS
        }
    });
    let mut resp = Json(body).into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

/// All places as GeoJSON (06 §6.3.2), for the SPA to join by id.
pub async fn places(State(state): State<Arc<AppState>>) -> Response {
    let rd = state.refdata.load();
    let mut title_counts = std::collections::HashMap::<&str, usize>::new();
    for t in rd.titles.values() {
        *title_counts.entry(t.place_id.as_str()).or_default() += 1;
    }
    let features: Vec<_> = rd
        .places
        .iter()
        .map(|p| {
            json!({
                "type": "Feature",
                "id": p.id,
                "geometry": { "type": "Point", "coordinates": [p.lon, p.lat] },
                "properties": {
                    "name": p.name,
                    "state": p.state,
                    "precision": p.precision,
                    "titles": title_counts.get(p.id.as_str()).copied().unwrap_or(0)
                }
            })
        })
        .collect();
    let body =
        json!({ "type": "FeatureCollection", "index_version": rd.version(), "features": features });
    let mut resp = Json(body).into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=3600"),
    );
    resp
}
