use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand};
use tracing_subscriber::EnvFilter;
use usnm_ingest::docs::{DocStore, FileDocs};
use usnm_ingest::release::Release;
use usnm_ingest::sink::{IndexSink, JsonlSink, QuickwitNode, QuickwitSink};
use usnm_ingest::source::{self, ListedBatch};
use usnm_ingest::state::State;
use usnm_ingest::worker::Worker;
use usnm_ingest::{cosmos::CosmosDocs, owner_id};
use usnm_store::credential;

/// US News Map ingest pipeline (docs/design/04 §4.4).
#[derive(Parser)]
#[command(version)]
struct Cli {
    #[command(flatten)]
    stores: Stores,
    #[command(subcommand)]
    command: Command,
}

#[derive(Args)]
struct Stores {
    /// Cosmos DB endpoint for pipeline state (Azure).
    #[arg(long, env = "USNM_COSMOS_ENDPOINT", conflicts_with = "state_file")]
    cosmos: Option<String>,
    /// Local JSON file for pipeline state (development).
    #[arg(long, env = "USNM_STATE_FILE")]
    state_file: Option<PathBuf>,
    /// Curated lake: a Blob container URL or a directory.
    #[arg(long, env = "USNM_CURATED_URL")]
    curated: String,
    /// Reference store (catalog, snapshots, current.json): a Blob container URL or a directory.
    #[arg(long, env = "USNM_REFERENCE_URL")]
    reference: String,
    /// Scratch space for the Quickwit writer node.
    #[arg(long, env = "USNM_WORK_DIR", default_value_os_t = std::env::temp_dir().join("usnm-ingest"))]
    work_dir: PathBuf,
}

#[derive(Args, Default)]
struct IndexTarget {
    /// Write JSONL indexes to this directory (the API's memory backend).
    #[arg(long, env = "USNM_INDEX_DIR")]
    index_dir: Option<PathBuf>,
    /// An already-running Quickwit writer node.
    #[arg(long, env = "USNM_QUICKWIT_URL")]
    quickwit_url: Option<String>,
    /// Run a Quickwit writer node from this binary for the length of the release.
    #[arg(long, env = "USNM_QUICKWIT_BIN")]
    quickwit_bin: Option<PathBuf>,
    /// Metastore URI for the writer node (`azure://qw-index` in Azure).
    #[arg(long, env = "USNM_QUICKWIT_METASTORE")]
    quickwit_metastore: Option<String>,
    /// Where new indexes are created (`azure://qw-index` in Azure).
    #[arg(long, env = "USNM_QUICKWIT_INDEX_ROOT")]
    quickwit_index_root: Option<String>,
    #[arg(long, env = "USNM_QUICKWIT_PORT", default_value_t = 7380)]
    quickwit_port: u16,
}

#[derive(Subcommand)]
enum Command {
    /// Record listed batches and queue new ones and new versions.
    Enqueue {
        /// Batch list: JSON `[{name, url, sha256?, ocr_source?}]` or LoC's
        /// collection JSON, as a path or https URL.
        #[arg(long, default_value = source::LOC_DATASETS)]
        list: String,
        /// Only these batches (names with their version suffix), e.g. to try a few.
        #[arg(long, value_delimiter = ',')]
        batches: Vec<String>,
    },
    /// Claim and curate queued batches until none are left.
    Curate {
        #[arg(long)]
        max_batches: Option<usize>,
        /// Enqueue from `--list` first (safe to run in many workers at once).
        #[arg(long)]
        enqueue: bool,
        #[arg(long, default_value = source::LOC_DATASETS)]
        list: String,
    },
    /// Build a new index and reference snapshot from curated batches, then publish.
    Release {
        /// Rebuild everything into a new base index (compaction).
        #[arg(long)]
        full: bool,
        /// Label the version as synthetic demo data (the fixture batches).
        #[arg(long)]
        synthetic: bool,
        #[command(flatten)]
        target: IndexTarget,
    },
    /// Enqueue (if a list is given), curate everything queued, then release.
    Run {
        /// As for `enqueue`: LoC's listing unless another list is given.
        #[arg(long, default_value = source::LOC_DATASETS)]
        list: String,
        /// Only these batches from the list.
        #[arg(long, value_delimiter = ',')]
        batches: Vec<String>,
        #[arg(long)]
        full: bool,
        #[arg(long)]
        synthetic: bool,
        #[command(flatten)]
        target: IndexTarget,
    },
}

fn state(s: &Stores) -> anyhow::Result<State> {
    let docs: Arc<dyn DocStore> = match (&s.cosmos, &s.state_file) {
        (Some(endpoint), _) => Arc::new(CosmosDocs::new(
            endpoint,
            "usnm",
            credential::from_env_for(credential::COSMOS_RESOURCE),
        )?),
        (None, Some(path)) => Arc::new(FileDocs::open(path)?),
        (None, None) => bail!("set --cosmos (Azure) or --state-file (local)"),
    };
    Ok(State::new(docs))
}

async fn read_list(list: &str) -> anyhow::Result<Vec<ListedBatch>> {
    let dest = tempfile::NamedTempFile::new()?;
    source::fetch(list, dest.path()).await?;
    source::parse_list(&std::fs::read(dest.path())?)
}

async fn enqueue(state: &State, list: &str, only: &[String]) -> anyhow::Result<()> {
    let mut batches = read_list(list).await?;
    if !only.is_empty() {
        batches.retain(|b| only.contains(&b.name));
        let found: Vec<&str> = batches.iter().map(|b| b.name.as_str()).collect();
        let missing: Vec<&String> = only
            .iter()
            .filter(|o| !found.contains(&o.as_str()))
            .collect();
        if !missing.is_empty() {
            bail!("not in the list: {missing:?}");
        }
    }
    let report = source::enqueue(state, &batches).await?;
    tracing::info!(?report, "enqueued");
    Ok(())
}

async fn curate(cli: &Stores, state: &State, max: Option<usize>) -> anyhow::Result<()> {
    let worker = Worker {
        state: state.clone(),
        curated: usnm_store::open(&cli.curated)?,
        owner: owner_id(),
        lease: chrono::Duration::hours(2),
    };
    let n = worker.run(max).await?;
    tracing::info!(curated = n, "curation finished");
    Ok(())
}

async fn release(
    cli: &Stores,
    state: &State,
    full: bool,
    synthetic: bool,
    t: &IndexTarget,
) -> anyhow::Result<()> {
    let r = Release {
        state: state.clone(),
        curated: usnm_store::open(&cli.curated)?,
        reference: usnm_store::open(&cli.reference)?,
        owner: owner_id(),
        full,
        synthetic,
        now: chrono::Utc::now(),
    };
    // Held from before the writer node starts until after it stops, and
    // released on every path.
    let lease = r.lock().await?;
    let result = release_locked(cli, &r, &lease, t).await;
    let unlocked = r.unlock(lease).await;
    let result = result.and_then(|p| unlocked.map(|()| p));
    match result? {
        Some(p) => {
            tracing::info!(version = %p.index_version, docs = p.docs, pages = p.pages, full = p.full, "released")
        }
        None => tracing::info!("nothing to release"),
    }
    Ok(())
}

/// Start the index target (and writer node), release, stop the node.
async fn release_locked(
    cli: &Stores,
    r: &Release,
    lease: &usnm_ingest::release::WriterLease,
    t: &IndexTarget,
) -> anyhow::Result<Option<usnm_ingest::release::Published>> {
    let mut node = None;
    let mut sink: Box<dyn IndexSink> = match (&t.index_dir, &t.quickwit_url, &t.quickwit_bin) {
        (Some(dir), None, None) => Box::new(JsonlSink::new(dir)),
        (None, Some(url), None) => Box::new(QuickwitSink::new(url, root(t)?)?),
        (None, None, Some(bin)) => {
            let metastore = t
                .quickwit_metastore
                .as_deref()
                .context("--quickwit-bin needs --quickwit-metastore")?;
            let dir = cli.work_dir.join("quickwit");
            std::fs::create_dir_all(&dir)?;
            let n = QuickwitNode::start(bin, &dir, t.quickwit_port, metastore, root(t)?).await?;
            let sink = QuickwitSink::new(&n.url, root(t)?)?;
            node = Some(n);
            Box::new(sink)
        }
        _ => bail!("choose one of --index-dir, --quickwit-url or --quickwit-bin"),
    };
    let result = r.run_held(sink.as_mut(), lease).await;
    if let Some(n) = node {
        n.stop().await?;
    }
    result
}

fn root(t: &IndexTarget) -> anyhow::Result<&str> {
    t.quickwit_index_root
        .as_deref()
        .context("Quickwit targets need --quickwit-index-root")
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")),
        )
        .init();
    let cli = Cli::parse();
    let state = state(&cli.stores)?;
    match &cli.command {
        Command::Enqueue { list, batches } => enqueue(&state, list, batches).await,
        Command::Curate {
            max_batches,
            enqueue: first,
            list,
        } => {
            if *first {
                enqueue(&state, list, &[]).await?;
            }
            curate(&cli.stores, &state, *max_batches).await
        }
        Command::Release {
            full,
            synthetic,
            target,
        } => release(&cli.stores, &state, *full, *synthetic, target).await,
        Command::Run {
            list,
            batches,
            full,
            synthetic,
            target,
        } => {
            enqueue(&state, list, batches).await?;
            curate(&cli.stores, &state, None).await?;
            release(&cli.stores, &state, *full, *synthetic, target).await
        }
    }
}
