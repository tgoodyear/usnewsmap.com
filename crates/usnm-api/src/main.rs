use std::sync::Arc;

use usnm_api::config::{BackendKind, Config};
use usnm_api::status::PipelineSource;
use usnm_api::{app, spawn_background, telemetry, AppState, Engine, Loader};
use usnm_search::quickwit::QuickwitBackend;
use usnm_state::cosmos::CosmosDocs;
use usnm_store::credential;

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let exporters = usnm_telemetry::init(telemetry::SERVICE);
    let result = serve().await;
    // A failure is one JSON line (not also plain text from Rust's handler).
    let code = match &result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(error = %e, "usnm-api failed");
            std::process::ExitCode::FAILURE
        }
    };
    // Flush what's buffered before exit, on every path.
    exporters.shutdown().await;
    code
}

async fn serve() -> Result<(), Box<dyn std::error::Error>> {
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
    let snapshot = initial_snapshot(&loader).await?;
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
    // The pipeline state, read-only, for /v1/status. The status page would
    // rather say "busy" than wait out Cosmos throttling for minutes.
    let pipeline = match (&config.cosmos_endpoint, &config.state_file) {
        (Some(endpoint), _) => PipelineSource::Docs(Arc::new(
            CosmosDocs::new(
                endpoint,
                usnm_state::DATABASE,
                credential::from_env_for(credential::COSMOS_RESOURCE),
            )?
            .with_rate_limit_retry(std::time::Duration::from_secs(10)),
        )),
        (None, Some(path)) => PipelineSource::File(path.clone()),
        (None, None) => PipelineSource::None,
    };
    tracing::info!(
        pipeline_state = match &pipeline {
            PipelineSource::Docs(_) => "cosmos",
            PipelineSource::File(_) => "file",
            PipelineSource::None => "none",
        },
        "status page source"
    );
    let bind = config.bind.clone();
    let mut state = AppState::with_loader(config, snapshot, Some(loader)).with_pipeline(pipeline);
    if let Some(store) = responses {
        tracing::info!(store = ?store, "persistent response cache enabled");
        state = state.with_response_store(store);
    }
    let state = Arc::new(state);
    telemetry::observe_index_version(
        &state,
        &opentelemetry::global::meter(telemetry::SERVICE.name),
    );
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

/// The first snapshot, retried while the search sidecar starts alongside the
/// API (and while a new identity's role assignments propagate).
async fn initial_snapshot(loader: &Loader) -> Result<usnm_api::Snapshot, String> {
    const ATTEMPTS: u32 = 30;
    let mut delay = std::time::Duration::from_secs(1);
    let mut attempt = 1;
    loop {
        match loader.snapshot().await {
            Ok(s) => return Ok(s),
            Err(e) if attempt < ATTEMPTS => {
                tracing::warn!(error = %e, attempt, "initial load failed; retrying");
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(15));
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
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
