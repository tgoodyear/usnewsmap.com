//! `usnm-ocrdash`: the on-prem OCR pipeline's LAN dashboard (12 §12.2.5).

use std::collections::BTreeMap;
use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Duration;

use anyhow::{bail, Context};
use clap::Parser;
use usnm_ocr_dash::fetch::Fetch;
use usnm_ocr_dash::hosts::Hosts;
use usnm_ocr_dash::net::Cidr;
use usnm_ocr_dash::nodes::{check_name, Nodes};
use usnm_ocr_dash::view::Settings;
use usnm_ocr_dash::{router, App};

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
    /// Where to listen.
    #[arg(long, env = "USNM_OCRDASH_LISTEN", default_value = "0.0.0.0:80")]
    listen: SocketAddr,
    /// Address ranges allowed to ask anything (`192.168.0.0/24`),
    /// comma-separated or repeated.
    #[arg(
        long,
        env = "USNM_OCRDASH_ALLOW",
        value_delimiter = ',',
        required = true
    )]
    allow: Vec<Cidr>,
    /// The fetcher's output (`status-full.json`, `index-full.jsonl`,
    /// `failures-full.jsonl`), read-only.
    #[arg(long, env = "USNM_OCRDASH_SCANS", default_value = "/tank/ocr/scans")]
    scans: PathBuf,
    /// The manifest, when the fetcher's status doesn't name one.
    #[arg(long, env = "USNM_OCRDASH_MANIFEST")]
    manifest: Option<PathBuf>,
    /// A host stats file to read (`<host>.json`), comma-separated or
    /// repeated.
    #[arg(long, env = "USNM_OCRDASH_HOST_FILE", value_delimiter = ',')]
    host_file: Vec<PathBuf>,
    /// A host that pushes its stats, as `<name>=<its address>`.
    #[arg(long, env = "USNM_OCRDASH_HOST_PUSH", value_delimiter = ',')]
    host_push: Vec<String>,
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
        let (name, ip) = pair(p, "--host-push")?;
        let ip: IpAddr = ip
            .parse()
            .with_context(|| format!("--host-push {p:?}: not an address"))?;
        pushers.insert(name, ip);
    }
    let app = Arc::new(App {
        fetch: Mutex::new(Fetch::new(cli.scans.clone(), cli.manifest.clone())),
        nodes: Mutex::new(Nodes::open(cli.state_dir.clone(), &workers)?),
        hosts: Mutex::new(Hosts::new(cli.host_file.clone())),
        seed_plan_path: cli.seed_plan.clone(),
        seed_plan: Mutex::new(None),
        settings: Settings {
            guest_prefix: cli.guest_prefix.clone(),
            next_n: cli.rows.clamp(1, 200),
            workbook_url: cli.workbook_url.clone(),
            refresh_secs: cli.refresh_secs.max(5),
        },
        allow: cli.allow.clone(),
        pushers,
    });
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
                app.poll_nodes(&client).await;
            }
        });
    }
    let listener = tokio::net::TcpListener::bind(cli.listen)
        .await
        .with_context(|| format!("listening on {}", cli.listen))?;
    tracing::info!(listen = %cli.listen, version = VERSION.as_str(), "usnm-ocrdash started");
    axum::serve(
        listener,
        router(app).into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async {
        let _ = tokio::signal::ctrl_c().await;
    })
    .await?;
    Ok(())
}
