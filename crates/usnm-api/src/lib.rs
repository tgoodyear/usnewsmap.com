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

/// One published version: its reference data and the backend that serves its
/// index set. Swapped atomically so a request never mixes versions.
pub struct Snapshot {
    pub refdata: RefData,
    pub backend: Arc<dyn SearchBackend>,
}

/// Builds the snapshot for whatever `current.json` names now.
pub type Reloader = Arc<dyn Fn() -> Result<Snapshot, String> + Send + Sync>;

pub struct AppState {
    pub config: Config,
    pub snapshot: ArcSwap<Snapshot>,
    /// `None` disables hot reload.
    pub reloader: Option<Reloader>,
    /// Serialized responses keyed by `{index_version}|{endpoint}|{canonical}`.
    pub cache: Cache<String, Arc<Vec<u8>>>,
}

impl AppState {
    pub fn new(config: Config, backend: Arc<dyn SearchBackend>, refdata: RefData) -> Self {
        Self::with_reloader(config, Snapshot { refdata, backend }, None)
    }

    pub fn with_reloader(config: Config, snapshot: Snapshot, reloader: Option<Reloader>) -> Self {
        let cache = Cache::builder()
            .max_capacity(config.cache_bytes)
            .weigher(|k: &String, v: &Arc<Vec<u8>>| {
                u32::try_from(k.len() + v.len()).unwrap_or(u32::MAX)
            })
            .time_to_live(Duration::from_secs(24 * 3600))
            .build();
        Self {
            config,
            snapshot: ArcSwap::from_pointee(snapshot),
            reloader,
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

/// Build a snapshot for the in-memory backend: the reference data plus exactly
/// the JSONL indexes that `current.json` names (`{data_dir}/indexes/{id}.jsonl`).
pub fn memory_snapshot(data_dir: &std::path::Path) -> Result<Snapshot, String> {
    use std::io::BufRead;
    let refdata = RefData::load(data_dir)?;
    let mut backend = usnm_search::memory::MemoryBackend::new();
    for id in &refdata.current.indexes {
        let path = data_dir.join("indexes").join(format!("{id}.jsonl"));
        let file = std::fs::File::open(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut docs = Vec::new();
        for line in std::io::BufReader::new(file).lines() {
            let line = line.map_err(|e| format!("{}: {e}", path.display()))?;
            docs.push(
                serde_json::from_str::<usnm_search::PageDoc>(&line)
                    .map_err(|e| format!("{}: {e}", path.display()))?,
            );
        }
        tracing::info!(index = %id, docs = docs.len(), "loaded memory index");
        backend.add_index(id, docs);
    }
    Ok(Snapshot {
        refdata,
        backend: Arc::new(backend),
    })
}

/// If `current.json` names a different version than the one serving, build
/// the new snapshot and swap it in atomically. Returns whether it swapped.
pub async fn reload_if_changed(state: &AppState) -> Result<bool, String> {
    let Some(reloader) = state.reloader.clone() else {
        return Ok(false);
    };
    let serving = state.snapshot.load().refdata.version().to_owned();
    let dir = state.config.data_dir.clone();
    let next = tokio::task::spawn_blocking(move || refdata::read_current(&dir))
        .await
        .map_err(|e| e.to_string())??;
    if next.index_version == serving {
        return Ok(false);
    }
    let snapshot = tokio::task::spawn_blocking(move || reloader())
        .await
        .map_err(|e| e.to_string())??;
    tracing::info!(
        version = snapshot.refdata.version(),
        "publishing new index version"
    );
    state.snapshot.store(Arc::new(snapshot));
    Ok(true)
}

/// Poll `current.json` every `refresh_interval` (no-op without a reloader).
pub fn spawn_refresher(state: Arc<AppState>) {
    if state.reloader.is_none() {
        return;
    }
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(state.config.refresh_interval);
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Err(e) = reload_if_changed(&state).await {
                tracing::warn!(error = %e, "reload failed; keeping current version");
            }
        }
    });
}
