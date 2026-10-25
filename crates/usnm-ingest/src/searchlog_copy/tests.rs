//! Synthetic day files only (ADR-0012 and CONTRIBUTING: no real search content). Every record
//! holds the marker `SENTINEL`, which no report, status file or error may ever contain.

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::sync::Mutex;

use async_trait::async_trait;
use bytes::Bytes;
use usnm_store::{ByteStream, LocalStore, StoreError};

use super::*;

const SENTINEL: &str = "SENTINEL-aardvark";

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

/// A synthetic day file: `lines` records, every one carrying the sentinel.
fn body(day: &str, lines: usize) -> String {
    (0..lines)
        .map(|i| format!("{{\"v\":1,\"day\":\"{day}\",\"q\":\"{SENTINEL} {i}\"}}\n"))
        .collect()
}

/// A stand-in for the `searches` container, on disk.
struct Lake {
    dir: tempfile::TempDir,
}

impl Lake {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn store(&self) -> LocalStore {
        LocalStore::new(self.dir.path())
    }

    /// Write `days/{name}` as the API would, written (by the store's clock) at `written`.
    fn put(&self, name: &str, text: &str, written: &str) {
        let path = self.dir.path().join("days").join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, text).unwrap();
        std::fs::File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(at(written).into())
            .unwrap();
    }

    /// A day file written an hour after the day ended, with `lines` records.
    fn put_day(&self, d: &str, lines: usize) -> String {
        let text = body(d, lines);
        let written = format!("{}T01:00:00Z", day(d).succ_opt().unwrap());
        self.put(&format!("{d}.jsonl"), &text, &written);
        text
    }
}

/// How the faulty store misbehaves for one path.
#[derive(Debug, Clone)]
enum Fault {
    /// The stream ends after this many bytes, as a connection that closed early would.
    Truncate(usize),
    /// The stream has these extra bytes at the end.
    Extend(&'static str),
    /// The connection fails after this many bytes.
    Reset(usize),
    /// The object is gone between the listing and the read.
    Gone,
    /// The first byte is different; the size is the same.
    Flip,
    /// Another writer puts this file in place while the stream is being read.
    Race(PathBuf, &'static str),
}

/// A store that is the lake but can fail a read, and can leave out the creation times.
#[derive(Debug)]
struct Faulty {
    inner: LocalStore,
    faults: Mutex<HashMap<String, Fault>>,
    no_created: bool,
}

impl Faulty {
    fn new(lake: &Lake) -> Self {
        Self {
            inner: lake.store(),
            faults: Mutex::new(HashMap::new()),
            no_created: false,
        }
    }

    fn fail(&self, d: &str, fault: Fault) {
        self.faults
            .lock()
            .unwrap()
            .insert(format!("days/{d}.jsonl"), fault);
    }

    fn mend(&self) {
        self.faults.lock().unwrap().clear();
    }
}

#[async_trait]
impl ObjectStore for Faulty {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, StoreError> {
        self.inner.get(path).await
    }

    async fn put_new(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<bool, StoreError> {
        self.inner.put_new(path, body, ct).await
    }

    async fn put(&self, path: &str, body: Vec<u8>, ct: &str) -> Result<(), StoreError> {
        self.inner.put(path, body, ct).await
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, StoreError> {
        self.inner.list(prefix).await
    }

    async fn list_info(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
        let mut listed = self.inner.list_info(prefix).await?;
        if self.no_created {
            for info in &mut listed {
                info.created = None;
            }
        }
        Ok(listed)
    }

    async fn get_stream(&self, path: &str) -> Result<Option<ByteStream>, StoreError> {
        let fault = self.faults.lock().unwrap().get(path).cloned();
        let Some(mut whole) = self.inner.get(path).await? else {
            return Ok(None);
        };
        let items: Vec<Result<Bytes, StoreError>> = match fault {
            None => vec![Ok(Bytes::from(whole))],
            Some(Fault::Truncate(n)) => {
                whole.truncate(n);
                vec![Ok(Bytes::from(whole))]
            }
            Some(Fault::Extend(more)) => {
                whole.extend_from_slice(more.as_bytes());
                vec![Ok(Bytes::from(whole))]
            }
            Some(Fault::Reset(n)) => {
                whole.truncate(n);
                vec![
                    Ok(Bytes::from(whole)),
                    Err(StoreError::Io(format!("get `{path}`: connection reset"))),
                ]
            }
            Some(Fault::Gone) => return Ok(None),
            Some(Fault::Flip) => {
                whole[0] ^= 0x01;
                vec![Ok(Bytes::from(whole))]
            }
            Some(Fault::Race(file, text)) => {
                std::fs::write(file, text).unwrap();
                vec![Ok(Bytes::from(whole))]
            }
        };
        Ok(Some(Box::pin(futures::stream::iter(items))))
    }
}

fn options<'a>(dest: &'a Path, now: &str) -> Options<'a> {
    Options {
        dest,
        status: None,
        now: at(now),
        alert_after: Duration::from_secs(24 * 3600),
        verify: false,
        require_mount: false,
    }
}

fn read(dest: &Path, d: &str) -> String {
    std::fs::read_to_string(dest.join("days").join(format!("{d}.jsonl"))).unwrap()
}

fn names(dest: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dest.join("days"))
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

/// Nothing a person searched for is in the report, however it is rendered.
fn assert_no_text(report: &Report) {
    for rendered in [
        format!("{report:?}"),
        serde_json::to_string(report).unwrap(),
    ] {
        assert!(!rendered.contains(SENTINEL), "{rendered}");
    }
}

#[tokio::test]
async fn copies_every_missing_day_and_leaves_the_rest() {
    let lake = Lake::new();
    let a = lake.put_day("2026-10-20", 3);
    let b = lake.put_day("2026-10-21", 5);
    let c = lake.put_day("2026-10-22", 1);
    let dest = tempfile::tempdir().unwrap();
    // 2026-10-20 is there already, with the same bytes.
    let mut o = options(dest.path(), "2026-10-23T06:00:00Z");
    std::fs::create_dir_all(dest.path().join("days")).unwrap();
    std::fs::write(dest.path().join("days/2026-10-20.jsonl"), &a).unwrap();
    let mtime = std::fs::metadata(dest.path().join("days/2026-10-20.jsonl"))
        .unwrap()
        .modified()
        .unwrap();

    let report = copy_days(&lake.store(), &o).await.unwrap();

    assert_eq!(read(dest.path(), "2026-10-20"), a);
    assert_eq!(read(dest.path(), "2026-10-21"), b);
    assert_eq!(read(dest.path(), "2026-10-22"), c);
    assert_eq!(
        std::fs::metadata(dest.path().join("days/2026-10-20.jsonl"))
            .unwrap()
            .modified()
            .unwrap(),
        mtime,
        "a day already there is not touched"
    );
    assert_eq!(
        names(dest.path()),
        ["2026-10-20.jsonl", "2026-10-21.jsonl", "2026-10-22.jsonl"],
        "no partial is left"
    );
    assert!(report.ok);
    assert_eq!(report.source_days, 3);
    assert_eq!(report.copied_days, 3);
    assert_eq!(report.copied_this_run, 2);
    assert_eq!(report.bytes_copied, (b.len() + c.len()) as u64);
    assert_eq!(report.newest_source_day, Some(day("2026-10-22")));
    assert_eq!(report.newest_copied_day, Some(day("2026-10-22")));
    assert!(report.missing_days.is_empty() && report.mismatched_days.is_empty());
    assert_eq!(report.last_success, Some(o.now));
    assert_no_text(&report);

    // The next run finds nothing to do, with or without the full comparison.
    for verify in [false, true] {
        o.verify = verify;
        let again = copy_days(&lake.store(), &o).await.unwrap();
        assert_eq!((again.copied_this_run, again.copied_days), (0, 3));
        assert!(again.ok);
    }
}

#[tokio::test]
async fn a_copy_is_read_only_and_private_to_its_group() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 2);
    let dest = tempfile::tempdir().unwrap();
    let status = dest.path().join("status.json");
    let mut o = options(dest.path(), "2026-10-21T06:00:00Z");
    o.status = Some(&status);

    copy_days(&lake.store(), &o).await.unwrap();

    let mode = |p: PathBuf| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode(dest.path().join("days/2026-10-20.jsonl")), 0o440);
    assert_eq!(mode(dest.path().join("days")), 0o750);
    assert_eq!(mode(status.clone()), 0o644);
}

#[tokio::test]
async fn never_replaces_a_copy_that_differs() {
    let lake = Lake::new();
    let good = lake.put_day("2026-10-20", 3);
    lake.put_day("2026-10-21", 3);
    let dest = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(dest.path().join("days")).unwrap();
    // A different size, and the same size with other bytes.
    let short = "damaged\n";
    let mut same_size = body("2026-10-21", 3).into_bytes();
    same_size[0] ^= 0x01;
    std::fs::write(dest.path().join("days/2026-10-20.jsonl"), short).unwrap();
    std::fs::write(dest.path().join("days/2026-10-21.jsonl"), &same_size).unwrap();
    let mut o = options(dest.path(), "2026-10-23T06:00:00Z");

    // By size alone, the same-size copy is not told apart.
    let report = copy_days(&lake.store(), &o).await.unwrap();
    assert_eq!(report.mismatched_days, [day("2026-10-20")]);
    assert_eq!(report.copied_this_run, 0);
    assert!(!report.ok, "written more than a day ago: overdue");
    assert_eq!(report.overdue_days, [day("2026-10-20")]);
    assert_ne!(read(dest.path(), "2026-10-20"), good);

    // With the full comparison both are found.
    o.verify = true;
    let report = copy_days(&lake.store(), &o).await.unwrap();
    assert_eq!(
        report.mismatched_days,
        [day("2026-10-20"), day("2026-10-21")]
    );
    assert_eq!(report.overdue_days, report.mismatched_days);
    assert_eq!(report.copied_days, 0);
    assert_eq!(report.last_success, None);
    // Neither was touched.
    assert_eq!(read(dest.path(), "2026-10-20"), short);
    assert_eq!(
        std::fs::read(dest.path().join("days/2026-10-21.jsonl")).unwrap(),
        same_size
    );
    assert_no_text(&report);

    // The operator moves a damaged copy aside; the next run copies the day again.
    std::fs::rename(
        dest.path().join("days/2026-10-20.jsonl"),
        dest.path().join("moved-aside"),
    )
    .unwrap();
    let report = copy_days(&lake.store(), &o).await.unwrap();
    assert_eq!(report.copied_this_run, 1);
    assert_eq!(read(dest.path(), "2026-10-20"), good);
}

#[tokio::test]
async fn a_symlink_or_directory_is_not_a_copy() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 2);
    lake.put_day("2026-10-21", 2);
    let dest = tempfile::tempdir().unwrap();
    let days = dest.path().join("days");
    std::fs::create_dir_all(&days).unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    std::fs::write(elsewhere.path().join("x"), body("2026-10-20", 2)).unwrap();
    std::os::unix::fs::symlink(elsewhere.path().join("x"), days.join("2026-10-20.jsonl")).unwrap();
    std::fs::create_dir(days.join("2026-10-21.jsonl")).unwrap();

    let report = copy_days(&lake.store(), &options(dest.path(), "2026-10-23T06:00:00Z"))
        .await
        .unwrap();

    assert_eq!(
        report.mismatched_days,
        [day("2026-10-20"), day("2026-10-21")]
    );
    assert_eq!(report.copied_this_run, 0);
}

#[tokio::test]
async fn a_day_missing_for_more_than_the_limit_is_overdue() {
    let lake = Lake::new();
    // Written 2026-10-21 01:00.
    lake.put_day("2026-10-20", 2);
    let flaky = Faulty::new(&lake);
    flaky.fail("2026-10-20", Fault::Reset(10));
    let dest = tempfile::tempdir().unwrap();

    // Within the limit: reported, not overdue, the command would succeed.
    let report = copy_days(&flaky, &options(dest.path(), "2026-10-22T01:00:00Z"))
        .await
        .unwrap();
    assert_eq!(report.missing_days, [day("2026-10-20")]);
    assert!(report.ok && report.overdue_days.is_empty());
    assert_eq!(report.copied_days, 0);
    assert_eq!(report.last_success, None);

    // A second past the limit: overdue.
    let report = copy_days(&flaky, &options(dest.path(), "2026-10-22T01:00:01Z"))
        .await
        .unwrap();
    assert_eq!(report.overdue_days, [day("2026-10-20")]);
    assert!(!report.ok);
    assert_no_text(&report);

    // The lake answers again: copied, and no longer overdue.
    flaky.mend();
    let report = copy_days(&flaky, &options(dest.path(), "2026-10-22T06:00:00Z"))
        .await
        .unwrap();
    assert!(report.ok && report.missing_days.is_empty());
    assert_eq!(report.copied_this_run, 1);
}

#[tokio::test]
async fn each_day_is_overdue_by_its_own_age() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    lake.put_day("2026-10-21", 1);
    let flaky = Faulty::new(&lake);
    flaky.fail("2026-10-20", Fault::Gone);
    flaky.fail("2026-10-21", Fault::Gone);
    let dest = tempfile::tempdir().unwrap();
    // Day 20 was written 2026-10-21 01:00, day 21 on 2026-10-22 01:00.
    let mut o = options(dest.path(), "2026-10-22T12:00:00Z");
    o.alert_after = Duration::from_secs(12 * 3600);

    let report = copy_days(&flaky, &o).await.unwrap();

    assert_eq!(report.missing_days, [day("2026-10-20"), day("2026-10-21")]);
    assert_eq!(report.overdue_days, [day("2026-10-20")]);
}

#[tokio::test]
async fn a_short_or_long_or_broken_stream_leaves_no_file() {
    let lake = Lake::new();
    for d in ["2026-10-20", "2026-10-21", "2026-10-22", "2026-10-23"] {
        lake.put_day(d, 4);
    }
    let flaky = Faulty::new(&lake);
    flaky.fail("2026-10-20", Fault::Truncate(30));
    flaky.fail("2026-10-21", Fault::Extend("more\n"));
    flaky.fail("2026-10-22", Fault::Reset(30));
    flaky.fail("2026-10-23", Fault::Gone);
    let dest = tempfile::tempdir().unwrap();

    let report = copy_days(&flaky, &options(dest.path(), "2026-10-25T06:00:00Z"))
        .await
        .unwrap();

    assert!(names(dest.path()).is_empty(), "{:?}", names(dest.path()));
    assert_eq!(report.copied_days, 0);
    assert_eq!(report.missing_days.len(), 4);
    assert_eq!(report.overdue_days.len(), 4);
    assert_no_text(&report);
}

#[tokio::test]
async fn verify_compares_every_copy_with_the_lakes_bytes() {
    // A copy made from what the lake sent matches it; once the lake sends other bytes of the
    // same size for the day, only `verify` can tell.
    let lake = Lake::new();
    lake.put_day("2026-10-20", 2);
    let flaky = Faulty::new(&lake);
    let dest = tempfile::tempdir().unwrap();
    let mut o = options(dest.path(), "2026-10-21T06:00:00Z");
    o.verify = true;
    copy_days(&flaky, &o).await.unwrap();

    flaky.fail("2026-10-20", Fault::Flip);
    let report = copy_days(&flaky, &o).await.unwrap();
    assert_eq!(report.mismatched_days, [day("2026-10-20")]);
}

#[tokio::test]
async fn a_file_that_appears_during_the_copy_is_not_replaced() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 3);
    let flaky = Faulty::new(&lake);
    let dest = tempfile::tempdir().unwrap();
    let there = dest.path().join("days/2026-10-20.jsonl");
    std::fs::create_dir_all(dest.path().join("days")).unwrap();
    // Another writer links its own file in after the listing and before the link.
    flaky.fail("2026-10-20", Fault::Race(there.clone(), "theirs\n"));
    let mut o = options(dest.path(), "2026-10-23T06:00:00Z");
    // `survey` has run by then: the day looks absent, as it would to a second tool.

    let report = copy_days(&flaky, &o).await.unwrap();

    assert_eq!(std::fs::read_to_string(&there).unwrap(), "theirs\n");
    assert_eq!(report.copied_this_run, 0);
    assert_eq!(report.missing_days, [day("2026-10-20")]);
    assert_eq!(
        names(dest.path()),
        ["2026-10-20.jsonl"],
        "no partial is left"
    );

    // The next run sees the file for what it is.
    flaky.mend();
    o.verify = true;
    let report = copy_days(&flaky, &o).await.unwrap();
    assert_eq!(report.mismatched_days, [day("2026-10-20")]);
    assert_eq!(std::fs::read_to_string(&there).unwrap(), "theirs\n");
}

#[tokio::test]
async fn a_stale_partial_is_removed_and_foreign_files_are_left() {
    let lake = Lake::new();
    let text = lake.put_day("2026-10-20", 3);
    let dest = tempfile::tempdir().unwrap();
    let days = dest.path().join("days");
    std::fs::create_dir_all(&days).unwrap();
    // An interrupted run's partial, and files the tool did not make.
    std::fs::write(days.join("2026-10-20.jsonl.partial"), "half a fi").unwrap();
    std::fs::write(days.join("2026-10-19.jsonl.partial"), "x").unwrap();
    std::fs::write(days.join("notes.txt"), "not ours").unwrap();
    std::fs::write(days.join("2026-1-4.jsonl"), "not a day").unwrap();

    let report = copy_days(&lake.store(), &options(dest.path(), "2026-10-21T06:00:00Z"))
        .await
        .unwrap();

    assert_eq!(read(dest.path(), "2026-10-20"), text);
    assert_eq!(report.stale_partials_removed, 2);
    assert_eq!(report.foreign_files, 2);
    assert_eq!(
        names(dest.path()),
        ["2026-1-4.jsonl", "2026-10-20.jsonl", "notes.txt"]
    );
    assert_eq!(std::fs::read(days.join("notes.txt")).unwrap(), b"not ours");
}

#[tokio::test]
async fn only_exact_day_files_in_the_lake_count() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    for name in [
        "2026-1-4.jsonl",
        "2026-02-30.jsonl",
        "2026-10-20.jsonl.bak",
        "readme.txt",
        "nested/2026-10-21.jsonl",
        "2026-10-21.JSONL",
    ] {
        lake.put(name, "ignored\n", "2026-10-22T01:00:00Z");
    }
    // Staged batches are another prefix altogether.
    let staged = lake.dir.path().join("staging/2026-10-21");
    std::fs::create_dir_all(&staged).unwrap();
    std::fs::write(staged.join("abc.jsonl"), body("2026-10-21", 1)).unwrap();
    let dest = tempfile::tempdir().unwrap();

    let report = copy_days(&lake.store(), &options(dest.path(), "2026-10-23T06:00:00Z"))
        .await
        .unwrap();

    assert_eq!(report.source_days, 1);
    assert_eq!(report.ignored_objects, 6);
    assert_eq!(names(dest.path()), ["2026-10-20.jsonl"]);
}

#[tokio::test]
async fn a_destination_that_is_not_a_mount_point_is_refused() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    let dest = tempfile::tempdir().unwrap();
    let mut o = options(dest.path(), "2026-10-21T06:00:00Z");
    o.require_mount = true;

    let err = copy_days(&lake.store(), &o).await.unwrap_err();

    assert!(format!("{err:#}").contains("not a mount point"), "{err:#}");
    assert!(
        std::fs::read_dir(dest.path()).unwrap().next().is_none(),
        "nothing was written"
    );

    // A directory that is not there, and a file, are refused too.
    o.dest = Path::new("/nonexistent/usnm-searchlog");
    assert!(copy_days(&lake.store(), &o).await.is_err());
    let file = dest.path().join("file");
    std::fs::write(&file, "x").unwrap();
    o.dest = &file;
    assert!(copy_days(&lake.store(), &o).await.is_err());
}

#[test]
fn a_mount_point_is_told_from_a_directory() {
    // `/dev` is a mount of its own on Linux and macOS alike; a directory made in a temporary
    // directory is on its parent's file system.
    assert!(is_mount_point(Path::new("/dev")).unwrap());
    let dir = tempfile::tempdir().unwrap();
    assert!(!is_mount_point(dir.path()).unwrap());
    assert!(is_mount_point(&dir.path().join("missing")).is_err());
}

#[tokio::test]
async fn one_run_at_a_time() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    let dest = tempfile::tempdir().unwrap();
    let o = options(dest.path(), "2026-10-21T06:00:00Z");

    let held = prepare(&o).unwrap();
    let err = copy_days(&lake.store(), &o).await.unwrap_err();
    assert!(
        err.to_string().contains("another search log copy"),
        "{err:#}"
    );
    assert!(names(dest.path()).is_empty());

    drop(held);
    assert_eq!(
        copy_days(&lake.store(), &o).await.unwrap().copied_this_run,
        1
    );
}

#[tokio::test]
async fn a_lake_that_will_not_list_fails_the_run() {
    #[derive(Debug)]
    struct Down;
    #[async_trait]
    impl ObjectStore for Down {
        async fn get(&self, _: &str) -> Result<Option<Vec<u8>>, StoreError> {
            unreachable!()
        }
        async fn put_new(&self, _: &str, _: Vec<u8>, _: &str) -> Result<bool, StoreError> {
            unreachable!()
        }
        async fn put(&self, _: &str, _: Vec<u8>, _: &str) -> Result<(), StoreError> {
            unreachable!()
        }
        async fn list(&self, _: &str) -> Result<Vec<String>, StoreError> {
            unreachable!()
        }
        async fn list_info(&self, prefix: &str) -> Result<Vec<ObjectInfo>, StoreError> {
            Err(StoreError::Http {
                op: "list",
                path: prefix.into(),
                status: 403,
            })
        }
    }
    let dest = tempfile::tempdir().unwrap();
    let status = dest.path().join("status.json");
    std::fs::write(&status, "{\"last_success\":\"2026-10-20T02:30:00Z\"}").unwrap();
    let mut o = options(dest.path(), "2026-10-21T06:00:00Z");
    o.status = Some(&status);

    let err = copy_days(&Down, &o).await.unwrap_err();

    assert!(format!("{err:#}").contains("403"), "{err:#}");
    // The last status stays as it was: it does not claim a check that did not happen.
    assert!(std::fs::read_to_string(&status)
        .unwrap()
        .contains("2026-10-20T02:30:00Z"));
}

#[tokio::test]
async fn last_success_stays_until_the_copy_has_caught_up() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    let flaky = Faulty::new(&lake);
    let dest = tempfile::tempdir().unwrap();
    let out = tempfile::tempdir().unwrap();
    let status = out.path().join("status.json");
    let run = |now: &'static str| {
        let mut o = options(dest.path(), now);
        o.status = Some(&status);
        o
    };

    let first = copy_days(&flaky, &run("2026-10-21T02:30:00Z"))
        .await
        .unwrap();
    assert_eq!(first.last_success, Some(at("2026-10-21T02:30:00Z")));

    // A new day the lake will not give: the run fails to catch up and keeps the old time.
    lake.put_day("2026-10-21", 1);
    flaky.fail("2026-10-21", Fault::Gone);
    let second = copy_days(&flaky, &run("2026-10-22T02:30:00Z"))
        .await
        .unwrap();
    assert_eq!(second.last_success, Some(at("2026-10-21T02:30:00Z")));
    assert_eq!(second.missing_days, [day("2026-10-21")]);

    // What a watcher reads is the same report, as JSON with the day names and counts.
    let written: Report = serde_json::from_slice(&std::fs::read(&status).unwrap()).unwrap();
    assert_eq!(written, second);
    let text = std::fs::read_to_string(&status).unwrap();
    assert!(text.contains("2026-10-21") && !text.contains(SENTINEL));

    flaky.mend();
    let third = copy_days(&flaky, &run("2026-10-22T08:00:00Z"))
        .await
        .unwrap();
    assert_eq!(third.last_success, Some(at("2026-10-22T08:00:00Z")));

    // No other file is left beside the status file.
    assert_eq!(
        std::fs::read_dir(out.path()).unwrap().count(),
        1,
        "the status file is written through a temporary file that is renamed"
    );
}

#[tokio::test]
async fn a_day_gone_from_the_lake_stays_in_the_copy() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    lake.put_day("2026-10-21", 1);
    let dest = tempfile::tempdir().unwrap();
    let o = options(dest.path(), "2026-10-23T06:00:00Z");
    copy_days(&lake.store(), &o).await.unwrap();

    std::fs::remove_file(lake.dir.path().join("days/2026-10-20.jsonl")).unwrap();
    let report = copy_days(&lake.store(), &o).await.unwrap();

    assert_eq!(report.local_only_days, [day("2026-10-20")]);
    assert_eq!(report.source_days, 1);
    assert!(report.ok);
    assert_eq!(names(dest.path()), ["2026-10-20.jsonl", "2026-10-21.jsonl"]);
}

#[tokio::test]
async fn without_a_creation_time_a_day_is_written_an_hour_after_it_ends() {
    let lake = Lake::new();
    lake.put_day("2026-10-20", 1);
    let mut flaky = Faulty::new(&lake);
    flaky.no_created = true;
    flaky.fail("2026-10-20", Fault::Gone);
    let dest = tempfile::tempdir().unwrap();

    // Written 2026-10-21 01:00 by the rule: 24 hours later is not overdue, a second after is.
    let report = copy_days(&flaky, &options(dest.path(), "2026-10-22T01:00:00Z"))
        .await
        .unwrap();
    assert!(report.overdue_days.is_empty() && report.missing_days.len() == 1);
    let report = copy_days(&flaky, &options(dest.path(), "2026-10-22T01:00:01Z"))
        .await
        .unwrap();
    assert_eq!(report.overdue_days, [day("2026-10-20")]);
}

#[tokio::test]
async fn an_empty_lake_is_a_clean_run() {
    let lake = Lake::new();
    let dest = tempfile::tempdir().unwrap();

    let report = copy_days(&lake.store(), &options(dest.path(), "2026-10-21T06:00:00Z"))
        .await
        .unwrap();

    assert!(report.ok);
    assert_eq!((report.source_days, report.copied_days), (0, 0));
    assert_eq!(report.last_success, Some(at("2026-10-21T06:00:00Z")));
    assert_eq!(report.newest_source_day, None);
}

#[test]
fn day_files_are_named_as_the_api_names_them() {
    let d = day("2026-10-20");
    assert_eq!(source_path(d), usnm_api::searchlog::day_path(d));
    assert_eq!(day_of_path(&source_path(d)), Some(d));
    assert_eq!(day_of_path("days/2026-10-20.jsonl.partial"), None);
    assert_eq!(day_of_path("staging/2026-10-20.jsonl"), None);
}

#[test]
fn a_day_file_is_not_written_before_the_lake_would_have() {
    // The fallback rule restates the API's: a day closes an hour after it ends.
    assert_eq!(
        WRITTEN_AFTER_DAY_END,
        usnm_api::searchlog::CLOSE_AFTER,
        "update searchlog_copy::WRITTEN_AFTER_DAY_END"
    );
}
