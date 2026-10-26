use super::publish::SPLIT_TRIES;
use crate::cluster::local::METASTORE_FILE;
use bytes::Bytes;
use chrono::Utc;
use std::future::Future;
use std::path::{Path, PathBuf};
use usnm_state::state::{BatchStatus, IndexRun, RunStatus, State};
use usnm_store::{ObjectStore, StoreError};

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
        serde_json::from_slice(&f.index.get(METASTORE_MANIFEST).await.unwrap().unwrap()).unwrap();
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
        serde_json::from_slice(&f.index.get(METASTORE_MANIFEST).await.unwrap().unwrap()).unwrap();
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
        serde_json::from_slice(&f.index.get(METASTORE_MANIFEST).await.unwrap().unwrap()).unwrap();
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
