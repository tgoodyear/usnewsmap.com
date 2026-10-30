//! What more than one command group uses: where the pipeline's state and stores are (`Stores`,
//! the top-level options), the Quickwit target of a release or a prune (`IndexTarget`), and the
//! few functions every group calls.
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::Args;
use usnm_ingest::docs::{DocStore, FileDocs};
use usnm_ingest::source::{self, ListedBatch};
use usnm_ingest::state::State;

#[derive(Args)]
pub(crate) struct Stores {
    /// Pipeline state: Cosmos DB endpoint or certificate-authenticated PostgreSQL URL.
    #[arg(long, env = "USNM_COSMOS_ENDPOINT", conflicts_with = "state_file")]
    pub cosmos: Option<String>,
    /// Local JSON file for pipeline state (development).
    #[arg(long, env = "USNM_STATE_FILE")]
    pub state_file: Option<PathBuf>,
    /// Curated lake: a Blob container URL or a directory.
    #[arg(long, env = "USNM_CURATED_URL")]
    pub curated: String,
    /// Reference store (catalog, snapshots, current.json): a Blob container URL or a directory.
    #[arg(long, env = "USNM_REFERENCE_URL")]
    pub reference: String,
    /// Where curation retains each batch archive it downloads, and reads it
    /// back instead of LoC (`USNM_RETAIN_RAW`: a Blob container URL or a
    /// directory). Unset keeps nothing (ADR-0006).
    #[arg(long, env = "USNM_RAW_URL")]
    pub raw: Option<String>,
    /// Scratch space for the Quickwit writer node.
    #[arg(long, env = "USNM_WORK_DIR", default_value_os_t = std::env::temp_dir().join("usnm-ingest"))]
    pub work_dir: PathBuf,
}

#[derive(Args, Default)]
pub(super) struct IndexTarget {
    /// Build a fresh full version in the separate index account, publishing
    /// reference/index-storage-pending.json instead of the serving pointer.
    /// Promote it only during the documented searcher cutover (#283).
    #[arg(long)]
    pub stage_index_storage: bool,
    /// Write JSONL indexes to this directory (the API's memory backend).
    #[arg(long, env = "USNM_INDEX_DIR")]
    pub index_dir: Option<PathBuf>,
    /// An already-running Quickwit writer node.
    #[arg(long, env = "USNM_QUICKWIT_URL")]
    pub quickwit_url: Option<String>,
    /// Run a Quickwit writer node from this binary for the length of the release.
    #[arg(long, env = "USNM_QUICKWIT_BIN")]
    pub quickwit_bin: Option<PathBuf>,
    /// Metastore URI for the writer node (`azure://qw-index` in Azure).
    #[arg(long, env = "USNM_QUICKWIT_METASTORE")]
    pub quickwit_metastore: Option<String>,
    /// Where new indexes are created (`azure://qw-index` in Azure).
    #[arg(long, env = "USNM_QUICKWIT_INDEX_ROOT")]
    pub quickwit_index_root: Option<String>,
    #[arg(long, env = "USNM_QUICKWIT_PORT", default_value_t = 7380)]
    pub quickwit_port: u16,
    /// Seconds to wait for the new index's merges before giving up without
    /// publishing (Quickwit targets). Default: 4 hours for a release, 24
    /// for `release-finish`.
    #[arg(long, env = "USNM_MERGE_TIMEOUT_SECS")]
    pub merge_timeout_secs: Option<u64>,
    /// Refuse to start a writer node with less free disk than this (GiB) in
    /// the work directory: the scratch volume isn't mounted, or a previous
    /// run's files fill it.
    #[arg(long, env = "USNM_WORK_MIN_FREE_GIB", default_value_t = 0)]
    pub min_free_gib: u64,
}

pub(crate) fn state(s: &Stores) -> anyhow::Result<State> {
    let docs: Arc<dyn DocStore> = match (&s.cosmos, &s.state_file) {
        (Some(endpoint), _) => usnm_state::docs::open_endpoint(endpoint)?,
        (None, Some(path)) => Arc::new(FileDocs::open(path)?),
        (None, None) => bail!("set --cosmos (Cosmos/PostgreSQL) or --state-file (development)"),
    };
    Ok(State::new(docs))
}

/// Read a batch list. With a raw store (`USNM_RAW_URL`), a copy of a remote
/// list as fetched is kept there too, under `listings/` by time.
pub(super) async fn read_list(list: &str, raw: Option<&str>) -> anyhow::Result<Vec<ListedBatch>> {
    let dest = tempfile::NamedTempFile::new()?;
    let sha = source::fetch(list, dest.path()).await?;
    let bytes = std::fs::read(dest.path())?;
    if let (Some(raw), true) = (raw, list.starts_with("https://")) {
        let path = format!(
            "listings/{}-{}.json",
            chrono::Utc::now().format("%Y%m%dT%H%M%SZ"),
            &sha[..12]
        );
        usnm_store::open(raw)?
            .put(&path, bytes.clone(), "application/json")
            .await?;
        tracing::info!(list, path, "batch list retained");
    }
    source::parse_list(&bytes)
}

pub(super) fn root(t: &IndexTarget) -> anyhow::Result<&str> {
    t.quickwit_index_root
        .as_deref()
        .context("Quickwit targets need --quickwit-index-root")
}

/// `secs` from `start`, if given.
pub(super) fn deadline(
    start: tokio::time::Instant,
    secs: Option<u64>,
) -> Option<tokio::time::Instant> {
    secs.map(|s| start + std::time::Duration::from_secs(s))
}
