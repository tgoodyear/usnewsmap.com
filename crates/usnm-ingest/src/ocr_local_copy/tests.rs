use std::os::unix::fs::PermissionsExt;
use std::sync::Arc;

use async_trait::async_trait;
use usnm_store::local::LocalStore;
use usnm_store::{ByteStream, StoreError};

use super::*;

/// A local store whose listing has a Content-MD5, as Blob Storage's does.
#[derive(Debug)]
struct Hashed {
    inner: LocalStore,
    root: std::path::PathBuf,
    wrong_md5: bool,
}

#[async_trait]
impl ObjectStore for Hashed {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(path).await
    }
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, StoreError> {
        self.inner.put_new(path, body, content_type).await
    }
    async fn put(&self, path: &str, body: Vec<u8>, content_type: &str) -> Result<(), StoreError> {
        self.inner.put(path, body, content_type).await
    }
    async fn get_stream(&self, path: &str) -> Result<Option<ByteStream>, StoreError> {
        self.inner.get_stream(path).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.inner.list(prefix).await
    }
    async fn list_info(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let mut infos = self.inner.list_info(prefix).await?;
        for info in &mut infos {
            let bytes = std::fs::read(self.root.join(&info.path)).unwrap();
            let bytes = if self.wrong_md5 { vec![0] } else { bytes };
            info.content_md5 = Some(md5_of(&Md5::digest(&bytes)));
        }
        Ok(infos)
    }
}

struct Fixture {
    _dir: tempfile::TempDir,
    root: std::path::PathBuf,
    dest: std::path::PathBuf,
    store: Arc<Hashed>,
}

fn fixture(wrong_md5: bool) -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("lake");
    let dest = dir.path().join("copy");
    std::fs::create_dir_all(&dest).unwrap();
    for (path, body) in [
        ("ocr-local/alto/a.node.1.xml", "alto a"),
        ("ocr-local/pages/sn1/run.parquet", "part"),
        ("ocr-local/requests/2026-10-16/lookup.json", "mutable"),
    ] {
        let full = root.join(path);
        std::fs::create_dir_all(full.parent().unwrap()).unwrap();
        std::fs::write(full, body).unwrap();
    }
    Fixture {
        store: Arc::new(Hashed {
            inner: LocalStore::new(&root),
            root: root.clone(),
            wrong_md5,
        }),
        _dir: dir,
        root,
        dest,
    }
}

fn options(dest: &Path) -> Options<'_> {
    Options {
        dest,
        status: None,
        now: Utc::now(),
        alert_after: Duration::from_secs(3600),
        verify: false,
        require_mount: false,
    }
}

#[tokio::test]
async fn copies_what_is_missing_read_only_and_skips_requests() {
    let f = fixture(false);
    let report = copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    assert_eq!(report.copied_this_run, 2);
    assert_eq!(report.ignored_objects, 1);
    assert!(report.ok);
    let copy = f.dest.join("pages/sn1/run.parquet");
    assert_eq!(std::fs::read(&copy).unwrap(), b"part");
    assert_eq!(
        std::fs::metadata(&copy).unwrap().permissions().mode() & 0o777,
        0o440
    );
    assert!(!f.dest.join("requests").exists());
    let again = copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    assert_eq!((again.copied_this_run, again.copied_objects), (0, 2));
}

#[tokio::test]
async fn a_seeded_file_is_kept_and_verify_reads_only_the_copy() {
    let f = fixture(false);
    std::fs::create_dir_all(f.dest.join("alto")).unwrap();
    std::fs::write(f.dest.join("alto/a.node.1.xml"), "alto a").unwrap();
    let verify = Options {
        verify: true,
        ..options(&f.dest)
    };
    let report = copy_objects(f.store.as_ref(), &verify).await.unwrap();
    assert_eq!(report.copied_this_run, 1, "only the part crossed");
    assert_eq!(report.copied_objects, 2);
    assert_eq!(report.mismatched_objects, 0);
}

#[tokio::test]
async fn a_copy_that_differs_is_left_and_verify_finds_same_sized_damage() {
    let f = fixture(false);
    std::fs::create_dir_all(f.dest.join("alto")).unwrap();
    let copy = f.dest.join("alto/a.node.1.xml");
    std::fs::write(&copy, "alto b").unwrap();
    let verify = Options {
        verify: true,
        ..options(&f.dest)
    };
    let report = copy_objects(f.store.as_ref(), &verify).await.unwrap();
    assert_eq!(report.mismatched_objects, 1);
    assert_eq!(std::fs::read(&copy).unwrap(), b"alto b");
}

#[tokio::test]
async fn bytes_that_do_not_match_the_listing_never_land() {
    let f = fixture(true);
    let report = copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    assert_eq!(report.missing_objects, 2);
    assert!(!f.dest.join("alto/a.node.1.xml").exists());
    assert!(!f.dest.join("alto/a.node.1.xml.partial").exists());
}

#[tokio::test]
async fn an_old_missing_object_is_overdue_and_a_new_one_is_not() {
    let f = fixture(true);
    let fresh = copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    assert!(fresh.ok);
    let later = Options {
        now: Utc::now() + chrono::TimeDelta::hours(2),
        ..options(&f.dest)
    };
    let late = copy_objects(f.store.as_ref(), &later).await.unwrap();
    assert!(!late.ok);
    assert_eq!(late.overdue_objects, 2);
    assert_eq!(late.overdue_sample.len(), 2);
}

#[tokio::test]
async fn a_link_in_the_destination_is_not_followed() {
    let f = fixture(false);
    let outside = f.root.join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, f.dest.join("alto")).unwrap();
    let report = copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    assert!(report.mismatched_objects + report.missing_objects >= 1);
    assert_eq!(std::fs::read_dir(&outside).unwrap().count(), 0);
}

#[tokio::test]
async fn the_status_file_keeps_the_last_success() {
    let f = fixture(false);
    let status = f.dest.parent().unwrap().join("status.json");
    let o = Options {
        status: Some(&status),
        ..options(&f.dest)
    };
    let report = copy_objects(f.store.as_ref(), &o).await.unwrap();
    let saved: Report = serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
    assert_eq!(saved.last_success, report.last_success);
    assert!(saved.ok);
}
