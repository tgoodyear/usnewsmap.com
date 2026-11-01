//! `usnm-ocrdash`: the on-prem OCR pipeline's LAN dashboard (12 §12.2.5).

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use axum_server::tls_rustls::RustlsConfig;
use clap::Parser;
use usnm_ocr_dash::fetch::Fetch;
use usnm_ocr_dash::hosts::Hosts;
use usnm_ocr_dash::net::{Allow, Source};
use usnm_ocr_dash::nodes::{check_name, Nodes};
use usnm_ocr_dash::sset::Ssets;
use usnm_ocr_dash::view::Settings;
use usnm_ocr_dash::{public_router, redirect_router, router, App};

static VERSION: LazyLock<String> = LazyLock::new(|| {
    format!(
        "{} ({})",
        env!("CARGO_PKG_VERSION"),
        option_env!("USNM_GIT_SHA")
            .filter(|s| !s.is_empty())
            .map_or_else(|| "unknown commit".to_owned(), |s| format!("commit {s}"))
    )
});

/// A read-only LAN dashboard of the scan fetch, the OCR nodes and the
/// Proxmox hosts. No page text, ever.
#[derive(Parser)]
#[command(version = VERSION.as_str())]
struct Cli {
    /// Where to listen. With `--tls-cert`, plain HTTP here only redirects
    /// to `https://<--https-name>` (and takes the hosts' stats pushes).
    #[arg(long, env = "USNM_OCRDASH_LISTEN", default_value = "0.0.0.0:80")]
    listen: SocketAddr,
    /// The certificate chain (PEM) to serve HTTPS with, on `--tls-listen`.
    /// Read again on SIGHUP (`systemctl reload`), with `--tls-key`.
    #[arg(
        long,
        env = "USNM_OCRDASH_TLS_CERT",
        requires_all = ["tls_key", "https_name"]
    )]
    tls_cert: Option<PathBuf>,
    /// The certificate's private key (PEM).
    #[arg(long, env = "USNM_OCRDASH_TLS_KEY", requires = "tls_cert")]
    tls_key: Option<PathBuf>,
    /// A second, public listener, plain HTTP, for a tunnel (cloudflared) to
    /// put on the internet: only GET and HEAD of the page, `/healthz` and
    /// `/api/{view,pipeline,fetch}.json`, redacted, with no `--allow` check
    /// and no push routes. Off unless set; bind it to loopback or an
    /// address only the tunnel reaches.
    #[arg(long, env = "USNM_OCRDASH_PUBLIC_LISTEN")]
    public_listen: Option<SocketAddr>,
    /// Where to serve HTTPS.
    #[arg(long, env = "USNM_OCRDASH_TLS_LISTEN", default_value = "0.0.0.0:443")]
    tls_listen: SocketAddr,
    /// The name in the certificate, which plain HTTP redirects to (with
    /// `--tls-listen`'s port unless it's 443).
    #[arg(long, env = "USNM_OCRDASH_HTTPS_NAME", requires = "tls_cert")]
    https_name: Option<String>,
    /// Who may ask anything: address ranges, host names (resolved at start
    /// and every five minutes) or `lan` (the subnets this machine is on),
    /// comma-separated or repeated.
    #[arg(
        long,
        env = "USNM_OCRDASH_ALLOW",
        value_delimiter = ',',
        required = true
    )]
    allow: Vec<Source>,
    /// The fetcher's output (`status-full.json`, `index-full.jsonl`,
    /// `failures-full.jsonl`), read-only.
    #[arg(long, env = "USNM_OCRDASH_SCANS", default_value = "/tank/ocr/scans")]
    scans: PathBuf,
    /// The plan to count the keep and buffer sets against, pinned: the
    /// fetcher's status naming another manifest doesn't replace it. Without
    /// it, the status's manifest is used unless it is a queued one.
    #[arg(long, env = "USNM_OCRDASH_MANIFEST")]
    manifest: Option<PathBuf>,
    /// Where the fetch queue's manifests are (`queue-ocr-*/`,
    /// `queue-quality-*/`), read-only. `none` to not read a queue.
    #[arg(
        long,
        env = "USNM_OCRDASH_MANIFESTS",
        default_value = "/tank/ocr/manifests"
    )]
    manifests: PathBuf,
    /// The bulk OCR archive download's `bulk-status.json`, read-only.
    #[arg(long, env = "USNM_OCRDASH_BULK_STATUS")]
    bulk_status: Option<PathBuf>,
    /// The global LoC pacer's `pacer.json`, read-only.
    #[arg(long, env = "USNM_OCRDASH_PACER")]
    pacer: Option<PathBuf>,
    /// Read-only systemd completion metadata for publish-fetched.
    #[arg(long, env = "USNM_OCRDASH_PUBLISH_STATUS")]
    publish_status: Option<PathBuf>,
    /// Read-only PostgreSQL pgmq metrics, with a dash_reader client certificate.
    #[arg(long, env = "USNM_OCRDASH_QUEUE_URL")]
    queue_url: Option<String>,
    /// A host stats file to read (`<host>.json`), comma-separated or
    /// repeated.
    #[arg(long, env = "USNM_OCRDASH_HOST_FILE", value_delimiter = ',')]
    host_file: Vec<PathBuf>,
    /// A host that pushes its stats, as `<name>=<its host name or address>`.
    #[arg(long, env = "USNM_OCRDASH_HOST_PUSH", value_delimiter = ',')]
    host_push: Vec<String>,
    /// A Serial Set fetch that pushes its progress, as `<name>=<the host
    /// name or address of the node running it>`.
    #[arg(long, env = "USNM_OCRDASH_SSET_PUSH", value_delimiter = ',')]
    sset_push: Vec<String>,
    /// A throughput collector that pushes its hourly counts, as `<source>=<the
    /// host name or address of the host running it>`, with source `controller`,
    /// `ship` or `boinc`.
    #[arg(long, env = "USNM_OCRDASH_THROUGHPUT_PUSH", value_delimiter = ',')]
    throughput_push: Vec<String>,
    /// An OCR node's worker status, as `<name>=http://<address>:<port>`.
    #[arg(long, env = "USNM_OCRDASH_WORKER", value_delimiter = ',')]
    worker: Vec<String>,
    /// Where the copies of the workers' journals are kept.
    #[arg(
        long,
        env = "USNM_OCRDASH_STATE_DIR",
        default_value = "/var/lib/usnm-ocrdash"
    )]
    state_dir: PathBuf,
    /// A seed plan to show: a flat JSON object.
    #[arg(long, env = "USNM_OCRDASH_SEED_PLAN")]
    seed_plan: Option<PathBuf>,
    /// Local corpus-catalog JSON snapshot, read-only; no database or cloud access.
    #[arg(long, env = "USNM_OCRDASH_PIPELINE_SNAPSHOT")]
    pipeline_snapshot: Option<PathBuf>,
    /// Maximum age of source evidence in seconds (not the snapshot generation time).
    #[arg(
        long,
        env = "USNM_OCRDASH_PIPELINE_MAX_AGE_SECS",
        default_value_t = 900,
        value_parser = clap::value_parser!(u64).range(1..=604800)
    )]
    pipeline_max_age_secs: u64,
    /// The Azure workbook of the OCR nodes, linked for what only Azure has.
    #[arg(long, env = "USNM_OCRDASH_WORKBOOK_URL")]
    workbook_url: Option<String>,
    /// Guests shown, by name prefix.
    #[arg(long, env = "USNM_OCRDASH_GUEST_PREFIX", default_value = "usnm-")]
    guest_prefix: String,
    /// Rows in each list.
    #[arg(long, env = "USNM_OCRDASH_ROWS", default_value_t = 20)]
    rows: usize,
    /// Seconds between reads of the files and the nodes.
    #[arg(long, env = "USNM_OCRDASH_POLL_SECS", default_value_t = 15)]
    poll_secs: u64,
    /// Seconds between the page's refreshes.
    #[arg(long, env = "USNM_OCRDASH_REFRESH_SECS", default_value_t = 30)]
    refresh_secs: u64,
}

fn pair(s: &str, what: &str) -> anyhow::Result<(String, String)> {
    let (k, v) = s
        .split_once('=')
        .with_context(|| format!("{what}: `<name>=<value>`, not {s:?}"))?;
    check_name(k).with_context(|| format!("{what} {s:?}"))?;
    Ok((k.to_owned(), v.to_owned()))
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .json()
        .init();
    let cli = Cli::parse();
    let mut workers = Vec::new();
    for w in &cli.worker {
        let (name, url) = pair(w, "--worker")?;
        if !url.starts_with("http://") && !url.starts_with("https://") {
            bail!("--worker {w:?}: the URL must be http:// or https://");
        }
        workers.push((name, url));
    }
    let mut pushers = BTreeMap::new();
    for p in &cli.host_push {
        let (name, host) = pair(p, "--host-push")?;
        let source: Source = host
            .parse()
            .map_err(|e| anyhow::anyhow!("--host-push {p:?}: {e}"))?;
        pushers.insert(name, Allow::new(vec![source]));
    }
    let mut sset_pushers = BTreeMap::new();
    for p in &cli.sset_push {
        let (name, host) = pair(p, "--sset-push")?;
        let source: Source = host
            .parse()
            .map_err(|e| anyhow::anyhow!("--sset-push {p:?}: {e}"))?;
        sset_pushers.insert(name, Allow::new(vec![source]));
    }
    let mut throughput_pushers = BTreeMap::new();
    for p in &cli.throughput_push {
        let (name, host) = pair(p, "--throughput-push")?;
        if !usnm_ocr_dash::throughput::SOURCES.contains(&name.as_str()) {
            bail!("--throughput-push {p:?}: the source must be controller, ship or boinc");
        }
        let source: Source = host
            .parse()
            .map_err(|e| anyhow::anyhow!("--throughput-push {p:?}: {e}"))?;
        throughput_pushers.insert(name, Allow::new(vec![source]));
    }
    let app = Arc::new(App {
        pipeline: Mutex::new(usnm_ocr_dash::pipeline::Pipeline::new(
            cli.pipeline_snapshot.clone(),
            cli.pipeline_max_age_secs,
        )),
        fetch: Mutex::new(
            Fetch::new(cli.scans.clone(), cli.manifest.clone())
                .with_manifests((cli.manifests != Path::new("none")).then(|| cli.manifests.clone()))
                .with_files(cli.bulk_status.clone(), cli.pacer.clone())
                .with_publish(cli.publish_status.clone()),
        ),
        nodes: Mutex::new(Nodes::open(cli.state_dir.clone(), &workers)?),
        hosts: Mutex::new(Hosts::new(cli.host_file.clone())),
        ssets: Mutex::new(Ssets::default()),
        queues: Mutex::new(usnm_ocr_dash::queues::Queues::new(
            cli.queue_url.as_deref(),
        )?),
        throughput: Mutex::new(usnm_ocr_dash::throughput::Throughput::open(&cli.state_dir)),
        seed_plan_path: cli.seed_plan.clone(),
        seed_plan: Mutex::new(None),
        settings: Settings {
            guest_prefix: cli.guest_prefix.clone(),
            next_n: cli.rows.clamp(1, 200),
            workbook_url: cli.workbook_url.clone(),
            refresh_secs: cli.refresh_secs.max(5),
        },
        allow: Allow::new(cli.allow.clone()),
        pushers,
        sset_pushers,
        throughput_pushers,
    });
    // Names and subnets now, then every five minutes.
    let refresh = |app: Arc<App>| async move {
        app.allow.refresh().await;
        for a in app
            .pushers
            .values()
            .chain(app.sset_pushers.values())
            .chain(app.throughput_pushers.values())
        {
            a.refresh().await;
        }
    };
    refresh(app.clone()).await;
    {
        let app = app.clone();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(300)).await;
                refresh(app.clone()).await;
            }
        });
    }
    let client = reqwest::Client::builder()
        .no_proxy()
        .connect_timeout(Duration::from_secs(2))
        .user_agent(format!("usnm-ocrdash/{}", env!("CARGO_PKG_VERSION")))
        .build()?;
    {
        let app = app.clone();
        let every = Duration::from_secs(cli.poll_secs.max(5));
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(every);
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let a = app.clone();
                if let Err(e) = tokio::task::spawn_blocking(move || a.poll_files()).await {
                    tracing::error!(error = %e, "reading the files panicked");
                }
                tokio::join!(app.poll_nodes(&client), app.poll_queues());
            }
        });
    }
    if let Some(addr) = cli.public_listen {
        let public = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("listening on {addr} (--public-listen)"))?;
        tracing::info!(public_listen = %addr, "serving the public, read-only view");
        let app = app.clone();
        tokio::spawn(async move {
            let r = axum::serve(public, public_router(app).into_make_service())
                .with_graceful_shutdown(stop_requested())
                .await;
            if let Err(e) = r {
                tracing::error!(error = %e, "the public listener stopped");
                std::process::exit(1);
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("listening on {}", cli.listen))?;
    let (Some(cert), Some(key), Some(name)) = (cli.tls_cert, cli.tls_key, cli.https_name) else {
        tracing::info!(listen = %cli.listen, version = VERSION.as_str(), "usnm-ocrdash started");
        axum::serve(
            listener,
            router(app).into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(stop_requested())
        .await?;
        return Ok(());
    };
    check_name_for_url(&name)?;
    let tls = RustlsConfig::from_pem_file(&cert, &key)
        .await
        .with_context(|| format!("reading {} and {}", cert.display(), key.display()))?;
    reload_on_sighup(tls.clone(), cert, key)?;
    let handle = axum_server::Handle::new();
    let (stop_tx, mut stop_rx) = tokio::sync::watch::channel(false);
    {
        let handle = handle.clone();
        tokio::spawn(async move {
            stop_requested().await;
            handle.graceful_shutdown(Some(Duration::from_secs(10)));
            let _ = stop_tx.send(true);
        });
    }
    tracing::info!(
        listen = %cli.listen,
        tls_listen = %cli.tls_listen,
        https_name = name,
        version = VERSION.as_str(),
        "usnm-ocrdash started"
    );
    let authority = match cli.tls_listen.port() {
        443 => name.clone(),
        port => format!("{name}:{port}"),
    };
    let plain = axum::serve(
        listener,
        redirect_router(app.clone(), &authority)
            .into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move {
        let _ = stop_rx.wait_for(|stop| *stop).await;
    });
    let https = axum_server::bind_rustls(cli.tls_listen, tls)
        .handle(handle)
        .serve(router(app).into_make_service_with_connect_info::<SocketAddr>());
    tokio::try_join!(async { plain.await.context("plain HTTP") }, async {
        https
            .await
            .with_context(|| format!("HTTPS on {}", cli.tls_listen))
    },)?;
    Ok(())
}

/// SIGINT, or SIGTERM from `systemctl stop` and `restart`.
async fn stop_requested() {
    use tokio::signal::unix::{signal, SignalKind};
    match signal(SignalKind::terminate()) {
        Ok(mut term) => {
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
        }
        Err(_) => {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
    tracing::info!("stop requested");
}

/// `--https-name` goes into redirects: a host name only.
fn check_name_for_url(name: &str) -> anyhow::Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 253
        && name
            .split('.')
            .all(|l| !l.is_empty() && l.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'));
    if !ok {
        bail!("--https-name: a host name, not {name:?}");
    }
    Ok(())
}

/// The certificate is renewed outside (home-acme on the Proxmox host),
/// which then runs `systemctl reload`: read the files again. A bad pair is
/// logged and the old one kept.
fn reload_on_sighup(tls: RustlsConfig, cert: PathBuf, key: PathBuf) -> anyhow::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut hup = signal(SignalKind::hangup()).context("SIGHUP handler")?;
    tokio::spawn(async move {
        while hup.recv().await.is_some() {
            match tls.reload_from_pem_file(&cert, &key).await {
                Ok(()) => tracing::info!(cert = %cert.display(), "reloaded the certificate"),
                Err(e) => {
                    tracing::error!(error = %e, cert = %cert.display(), "can't reload the certificate; keeping the old one")
                }
            }
        }
    });
    Ok(())
}
