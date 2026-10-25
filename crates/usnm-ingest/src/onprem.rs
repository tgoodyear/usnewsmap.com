//! A full release built on an on-prem host (#283, #917): the Quickwit writer
//! works on the host's own disk (`file://` metastore and index root), the
//! reference store is a local mirror of the lake's `reference`, and pipeline
//! state is a local file. Nothing in Azure changes while it builds. Two steps
//! go with it:
//!
//! - [`seed_state`] writes the state file's batch records from a published
//!   version's batch list, since the state file has none of Cosmos's.
//! - [`publish_staged`] then copies what the staged release wrote into Azure:
//!   each index of `index-storage-pending.json` into the index account (its
//!   published splits, then its `metastore.json` pointed at the account, then
//!   the account's metastore manifest), the version's reference snapshot into
//!   the lake, and the pending pointer last. A rerun skips what is already
//!   there with the same size, so an interrupted copy continues.
//!
//! The copy rides out a flaky uplink: each request is tried again after a
//! failure that may pass (a dropped or hung connection, a 408, 429 or 5xx),
//! a split whose blocks ran out of tries is sent again after a wait, and a
//! split sent again (in this run or a rerun) sends only the blocks that
//! weren't staged already. A refusal (403, a different object of the same
//! name) stops it at once.
//!
//! Promotion stays the operator step of docs/operations/rebuild-and-index-storage.md, "Index storage
//! cutover".
//!
//! A restore (`Staged::restore`, #1061) is the same copy for a version the
//! lake already serves, which is the state after serving moves on-prem
//! (ADR-0027, Phase 4): `current.json` names the release's own version, not
//! its rollback, so the plain publish refuses it. The restore refuses unless
//! the lake serves exactly this release's version and indexes, copies what
//! the account and the lake lack, and writes neither the pending pointer nor
//! `current.json`.

use std::collections::BTreeSet;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use bytes::Bytes;
use chrono::Utc;
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use usnm_state::state::{Batch, BatchStatus, IndexRun, RunStatus, State};
use usnm_store::retry::{retryable_write, retrying, Retry};
use usnm_store::{ObjectStore, StoreError};

use crate::cluster::local::{rewrite_metastore, METASTORE_FILE};
use crate::cluster::sample::run_batches;

/// The staged pointer a release with `--stage-index-storage` writes.
pub const PENDING_POINTER: &str = "index-storage-pending.json";
/// The file-backed metastore's list of indexes, at the metastore's root.
pub const METASTORE_MANIFEST: &str = "manifest.json";
/// The staged run's record in its version's snapshot (`{version}/index_run.json`),
/// for [`import_staged_run`]: the host's state is a local file, so this is how
/// its run reaches Cosmos (the release script's publish step, or the ingest job).
pub const RUN_FILE: &str = "index_run.json";
/// Days from today whose version names `seed_state` reserves.
const RESERVE_DAYS: i64 = 3;
/// Objects copied at once.
const COPIES: usize = 4;
/// Bytes per chunk read from a local split.
const CHUNK: usize = 8 * 1024 * 1024;
/// Tries of each request to Azure other than a split's blocks and commit
/// (the store tries those): listings, metastores, the manifest, the
/// snapshot and the pointer.
const RETRY: Retry = if cfg!(test) {
    Retry {
        tries: 3,
        first_wait: Duration::from_millis(1),
        max_wait: Duration::from_millis(1),
    }
} else {
    Retry::STANDARD
};
/// Tries of one split's whole upload, [`SPLIT_WAIT`] apart, once its
/// blocks or commit ran out of tries. A later try sends only what the
/// earlier ones didn't stage, so an outage longer than a block's tries costs
/// a wait, not the split.
const SPLIT_TRIES: u32 = 4;
const SPLIT_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_secs(300)
};
/// How often an index's copy logs how far it is.
const PROGRESS_EVERY: Duration = Duration::from_secs(60);

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

/// Where [`publish_staged`] copies from and to.
pub struct Staged<'a> {
    /// The release's local pipeline state (its run record).
    pub state: &'a State,
    /// The release's local reference store (the mirror it wrote to).
    pub local_reference: &'a dyn ObjectStore,
    /// The writer's local metastore and index root (the directory of its
    /// `file://` URI).
    pub local_index_root: PathBuf,
    /// The lake's `reference` container.
    pub reference: &'a dyn ObjectStore,
    /// The index account's metastore and index root (`qw-index`).
    pub index: &'a dyn ObjectStore,
    /// The index root as the searchers name it (`azure://qw-index`).
    pub index_root_uri: String,
    /// Check and list only; copy nothing.
    pub dry_run: bool,
    /// Copy the version the lake already serves, instead of a release the
    /// lake hasn't promoted: the lake's `current.json` must name this
    /// release's own version and indexes, and the pending pointer isn't
    /// written. For serving from Azure again after it was retired.
    pub restore: bool,
}

/// What [`publish_staged`] did.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Published {
    pub version: String,
    pub indexes: Vec<IndexCopy>,
    pub reference_files: usize,
    pub reference_skipped: usize,
    pub pointer_written: bool,
    pub secs: f64,
}

/// One index's copy.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct IndexCopy {
    pub index: String,
    pub splits: usize,
    pub docs: u64,
    pub skipped: usize,
    pub bytes: u64,
}

/// Every index id a pointer names: its `indexes`, and those of the blocks
/// that carry their own (`ja`, `congress`).
pub fn pointer_indexes(pointer: &Value) -> anyhow::Result<Vec<String>> {
    let mut out = Vec::new();
    let mut add = |v: &Value, at: &str| -> anyhow::Result<()> {
        let Some(list) = v.get("indexes") else {
            return Ok(());
        };
        for id in list
            .as_array()
            .with_context(|| format!("{at}.indexes isn't a list"))?
        {
            let id = id
                .as_str()
                .with_context(|| format!("{at}.indexes holds a non-string"))?;
            if !usnm_store::is_safe_segment(id) {
                bail!("`{id}` isn't an index id");
            }
            out.push(id.to_owned());
        }
        Ok(())
    };
    add(pointer, "pointer")?;
    for block in ["ja", "congress"] {
        if let Some(b) = pointer.get(block).filter(|b| b.is_object()) {
            add(b, block)?;
        }
    }
    let unique: BTreeSet<&String> = out.iter().collect();
    ensure!(
        unique.len() == out.len(),
        "the pointer names an index twice"
    );
    ensure!(!out.is_empty(), "the pointer names no index");
    Ok(out)
}

/// One split of a file-backed `metastore.json`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    pub id: String,
    pub state: String,
    pub docs: u64,
}

/// The splits a file-backed `metastore.json` lists, with their states.
/// Quickwit 0.9 keeps them as a list of `{split_state, split_metadata}`.
pub fn metastore_splits(meta: &Value) -> anyhow::Result<Vec<Split>> {
    let splits = meta
        .get("splits")
        .and_then(Value::as_array)
        .context("metastore.json has no splits list")?;
    splits
        .iter()
        .map(|s| {
            let m = s.get("split_metadata").unwrap_or(s);
            let id = m["split_id"]
                .as_str()
                .context("a split without split_id")?
                .to_owned();
            if !usnm_store::is_safe_segment(&id) {
                bail!("`{id}` isn't a split id");
            }
            Ok(Split {
                state: s["split_state"]
                    .as_str()
                    .with_context(|| format!("split {id} has no split_state"))?
                    .to_owned(),
                docs: m["num_docs"].as_u64().unwrap_or(0),
                id,
            })
        })
        .collect()
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

/// Copy a staged release from the host to Azure (module docs). Refuses
/// unless the lake still serves the version the release took as its
/// rollback, every index's splits are all published, and nothing in Azure
/// with the same name differs. A restore (`s.restore`) instead needs the
/// lake to serve the release's own version and indexes, and leaves the
/// lake's pointers as they are.
pub async fn publish_staged(s: &Staged<'_>) -> anyhow::Result<Published> {
    let started = Instant::now();
    let pointer_bytes = s
        .local_reference
        .get(PENDING_POINTER)
        .await?
        .context("the local reference has no index-storage-pending.json: did the release run with --stage-index-storage and finish?")?;
    let pointer: Value = serde_json::from_slice(&pointer_bytes).context(PENDING_POINTER)?;
    let version = pointer["index_version"]
        .as_str()
        .filter(|v| usnm_store::is_safe_segment(v))
        .context("the pending pointer has no usable index_version")?
        .to_owned();
    let ids = pointer_indexes(&pointer)?;
    // The release took the lake's serving version as its rollback; one
    // published since then would be lost from the chain.
    let serving: Value = serde_json::from_slice(
        &s.reference
            .get("current.json")
            .await?
            .context("the lake has no current.json")?,
    )
    .context("current.json")?;
    if s.restore {
        ensure_restorable(&serving, &pointer, &version, &ids)?;
        tracing::info!(%version, indexes = ?ids, dry_run = s.dry_run, "restoring the version the lake serves");
    } else {
        ensure!(
            serving["index_version"] == pointer["previous_version"],
            "the lake serves {}, but the release's rollback is {}: mirror reference again and rebuild",
            serving["index_version"],
            pointer["previous_version"]
        );
        tracing::info!(%version, indexes = ?ids, dry_run = s.dry_run, "publishing a staged release");
    }

    let local_index = usnm_store::LocalStore::new(&s.local_index_root);
    let mut out = Published {
        version: version.clone(),
        ..Published::default()
    };
    // The release writes its snapshot's manifest last: without it the
    // snapshot is incomplete, and the API couldn't load the version.
    ensure!(
        s.local_reference
            .exists(&format!("{version}/manifest.json"))
            .await?,
        "the local reference has no {version}/manifest.json: the release didn't finish its snapshot"
    );
    // Check every index before copying any, so a bad one stops the copy
    // with nothing written.
    let mut plans = Vec::new();
    for id in &ids {
        plans.push(plan_index(&local_index, s, id).await?);
    }
    for (id, meta, splits) in plans {
        out.indexes
            .push(copy_index(&local_index, s, &id, &meta, &splits).await?);
    }
    if !s.dry_run {
        add_to_manifest(s.index, &ids).await?;
    }

    // The run's record goes with the snapshot, for the Cosmos-backed
    // release (or prune) that next finds the version serving.
    let (run, _) = s
        .state
        .run(&version)
        .await?
        .with_context(|| format!("the local state has no run for {version}"))?;
    ensure!(
        run.status == RunStatus::Published,
        "the local run for {version} is {:?}, not published",
        run.status
    );
    s.local_reference
        .put(
            &format!("{version}/{RUN_FILE}"),
            serde_json::to_vec_pretty(&run)?,
            "application/json",
        )
        .await?;

    // The reference snapshot: create-only, as the release writes it.
    let prefix = format!("{version}/");
    for path in s
        .local_reference
        .list(&version)
        .await?
        .into_iter()
        .filter(|p| p.starts_with(&prefix))
    {
        let body = s
            .local_reference
            .get(&path)
            .await?
            .with_context(|| format!("{path} went away"))?;
        match s.reference.get(&path).await? {
            Some(there) if there == body => out.reference_skipped += 1,
            Some(_) => bail!("the lake's reference has a different {path}"),
            None if s.dry_run => out.reference_files += 1,
            None => {
                create_same(s.reference, &path, body, "the lake's reference").await?;
                out.reference_files += 1;
            }
        }
    }
    // The API loads a version from its manifest: no manifest in the lake,
    // no pointer.
    let manifest = format!("{version}/manifest.json");
    ensure!(
        s.dry_run || again("head", &manifest, |_| s.reference.exists(&manifest)).await?,
        "the lake's reference has no {manifest} after the copy"
    );
    // A restore leaves the lake's pointers alone: `current.json` already
    // names the version, and the pending pointer is not what serves.
    if !s.dry_run && !s.restore {
        // Replacing the pointer with the same bytes again is harmless.
        again("put", PENDING_POINTER, |_| {
            s.reference
                .put(PENDING_POINTER, pointer_bytes.clone(), "application/json")
        })
        .await?;
        out.pointer_written = true;
    }
    out.secs = started.elapsed().as_secs_f64();
    if s.restore {
        tracing::info!(published = ?out, "serving version restored");
    } else {
        tracing::info!(published = ?out, "staged release copied");
    }
    Ok(out)
}

/// A restore's counterpart of the rollback check: the lake's `current.json`
/// must name this release's own `version` and the indexes the release holds
/// (`ids`), so the copy brings back exactly what the lake says serves.
fn ensure_restorable(
    serving: &Value,
    pointer: &Value,
    version: &str,
    ids: &[String],
) -> anyhow::Result<()> {
    let name = serving["index_version"].as_str().unwrap_or("no version");
    if name != version {
        if serving["index_version"] == pointer["previous_version"] {
            bail!(
                "the lake serves {name}, this release's rollback, not {version}: a restore copies the version the lake serves; publish the release without --restore to copy {version} to Azure"
            );
        }
        bail!(
            "the lake serves {name}, not {version}: a restore copies only the version the lake serves, from that version's own run directory"
        );
    }
    let mut served = pointer_indexes(serving).context("current.json")?;
    let mut held = ids.to_vec();
    served.sort();
    held.sort();
    ensure!(
        served == held,
        "the lake's current.json names {served:?} for {version}, but this release holds {held:?}: another run directory holds the indexes it serves"
    );
    Ok(())
}

/// An index's local metastore, checked, and its published splits.
async fn plan_index(
    local: &usnm_store::LocalStore,
    s: &Staged<'_>,
    id: &str,
) -> anyhow::Result<(String, Vec<u8>, Vec<Split>)> {
    let meta_path = format!("{id}/{METASTORE_FILE}");
    let meta = local
        .get(&meta_path)
        .await?
        .with_context(|| format!("no {meta_path} under {}", s.local_index_root.display()))?;
    let parsed: Value = serde_json::from_slice(&meta).context(meta_path.clone())?;
    let splits = metastore_splits(&parsed)?;
    let staged: Vec<&str> = splits
        .iter()
        .filter(|sp| sp.state == "Staged")
        .map(|sp| sp.id.as_str())
        .collect();
    ensure!(
        staged.is_empty(),
        "{id} has staged splits {staged:?}: the writer didn't finish"
    );
    let published: Vec<Split> = splits
        .into_iter()
        .filter(|sp| sp.state == "Published")
        .collect();
    ensure!(!published.is_empty(), "{id} has no published split");
    for sp in &published {
        let path = s.local_index_root.join(id).join(format!("{}.split", sp.id));
        ensure!(
            tokio::fs::try_exists(&path).await?,
            "{} is published in {id}'s metastore but not on disk",
            path.display()
        );
    }
    if let Some(there) = s.index.get(&meta_path).await? {
        let want = rewrite_metastore(&meta, &format!("{}/{id}", s.index_root_uri))?;
        ensure!(
            there == want,
            "the index account has a different {meta_path}: another build used the name"
        );
    }
    Ok((id.to_owned(), meta, published))
}

/// Copy one index's published splits, then its metastore.
async fn copy_index(
    local: &usnm_store::LocalStore,
    s: &Staged<'_>,
    id: &str,
    meta: &[u8],
    splits: &[Split],
) -> anyhow::Result<IndexCopy> {
    let there: std::collections::HashMap<String, u64> =
        again("list", id, |_| s.index.list_info(id))
            .await?
            .into_iter()
            .map(|o| (o.path, o.size))
            .collect();
    let mut copy = IndexCopy {
        index: id.to_owned(),
        splits: splits.len(),
        docs: splits.iter().map(|sp| sp.docs).sum(),
        ..IndexCopy::default()
    };
    let jobs: Vec<(String, PathBuf, u64)> = {
        let mut jobs = Vec::new();
        for sp in splits {
            let path = format!("{id}/{}.split", sp.id);
            let file = local.root().join(&path);
            let size = tokio::fs::metadata(&file).await?.len();
            match there.get(&path) {
                Some(&n) if n == size => copy.skipped += 1,
                Some(&n) => bail!("the index account's {path} has {n} bytes, not {size}"),
                None => jobs.push((path, file, size)),
            }
        }
        jobs
    };
    let to_copy: u64 = jobs.iter().map(|(_, _, n)| n).sum();
    tracing::info!(
        index = id,
        splits = copy.splits,
        to_copy = jobs.len(),
        gb = gb(to_copy),
        skipped = copy.skipped,
        "copying an index"
    );
    if !s.dry_run {
        let index = s.index;
        let progress = Progress {
            index: id.to_owned(),
            splits: jobs.len(),
            bytes: to_copy,
            copied: AtomicUsize::new(0),
            sent: AtomicU64::new(0),
        };
        let p = &progress;
        let copies = stream::iter(jobs)
            .map(|(path, file, size)| async move {
                copy_split(index, &path, &file, size, p).await?;
                Ok::<u64, anyhow::Error>(size)
            })
            .buffer_unordered(COPIES)
            .try_collect::<Vec<u64>>();
        let sizes = progress.logging(copies).await?;
        copy.bytes = sizes.iter().sum();
        let meta_path = format!("{id}/{METASTORE_FILE}");
        let rewritten = rewrite_metastore(meta, &format!("{}/{id}", s.index_root_uri))?;
        create_same(s.index, &meta_path, rewritten, "the index account").await?;
    } else {
        copy.bytes = jobs.iter().map(|(_, _, n)| n).sum();
    }
    Ok(copy)
}

/// How far one index's copy is, for the log.
struct Progress {
    index: String,
    splits: usize,
    bytes: u64,
    copied: AtomicUsize,
    /// Bytes read into uploads: the splits copied, and how far the ones in
    /// flight are.
    sent: AtomicU64,
}

impl Progress {
    /// Run `copy`, logging every [`PROGRESS_EVERY`] how far it is, so a
    /// copy that is moving can be told from one that stalled.
    async fn logging<T>(&self, copy: impl Future<Output = T>) -> T {
        let started = Instant::now();
        let ticker = async {
            let mut tick = tokio::time::interval(PROGRESS_EVERY);
            tick.tick().await;
            let mut last = 0u64;
            loop {
                tick.tick().await;
                let sent = self.sent.load(Ordering::Relaxed);
                tracing::info!(
                    index = %self.index,
                    splits_copied = self.copied.load(Ordering::Relaxed),
                    splits = self.splits,
                    gb_sent = gb(sent),
                    gb = gb(self.bytes),
                    mb_per_s = mb_per_s(sent.saturating_sub(last), PROGRESS_EVERY.as_secs_f64()),
                    mins = started.elapsed().as_secs() / 60,
                    "index copy progress"
                );
                last = sent;
            }
        };
        tokio::select! {
            r = copy => r,
            () = ticker => unreachable!("the ticker never ends"),
        }
    }
}

fn gb(bytes: u64) -> f64 {
    (bytes as f64 / 1e8).round() / 10.0
}

fn mb_per_s(bytes: u64, secs: f64) -> f64 {
    (bytes as f64 / 1e5 / secs.max(0.001)).round() / 10.0
}

/// Copy one split, sending it again after [`SPLIT_WAIT`] (as many as
/// [`SPLIT_TRIES`] times) while it fails in a way that may pass.
async fn copy_split(
    index: &dyn ObjectStore,
    path: &str,
    file: &Path,
    size: u64,
    progress: &Progress,
) -> anyhow::Result<()> {
    let started = Instant::now();
    tracing::info!(path, mb = size / 1_000_000, "copying a split");
    let mut round = 1;
    loop {
        match upload_file(index, path, file, &progress.sent).await {
            Ok(()) => break,
            Err(e)
                if round < SPLIT_TRIES
                    && e.downcast_ref::<StoreError>().is_some_and(retryable_write) =>
            {
                tracing::warn!(
                    path,
                    round,
                    rounds = SPLIT_TRIES,
                    wait_secs = SPLIT_WAIT.as_secs(),
                    error = %format!("{e:#}"),
                    "split upload failed; sending it again, keeping the blocks it staged"
                );
                tokio::time::sleep(SPLIT_WAIT).await;
                round += 1;
            }
            Err(e) => return Err(e),
        }
    }
    let secs = started.elapsed().as_secs_f64();
    tracing::info!(
        index = %progress.index,
        path,
        split = progress.copied.fetch_add(1, Ordering::Relaxed) + 1,
        of = progress.splits,
        mb = size / 1_000_000,
        secs = secs.round(),
        mb_per_s = mb_per_s(size, secs),
        "split copied"
    );
    Ok(())
}

/// Stream a local file into `store` at `path` (create-only), adding the
/// bytes read to `sent` (and taking them off again if it fails).
async fn upload_file(
    store: &dyn ObjectStore,
    path: &str,
    file: &Path,
    sent: &AtomicU64,
) -> anyhow::Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(2);
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    let read_here = AtomicU64::new(0);
    let read = &read_here;
    let reader = async move {
        let mut f = tokio::fs::File::open(file)
            .await
            .with_context(|| format!("opening {}", file.display()))?;
        loop {
            let mut buf = vec![0u8; CHUNK];
            let mut filled = 0;
            while filled < CHUNK {
                let n = f
                    .read(&mut buf[filled..])
                    .await
                    .with_context(|| format!("reading {}", file.display()))?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            buf.truncate(filled);
            read.fetch_add(filled as u64, Ordering::Relaxed);
            sent.fetch_add(filled as u64, Ordering::Relaxed);
            if tx.send(Bytes::from(buf)).await.is_err() {
                // The upload stopped taking chunks: its error says why.
                return Ok(());
            }
        }
        drop(tx);
        let _ = commit_tx.send(true);
        Ok::<(), anyhow::Error>(())
    };
    let (read_result, put) = tokio::join!(
        reader,
        store.put_stream(path, "application/octet-stream", None, rx, commit_rx)
    );
    let result = match (read_result, put) {
        // A local read failed, and the upload was abandoned for it.
        (Err(e), _) => Err(e),
        (Ok(()), Ok(_)) => Ok(()),
        (Ok(()), Err(StoreError::AlreadyExists(p))) => Err(anyhow::anyhow!(
            "{p} appeared in the index account meanwhile"
        )),
        (Ok(()), Err(e)) => Err(e.into()),
    };
    if result.is_err() {
        sent.fetch_sub(read.load(Ordering::Relaxed), Ordering::Relaxed);
    }
    result
}

/// `f`, tried again as [`RETRY`] says after a failure that may pass.
async fn again<T, F, Fut>(op: &str, path: &str, f: F) -> Result<T, StoreError>
where
    F: FnMut(u32) -> Fut,
    Fut: Future<Output = Result<T, StoreError>>,
{
    retrying(op, path, RETRY, retryable_write, f).await
}

/// Create `path` in `store` with `body`, create-only, tried again after a
/// failure that may pass. An object already there is accepted only with
/// these bytes: what an earlier try wrote when only its answer was lost,
/// or what an earlier run copied. Anything else there stops the publish.
async fn create_same(
    store: &dyn ObjectStore,
    path: &str,
    body: Vec<u8>,
    place: &str,
) -> anyhow::Result<()> {
    let kind = content_type(path);
    if !again("put", path, |_| store.put_new(path, body.clone(), kind)).await? {
        let there = store.get(path).await?;
        ensure!(
            there.as_deref() == Some(body.as_slice()),
            "{path} appeared in {place} meanwhile"
        );
    }
    Ok(())
}

/// Add `ids` to the index account's metastore manifest as active indexes,
/// keeping every index and template it lists.
async fn add_to_manifest(index: &dyn ObjectStore, ids: &[String]) -> anyhow::Result<()> {
    let existing = index.get(METASTORE_MANIFEST).await?;
    let absent = existing.is_none();
    let mut m: Value = match existing {
        Some(b) => serde_json::from_slice(&b).context("the index account's manifest.json")?,
        None => serde_json::json!({"version": "0.9", "indexes": {}, "templates": []}),
    };
    let list = m
        .get_mut("indexes")
        .and_then(Value::as_object_mut)
        .context("the index account's manifest.json has no indexes map")?;
    let mut added = 0;
    for id in ids {
        match list.get(id).and_then(Value::as_str) {
            Some("active") => {}
            Some(other) => bail!("the index account's manifest lists {id} as {other}"),
            None => {
                list.insert(id.clone(), Value::String("active".into()));
                added += 1;
            }
        }
    }
    if added > 0 || absent {
        // Replacing it with the same bytes again is harmless.
        let body = serde_json::to_vec_pretty(&m)?;
        again("put", METASTORE_MANIFEST, |_| {
            index.put(METASTORE_MANIFEST, body.clone(), "application/json")
        })
        .await?;
    }
    tracing::info!(added, "index account's metastore manifest");
    Ok(())
}

fn content_type(path: &str) -> &'static str {
    if path.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cluster::local::manifest;
    use serde_json::json;
    use std::sync::Arc;
    use usnm_state::docs::MemoryDocs;
    use usnm_store::LocalStore;

    fn meta(index_uri: &str, splits: &[(&str, &str, u64)]) -> Vec<u8> {
        serde_json::to_vec(&json!({
            "version": "0.9",
            "index": {"index_config": {"index_id": "i", "index_uri": index_uri}},
            "splits": splits.iter().map(|(id, st, n)| json!({
                "split_state": st,
                "split_metadata": {"split_id": id, "num_docs": n},
            })).collect::<Vec<_>>(),
        }))
        .unwrap()
    }

    #[test]
    fn a_pointer_names_its_main_ja_and_congress_indexes() {
        let p = json!({
            "indexes": ["base", "delta"],
            "ja": {"indexes": ["ja1"], "fold": 2},
            "congress": {"indexes": ["c1"]},
        });
        assert_eq!(pointer_indexes(&p).unwrap(), ["base", "delta", "ja1", "c1"]);
        assert!(pointer_indexes(&json!({"indexes": ["a", "a"]})).is_err());
        assert!(pointer_indexes(&json!({"indexes": ["../x"]})).is_err());
        assert!(pointer_indexes(&json!({})).is_err());
    }

    #[test]
    fn splits_are_read_with_their_states() {
        let m: Value = serde_json::from_slice(&meta(
            "file:///x/i",
            &[("s1", "Published", 3), ("s0", "MarkedForDeletion", 2)],
        ))
        .unwrap();
        let s = metastore_splits(&m).unwrap();
        assert_eq!(s.len(), 2);
        assert_eq!(
            s[0],
            Split {
                id: "s1".into(),
                state: "Published".into(),
                docs: 3
            }
        );
    }

    async fn seeded_reference(dir: &Path) -> LocalStore {
        let r = LocalStore::new(dir);
        let batches: Vec<Value> = (0..250)
            .map(|i| {
                json!({"batch": format!("b{i:03}_ver01"), "curated": {
                    "version": 1, "parts": [], "counts": "", "pages": 10, "ok_pages": 9,
                    "lccns": [], "source_sha256": "ab", "first": "", "last": ""}})
            })
            .collect();
        r.put(
            "v1/batches.json",
            serde_json::to_vec(&batches).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
        r
    }

    #[tokio::test]
    async fn the_state_gets_every_hundredth_batch_from_the_fortieth() {
        let tmp = tempfile::tempdir().unwrap();
        let reference = seeded_reference(tmp.path()).await;
        let state = State::new(Arc::new(MemoryDocs::default()));
        let every = Every {
            every: 100,
            offset: 39,
        };
        let s = seed_state(&state, &reference, "v1", Some(every))
            .await
            .unwrap();
        assert_eq!((s.listed, s.seeded, s.pages, s.ok_pages), (250, 3, 30, 27));
        let names: Vec<String> = state
            .batches(&[BatchStatus::Curated])
            .await
            .unwrap()
            .into_iter()
            .map(|(b, _)| b.batch)
            .collect();
        assert_eq!(names, ["b039_ver01", "b139_ver01", "b239_ver01"]);
        // Again: nothing new.
        let again = seed_state(&state, &reference, "v1", Some(every))
            .await
            .unwrap();
        assert_eq!((again.seeded, again.kept), (0, 3));
    }

    struct Fixture {
        _tmp: tempfile::TempDir,
        state: State,
        local_reference: LocalStore,
        index_root: PathBuf,
        reference: LocalStore,
        index: LocalStore,
    }

    async fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let local_reference = LocalStore::new(tmp.path().join("local-ref"));
        let index_root = tmp.path().join("qw");
        let reference = LocalStore::new(tmp.path().join("lake-ref"));
        let index = LocalStore::new(tmp.path().join("account"));
        let pointer = json!({
            "index_version": "v2", "previous_version": "v1",
            "indexes": ["base2"], "ja": {"indexes": ["ja2"]},
        });
        local_reference
            .put(
                PENDING_POINTER,
                serde_json::to_vec(&pointer).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        local_reference
            .put("v2/manifest.json", b"{}".to_vec(), "application/json")
            .await
            .unwrap();
        reference
            .put(
                "current.json",
                br#"{"index_version":"v1"}"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        index
            .put(METASTORE_MANIFEST, manifest("base1"), "application/json")
            .await
            .unwrap();
        for (id, split) in [("base2", "sA"), ("ja2", "sB")] {
            let dir = index_root.join(id);
            std::fs::create_dir_all(&dir).unwrap();
            let uri = format!("file://{}/{id}", index_root.display());
            std::fs::write(
                dir.join(METASTORE_FILE),
                meta(
                    &uri,
                    &[(split, "Published", 5), ("gone", "MarkedForDeletion", 5)],
                ),
            )
            .unwrap();
            std::fs::write(dir.join(format!("{split}.split")), vec![7u8; 20_000_000]).unwrap();
        }
        let state = State::new(Arc::new(MemoryDocs::default()));
        state.create_run(&published_run("v2")).await.unwrap();
        Fixture {
            _tmp: tmp,
            state,
            local_reference,
            index_root,
            reference,
            index,
        }
    }

    fn published_run(v: &str) -> IndexRun {
        IndexRun {
            id: v.into(),
            index_version: v.into(),
            full: true,
            indexes: vec!["base2".into()],
            new_index: "base2".into(),
            batch_count: Some(1),
            batch_list: Some(format!("{v}/batches.json")),
            batches: None,
            status: RunStatus::Published,
            docs: 5,
            pages: 5,
            duplicate_pages: 0,
            started_at: Utc::now(),
            published_at: Some(Utc::now()),
            previous_version: Some("v1".into()),
            last_error: None,
            failed_at: None,
            build: None,
        }
    }

    fn staged(f: &Fixture, dry_run: bool) -> Staged<'_> {
        Staged {
            state: &f.state,
            local_reference: &f.local_reference,
            local_index_root: f.index_root.clone(),
            reference: &f.reference,
            index: &f.index,
            index_root_uri: "azure://qw-index".into(),
            dry_run,
            restore: false,
        }
    }

    fn restoring(f: &Fixture, dry_run: bool) -> Staged<'_> {
        Staged {
            restore: true,
            ..staged(f, dry_run)
        }
    }

    /// What the lake's `current.json` says once v2 serves: the pointer the
    /// release staged, promoted as it is.
    fn v2_serves() -> Vec<u8> {
        serde_json::to_vec(&json!({
            "index_version": "v2", "previous_version": "v1",
            "indexes": ["base2"], "ja": {"indexes": ["ja2"]},
        }))
        .unwrap()
    }

    async fn lake_serves(f: &Fixture, current: Vec<u8>) {
        f.reference
            .put("current.json", current, "application/json")
            .await
            .unwrap();
    }

    /// An index account as a re-provisioned one is: empty.
    fn empty_account(f: &Fixture) {
        std::fs::remove_file(f.index.root().join(METASTORE_MANIFEST)).unwrap();
    }

    #[tokio::test]
    async fn a_staged_release_reaches_the_account_and_the_lake() {
        let f = fixture().await;
        let dry = publish_staged(&staged(&f, true)).await.unwrap();
        assert!(!dry.pointer_written);
        assert!(f.index.get("base2/sA.split").await.unwrap().is_none());
        assert_eq!(dry.indexes[0].bytes, 20_000_000);

        let p = publish_staged(&staged(&f, false)).await.unwrap();
        assert!(p.pointer_written);
        assert_eq!(p.indexes.len(), 2);
        assert_eq!((p.indexes[0].splits, p.indexes[0].docs), (1, 5));
        assert_eq!(
            f.index.get("base2/sA.split").await.unwrap().unwrap().len(),
            20_000_000
        );
        assert!(f.index.get("base2/gone.split").await.unwrap().is_none());
        let m: Value =
            serde_json::from_slice(&f.index.get("base2/metastore.json").await.unwrap().unwrap())
                .unwrap();
        assert_eq!(
            m["index"]["index_config"]["index_uri"],
            "azure://qw-index/base2"
        );
        let list: Value =
            serde_json::from_slice(&f.index.get(METASTORE_MANIFEST).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(
            list["indexes"],
            json!({"base1": "active", "base2": "active", "ja2": "active"})
        );
        assert!(f.reference.get("v2/manifest.json").await.unwrap().is_some());
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_some());

        // A rerun copies nothing again.
        let again = publish_staged(&staged(&f, false)).await.unwrap();
        assert_eq!(again.indexes[0].skipped, 1);
        assert_eq!(again.indexes[0].bytes, 0);
        // The snapshot file and the run record.
        assert_eq!(again.reference_skipped, 2);
    }

    #[tokio::test]
    async fn a_changed_serving_version_or_unfinished_writer_stops_it() {
        let f = fixture().await;
        f.reference
            .put(
                "current.json",
                br#"{"index_version":"v9"}"#.to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        let e = publish_staged(&staged(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("mirror reference again"), "{e:#}");

        let f = fixture().await;
        let uri = format!("file://{}/ja2", f.index_root.display());
        std::fs::write(
            f.index_root.join("ja2").join(METASTORE_FILE),
            meta(&uri, &[("sB", "Published", 5), ("sC", "Staged", 1)]),
        )
        .unwrap();
        let e = publish_staged(&staged(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("staged splits"), "{e:#}");
        // Checked before anything was copied.
        assert!(f.index.get("base2/sA.split").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn a_different_object_of_the_same_name_stops_it() {
        let f = fixture().await;
        f.index
            .put("base2/sA.split", vec![1u8; 10], "application/octet-stream")
            .await
            .unwrap();
        let e = publish_staged(&staged(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("not 20000000"), "{e:#}");
    }

    #[tokio::test]
    async fn a_mirror_copies_the_named_prefixes_and_files() {
        let tmp = tempfile::tempdir().unwrap();
        let from = LocalStore::new(tmp.path().join("from"));
        let to = LocalStore::new(tmp.path().join("to"));
        for p in [
            "current.json",
            "catalog/titles.json",
            "v1/titles.json",
            "v0/titles.json",
        ] {
            from.put(p, p.as_bytes().to_vec(), "application/json")
                .await
                .unwrap();
        }
        let m = mirror(
            &from,
            &to,
            &["catalog".into(), "v1".into()],
            &["current.json".into()],
        )
        .await
        .unwrap();
        assert_eq!((m.copied, m.same), (3, 0));
        assert!(to.get("v0/titles.json").await.unwrap().is_none());
        let again = mirror(&from, &to, &["v1".into()], &[]).await.unwrap();
        assert_eq!((again.copied, again.same), (0, 1));
        to.put("v1/titles.json", b"other".to_vec(), "application/json")
            .await
            .unwrap();
        assert!(mirror(&from, &to, &["v1".into()], &[]).await.is_err());
        assert!(mirror(&from, &to, &["nothing".into()], &[]).await.is_err());
    }

    /// Cosmos (here another state) gets the on-prem run from the snapshot.
    #[tokio::test]
    async fn the_run_record_travels_with_the_snapshot() {
        let f = fixture().await;
        publish_staged(&staged(&f, false)).await.unwrap();
        assert!(f
            .reference
            .get("v2/index_run.json")
            .await
            .unwrap()
            .is_some());
        let cosmos = State::new(Arc::new(MemoryDocs::default()));
        let (run, _) = import_staged_run(&cosmos, &f.reference, "v2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (run.index_version.as_str(), run.status),
            ("v2", RunStatus::Published)
        );
        assert!(cosmos.run("v2").await.unwrap().is_some());
        assert!(import_staged_run(&cosmos, &f.reference, "v9")
            .await
            .unwrap()
            .is_none());
        // Another version's record under this name is refused.
        f.reference
            .put(
                "v3/index_run.json",
                serde_json::to_vec(&published_run("v2")).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        assert!(import_staged_run(&cosmos, &f.reference, "v3")
            .await
            .is_err());
    }

    /// An index account with no metastore manifest yet gets one, even for
    /// a single index.
    #[tokio::test]
    async fn an_empty_account_gets_a_manifest() {
        let f = fixture().await;
        std::fs::remove_file(f.index.root().join(METASTORE_MANIFEST)).unwrap();
        f.local_reference
            .put(
                PENDING_POINTER,
                serde_json::to_vec(
                    &json!({"index_version": "v2", "previous_version": "v1", "indexes": ["base2"]}),
                )
                .unwrap(),
                "application/json",
            )
            .await
            .unwrap();
        publish_staged(&staged(&f, false)).await.unwrap();
        let list: Value =
            serde_json::from_slice(&f.index.get(METASTORE_MANIFEST).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(list["indexes"], json!({"base2": "active"}));
    }

    #[tokio::test]
    async fn names_the_lake_has_are_reserved() {
        let tmp = tempfile::tempdir().unwrap();
        let lake = LocalStore::new(tmp.path());
        for v in [
            "pages-v20261020-1",
            "pages-v20261020-2",
            "pages-v20261021-1",
        ] {
            lake.put(
                &format!("{v}/manifest.json"),
                b"{}".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
        }
        let state = State::new(Arc::new(MemoryDocs::default()));
        let day = chrono::NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
        let r = reserve_names(&state, &lake, day).await.unwrap();
        assert_eq!(
            r,
            [
                "pages-v20261020-1",
                "pages-v20261020-2",
                "pages-v20261021-1"
            ]
        );
        let (run, _) = state.run("pages-v20261020-2").await.unwrap().unwrap();
        assert_eq!(run.status, RunStatus::Failed);
        assert!(state.run("pages-v20261020-3").await.unwrap().is_none());
        // Again: nothing new, nothing replaced.
        assert_eq!(reserve_names(&state, &lake, day).await.unwrap().len(), 3);
    }

    /// A failed cloud attempt under the same name gives way; a different
    /// published version under it is refused; a repeat changes nothing.
    #[tokio::test]
    async fn an_existing_run_of_the_same_name_is_reconciled_or_refused() {
        let f = fixture().await;
        publish_staged(&staged(&f, false)).await.unwrap();
        let cosmos = State::new(Arc::new(MemoryDocs::default()));
        let mut failed = published_run("v2");
        failed.status = RunStatus::Failed;
        failed.batch_list = None;
        failed.new_index = "pages-base-other".into();
        failed.last_error = Some("writer timed out".into());
        cosmos.create_run(&failed).await.unwrap();
        let (run, _) = import_staged_run(&cosmos, &f.reference, "v2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            (run.status, run.new_index.as_str()),
            (RunStatus::Published, "base2")
        );
        assert_eq!(run.batch_list.as_deref(), Some("v2/batches.json"));
        // Again: kept as it is.
        let (again, _) = import_staged_run(&cosmos, &f.reference, "v2")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(again.new_index, "base2");

        let other = State::new(Arc::new(MemoryDocs::default()));
        let mut published = published_run("v2");
        published.new_index = "pages-base-cloud".into();
        other.create_run(&published).await.unwrap();
        let e = import_staged_run(&other, &f.reference, "v2")
            .await
            .unwrap_err();
        assert!(
            format!("{e:#}").contains("two versions under one name"),
            "{e:#}"
        );
    }

    async fn promote(f: &Fixture, version: &str) {
        f.reference
            .put(
                "current.json",
                serde_json::to_vec(&json!({"index_version": version})).unwrap(),
                "application/json",
            )
            .await
            .unwrap();
    }

    /// Once `current.json` names the staged version, `ops/current` follows;
    /// again, nothing changes.
    #[tokio::test]
    async fn a_promoted_version_becomes_the_current_one() {
        let f = fixture().await;
        f.state.set_current_version("v1").await.unwrap();
        promote(&f, "v2").await;
        let (run, _) = f.state.run("v2").await.unwrap().unwrap();
        let recorded = record_current(&f.state, &f.reference, &run).await.unwrap();
        assert_eq!(
            (recorded.version.as_str(), recorded.previous.as_deref()),
            ("v2", Some("v1"))
        );
        assert!(recorded.changed());
        assert_eq!(
            f.state.current_version().await.unwrap().as_deref(),
            Some("v2")
        );
        let again = record_current(&f.state, &f.reference, &run).await.unwrap();
        assert!(!again.changed());
        assert_eq!(again.previous.as_deref(), Some("v2"));
    }

    /// Never ahead of the live pointer: before the promotion, without a
    /// pointer, or for a run that isn't published, `ops/current` is kept.
    #[tokio::test]
    async fn the_current_version_follows_only_a_published_live_pointer() {
        let f = fixture().await;
        f.state.set_current_version("v1").await.unwrap();
        let (run, etag) = f.state.run("v2").await.unwrap().unwrap();
        let e = record_current(&f.state, &f.reference, &run)
            .await
            .unwrap_err();
        assert!(
            format!("{e:#}").contains("current.json names `v1`, not `v2`"),
            "{e:#}"
        );
        std::fs::remove_file(f.reference.root().join("current.json")).unwrap();
        let e = record_current(&f.state, &f.reference, &run)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("no current.json"), "{e:#}");
        promote(&f, "v2").await;
        let mut building = run.clone();
        building.status = RunStatus::Building;
        building.published_at = None;
        f.state.update_run(&building, &etag).await.unwrap();
        let e = record_current(&f.state, &f.reference, &building)
            .await
            .unwrap_err();
        assert!(format!("{e:#}").contains("not published"), "{e:#}");
        assert_eq!(
            f.state.current_version().await.unwrap().as_deref(),
            Some("v1")
        );
    }

    /// A failure [`Flaky`] gives an operation: before doing it, after (done,
    /// but the answer lost), or another writer's bytes at the path first.
    enum Fault {
        Before(StoreError),
        After(StoreError),
        Raced(&'static [u8]),
    }

    fn io() -> StoreError {
        StoreError::Io("connection reset".into())
    }

    fn http(status: u16) -> StoreError {
        StoreError::Http {
            op: "test",
            path: "p".into(),
            status,
        }
    }

    /// A local store whose operations fail as scripted: each call takes the
    /// first fault left for its operation.
    #[derive(Debug)]
    struct Flaky {
        inner: LocalStore,
        faults: std::sync::Mutex<Vec<(&'static str, Fault)>>,
        calls: std::sync::Mutex<Vec<&'static str>>,
    }

    impl std::fmt::Debug for Fault {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            match self {
                Fault::Before(e) => write!(f, "before: {e}"),
                Fault::After(e) => write!(f, "after: {e}"),
                Fault::Raced(_) => write!(f, "raced"),
            }
        }
    }

    impl Flaky {
        fn new(root: &Path, faults: Vec<(&'static str, Fault)>) -> Self {
            Self {
                inner: LocalStore::new(root),
                faults: std::sync::Mutex::new(faults),
                calls: std::sync::Mutex::default(),
            }
        }

        fn fault(&self, op: &'static str) -> Option<Fault> {
            self.calls.lock().unwrap().push(op);
            let mut faults = self.faults.lock().unwrap();
            let i = faults.iter().position(|(o, _)| *o == op)?;
            Some(faults.remove(i).1)
        }

        fn calls(&self, op: &str) -> usize {
            self.calls
                .lock()
                .unwrap()
                .iter()
                .filter(|o| **o == op)
                .count()
        }

        fn faults_left(&self) -> usize {
            self.faults.lock().unwrap().len()
        }

        async fn run<T>(
            &self,
            op: &'static str,
            path: &str,
            f: impl Future<Output = Result<T, StoreError>>,
        ) -> Result<T, StoreError> {
            match self.fault(op) {
                Some(Fault::Before(e)) => Err(e),
                Some(Fault::After(e)) => {
                    f.await?;
                    Err(e)
                }
                Some(Fault::Raced(other)) => {
                    self.inner.put(path, other.to_vec(), "").await?;
                    f.await
                }
                None => f.await,
            }
        }
    }

    #[async_trait::async_trait]
    impl ObjectStore for Flaky {
        async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
            self.run("get", path, self.inner.get(path)).await
        }
        async fn put_new(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<bool, StoreError> {
            self.run("put_new", path, self.inner.put_new(path, body, ct))
                .await
        }
        async fn put(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<(), StoreError> {
            self.run("put", path, self.inner.put(path, body, ct)).await
        }
        async fn exists(&self, path: &str) -> Result<bool, StoreError> {
            self.run("exists", path, self.inner.exists(path)).await
        }
        async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
            self.run("list", prefix, self.inner.list(prefix)).await
        }
        async fn list_info(&self, prefix: &str) -> Result<Vec<usnm_store::ObjectInfo>, StoreError> {
            self.run("list_info", prefix, self.inner.list_info(prefix))
                .await
        }
        async fn put_stream(
            &self,
            path: &str,
            content_type: &str,
            tier: Option<&str>,
            mut chunks: tokio::sync::mpsc::Receiver<Bytes>,
            commit: tokio::sync::oneshot::Receiver<bool>,
        ) -> Result<u64, StoreError> {
            match self.fault("put_stream") {
                // Mid-upload: a chunk taken, then the failure.
                Some(Fault::Before(e) | Fault::After(e)) => {
                    let _ = chunks.recv().await;
                    Err(e)
                }
                Some(Fault::Raced(_)) | None => {
                    self.inner
                        .put_stream(path, content_type, tier, chunks, commit)
                        .await
                }
            }
        }
    }

    /// Failures that may pass, in the splits' uploads and every other write
    /// and listing, are tried again, and the publish completes. Writes whose
    /// answer was lost are found done.
    #[tokio::test]
    async fn transient_failures_are_ridden_out() {
        let f = fixture().await;
        let index = Flaky::new(
            f.index.root(),
            vec![
                ("list_info", Fault::Before(io())),
                ("put_stream", Fault::Before(io())),
                ("put_stream", Fault::Before(http(503))),
                ("put_new", Fault::After(io())),
                ("put", Fault::Before(http(500))),
            ],
        );
        let reference = Flaky::new(
            f.reference.root(),
            vec![
                ("put_new", Fault::After(http(503))),
                ("exists", Fault::Before(io())),
                (
                    "put",
                    Fault::Before(StoreError::Credential("refresh".into())),
                ),
            ],
        );
        let p = publish_staged(&Staged {
            index: &index,
            reference: &reference,
            ..staged(&f, false)
        })
        .await
        .unwrap();
        assert!(p.pointer_written);
        assert_eq!(p.indexes[0].bytes, 20_000_000);
        assert_eq!((index.faults_left(), reference.faults_left()), (0, 0));
        // base2's split three times, ja2's once.
        assert_eq!(index.calls("put_stream"), 4);
        assert_eq!(
            f.index.get("base2/sA.split").await.unwrap().unwrap().len(),
            20_000_000
        );
        assert!(f.index.get("base2/metastore.json").await.unwrap().is_some());
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_some());
    }

    /// A refusal stops the publish at its first try, and a split that keeps
    /// failing stops it after its tries; nothing after the splits is written.
    #[tokio::test]
    async fn refusals_and_lasting_failures_stop_it() {
        let f = fixture().await;
        let index = Flaky::new(
            f.index.root(),
            vec![("put_stream", Fault::Before(http(403)))],
        );
        let e = publish_staged(&Staged {
            index: &index,
            ..staged(&f, false)
        })
        .await
        .unwrap_err();
        // The store's own error, not the reader's "stopped".
        assert!(format!("{e:#}").contains("HTTP 403"), "{e:#}");
        assert_eq!(index.calls("put_stream"), 1);
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_none());

        let index = Flaky::new(
            f.index.root(),
            (0..SPLIT_TRIES + 1)
                .map(|_| ("put_stream", Fault::Before(io())))
                .collect(),
        );
        let e = publish_staged(&Staged {
            index: &index,
            ..staged(&f, false)
        })
        .await
        .unwrap_err();
        assert!(format!("{e:#}").contains("connection reset"), "{e:#}");
        assert_eq!(index.calls("put_stream"), SPLIT_TRIES as usize);
        assert!(f.index.get("base2/metastore.json").await.unwrap().is_none());

        // Another writer's bytes under a snapshot file's name, between the
        // check and the create: they stay, and stop it.
        let f = fixture().await;
        let reference = Flaky::new(
            f.reference.root(),
            vec![("put_new", Fault::Raced(b"other"))],
        );
        let e = publish_staged(&Staged {
            reference: &reference,
            ..staged(&f, false)
        })
        .await
        .unwrap_err();
        assert!(
            format!("{e:#}").contains("appeared in the lake's reference meanwhile"),
            "{e:#}"
        );
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_none());
    }

    /// No snapshot manifest, no copy and no pointer.
    #[tokio::test]
    async fn a_snapshot_without_its_manifest_stops_it() {
        let f = fixture().await;
        std::fs::remove_file(f.local_reference.root().join("v2/manifest.json")).unwrap();
        let e = publish_staged(&staged(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("v2/manifest.json"), "{e:#}");
        assert!(f.index.get("base2/sA.split").await.unwrap().is_none());
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_none());
    }

    /// After Phase 4 the lake's `current.json` names the release's own
    /// version. The plain publish refuses that (as it always has); a restore
    /// copies the version into an empty account and the lake's missing
    /// snapshot, and writes neither pointer.
    #[tokio::test]
    async fn a_restore_copies_the_version_the_lake_serves() {
        let f = fixture().await;
        empty_account(&f);
        lake_serves(&f, v2_serves()).await;

        let e = publish_staged(&staged(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("mirror reference again"), "{e:#}");
        assert!(f.index.get("base2/sA.split").await.unwrap().is_none());

        let dry = publish_staged(&restoring(&f, true)).await.unwrap();
        assert!(!dry.pointer_written);
        assert_eq!(dry.indexes[0].bytes, 20_000_000);
        assert_eq!(dry.reference_files, 2);
        assert!(f.index.get("base2/sA.split").await.unwrap().is_none());
        assert!(f.index.get(METASTORE_MANIFEST).await.unwrap().is_none());
        assert!(f.reference.get("v2/manifest.json").await.unwrap().is_none());

        let p = publish_staged(&restoring(&f, false)).await.unwrap();
        assert!(!p.pointer_written, "a restore never writes a pointer");
        assert_eq!(p.version, "v2");
        assert_eq!(p.indexes.len(), 2);
        assert_eq!(
            f.index.get("base2/sA.split").await.unwrap().unwrap().len(),
            20_000_000
        );
        assert_eq!(
            f.index.get("ja2/sB.split").await.unwrap().unwrap().len(),
            20_000_000
        );
        let m: Value =
            serde_json::from_slice(&f.index.get("base2/metastore.json").await.unwrap().unwrap())
                .unwrap();
        assert_eq!(
            m["index"]["index_config"]["index_uri"],
            "azure://qw-index/base2"
        );
        let list: Value =
            serde_json::from_slice(&f.index.get(METASTORE_MANIFEST).await.unwrap().unwrap())
                .unwrap();
        assert_eq!(list["indexes"], json!({"base2": "active", "ja2": "active"}));
        // The snapshot and the run record the API loads, and nothing else.
        assert!(f.reference.get("v2/manifest.json").await.unwrap().is_some());
        assert!(f
            .reference
            .get("v2/index_run.json")
            .await
            .unwrap()
            .is_some());
        assert_eq!(p.reference_files, 2);
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_none());
        assert_eq!(
            f.reference.get("current.json").await.unwrap().unwrap(),
            v2_serves()
        );

        // A rerun copies nothing again.
        let again = publish_staged(&restoring(&f, false)).await.unwrap();
        assert_eq!(again.indexes[0].skipped, 1);
        assert_eq!((again.indexes[0].bytes, again.reference_files), (0, 0));
        assert_eq!(again.reference_skipped, 2);
    }

    /// Every file under `dir`, by relative path, with its size and a hash of
    /// its bytes.
    fn tree(dir: &Path) -> std::collections::BTreeMap<String, (u64, u64)> {
        use std::hash::{Hash, Hasher};
        fn walk(dir: &Path, at: &str, out: &mut std::collections::BTreeMap<String, (u64, u64)>) {
            for e in std::fs::read_dir(dir).unwrap() {
                let e = e.unwrap();
                let name = format!("{at}{}", e.file_name().to_string_lossy());
                if e.file_type().unwrap().is_dir() {
                    walk(&e.path(), &format!("{name}/"), out);
                } else {
                    let bytes = std::fs::read(e.path()).unwrap();
                    let mut h = std::collections::hash_map::DefaultHasher::new();
                    bytes.hash(&mut h);
                    out.insert(name, (bytes.len() as u64, h.finish()));
                }
            }
        }
        let mut out = std::collections::BTreeMap::new();
        walk(dir, "", &mut out);
        out
    }

    /// Before Phase 4 the apps and the index account still hold the version
    /// that serves, so restoring it changes nothing in Azure: the rehearsal
    /// that proves the checks, not a copy.
    #[tokio::test]
    async fn restoring_what_azure_already_holds_changes_nothing() {
        let f = fixture().await;
        publish_staged(&staged(&f, false)).await.unwrap();
        // The cutover promotes the pending pointer as it is.
        lake_serves(&f, v2_serves()).await;
        let (account_before, lake_before) = (tree(f.index.root()), tree(f.reference.root()));
        let index = Flaky::new(f.index.root(), vec![]);
        let reference = Flaky::new(f.reference.root(), vec![]);
        let p = publish_staged(&Staged {
            index: &index,
            reference: &reference,
            ..restoring(&f, false)
        })
        .await
        .unwrap();
        assert!(!p.pointer_written);
        assert_eq!((p.reference_files, p.reference_skipped), (0, 2));
        assert!(p.indexes.iter().all(|i| i.skipped == 1 && i.bytes == 0));
        assert_eq!(tree(f.index.root()), account_before);
        assert_eq!(tree(f.reference.root()), lake_before);
        // The lake isn't written to at all. The account is asked to create
        // each index's metastore, which is there already, and nothing else.
        for op in ["put", "put_new", "put_stream"] {
            assert_eq!(reference.calls(op), 0, "lake {op}");
        }
        assert_eq!(index.calls("put"), 0);
        assert_eq!(index.calls("put_stream"), 0);
        assert_eq!(index.calls("put_new"), 2);
    }

    /// A dry-run restore, like a dry-run publish, writes nothing to Azure.
    #[tokio::test]
    async fn a_dry_run_restore_writes_nothing() {
        let f = fixture().await;
        empty_account(&f);
        lake_serves(&f, v2_serves()).await;
        let index = Flaky::new(f.index.root(), vec![]);
        let reference = Flaky::new(f.reference.root(), vec![]);
        let p = publish_staged(&Staged {
            index: &index,
            reference: &reference,
            ..restoring(&f, true)
        })
        .await
        .unwrap();
        assert_eq!(p.indexes.len(), 2);
        assert!(!p.pointer_written);
        for op in ["put", "put_new", "put_stream"] {
            assert_eq!(index.calls(op), 0, "index account {op}");
            assert_eq!(reference.calls(op), 0, "lake {op}");
        }
    }

    /// A restore copies only the version, and the indexes, the lake serves;
    /// anything else stops it before it copies. The rollback is the plain
    /// publish's version, not a restore's.
    #[tokio::test]
    async fn a_restore_of_something_the_lake_does_not_serve_is_refused() {
        let wrong_indexes = serde_json::to_vec(&json!({
            "index_version": "v2", "previous_version": "v1",
            "indexes": ["base2", "delta3"], "ja": {"indexes": ["ja2"]},
        }))
        .unwrap();
        let no_indexes =
            serde_json::to_vec(&json!({"index_version": "v2", "previous_version": "v1"})).unwrap();
        let cases: [(&[u8], &[&str]); 5] = [
            // The release's rollback: the plain publish does that.
            (
                br#"{"index_version":"v1"}"#,
                &["rollback", "without --restore"],
            ),
            // Another version altogether.
            (br#"{"index_version":"v9"}"#, &["serves v9, not v2"]),
            (b"{}", &["serves no version, not v2"]),
            (&wrong_indexes, &["delta3", "holds"]),
            (&no_indexes, &["current.json", "names no index"]),
        ];
        for (current, wanted) in cases {
            let f = fixture().await;
            lake_serves(&f, current.to_vec()).await;
            let e = format!(
                "{:#}",
                publish_staged(&restoring(&f, false)).await.unwrap_err()
            );
            for w in wanted {
                assert!(e.contains(w), "{w} not in: {e}");
            }
            // Checked before anything was copied or the lake touched.
            assert!(f.index.get("base2/sA.split").await.unwrap().is_none());
            assert!(f.reference.get("v2/manifest.json").await.unwrap().is_none());
            assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_none());
            assert_eq!(
                f.reference.get("current.json").await.unwrap().unwrap(),
                current
            );
        }
        // No current.json at all is no restore either.
        let f = fixture().await;
        std::fs::remove_file(f.reference.root().join("current.json")).unwrap();
        let e = publish_staged(&restoring(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("no current.json"), "{e:#}");
    }

    /// The rest of the safety checks apply to a restore as they do to a
    /// publish: a different object of the same name in Azure stops it.
    #[tokio::test]
    async fn a_restore_does_not_replace_a_different_object() {
        let f = fixture().await;
        lake_serves(&f, v2_serves()).await;
        f.index
            .put("base2/sA.split", vec![1u8; 10], "application/octet-stream")
            .await
            .unwrap();
        let e = publish_staged(&restoring(&f, false)).await.unwrap_err();
        assert!(format!("{e:#}").contains("not 20000000"), "{e:#}");

        let f = fixture().await;
        lake_serves(&f, v2_serves()).await;
        f.reference
            .put("v2/manifest.json", b"other".to_vec(), "application/json")
            .await
            .unwrap();
        let e = publish_staged(&restoring(&f, false)).await.unwrap_err();
        assert!(
            format!("{e:#}").contains("different v2/manifest.json"),
            "{e:#}"
        );
        assert!(f.reference.get(PENDING_POINTER).await.unwrap().is_none());
    }

    /// The result's serialized form is what it was before restores existed.
    #[test]
    fn the_published_result_has_the_same_fields() {
        let v = serde_json::to_value(Published::default()).unwrap();
        let mut keys: Vec<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            [
                "indexes",
                "pointer_written",
                "reference_files",
                "reference_skipped",
                "secs",
                "version"
            ]
        );
    }
}
