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
//! Promotion stays the operator step of docs/operations.md, "Index storage
//! cutover".

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Instant;

use anyhow::{bail, ensure, Context};
use bytes::Bytes;
use chrono::Utc;
use futures::stream::{self, StreamExt, TryStreamExt};
use serde::Serialize;
use serde_json::Value;
use tokio::io::AsyncReadExt;
use usnm_state::state::{Batch, BatchStatus, State};
use usnm_store::{ObjectStore, StoreError};

use crate::cluster::local::{manifest, rewrite_metastore, METASTORE_FILE};
use crate::cluster::sample::run_batches;

/// The staged pointer a release with `--stage-index-storage` writes.
pub const PENDING_POINTER: &str = "index-storage-pending.json";
/// The file-backed metastore's list of indexes, at the metastore's root.
pub const METASTORE_MANIFEST: &str = "manifest.json";
/// Objects copied at once.
const COPIES: usize = 4;
/// Bytes per chunk read from a local split.
const CHUNK: usize = 8 * 1024 * 1024;

/// Which of a version's batches to seed: every `every`th by name, from the
/// `offset`th (0-based), as docs/operations.md's 1% set takes them
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
/// with the same name differs.
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
    ensure!(
        serving["index_version"] == pointer["previous_version"],
        "the lake serves {}, but the release's rollback is {}: mirror reference again and rebuild",
        serving["index_version"],
        pointer["previous_version"]
    );
    tracing::info!(%version, indexes = ?ids, dry_run = s.dry_run, "publishing a staged release");

    let local_index = usnm_store::LocalStore::new(&s.local_index_root);
    let mut out = Published {
        version: version.clone(),
        ..Published::default()
    };
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
                if !s
                    .reference
                    .put_new(&path, body, content_type(&path))
                    .await?
                {
                    bail!("{path} appeared in the lake's reference meanwhile");
                }
                out.reference_files += 1;
            }
        }
    }
    ensure!(
        out.reference_files + out.reference_skipped > 0,
        "the local reference has no {prefix} snapshot"
    );
    if !s.dry_run {
        s.reference
            .put(PENDING_POINTER, pointer_bytes, "application/json")
            .await?;
        out.pointer_written = true;
    }
    out.secs = started.elapsed().as_secs_f64();
    tracing::info!(published = ?out, "staged release copied");
    Ok(out)
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
    let there: std::collections::HashMap<String, u64> = s
        .index
        .list_info(id)
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
    tracing::info!(
        index = id,
        splits = copy.splits,
        to_copy = jobs.len(),
        skipped = copy.skipped,
        "copying an index"
    );
    if !s.dry_run {
        let index = s.index;
        let sizes: Vec<u64> = stream::iter(jobs)
            .map(|(path, file, size)| async move {
                upload_file(index, &path, &file).await?;
                tracing::info!(path, mb = size / 1_000_000, "split copied");
                Ok::<u64, anyhow::Error>(size)
            })
            .buffer_unordered(COPIES)
            .try_collect()
            .await?;
        copy.bytes = sizes.iter().sum();
        let meta_path = format!("{id}/{METASTORE_FILE}");
        let rewritten = rewrite_metastore(meta, &format!("{}/{id}", s.index_root_uri))?;
        if s.index.get(&meta_path).await?.is_none()
            && !s
                .index
                .put_new(&meta_path, rewritten, "application/json")
                .await?
        {
            bail!("{meta_path} appeared in the index account meanwhile");
        }
    } else {
        copy.bytes = jobs.iter().map(|(_, _, n)| n).sum();
    }
    Ok(copy)
}

/// Stream a local file into `store` at `path` (create-only).
async fn upload_file(store: &dyn ObjectStore, path: &str, file: &Path) -> anyhow::Result<()> {
    let (tx, rx) = tokio::sync::mpsc::channel::<Bytes>(2);
    let (commit_tx, commit_rx) = tokio::sync::oneshot::channel();
    let file = file.to_owned();
    let reader = async move {
        let mut f = tokio::fs::File::open(&file)
            .await
            .with_context(|| format!("opening {}", file.display()))?;
        loop {
            let mut buf = vec![0u8; CHUNK];
            let mut filled = 0;
            while filled < CHUNK {
                let n = f.read(&mut buf[filled..]).await?;
                if n == 0 {
                    break;
                }
                filled += n;
            }
            if filled == 0 {
                break;
            }
            buf.truncate(filled);
            if tx.send(Bytes::from(buf)).await.is_err() {
                bail!("the upload of {} stopped reading", file.display());
            }
        }
        drop(tx);
        let _ = commit_tx.send(true);
        Ok::<(), anyhow::Error>(())
    };
    let (read, put) = tokio::join!(
        reader,
        store.put_stream(path, "application/octet-stream", None, rx, commit_rx)
    );
    read?;
    match put {
        Ok(_) => Ok(()),
        Err(StoreError::AlreadyExists(p)) => bail!("{p} appeared in the index account meanwhile"),
        Err(e) => Err(e.into()),
    }
}

/// Add `ids` to the index account's metastore manifest as active indexes,
/// keeping every index and template it lists.
async fn add_to_manifest(index: &dyn ObjectStore, ids: &[String]) -> anyhow::Result<()> {
    let mut m: Value = match index.get(METASTORE_MANIFEST).await? {
        Some(b) => serde_json::from_slice(&b).context("the index account's manifest.json")?,
        None => serde_json::from_slice(&manifest(&ids[0]))?,
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
    if added > 0 {
        index
            .put(
                METASTORE_MANIFEST,
                serde_json::to_vec_pretty(&m)?,
                "application/json",
            )
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
        Fixture {
            _tmp: tmp,
            local_reference,
            index_root,
            reference,
            index,
        }
    }

    fn staged(f: &Fixture, dry_run: bool) -> Staged<'_> {
        Staged {
            local_reference: &f.local_reference,
            local_index_root: f.index_root.clone(),
            reference: &f.reference,
            index: &f.index,
            index_root_uri: "azure://qw-index".into(),
            dry_run,
        }
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
        assert_eq!(again.reference_skipped, 1);
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
}
