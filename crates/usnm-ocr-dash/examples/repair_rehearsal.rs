//! Local-only driver for scripts/boinc/onprem/ocr-feed/test_repair_rehearsal.py.
//! Uses production entry points; only the upstream image service is synthetic.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::Utc;
use clap::{Parser, Subcommand};
use usnm_ocr_dash::fetch::Fetch;
use usnm_ocr_worker::loc::{LocClient, LocConfig};
use usnm_ocr_worker::{prefetch, serve};

#[derive(Parser)]
#[command(about = "Rehearse repair protocol entry points with disposable localhost resources")]
struct Cli {
    /// Explicitly marked disposable rehearsal root.
    #[arg(long, env = "USNM_REPAIR_ROOT")]
    root: PathBuf,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Serve,
    #[command(visible_alias = "prefetch")]
    Fetch {
        #[arg(long, env = "USNM_PREFETCH_MANIFEST")]
        manifest: PathBuf,
        #[arg(long, env = "USNM_REPAIR_SOURCE")]
        source: String,
    },
    Dashboard,
    OrdinaryDecode {
        #[arg(long)]
        manifest: PathBuf,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let Cli { root, command } = Cli::parse();
    let root = root.canonicalize()?;
    anyhow::ensure!(
        root.join(".repair-rehearsal").is_file(),
        "requires an explicitly initialized disposable rehearsal root"
    );
    match command {
        Command::Serve => {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
            let opts = serve::ServeOptions {
                scans: root.join("scans"),
                manifests: root.join("manifests"),
                allow: Arc::new(serve::AllowList::resolve(&["127.0.0.1".into()]).await?),
            };
            println!("http://{}", listener.local_addr()?);
            axum::serve(
                listener,
                serve::router(opts).into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await?;
        }
        Command::Fetch { manifest, source } => {
            let manifest = manifest.canonicalize()?;
            anyhow::ensure!(manifest.starts_with(&root), "manifest outside rehearsal");
            let url = reqwest::Url::parse(&source)?;
            anyhow::ensure!(
                url.scheme() == "http" && url.host_str() == Some("127.0.0.1"),
                "synthetic source must be IPv4 loopback HTTP"
            );
            let opts = prefetch::Options {
                manifest,
                out: root.join("scans"),
                concurrency: 2,
                progress_every: 1,
                ..prefetch::Options::default()
            };
            let client = Arc::new(LocClient::new(LocConfig {
                api_root: source.clone(),
                iiif_root: format!("{source}/iiif/"),
                storage_root: format!("{source}/storage-services/service/ndnp/"),
                batches_root: format!("{source}/data/batches/"),
                batch_cache: Some(root.join("batch-cache")),
                global_pacer: Some(root.join("pacer.json")),
                attempts: 2,
                item_probes: false,
                html_is_block: true,
                ..LocConfig::default()
            })?);
            let (_tx, rx) = tokio::sync::watch::channel(false);
            println!(
                "{}",
                serde_json::to_string(&prefetch::run(opts, client, rx).await?)?
            );
        }
        Command::Dashboard => {
            let mut fetch = Fetch::new(root.join("scans"), None)
                .with_manifests(Some(root.join("manifests")))
                .with_files(None, Some(root.join("pacer.json")));
            fetch.poll(Utc::now());
            println!(
                "{}",
                serde_json::json!({
                    "queue": fetch.queue(Utc::now()),
                    "next": fetch.next_queued(100),
                    "queue_error": fetch.queue_error,
                    "index_error": fetch.index_error,
                    "status_error": fetch.status_error,
                    "status": fetch.status,
                    "pacer": fetch.pacer.value,
                })
            );
        }
        Command::OrdinaryDecode { manifest } => {
            // This is the ordinary-format contract, NOT an assertion about a deployed binary.
            let path = manifest.canonicalize()?;
            anyhow::ensure!(path.starts_with(&root), "manifest outside rehearsal");
            let rows =
                usnm_ocr_contract::prefetch::decode(bytes::Bytes::from(std::fs::read(path)?))?;
            println!("{}", rows.len());
        }
    }
    Ok(())
}
