use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context};
use clap::{Args, Parser, Subcommand};
use usnm_ingest::docs::{DocStore, FileDocs};
use usnm_ingest::merges::{self, MergeWait};
use usnm_ingest::release::Release;
use usnm_ingest::sink::{IndexSink, JsonlSink, QuickwitNode, QuickwitSink};
use usnm_ingest::source::{self, ListedBatch};
use usnm_ingest::state::{BatchStatus, State};
use usnm_ingest::telemetry;
use usnm_ingest::titles;
use usnm_ingest::worker::{self, Worker};
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
    /// Seconds to wait for the new index's merges before giving up without
    /// publishing (Quickwit targets).
    #[arg(long, env = "USNM_MERGE_TIMEOUT_SECS", default_value_t = merges::DEFAULT_TIMEOUT_SECS)]
    merge_timeout_secs: u64,
    /// Refuse to start a writer node with less free disk than this (GiB) in
    /// the work directory: the scratch volume isn't mounted, or a previous
    /// run's files fill it.
    #[arg(long, env = "USNM_WORK_MIN_FREE_GIB", default_value_t = 0)]
    min_free_gib: u64,
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
        /// Seconds between bulk downloads across every worker (LoC allows 10
        /// per 10 minutes per IP); 0 doesn't pace.
        #[arg(long, default_value_t = worker::FETCH_INTERVAL_SECS)]
        fetch_interval_secs: u32,
        /// Seconds after the start to stop claiming batches and waiting for
        /// download slots. A batch already downloading is finished (within
        /// the 45-minute watchdog), then the command exits 0; the rest stays
        /// queued for the next run. Keep it plus 45 minutes under the job's
        /// replica timeout.
        #[arg(long)]
        max_runtime_secs: Option<u64>,
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
    /// Fetch LoC's records for titles with pages, then rebuild the catalog (`geocode`).
    TitlesSync {
        /// Titles to fetch: those named in this batch list (LoC's listing
        /// names them), plus those in curated batches.
        #[arg(long, default_value = source::LOC_DATASETS)]
        list: String,
        /// Only these LCCNs instead.
        #[arg(long, value_delimiter = ',')]
        lccns: Vec<String>,
        /// Re-fetch records that are already cached.
        #[arg(long)]
        refresh: bool,
        #[arg(long, default_value = titles::LOC_ITEMS)]
        items: String,
    },
    /// Rebuild `catalog/titles.json` and `places.json` from the cached records
    /// and `overrides/places.json` (no network).
    Geocode,
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
        /// As `curate --max-runtime-secs`, counted from the start of `run`:
        /// batches still queued then wait for the next run, and what was
        /// curated is released. Leave room for titles-sync and the release
        /// under the job's replica timeout.
        #[arg(long)]
        curate_max_runtime_secs: Option<u64>,
        #[command(flatten)]
        target: IndexTarget,
    },
}

fn state(s: &Stores) -> anyhow::Result<State> {
    let docs: Arc<dyn DocStore> = match (&s.cosmos, &s.state_file) {
        (Some(endpoint), _) => Arc::new(CosmosDocs::new(
            endpoint,
            usnm_state::DATABASE,
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

async fn enqueue(state: &State, list: &str, only: &[String]) -> anyhow::Result<Vec<ListedBatch>> {
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
    Ok(batches)
}

/// Fetch missing title records for `listed` titles and every curated batch's
/// titles, then rebuild the catalog from what is cached. Returns whether LoC
/// rate limited the run (which then stopped early, keeping what it fetched).
async fn titles_sync(
    cli: &Stores,
    state: &State,
    listed: impl IntoIterator<Item = String>,
    refresh: bool,
    items: &str,
) -> anyhow::Result<bool> {
    let mut lccns: BTreeSet<String> = listed.into_iter().collect();
    for (b, _) in state.batches(&[BatchStatus::Curated]).await? {
        lccns.extend(b.curated.into_iter().flat_map(|c| c.lccns));
    }
    let reference = usnm_store::open(&cli.reference)?;
    let report = titles::sync(
        reference.as_ref(),
        &lccns,
        refresh,
        items,
        titles::LOC_INTERVAL,
    )
    .await?;
    tracing::info!(?report, "title records");
    geocode(reference.as_ref()).await?;
    Ok(report.throttled)
}

async fn geocode(reference: &dyn usnm_store::ObjectStore) -> anyhow::Result<()> {
    let report = titles::geocode(reference).await?;
    if !report.unresolved.is_empty() {
        tracing::warn!(unresolved = ?report.unresolved, "titles with no state were left out");
    }
    tracing::info!(?report, "catalog");
    Ok(())
}

async fn curate(
    cli: &Stores,
    state: &State,
    max: Option<usize>,
    fetch_interval_secs: u32,
    deadline: Option<tokio::time::Instant>,
) -> anyhow::Result<()> {
    let worker = Worker {
        state: state.clone(),
        curated: usnm_store::open(&cli.curated)?,
        owner: owner_id(),
        lease: chrono::Duration::hours(2),
        fetch_interval: (fetch_interval_secs > 0)
            .then_some(chrono::Duration::seconds(i64::from(fetch_interval_secs))),
        batch_limit: worker::BATCH_LIMIT,
        deadline,
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
    let wait = MergeWait {
        timeout: std::time::Duration::from_secs(t.merge_timeout_secs),
        ..MergeWait::default()
    };
    let mut sink: Box<dyn IndexSink> = match (&t.index_dir, &t.quickwit_url, &t.quickwit_bin) {
        (Some(dir), None, None) => Box::new(JsonlSink::new(dir)),
        (None, Some(url), None) => Box::new(QuickwitSink::new(url, root(t)?)?.merges(wait)),
        (None, None, Some(bin)) => {
            let metastore = t
                .quickwit_metastore
                .as_deref()
                .context("--quickwit-bin needs --quickwit-metastore")?;
            let dir = cli.work_dir.join("quickwit");
            std::fs::create_dir_all(&dir)?;
            check_free_disk(&dir, t.min_free_gib)?;
            let n = QuickwitNode::start(bin, &dir, t.quickwit_port, metastore, root(t)?).await?;
            let sink = QuickwitSink::new(&n.url, root(t)?)?
                .watching(&n)
                .merges(wait);
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

/// Fail before indexing anything if `dir` has less than `min_gib` free. A
/// previous writer's data in `dir/qwdata` is about to be removed, so it
/// counts as free.
fn check_free_disk(dir: &std::path::Path, min_gib: u64) -> anyhow::Result<()> {
    if min_gib == 0 {
        return Ok(());
    }
    let free = usnm_ingest::progress::disk_free(dir)
        .with_context(|| format!("reading the free space of {}", dir.display()))?;
    let stale = dir_size(&dir.join("qwdata"));
    let gib = |b: u64| b as f64 / (1024.0 * 1024.0 * 1024.0);
    if free.saturating_add(stale) < min_gib.saturating_mul(1 << 30) {
        bail!(
            "{} has {:.1} GiB free, under the {min_gib} GiB a release needs for indexing and \
             merges (08 §8.4); is the scratch volume mounted?",
            dir.display(),
            gib(free + stale)
        );
    }
    tracing::info!(dir = %dir.display(), free_gib = gib(free + stale).round(), "work disk");
    Ok(())
}

/// Bytes in the files under `path` (0 if it doesn't exist).
fn dir_size(path: &std::path::Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| match e.file_type() {
            Ok(t) if t.is_dir() => dir_size(&e.path()),
            Ok(_) => e.metadata().map_or(0, |m| m.len()),
            Err(_) => 0,
        })
        .sum()
}

fn root(t: &IndexTarget) -> anyhow::Result<&str> {
    t.quickwit_index_root
        .as_deref()
        .context("Quickwit targets need --quickwit-index-root")
}

#[tokio::main]
async fn main() -> std::process::ExitCode {
    let cli = Cli::parse();
    let telemetry = telemetry::init();
    let result = run(&cli).await;
    // Every failure ends with one JSON line (the whole error chain), which
    // the job-failure alert and the `errors-by-batch` query look for.
    // Returning an exit code rather than the error keeps Rust from printing
    // it again as plain text after that line.
    let code = match &result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!(command = cli.command.name(), error = %format!("{e:#}"), "command failed");
            std::process::ExitCode::FAILURE
        }
    };
    // Before exit on every path: whatever telemetry is still buffered is lost.
    telemetry.shutdown().await;
    code
}

impl Command {
    fn name(&self) -> &'static str {
        match self {
            Command::Enqueue { .. } => "enqueue",
            Command::Curate { .. } => "curate",
            Command::Release { .. } => "release",
            Command::TitlesSync { .. } => "titles-sync",
            Command::Geocode => "geocode",
            Command::Run { .. } => "run",
        }
    }
}

/// `secs` from `start`, if given.
fn deadline(start: tokio::time::Instant, secs: Option<u64>) -> Option<tokio::time::Instant> {
    secs.map(|s| start + std::time::Duration::from_secs(s))
}

async fn run(cli: &Cli) -> anyhow::Result<()> {
    let start = tokio::time::Instant::now();
    let state = state(&cli.stores)?;
    match &cli.command {
        Command::Enqueue { list, batches } => enqueue(&state, list, batches).await.map(drop),
        Command::TitlesSync {
            list,
            lccns,
            refresh,
            items,
        } => {
            let listed = if lccns.is_empty() {
                read_list(list)
                    .await?
                    .into_iter()
                    .flat_map(|b| b.lccns)
                    .collect()
            } else {
                lccns.clone()
            };
            if titles_sync(&cli.stores, &state, listed, *refresh, items).await? {
                // Fail, so the job reports it and a later run continues.
                bail!("LoC rate limited titles-sync; the cache kept what was fetched, and the next run continues");
            }
            Ok(())
        }
        Command::Geocode => geocode(usnm_store::open(&cli.stores.reference)?.as_ref()).await,
        Command::Curate {
            max_batches,
            enqueue: first,
            list,
            fetch_interval_secs,
            max_runtime_secs,
        } => {
            if *first {
                enqueue(&state, list, &[]).await?;
            }
            curate(
                &cli.stores,
                &state,
                *max_batches,
                *fetch_interval_secs,
                deadline(start, *max_runtime_secs),
            )
            .await
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
            curate_max_runtime_secs,
            target,
        } => {
            let listed = enqueue(&state, list, batches).await?;
            curate(
                &cli.stores,
                &state,
                None,
                worker::FETCH_INTERVAL_SECS,
                deadline(start, *curate_max_runtime_secs),
            )
            .await?;
            // Every curated title needs a catalog entry before release.
            let lccns = listed.into_iter().flat_map(|b| b.lccns);
            if titles_sync(&cli.stores, &state, lccns, false, titles::LOC_ITEMS).await? {
                // Release anyway: it refuses to publish if a curated title is
                // still missing from the catalog, and a partial backlog of
                // titles without pages shouldn't hold up new pages.
                tracing::warn!("LoC rate limited titles-sync; releasing with the catalog as it is");
            }
            release(&cli.stores, &state, *full, *synthetic, target).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_writer_needs_its_scratch_disk() {
        let dir = tempfile::tempdir().unwrap();
        check_free_disk(dir.path(), 0).unwrap();
        check_free_disk(dir.path(), 1).unwrap();
        // More than any test machine has: the volume isn't mounted.
        let err = check_free_disk(dir.path(), 1 << 30)
            .unwrap_err()
            .to_string();
        assert!(err.contains("scratch volume"), "{err}");
    }

    #[test]
    fn a_previous_writers_data_counts_as_free() {
        let dir = tempfile::tempdir().unwrap();
        let stale = dir.path().join("qwdata/wal");
        std::fs::create_dir_all(&stale).unwrap();
        std::fs::write(stale.join("a"), vec![0u8; 3000]).unwrap();
        std::fs::write(dir.path().join("qwdata/b"), vec![0u8; 500]).unwrap();
        assert_eq!(dir_size(&dir.path().join("qwdata")), 3500);
        assert_eq!(dir_size(&dir.path().join("missing")), 0);
    }

    #[test]
    fn merge_and_disk_settings_default() {
        let cli = Cli::try_parse_from([
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "release",
            "--index-dir",
            "x",
        ])
        .unwrap();
        let Command::Release { target, .. } = cli.command else {
            panic!("not a release");
        };
        assert_eq!(target.merge_timeout_secs, merges::DEFAULT_TIMEOUT_SECS);
        assert_eq!(target.min_free_gib, 0);
    }
}
