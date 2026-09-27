//! US News Map public search API (06).

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use arc_swap::ArcSwap;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderValue, Method};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use moka::future::Cache;
use tokio::sync::Semaphore;
use tower_http::compression::CompressionLayer;
use tower_http::cors::{AllowOrigin, CorsLayer};
use tower_http::trace::TraceLayer;
use usnm_search::SearchBackend;
use usnm_store::ObjectStore;

pub mod config;
pub mod error;
pub mod ratelimit;
pub mod refdata;
mod routes;
pub mod version;

use config::Config;
use error::ApiError;
use ratelimit::Limiter;
use refdata::{Current, RefData};

/// One published version: its reference data and the backend that serves its
/// index set. Swapped atomically so a request never mixes versions.
pub struct Snapshot {
    pub refdata: RefData,
    pub backend: Arc<dyn SearchBackend>,
}

/// How a snapshot's search backend is obtained.
pub enum Engine {
    /// Load exactly the JSONL indexes the version names from `{dir}/{id}.jsonl`.
    Memory { indexes_dir: PathBuf },
    /// A shared engine (Quickwit): the index set travels with the reference data.
    Shared(Arc<dyn SearchBackend>),
}

/// Builds snapshots from the reference store (Blob or a local directory).
pub struct Loader {
    pub reference: Arc<dyn ObjectStore>,
    pub engine: Engine,
}

impl Loader {
    pub async fn current(&self) -> Result<Current, String> {
        refdata::read_current(self.reference.as_ref()).await
    }

    /// The snapshot for whatever `current.json` names now.
    pub async fn snapshot(&self) -> Result<Snapshot, String> {
        let current = self.current().await?;
        self.load(current).await
    }

    pub async fn load(&self, current: Current) -> Result<Snapshot, String> {
        let refdata = RefData::load_for(self.reference.as_ref(), current).await?;
        let backend: Arc<dyn SearchBackend> = match &self.engine {
            Engine::Shared(b) => b.clone(),
            Engine::Memory { indexes_dir } => {
                let dir = indexes_dir.clone();
                let ids = refdata.current.indexes.clone();
                tokio::task::spawn_blocking(move || memory_backend(&dir, &ids))
                    .await
                    .map_err(|e| e.to_string())??
            }
        };
        Ok(Snapshot { refdata, backend })
    }
}

pub struct AppState {
    pub config: Config,
    pub snapshot: ArcSwap<Snapshot>,
    /// `None` disables hot reload.
    pub loader: Option<Loader>,
    /// Serialized responses keyed by `{index_version}|{endpoint}|{canonical}`.
    pub cache: Cache<String, Arc<Vec<u8>>>,
    /// Persistent response cache (06 §6.5), read on in-process misses.
    pub responses: Option<Arc<dyn ObjectStore>>,
    pub limiter: Option<Limiter>,
    /// Caps concurrent backend queries across all requests.
    pub permits: Semaphore,
}

impl AppState {
    pub fn new(config: Config, backend: Arc<dyn SearchBackend>, refdata: RefData) -> Self {
        Self::with_loader(config, Snapshot { refdata, backend }, None)
    }

    pub fn with_loader(config: Config, snapshot: Snapshot, loader: Option<Loader>) -> Self {
        let cache = Cache::builder()
            .max_capacity(config.cache_bytes)
            .weigher(|k: &String, v: &Arc<Vec<u8>>| {
                u32::try_from(k.len() + v.len()).unwrap_or(u32::MAX)
            })
            .time_to_live(Duration::from_secs(24 * 3600))
            .build();
        Self {
            limiter: config
                .rate_limit
                .map(|l| Limiter::new(l, config.trusted_proxy_hops)),
            permits: Semaphore::new(config.backend_concurrency),
            config,
            snapshot: ArcSwap::from_pointee(snapshot),
            loader,
            cache,
            responses: None,
        }
    }

    pub fn with_response_store(mut self, store: Arc<dyn ObjectStore>) -> Self {
        self.responses = Some(store);
        self
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
        .route("/coverage", get(routes::coverage))
        .route_layer(middleware::from_fn_with_state(state.clone(), rate_limit));

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

async fn rate_limit(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    if let Some(limiter) = &state.limiter {
        let peer = req
            .extensions()
            .get::<ConnectInfo<SocketAddr>>()
            .map(|c| c.0.ip());
        if let Err(wait) = limiter.check(req.headers(), peer) {
            return ApiError::RateLimited(wait).into_response();
        }
    }
    next.run(req).await
}

/// An in-memory backend holding exactly the named JSONL indexes.
fn memory_backend(dir: &std::path::Path, ids: &[String]) -> Result<Arc<dyn SearchBackend>, String> {
    use std::io::BufRead;
    let mut backend = usnm_search::memory::MemoryBackend::new();
    for id in ids {
        let path = dir.join(format!("{id}.jsonl"));
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
    Ok(Arc::new(backend))
}

/// If `current.json` names a different version than the one serving, build
/// the new snapshot and swap it in atomically. Returns whether it swapped.
pub async fn reload_if_changed(state: &AppState) -> Result<bool, String> {
    let Some(loader) = &state.loader else {
        return Ok(false);
    };
    let next = loader.current().await?;
    if next.index_version == state.snapshot.load().refdata.version() {
        return Ok(false);
    }
    let snapshot = loader.load(next).await?;
    tracing::info!(
        version = snapshot.refdata.version(),
        "publishing new index version"
    );
    state.snapshot.store(Arc::new(snapshot));
    Ok(true)
}

/// Poll `current.json` every `refresh_interval` (no-op without a loader), and
/// prune idle rate-limit buckets.
pub fn spawn_background(state: Arc<AppState>) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(state.config.refresh_interval);
        tick.tick().await;
        loop {
            tick.tick().await;
            if let Some(limiter) = &state.limiter {
                limiter.housekeeping();
            }
            if let Err(e) = reload_if_changed(&state).await {
                tracing::warn!(error = %e, "reload failed; keeping current version");
            }
        }
    });
}
