//! `enqueue` and `curate`: recording the listed batches and curating the queued ones.
use anyhow::bail;
use clap::Args;
use usnm_ingest::release::Published;
use usnm_ingest::source::{self, ListedBatch};
use usnm_ingest::state::State;
use usnm_ingest::worker::{self, Worker};
use usnm_state::owner_id;

use super::common::{deadline, read_list, Stores};
use crate::Cli;

/// Record listed batches and queue new ones and new versions.
#[derive(Args)]
pub(crate) struct EnqueueArgs {
    /// Batch list: JSON `[{name, url, sha256?, ocr_source?}]` or LoC's
    /// collection JSON, as a path or https URL.
    #[arg(long, default_value = source::LOC_DATASETS)]
    list: String,
    /// Only these batches (names with their version suffix), e.g. to try a few.
    #[arg(long, value_delimiter = ',')]
    batches: Vec<String>,
    /// Queue the listed batches again even if they are curated (not one
    /// being curated now), for a re-curation: e.g. to retain their
    /// archives (`USNM_RAW_URL`). Needs `--batches`.
    #[arg(long, requires = "batches")]
    force: bool,
}

/// Claim and curate queued batches until none are left.
#[derive(Args)]
pub(crate) struct CurateArgs {
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
}

pub(super) async fn enqueue(
    state: &State,
    list: &str,
    only: &[String],
    force: bool,
    raw: Option<&str>,
) -> anyhow::Result<Vec<ListedBatch>> {
    let mut batches = read_list(list, raw).await?;
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
    let report = if force {
        source::requeue(state, &batches).await?
    } else {
        source::enqueue(state, &batches).await?
    };
    tracing::info!(?report, "enqueued");
    Ok(batches)
}

pub(super) async fn curate(
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
        raw: cli.raw.as_deref().map(usnm_store::open).transpose()?,
    };
    let n = worker.run(max).await?;
    tracing::info!(curated = n, "curation finished");
    Ok(())
}

pub(crate) async fn enqueue_command(
    cli: &Cli,
    state: State,
    args: &EnqueueArgs,
) -> anyhow::Result<Option<Published>> {
    let EnqueueArgs {
        list,
        batches,
        force,
    } = args;
    enqueue(&state, list, batches, *force, cli.stores.raw.as_deref())
        .await
        .map(|_| None)
}

pub(crate) async fn curate_command(
    cli: &Cli,
    state: State,
    start: tokio::time::Instant,
    args: &CurateArgs,
) -> anyhow::Result<Option<Published>> {
    let CurateArgs {
        max_batches,
        enqueue: first,
        list,
        fetch_interval_secs,
        max_runtime_secs,
    } = args;
    if *first {
        enqueue(&state, list, &[], false, cli.stores.raw.as_deref()).await?;
    }
    curate(
        &cli.stores,
        &state,
        *max_batches,
        *fetch_interval_secs,
        deadline(start, *max_runtime_secs),
    )
    .await
    .map(|()| None)
}
