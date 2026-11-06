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
    assert_eq!(report.copied_this_run, 0, "verify downloads nothing");
    assert_eq!(report.copied_objects, 1);
    assert_eq!(report.missing_objects, 1);
    assert_eq!(report.mismatched_objects, 0);
    assert!(!f.dest.join("pages").exists(), "nor makes a directory");
    assert_eq!(report.last_success, None, "the gap isn't a success");
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
async fn a_mismatch_stays_reported_until_it_is_fixed() {
    let f = fixture(false);
    let status = f.dest.parent().unwrap().join("status.json");
    let plain = Options {
        status: Some(&status),
        ..options(&f.dest)
    };
    copy_objects(f.store.as_ref(), &plain).await.unwrap();
    let copy = f.dest.join("alto/a.node.1.xml");
    std::fs::remove_file(&copy).unwrap();
    std::fs::write(&copy, "alto b").unwrap();
    let path = "ocr-local/alto/a.node.1.xml";

    let before = copy_objects(f.store.as_ref(), &plain).await.unwrap();
    assert_eq!(before.mismatched_objects, 0, "size alone can't see it");

    let verify = Options {
        verify: true,
        ..plain.clone()
    };
    let found = copy_objects(f.store.as_ref(), &verify).await.unwrap();
    assert_eq!(found.mismatched_paths, [path]);

    let later = Options {
        now: Utc::now() + chrono::TimeDelta::hours(2),
        ..plain.clone()
    };
    let daily = copy_objects(f.store.as_ref(), &later).await.unwrap();
    assert_eq!(daily.mismatched_paths, [path], "a plain run keeps it");
    assert_eq!(daily.overdue_sample, [path]);
    assert!(!daily.ok);
    assert_eq!(daily.last_success, before.last_success);
    assert_eq!(std::fs::read(&copy).unwrap(), b"alto b");

    std::fs::remove_file(&copy).unwrap();
    let fixed = copy_objects(f.store.as_ref(), &plain).await.unwrap();
    assert_eq!(fixed.copied_this_run, 1);
    assert!(fixed.ok && fixed.mismatched_paths.is_empty());
    assert_eq!(fixed.last_success, Some(plain.now));
}

#[tokio::test]
async fn a_status_file_from_before_the_paths_still_gives_the_last_success() {
    let f = fixture(true);
    let status = f.dest.parent().unwrap().join("status.json");
    std::fs::write(&status, r#"{"last_success":"2026-10-30T03:30:00Z"}"#).unwrap();
    let o = Options {
        status: Some(&status),
        ..options(&f.dest)
    };
    let report = copy_objects(f.store.as_ref(), &o).await.unwrap();
    assert_eq!(report.missing_objects, 2);
    assert_eq!(
        report.last_success,
        Some("2026-10-30T03:30:00Z".parse().unwrap())
    );
}

#[tokio::test]
async fn verify_with_no_md5_reports_unverified_and_not_a_match() {
    let f = fixture(false);
    let status = f.dest.parent().unwrap().join("status.json");
    let store = LocalStore::new(&f.root);
    let plain = Options {
        status: Some(&status),
        ..options(&f.dest)
    };
    let copied = copy_objects(&store, &plain).await.unwrap();
    assert_eq!(copied.copied_objects, 2);
    std::fs::remove_file(f.dest.join("alto/a.node.1.xml")).unwrap();
    std::fs::write(f.dest.join("alto/a.node.1.xml"), "alto b").unwrap();
    let verify = Options {
        verify: true,
        now: Utc::now() + chrono::TimeDelta::hours(2),
        ..plain.clone()
    };
    let report = copy_objects(&store, &verify).await.unwrap();
    assert_eq!(report.unverified_objects, 2);
    assert_eq!(report.no_md5_objects, 2);
    assert_eq!(report.copied_objects, 0);
    assert_eq!(report.mismatched_objects, 0);
    assert_eq!(report.last_success, copied.last_success, "not advanced");
    assert_eq!(report.overdue_objects, 2);
    assert!(!report.ok);
}

#[tokio::test]
async fn a_status_file_that_cannot_be_read_stops_the_run() {
    let f = fixture(false);
    let status = f.dest.parent().unwrap().join("status.json");
    let o = Options {
        status: Some(&status),
        ..options(&f.dest)
    };
    std::fs::write(&status, "{ not json").unwrap();
    assert!(copy_objects(f.store.as_ref(), &o).await.is_err());
    assert_eq!(
        std::fs::read(&status).unwrap(),
        b"{ not json",
        "left as it is"
    );
    assert!(!f.dest.join("alto").exists(), "nothing copied");

    std::fs::remove_file(&status).unwrap();
    std::fs::create_dir(&status).unwrap();
    assert!(copy_objects(f.store.as_ref(), &o).await.is_err());
}

fn sync_fails(_: &OwnedFd) -> std::io::Result<()> {
    Err(std::io::Error::other("no sync"))
}

#[tokio::test]
async fn a_left_copy_is_accepted_only_once_its_directory_syncs() {
    let f = fixture(false);
    copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    // As a run leaves it when the unlink or the sync after the link fails.
    let copy = f.dest.join("alto/a.node.1.xml");
    std::fs::hard_link(&copy, f.dest.join("alto/a.node.1.xml.partial")).unwrap();

    let failed = copy_with(f.store.as_ref(), &options(&f.dest), sync_fails)
        .await
        .unwrap();
    assert_eq!(failed.copied_objects, 0);
    assert_eq!(failed.mismatched_objects, 2);
    assert_eq!(failed.last_success, None);
    assert!(copy.exists());

    let synced = copy_objects(f.store.as_ref(), &options(&f.dest))
        .await
        .unwrap();
    assert_eq!((synced.copied_objects, synced.mismatched_objects), (2, 0));
}

#[tokio::test]
async fn an_object_is_not_copied_when_its_new_directory_cannot_sync() {
    let f = fixture(false);
    let report = copy_with(f.store.as_ref(), &options(&f.dest), sync_fails)
        .await
        .unwrap();
    assert_eq!((report.copied_objects, report.missing_objects), (0, 2));
    assert!(!f.dest.join("alto/a.node.1.xml").exists());
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
