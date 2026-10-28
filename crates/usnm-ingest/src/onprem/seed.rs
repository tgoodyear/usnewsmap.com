//! Preparing local state and reference inputs for a staged release.

use anyhow::{bail, ensure, Context};
use chrono::Utc;
use serde::Serialize;
use usnm_state::state::{Batch, BatchStatus, IndexRun, RunStatus, State};
use usnm_store::ObjectStore;

use super::publish::content_type;
use super::RUN_FILE;
use crate::cluster::sample::run_batches;

/// Days from today whose version names `seed_state` reserves.
const RESERVE_DAYS: i64 = 3;
/// Which of a version's batches to seed: every `every`th by name, from the
/// `offset`th (0-based), as docs/operations/environments-and-deploys.md's 1% set takes them
/// (`every` 100, `offset` 39).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Every {
    pub every: usize,
    pub offset: usize,
}

/// What [`seed_state`] did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Seeded {
    pub version: String,
    pub listed: usize,
    pub seeded: usize,
    /// Batches the state file had already.
    pub kept: usize,
    pub pages: u64,
    pub ok_pages: u64,
}

/// Give `state` a curated batch record for each batch of `version`'s batch
/// list (`{version}/batches.json` in `reference`), or for every `every`th
/// one. A batch the state already has is left as it is.
pub async fn seed_state(
    state: &State,
    reference: &dyn ObjectStore,
    version: &str,
    every: Option<Every>,
) -> anyhow::Result<Seeded> {
    let mut batches = run_batches(reference, version).await?;
    batches.sort_by(|a, b| a.batch.cmp(&b.batch));
    let listed = batches.len();
    if let Some(Every { every, offset }) = every {
        ensure!(every > 0, "--every must be at least 1");
        batches = batches
            .into_iter()
            .enumerate()
            .filter(|(i, _)| *i >= offset && (i - offset) % every == 0)
            .map(|(_, b)| b)
            .collect();
    }
    let now = Utc::now();
    let mut out = Seeded {
        version: version.to_owned(),
        listed,
        seeded: 0,
        kept: 0,
        pages: 0,
        ok_pages: 0,
    };
    for b in batches {
        out.pages += b.curated.pages;
        out.ok_pages += b.curated.ok_pages;
        let record = Batch {
            id: b.batch.clone(),
            batch: b.batch.clone(),
            version: b.curated.version,
            // The release reads only `curated`; the source is for curation,
            // which a seeded state never runs.
            source_url: format!("seeded:{version}"),
            source_sha256: Some(b.curated.source_sha256.clone()),
            ocr_source: String::new(),
            status: BatchStatus::Curated,
            attempts: 0,
            lease: None,
            curated: Some(b.curated),
            last_error: None,
            updated_at: now,
            curated_at: None,
        };
        if state.create_batch(&record).await? {
            out.seeded += 1;
        } else {
            out.kept += 1;
        }
    }
    Ok(out)
}

/// The run record a staged on-prem release left in `version`'s snapshot,
/// recorded in `state`: what `import-staged-run` does in Azure before a
/// promotion (docs/operations/rebuild-and-index-storage.md, "Full rebuild on-prem"), and a
/// Cosmos-backed release or prune when `current.json` names a version built
/// on-prem. `None` when the snapshot has no such record and `state` has no
/// run for `version`.
///
/// A run `state` already has for the name is kept when it is the same
/// version: published, with the same new index and batch list (a later
/// repair may have changed its times). One that failed before writing a
/// snapshot (no batch list) is an earlier attempt that never got far, and
/// is replaced, conditionally on its ETag. Any other is refused: two
/// versions under one name.
pub async fn import_staged_run(
    state: &State,
    reference: &dyn ObjectStore,
    version: &str,
) -> anyhow::Result<Option<(IndexRun, String)>> {
    let existing = state.run(version).await?;
    let Some(bytes) = reference.get(&format!("{version}/{RUN_FILE}")).await? else {
        return Ok(existing);
    };
    let staged: IndexRun = serde_json::from_slice(&bytes).context(RUN_FILE)?;
    ensure!(
        staged.index_version == version && staged.id == version,
        "{version}/{RUN_FILE} is the record of {}",
        staged.index_version
    );
    ensure!(
        staged.status == RunStatus::Published,
        "{version}/{RUN_FILE} records a run that wasn't published"
    );
    match existing {
        None => {
            if state.create_run(&staged).await? {
                tracing::warn!(version, "recorded the run of a version built on-prem");
            }
        }
        Some((run, _))
            if run.status == RunStatus::Published
                && run.new_index == staged.new_index
                && run.batch_list == staged.batch_list =>
        {
            return state.run(version).await;
        }
        Some((run, etag))
            if run.status == RunStatus::Failed
                && run.batch_list.is_none()
                && run.batches.is_none() =>
        {
            state.update_run(&staged, &etag).await?;
            tracing::warn!(
                version,
                replaced_error = run.last_error.as_deref().unwrap_or(""),
                "replaced a failed attempt's run with the version built on-prem"
            );
        }
        Some((run, _)) => bail!(
            "the state has another run named {version} ({:?}, new index `{}`) than the one built \
             on-prem (new index `{}`): two versions under one name; promote neither",
            run.status,
            run.new_index,
            staged.new_index
        ),
    }
    state.run(version).await
}

/// Record `run`'s version, once promoted, as the pipeline state's current
/// one (`ops/current`): `import-staged-run --current`, after the operator
/// copied the pending pointer to `current.json`
/// (docs/operations/rebuild-and-index-storage.md, "Index storage cutover").
/// A staged release leaves `ops/current` as it was, and `/v1/status` and
/// the BOINC export read it. Refuses unless `current.json` names the
/// version and its run is published, as a release's repair does
/// ([`crate::release::record_current_version`]); an interrupted publish is
/// left to the release, which marks it published from the pointer.
pub async fn record_current(
    state: &State,
    reference: &dyn ObjectStore,
    run: &IndexRun,
) -> anyhow::Result<crate::release::CurrentVersion> {
    let (live, _) = crate::release::live_pointer(reference)
        .await?
        .context("there is no current.json: promote the version first")?;
    crate::release::record_current_version(state, &live, run).await
}

/// Record a failed placeholder run for every version name the lake's
/// `reference` already has for today and the next days, so a release on
/// this state (`Release::next_names`, which reads only the state) picks a
/// name nothing else uses. Returns the names reserved.
pub async fn reserve_names(
    state: &State,
    lake_reference: &dyn ObjectStore,
    today: chrono::NaiveDate,
) -> anyhow::Result<Vec<String>> {
    let mut reserved = Vec::new();
    for d in 0..RESERVE_DAYS {
        let stamp = (today + chrono::Duration::days(d)).format("%Y%m%d");
        for n in 1..1000 {
            let version = format!("pages-v{stamp}-{n}");
            let used = lake_reference
                .exists(&format!("{version}/manifest.json"))
                .await?
                || lake_reference
                    .exists(&format!("{version}/batches.json"))
                    .await?;
            if !used {
                break;
            }
            if state.run(&version).await?.is_none() {
                let now = Utc::now();
                state
                    .create_run(&IndexRun {
                        id: version.clone(),
                        index_version: version.clone(),
                        full: false,
                        indexes: Vec::new(),
                        new_index: String::new(),
                        batch_count: None,
                        batch_list: None,
                        batches: None,
                        status: RunStatus::Failed,
                        docs: 0,
                        pages: 0,
                        duplicate_pages: 0,
                        started_at: now,
                        published_at: None,
                        previous_version: None,
                        last_error: Some("reserved: the lake's reference has this version".into()),
                        failed_at: Some(now),
                        build: None,
                    })
                    .await?;
            }
            reserved.push(version);
        }
    }
    Ok(reserved)
}

/// What [`mirror`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Mirrored {
    pub copied: usize,
    pub same: usize,
    pub bytes: u64,
}

/// Copy the objects under each of `prefixes`, and each of `files`, from
/// `from` to `to`: the on-prem release's local mirror of the lake's
/// `reference`. One already in `to` with the same bytes is left; one with
/// other bytes stops the copy (mirror into an empty directory).
pub async fn mirror(
    from: &dyn ObjectStore,
    to: &dyn ObjectStore,
    prefixes: &[String],
    files: &[String],
) -> anyhow::Result<Mirrored> {
    let mut paths: Vec<String> = files.to_vec();
    for p in prefixes {
        let listed = from.list(p).await?;
        ensure!(!listed.is_empty(), "nothing under {p}/");
        paths.extend(listed);
    }
    let mut out = Mirrored::default();
    for path in paths {
        let body = from
            .get(&path)
            .await?
            .with_context(|| format!("{path} isn't there"))?;
        match to.get(&path).await? {
            Some(there) if there == body => out.same += 1,
            Some(_) => bail!("{path} is there already with other bytes"),
            None => {
                out.bytes += body.len() as u64;
                to.put_new(&path, body, content_type(&path)).await?;
                out.copied += 1;
            }
        }
    }
    tracing::info!(mirrored = ?out, "store mirrored");
    Ok(out)
}
