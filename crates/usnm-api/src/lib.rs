//! US News Map public search API (06).

use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::Request;
use axum::http::{HeaderValue, Method};
use axum::routing::get;
use axum::Router;
use moka::future::Cache;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use usnm_search::SearchBackend;

pub mod config;
pub mod error;
pub mod refdata;
mod routes;
pub mod version;

use config::Config;
use refdata::RefData;

pub struct AppState {
    pub config: Config,
    pub backend: Arc<dyn SearchBackend>,
    pub refdata: ArcSwap<RefData>,
    /// Serialized responses keyed by `{index_version}|{endpoint}|{canonical}`.
    pub cache: Cache<String, Arc<Vec<u8>>>,
}

impl AppState {
    pub fn new(config: Config, backend: Arc<dyn SearchBackend>, refdata: RefData) -> Self {
        let cache = Cache::builder()
            .max_capacity(config.cache_bytes)
            .weigher(|k: &String, v: &Arc<Vec<u8>>| {
                u32::try_from(k.len() + v.len()).unwrap_or(u32::MAX)
            })
            .time_to_live(Duration::from_secs(24 * 3600))
            .build();
        Self {
            config,
            backend,
            refdata: ArcSwap::from_pointee(refdata),
            cache,
        }
    }
}

pub fn app(state: Arc<AppState>) -> Router {
    let origins: Vec<HeaderValue> = state
        .config
        .allowed_origins
        .iter()
        .filter_map(|o| HeaderValue::from_str(o).ok())
        .collect();
    let cors = CorsLayer::new()
        .allow_origin(AllowOrigin::list(origins))
        .allow_methods([Method::GET]);

    let v1 = Router::new()
        .route("/meta", get(routes::meta))
        .route("/places", get(routes::places))
        .route("/aggregate", get(routes::aggregate))
        .route("/hits", get(routes::hits))
        .route("/coverage", get(routes::coverage));

    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/readyz", get(routes::readyz))
        // `/api/v1` lets SWA Standard link this app same-origin (ADR-0006).
        .nest("/v1", v1.clone())
        .nest("/api/v1", v1)
        .with_state(state)
        .layer(cors)
        .layer(CompressionLayer::new())
        // Log the path only: query strings carry search text (09 §9.4.2).
        .layer(TraceLayer::new_for_http().make_span_with(|req: &Request| {
            tracing::info_span!("request", method = %req.method(), path = %req.uri().path())
        }))
}

/// Reload `current.json` periodically and swap in a newly published version.
pub fn spawn_refresher(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(state.config.refresh_interval);
        tick.tick().await;
        loop {
            tick.tick().await;
            let dir = state.config.data_dir.clone();
            match tokio::task::spawn_blocking(move || RefData::load(&dir)).await {
                Ok(Ok(next)) if next.version() != state.refdata.load().version() => {
                    tracing::info!(version = next.version(), "publishing new index version");
                    state.refdata.store(Arc::new(next));
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "reference data reload failed; keeping current version")
                }
                Err(e) => tracing::warn!(error = %e, "reference data reload task failed"),
            }
        }
    });
}
