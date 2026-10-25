//! `seed-state`, `import-staged-run`, `mirror-store` and `publish-staged`: a release built on an
//! on-prem host and copied into Azure (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem").
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Args;
use usnm_ingest::release::Published;
use usnm_ingest::state::State;

use crate::Cli;

/// Give a local state file (`--state-file`) a curated record for each
/// batch of a published version's batch list, for a release built on an
/// on-prem host (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem"). With
/// `--every`, only every `every`th batch by name from `--offset` (the
/// 1% set: 100 and 39). Refuses Cosmos.
#[derive(Args)]
pub(crate) struct SeedStateArgs {
    /// The version whose `batches.json` (in the reference store) to take.
    #[arg(long)]
    from_version: String,
    /// The lake's `reference` container URL: its version names for
    /// today and the next two days are recorded as taken, so the
    /// release picks a name nothing else has.
    #[arg(long)]
    reserve_from: Option<String>,
    #[arg(long, requires = "offset")]
    every: Option<usize>,
    #[arg(long, requires = "every")]
    offset: Option<usize>,
}

/// Record in pipeline state (Cosmos) the run of a version built on-prem,
/// from its snapshot's `index_run.json`, before it is promoted
/// (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem"): prune then keeps it as
/// a newer published run, rollback or not. A failed earlier attempt of
/// the same name is replaced; another published version of it stops this.
#[derive(Args)]
pub(crate) struct ImportStagedRunArgs {
    #[arg(long)]
    version: String,
}

/// Copy objects from one store to another: the lake's `reference` into
/// the local mirror an on-prem release uses (docs/operations/rebuild-and-index-storage.md, "Full
/// rebuild on-prem"). Only the named prefixes and files; one already
/// there with other bytes stops it.
#[derive(Args)]
pub(crate) struct MirrorStoreArgs {
    /// The source store (a Blob container URL or a directory).
    #[arg(long)]
    from: String,
    /// The destination store.
    #[arg(long)]
    to: String,
    /// Directory-like prefixes to copy everything under.
    #[arg(long, value_delimiter = ',')]
    prefixes: Vec<String>,
    /// Single objects to copy.
    #[arg(long, value_delimiter = ',')]
    files: Vec<String>,
}

/// Copy a release staged on an on-prem host into Azure: each index of the
/// local `index-storage-pending.json` into the index account, then the
/// version's reference snapshot and the pending pointer into the lake
/// (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem"). The stores of the
/// release (`--reference`, the writer's `file://` index root) are the
/// local side. A rerun skips what is already copied. With `--restore` it
/// copies a version the lake already serves (after serving moved on-prem,
/// to serve from Azure again) and writes neither pointer.
#[derive(Args)]
pub(crate) struct PublishStagedArgs {
    /// The writer's local index root: the directory its `file://`
    /// metastore and index root URI named.
    #[arg(long)]
    local_index_root: PathBuf,
    /// The lake's `reference` container URL.
    #[arg(long)]
    to_reference: String,
    /// The index account's `qw-index` container URL.
    #[arg(long)]
    to_index: String,
    /// The index root as the searchers name it.
    #[arg(long, default_value = "azure://qw-index")]
    index_root_uri: String,
    /// Check and list only.
    #[arg(long)]
    dry_run: bool,
    /// Copy the version the lake's `current.json` already names, instead of
    /// a release the lake still has the rollback of. Refuses unless
    /// `current.json` names this release's own version and indexes; writes
    /// neither the pending pointer nor `current.json`.
    #[arg(long)]
    restore: bool,
}

pub(crate) async fn seed_state_command(
    cli: &Cli,
    state: State,
    args: &SeedStateArgs,
) -> anyhow::Result<Option<Published>> {
    let SeedStateArgs {
        from_version,
        every,
        offset,
        reserve_from,
    } = args;
    if cli.stores.cosmos.is_some() || cli.stores.state_file.is_none() {
        bail!("seed-state writes a local state file only: set --state-file, not --cosmos");
    }
    let every = every.map(|every| usnm_ingest::onprem::Every {
        every,
        offset: offset.unwrap_or(0),
    });
    let seeded = usnm_ingest::onprem::seed_state(
        &state,
        usnm_store::open(&cli.stores.reference)?.as_ref(),
        from_version,
        every,
    )
    .await?;
    tracing::info!(?seeded, "state seeded");
    if let Some(lake) = reserve_from {
        let reserved = usnm_ingest::onprem::reserve_names(
            &state,
            usnm_store::open(lake)?.as_ref(),
            chrono::Utc::now().date_naive(),
        )
        .await?;
        tracing::info!(?reserved, "version names reserved");
    }
    Ok(None)
}

pub(crate) async fn publish_staged_command(
    cli: &Cli,
    state: State,
    args: &PublishStagedArgs,
) -> anyhow::Result<Option<Published>> {
    let PublishStagedArgs {
        local_index_root,
        to_reference,
        to_index,
        index_root_uri,
        dry_run,
        restore,
    } = args;
    let local_reference = usnm_store::open(&cli.stores.reference)?;
    let reference = usnm_store::open(to_reference)?;
    let index = usnm_store::open(to_index)?;
    usnm_ingest::onprem::publish_staged(&usnm_ingest::onprem::Staged {
        state: &state,
        local_reference: local_reference.as_ref(),
        local_index_root: local_index_root.clone(),
        reference: reference.as_ref(),
        index: index.as_ref(),
        index_root_uri: index_root_uri.clone(),
        dry_run: *dry_run,
        restore: *restore,
    })
    .await?;
    Ok(None)
}

pub(crate) async fn import_staged_run_command(
    cli: &Cli,
    state: State,
    args: &ImportStagedRunArgs,
) -> anyhow::Result<Option<Published>> {
    let ImportStagedRunArgs { version } = args;
    let (run, _) = usnm_ingest::onprem::import_staged_run(
        &state,
        usnm_store::open(&cli.stores.reference)?.as_ref(),
        version,
    )
    .await?
    .with_context(|| format!("no run for {version} in the state or its snapshot"))?;
    tracing::info!(version, new_index = %run.new_index, status = ?run.status, "staged run recorded");
    Ok(None)
}

pub(crate) async fn mirror_store_command(
    args: &MirrorStoreArgs,
) -> anyhow::Result<Option<Published>> {
    let MirrorStoreArgs {
        from,
        to,
        prefixes,
        files,
    } = args;
    usnm_ingest::onprem::mirror(
        usnm_store::open(from)?.as_ref(),
        usnm_store::open(to)?.as_ref(),
        prefixes,
        files,
    )
    .await?;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::{Cli, Command};

    fn publish_staged(extra: &[&str]) -> super::PublishStagedArgs {
        let mut args = vec![
            "usnm-ingest",
            "--curated",
            "c",
            "--reference",
            "r",
            "publish-staged",
            "--local-index-root",
            "qw",
            "--to-reference",
            "https://lake.example/reference",
            "--to-index",
            "https://index.example/qw-index",
        ];
        args.extend_from_slice(extra);
        let Command::PublishStaged(args) = Cli::try_parse_from(args).unwrap().command else {
            panic!("not publish-staged");
        };
        args
    }

    /// A publish is the plain copy unless asked to restore.
    #[test]
    fn publish_staged_restores_only_when_asked() {
        let plain = publish_staged(&[]);
        assert!(!plain.restore);
        assert!(!plain.dry_run);
        let restore = publish_staged(&["--restore", "--dry-run"]);
        assert!(restore.restore);
        assert!(restore.dry_run);
    }
}
