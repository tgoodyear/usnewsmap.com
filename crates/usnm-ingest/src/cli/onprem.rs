//! `seed-state`, `import-staged-run`, `mirror-store` and `publish-staged`: a release built on an
//! on-prem host and copied into Azure (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem").
use std::path::PathBuf;

use anyhow::{bail, Context};
use clap::Args;
use usnm_ingest::owner_id;
use usnm_ingest::release::{self, Published};
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
/// With `--current`, after the promotion, it also makes `ops/current` name
/// the version ("Index storage cutover").
#[derive(Args)]
pub(crate) struct ImportStagedRunArgs {
    #[arg(long)]
    version: String,
    /// Also record the version as the pipeline state's current one
    /// (`ops/current`), which a staged release leaves unchanged. Refused
    /// unless `current.json` already names it and its run is published.
    /// Takes the writer lock like a release.
    #[arg(long)]
    current: bool,
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
/// local side. A rerun skips what is already copied.
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
    })
    .await?;
    Ok(None)
}

pub(crate) async fn import_staged_run_command(
    cli: &Cli,
    state: State,
    args: &ImportStagedRunArgs,
) -> anyhow::Result<Option<Published>> {
    let ImportStagedRunArgs { version, current } = args;
    let reference = usnm_store::open(&cli.stores.reference)?;
    let (run, _) = usnm_ingest::onprem::import_staged_run(&state, reference.as_ref(), version)
        .await?
        .with_context(|| format!("no run for {version} in the state or its snapshot"))?;
    tracing::info!(version, new_index = %run.new_index, status = ?run.status, "staged run recorded");
    if *current {
        // Under the writer lock, so no release publishes between reading
        // current.json and writing ops/current.
        let owner = owner_id();
        let lease = release::lock_writer(&state, &owner).await?;
        let recorded = usnm_ingest::onprem::record_current(&state, reference.as_ref(), &run).await;
        let unlocked = release::unlock_writer(&state, &owner, lease).await;
        let recorded = recorded?;
        // Logged before the unlock's error, so a failed unlock doesn't hide
        // a write that happened.
        let previous = recorded.previous.as_deref().unwrap_or("");
        if recorded.changed() {
            tracing::warn!(version, previous, "ops/current set to the promoted version");
        } else {
            tracing::info!(version, "ops/current already names the promoted version");
        }
        unlocked.context("releasing the writer lock after recording ops/current")?;
    }
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
    use super::*;
    use crate::Command;
    use clap::Parser;
    use std::sync::Arc;
    use usnm_ingest::docs::{DocStore, MemoryDocs};
    use usnm_ingest::state::{IndexRun, RunStatus, OPS};
    use usnm_store::ObjectStore;

    /// A lake `reference` holding `v2`'s staged run record and a
    /// `current.json` naming `live`, and a state whose `ops/current` names
    /// `v1`. `v1` was built in Azure: its published run is only in the state.
    struct Lake {
        tmp: tempfile::TempDir,
        docs: Arc<MemoryDocs>,
        state: State,
    }

    /// A published run of `version`, with one index.
    fn published(version: &str, previous: Option<&str>) -> IndexRun {
        let now = chrono::Utc::now();
        IndexRun {
            id: version.into(),
            index_version: version.into(),
            full: true,
            indexes: vec![format!("base-{version}")],
            new_index: format!("base-{version}"),
            batch_count: Some(1),
            batch_list: Some(format!("{version}/batches.json")),
            batches: None,
            status: RunStatus::Published,
            docs: 5,
            pages: 5,
            duplicate_pages: 0,
            started_at: now,
            published_at: Some(now),
            previous_version: previous.map(str::to_owned),
            last_error: None,
            failed_at: None,
            build: None,
        }
    }

    async fn lake(live: &str) -> Lake {
        let tmp = tempfile::tempdir().unwrap();
        let reference = usnm_store::LocalStore::new(tmp.path());
        reference
            .put(
                "v2/index_run.json",
                serde_json::to_vec(&published("v2", Some("v1"))).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        let docs = Arc::new(MemoryDocs::default());
        let state = State::new(docs.clone());
        assert!(state.create_run(&published("v1", None)).await.unwrap());
        state.set_current_version("v1").await.unwrap();
        let l = Lake { tmp, docs, state };
        l.promote(live).await;
        l
    }

    impl Lake {
        /// `import-staged-run --version v2` with `extra`.
        async fn import(&self, extra: &[&str]) -> anyhow::Result<Option<Published>> {
            self.import_version("v2", extra).await
        }

        async fn import_version(
            &self,
            version: &str,
            extra: &[&str],
        ) -> anyhow::Result<Option<Published>> {
            let reference = self.tmp.path().to_str().unwrap();
            let mut args = vec![
                "usnm-ingest",
                "--curated",
                "c",
                "--reference",
                reference,
                "import-staged-run",
                "--version",
                version,
            ];
            args.extend_from_slice(extra);
            let cli = Cli::try_parse_from(args).unwrap();
            let Command::ImportStagedRun(a) = &cli.command else {
                panic!("not import-staged-run");
            };
            import_staged_run_command(&cli, self.state.clone(), a).await
        }

        /// Make `current.json` name `version`, as the operator's upload does.
        async fn promote(&self, version: &str) {
            usnm_store::LocalStore::new(self.tmp.path())
                .put(
                    "current.json",
                    serde_json::to_vec(&serde_json::json!({"index_version": version})).unwrap(),
                    "application/json",
                )
                .await
                .unwrap();
        }

        async fn current(&self) -> Option<String> {
            self.state.current_version().await.unwrap()
        }

        /// `ops/current`'s ETag: it changes with every write.
        async fn current_etag(&self) -> String {
            self.docs
                .get(OPS, "current", "current")
                .await
                .unwrap()
                .unwrap()
                .etag
        }
    }

    /// Without `--current` the run is recorded and `ops/current` isn't
    /// touched, even after the promotion.
    #[tokio::test]
    async fn import_alone_leaves_the_current_version() {
        let l = lake("v2").await;
        let etag = l.current_etag().await;
        l.import(&[]).await.unwrap();
        let (run, _) = l.state.run("v2").await.unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Published);
        assert_eq!(l.current().await.as_deref(), Some("v1"));
        assert_eq!(l.current_etag().await, etag);
    }

    /// With `--current` after the promotion, `ops/current` names the
    /// version; again, it is left as it is.
    #[tokio::test]
    async fn current_records_the_promoted_version() {
        let l = lake("v2").await;
        l.import(&["--current"]).await.unwrap();
        assert_eq!(l.current().await.as_deref(), Some("v2"));
        let etag = l.current_etag().await;
        l.import(&["--current"]).await.unwrap();
        assert_eq!(l.current().await.as_deref(), Some("v2"));
        assert_eq!(
            l.current_etag().await,
            etag,
            "no write when already current"
        );
        // The writer lock is free again.
        release::lock_writer(&l.state, "next-release")
            .await
            .unwrap();
    }

    /// Before the promotion `--current` is refused: the run is still
    /// recorded, `ops/current` keeps the serving version.
    #[tokio::test]
    async fn current_is_refused_before_the_promotion() {
        let l = lake("v1").await;
        let etag = l.current_etag().await;
        let e = l.import(&["--current"]).await.unwrap_err();
        assert!(
            format!("{e:#}").contains("current.json names `v1`, not `v2`"),
            "{e:#}"
        );
        assert!(l.state.run("v2").await.unwrap().is_some());
        assert_eq!(l.current().await.as_deref(), Some("v1"));
        assert_eq!(l.current_etag().await, etag);
        release::lock_writer(&l.state, "next-release")
            .await
            .unwrap();
    }

    /// While a release holds the writer lock, `--current` is refused
    /// before it reads `current.json`: the run is still recorded and
    /// `ops/current` isn't written. Once the lock is free, it goes through.
    #[tokio::test]
    async fn current_is_refused_while_the_writer_lock_is_held() {
        let l = lake("v2").await;
        let etag = l.current_etag().await;
        let lease = release::lock_writer(&l.state, "release").await.unwrap();
        let e = l.import(&["--current"]).await.unwrap_err();
        assert!(
            format!("{e:#}").contains("lock `quickwit-writer` is held"),
            "{e:#}"
        );
        assert!(l.state.run("v2").await.unwrap().is_some());
        assert_eq!(l.current().await.as_deref(), Some("v1"));
        assert_eq!(l.current_etag().await, etag);
        release::unlock_writer(&l.state, "release", lease)
            .await
            .unwrap();
        l.import(&["--current"]).await.unwrap();
        assert_eq!(l.current().await.as_deref(), Some("v2"));
    }

    /// A rollback (cutover step 6): once `current.json` names the rollback
    /// version again, `--current` on it moves `ops/current` back, from the
    /// run already in the state. Rolling forward moves it on again.
    #[tokio::test]
    async fn current_follows_a_rollback_and_a_roll_forward() {
        let l = lake("v2").await;
        l.import(&["--current"]).await.unwrap();
        assert_eq!(l.current().await.as_deref(), Some("v2"));
        // Not before the pointer is restored.
        let e = l.import_version("v1", &["--current"]).await.unwrap_err();
        assert!(
            format!("{e:#}").contains("current.json names `v2`, not `v1`"),
            "{e:#}"
        );
        assert_eq!(l.current().await.as_deref(), Some("v2"));
        l.promote("v1").await;
        l.import_version("v1", &["--current"]).await.unwrap();
        assert_eq!(l.current().await.as_deref(), Some("v1"));
        l.promote("v2").await;
        l.import(&["--current"]).await.unwrap();
        assert_eq!(l.current().await.as_deref(), Some("v2"));
    }
}
