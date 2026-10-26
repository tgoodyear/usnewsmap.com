//! Checked, resumable publication and restore of a staged release.

use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context};
use bytes::Bytes;
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use usnm_state::state::{RunStatus, State};
use usnm_store::retry::{retryable_write, retrying, Retry};
use usnm_store::{ObjectStore, StoreError};

use super::{
    metastore_splits, pointer_indexes, Split, METASTORE_MANIFEST, PENDING_POINTER, RUN_FILE,
};
use crate::cluster::local::{rewrite_metastore, METASTORE_FILE};

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
pub(super) const SPLIT_TRIES: u32 = 4;
const SPLIT_WAIT: Duration = if cfg!(test) {
    Duration::from_millis(1)
} else {
    Duration::from_secs(300)
};
/// How often an index's copy logs how far it is.
const PROGRESS_EVERY: Duration = Duration::from_secs(60);

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

pub(super) fn content_type(path: &str) -> &'static str {
    if path.ends_with(".json") {
        "application/json"
    } else {
        "application/octet-stream"
    }
}
