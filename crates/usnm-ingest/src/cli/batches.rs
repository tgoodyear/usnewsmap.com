//! `archive-copy` and `duplicates`: the retained archives of the batches, and the pages more than
//! one curated batch ships.
use clap::Args;
use usnm_ingest::release::Published;
use usnm_ingest::state::State;

use crate::Cli;

/// Copy retained archives and kept batch lists between raw stores, e.g.
/// from this environment's own `raw` container into the archival
/// account (docs/operations/environments-and-deploys.md, "Archival storage"). Each archive is
/// checked against its manifest's sha256; one already there is skipped.
#[derive(Args)]
pub(crate) struct ArchiveCopyArgs {
    /// The source raw store (a Blob container URL or a directory).
    #[arg(long)]
    from: String,
    /// The destination (default: USNM_RAW_URL).
    #[arg(long, env = "USNM_RAW_URL")]
    to: String,
    /// The batches (with their version suffix) to copy.
    #[arg(long, value_delimiter = ',', required = true)]
    batches: Vec<String>,
}

pub(crate) async fn archive_copy_command(
    args: &ArchiveCopyArgs,
) -> anyhow::Result<Option<Published>> {
    let ArchiveCopyArgs { from, to, batches } = args;
    let copied = usnm_ingest::raw::copy(
        usnm_store::open(from)?.as_ref(),
        usnm_store::open(to)?.as_ref(),
        batches,
    )
    .await?;
    tracing::info!(?copied, "archives copied");
    Ok(None)
}

pub(crate) async fn duplicates_command(
    cli: &Cli,
    state: State,
) -> anyhow::Result<Option<Published>> {
    let curated = usnm_store::open(&cli.stores.curated)?;
    let report = usnm_ingest::dedup::report(&state, curated.as_ref()).await?;
    println!("{}", serde_json::to_string_pretty(&report)?);
    Ok(None)
}
