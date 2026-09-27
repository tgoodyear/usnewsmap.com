use std::sync::Arc;

use tracing_subscriber::EnvFilter;
use usnm_api::config::{BackendKind, Config};
use usnm_api::refdata::RefData;
use usnm_api::{app, memory_snapshot, spawn_refresher, AppState, Reloader, Snapshot};
use usnm_search::quickwit::QuickwitBackend;
use usnm_search::SearchBackend;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();

    let config = Config::from_env()?;
    let dir = config.data_dir.clone();
    // Each published version gets a fresh snapshot. The memory backend reloads
    // exactly the indexes the new version names; Quickwit is shared because
    // the index set travels with the reference data.
    let reloader: Reloader = match &config.backend {
        BackendKind::Memory => Arc::new(move || memory_snapshot(&dir)),
        BackendKind::Quickwit(url) => {
            let backend: Arc<dyn SearchBackend> =
                Arc::new(QuickwitBackend::new(url, config.search_timeout)?);
            Arc::new(move || {
                Ok(Snapshot {
                    refdata: RefData::load(&dir)?,
                    backend: backend.clone(),
                })
            })
        }
    };
    let snapshot = reloader()?;
    tracing::info!(
        version = snapshot.refdata.version(),
        synthetic = snapshot.refdata.current.synthetic,
        backend = ?config.backend,
        "starting usnm-api"
    );
    let bind = config.bind.clone();
    let state = Arc::new(AppState::with_reloader(config, snapshot, Some(reloader)));
    spawn_refresher(state.clone());
    let listener = tokio::net::TcpListener::bind(&bind).await?;
    tracing::info!(%bind, "listening");
    axum::serve(listener, app(state))
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
