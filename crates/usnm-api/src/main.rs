use std::io::BufRead;
use std::sync::Arc;

use tracing_subscriber::EnvFilter;
use usnm_api::config::{BackendKind, Config};
use usnm_api::refdata::RefData;
use usnm_api::{app, spawn_refresher, AppState};
use usnm_search::memory::MemoryBackend;
use usnm_search::quickwit::QuickwitBackend;
use usnm_search::{PageDoc, SearchBackend};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let refdata = RefData::load(&config.data_dir)?;
    let backend: Arc<dyn SearchBackend> = match &config.backend {
        BackendKind::Memory => Arc::new(load_memory_backend(&config)?),
        BackendKind::Quickwit(url) => Arc::new(QuickwitBackend::new(url, config.search_timeout)?),
    };
    tracing::info!(
        version = refdata.version(),
        synthetic = refdata.current.synthetic,
        backend = ?config.backend,
        "starting usnm-api"
    );
    let bind = config.bind.clone();
    let state = Arc::new(AppState::new(config, backend, refdata));
    spawn_refresher(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app(state))
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Load every `{data_dir}/indexes/{index_id}.jsonl` into the in-memory engine.
fn load_memory_backend(config: &Config) -> Result<MemoryBackend, Box<dyn std::error::Error>> {
    let mut backend = MemoryBackend::new();
    let dir = config.data_dir.join("indexes");
    for entry in std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))? {
        let path = entry?.path();
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or_default()
            .to_owned();
        let docs = std::io::BufReader::new(std::fs::File::open(&path)?)
            .lines()
            .map(|l| Ok(serde_json::from_str::<PageDoc>(&l?)?))
            .collect::<Result<Vec<_>, Box<dyn std::error::Error>>>()?;
        tracing::info!(index = %id, docs = docs.len(), "loaded memory index");
        backend.add_index(&id, docs);
    }
    Ok(backend)
}

async fn shutdown() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let term = async {
        if let Ok(mut s) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            s.recv().await;
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! { () = ctrl_c => {}, () = term => {} }
    tracing::info!("shutting down");
}
