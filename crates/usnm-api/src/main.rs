use std::sync::Arc;

use tracing_subscriber::EnvFilter;
use usnm_api::config::{BackendKind, Config};
use usnm_api::{app, spawn_background, AppState, Engine, Loader};
use usnm_search::quickwit::QuickwitBackend;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    // Each published version gets a fresh snapshot. The memory backend reloads
    // exactly the indexes the new version names; Quickwit is shared because
    // the index set travels with the reference data.
    let engine = match &config.backend {
        BackendKind::Memory => Engine::Memory {
            indexes_dir: config.data_dir.join("indexes"),
        },
        BackendKind::Quickwit(url) => {
            Engine::Shared(Arc::new(QuickwitBackend::new(url, config.search_timeout)?))
        }
    };
    let loader = Loader {
        reference: usnm_store::open(&config.reference_url)?,
        engine,
    };
    let snapshot = loader.snapshot().await?;
    tracing::info!(
        version = snapshot.refdata.version(),
        synthetic = snapshot.refdata.current.synthetic,
        backend = ?config.backend,
        reference = ?loader.reference,
        "starting usnm-api"
    );
    let responses = config
        .response_cache_url
        .as_deref()
        .map(usnm_store::open)
        .transpose()?;
    let bind = config.bind.clone();
    let mut state = AppState::with_loader(config, snapshot, Some(loader));
    if let Some(store) = responses {
        tracing::info!(store = ?store, "persistent response cache enabled");
        state = state.with_response_store(store);
    }
    let state = Arc::new(state);
    spawn_background(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    // Peer addresses feed the rate limiter when there is no trusted proxy.
    axum::serve(
        listener,
        app(state).into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .with_graceful_shutdown(shutdown())
    .await?;
    Ok(())
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
