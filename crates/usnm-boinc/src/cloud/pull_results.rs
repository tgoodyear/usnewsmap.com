//! `cloud pull-results`: finished workunits the cloud server exported to a results bucket, into
//! the same spool `cloud pull` writes (see `pull.rs` for the layout and the staging rules).
//!
//! The bucket holds a ledger `ready/<workunit id>-<run>.json` per publication and, for a
//! success, the output files under the ledger's `files_prefix` (`files/<id>-<run>/`). A failed
//! workunit has a ledger and no files. The ledger's `workunit` is the object
//! `GET /admin/v1/assimilate` lists, so the spool writer is the one `cloud pull` uses, with the
//! file bytes read from the bucket instead of the admin API. Only `ready/` is listed: `export/`
//! belongs to the exporter. A workunit can be published twice under different runs; the second
//! write finds the files in place and verified.
//!
//! Unless `--no-delete`, a ledger is deleted once its workunit is in the spool, and then the
//! objects under its files prefix. Run one puller per spool.
use super::pull::{pull_workunit, Source, Stats, Workunit};
use anyhow::{bail, ensure, Context};
use serde::Deserialize;
use std::{fmt, path::PathBuf, sync::Arc};
use usnm_store::ObjectStore;

pub struct Config {
    pub store: Arc<dyn ObjectStore>,
    pub spool: PathBuf,
    /// Only workunits of these apps (the ledger's `workunit.app`); empty means all.
    pub apps: Vec<String>,
    /// Shadow mode: leave the ledger and the files in the bucket.
    pub no_delete: bool,
}

#[derive(Debug, Default)]
pub struct Report {
    /// Ledgers taken up (written to the spool, or failed trying), not counting `--app` skips.
    pub ledgers: usize,
    pub files_written: usize,
    pub files_skipped: usize,
    pub ledgers_deleted: usize,
    pub files_deleted: usize,
    /// Ledgers left alone because their app is not one of `--app`.
    pub filtered: usize,
    /// Keys under `ready/` that are not a ledger name.
    pub ignored: usize,
    pub failed: Vec<String>,
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "pulled {} ledgers ({} files written, {} already present), deleted {} ledgers and {} files, \
             ignored {} keys, left {} for other apps, failed {}",
            self.ledgers,
            self.files_written,
            self.files_skipped,
            self.ledgers_deleted,
            self.files_deleted,
            self.ignored,
            self.filtered,
            self.failed.len()
        )?;
        for w in &self.failed {
            write!(f, "\n  failed: {w}")?;
        }
        Ok(())
    }
}

#[derive(Deserialize)]
struct Ledger {
    schema: u32,
    files_prefix: String,
    workunit: Workunit,
}

/// `(workunit id, run)` of a key `ready/<digits>-<[0-9a-f]+>.json`.
fn parse_key(key: &str) -> Option<(i64, &str)> {
    let name = key.strip_prefix("ready/")?.strip_suffix(".json")?;
    let (id, run) = name.split_once('-')?;
    let digits = !id.is_empty() && id.bytes().all(|b| b.is_ascii_digit());
    let hexrun = !run.is_empty() && run.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'));
    if digits && hexrun {
        Some((id.parse().ok()?, run))
    } else {
        None
    }
}

/// One pass over the bucket's ready ledgers. A ledger that fails stays in the bucket and is
/// listed in `failed`; the others continue. Errors only when the pass cannot start.
pub async fn pull_results_once(cfg: &Config, limit: Option<usize>) -> anyhow::Result<Report> {
    ensure!(
        cfg.spool.is_dir(),
        "--spool-dir {} does not exist; create it first (and sync its parent) so it is durable",
        cfg.spool.display()
    );
    let mut rep = Report::default();
    let mut ledgers = Vec::new();
    for o in cfg
        .store
        .list_info("ready")
        .await
        .context("listing ready/ in the results bucket")?
    {
        match parse_key(&o.path) {
            Some((id, _)) => ledgers.push((id, o)),
            None => rep.ignored += 1,
        }
    }
    ledgers.sort_by(|a, b| (a.0, &a.1.path).cmp(&(b.0, &b.1.path)));
    let mut left = limit.unwrap_or(usize::MAX);
    for (id, info) in ledgers {
        if left == 0 {
            break;
        }
        // A ledger that fails counts against --limit like one taken; one left for other apps or
        // gone since the listing does not.
        match one(cfg, id, &info, &mut rep).await {
            Ok(Took::Filtered | Took::Gone) => {}
            Ok(Took::Taken) => {
                left -= 1;
                rep.ledgers += 1;
            }
            Err(e) => {
                left -= 1;
                rep.ledgers += 1;
                rep.failed.push(format!("{}: {e:#}", info.path));
            }
        }
    }
    Ok(rep)
}

async fn one(
    cfg: &Config,
    id: i64,
    info: &usnm_store::ObjectInfo,
    rep: &mut Report,
) -> anyhow::Result<Took> {
    let key = &info.path;
    let (_, run) = parse_key(key).expect("listed ledger keys are parsed");
    // Gone since the listing: another pass took it.
    let Some(bytes) = cfg.store.get(key).await? else {
        return Ok(Took::Gone);
    };
    let ledger: Ledger = serde_json::from_slice(&bytes).context("parsing the ledger")?;
    ensure!(
        ledger.schema == 2,
        "ledger schema {}, expected 2",
        ledger.schema
    );
    let want = format!("files/{id}-{run}/");
    ensure!(
        ledger.files_prefix == want,
        "files_prefix {:?} does not match the key, expected {want:?}",
        ledger.files_prefix
    );
    ensure!(
        ledger.workunit.id == id,
        "the ledger's workunit id {} is not {id}",
        ledger.workunit.id
    );
    if !cfg.apps.is_empty() && !cfg.apps.contains(&ledger.workunit.app) {
        rep.filtered += 1;
        return Ok(Took::Filtered);
    }
    let src = Source::Store {
        store: cfg.store.as_ref(),
        files_prefix: &ledger.files_prefix,
    };
    let mut st = Stats::default();
    let r = pull_workunit(&cfg.spool, &src, &ledger.workunit, &mut st).await;
    rep.files_written += st.files_written;
    rep.files_skipped += st.files_skipped;
    r?;
    if cfg.no_delete {
        return Ok(Took::Taken);
    }
    // The ledger first, so a pass that dies between the two never sees a ledger without files.
    // A ledger already gone or written again is not this pass's, and neither are its files.
    if !cfg
        .store
        .delete_if_match(key, &info.etag)
        .await
        .context("deleting the ledger")?
    {
        return Ok(Took::Taken);
    }
    rep.ledgers_deleted += 1;
    let dir = ledger.files_prefix.trim_end_matches('/');
    let mut failed = 0usize;
    for o in cfg
        .store
        .list_info(dir)
        .await
        .context("listing the files")?
    {
        match cfg.store.delete_if_match(&o.path, &o.etag).await {
            Ok(true) => rep.files_deleted += 1,
            Ok(false) => {}
            Err(e) => {
                failed += 1;
                eprintln!("deleting {}: {e}", o.path);
            }
        }
    }
    if failed > 0 {
        bail!("{failed} file(s) under {dir}/ were not deleted");
    }
    Ok(Took::Taken)
}

/// What became of one listed ledger.
enum Took {
    Taken,
    /// Its app is not one of `--app`.
    Filtered,
    /// Gone since the listing.
    Gone,
}

#[cfg(test)]
mod tests {
    use super::*;
    use md5::{Digest, Md5};
    use serde_json::{json, Value};
    use std::{fs, path::Path};
    use usnm_store::LocalStore;

    fn md5_of(b: &[u8]) -> String {
        Md5::digest(b).iter().map(|x| format!("{x:02x}")).collect()
    }

    struct Bucket {
        _dir: tempfile::TempDir,
        store: Arc<LocalStore>,
        spool: tempfile::TempDir,
    }

    fn bucket() -> Bucket {
        let dir = tempfile::tempdir().unwrap();
        Bucket {
            store: Arc::new(LocalStore::new(dir.path())),
            _dir: dir,
            spool: tempfile::tempdir().unwrap(),
        }
    }

    impl Bucket {
        fn cfg(&self, apps: &[&str], no_delete: bool) -> Config {
            Config {
                store: self.store.clone(),
                spool: self.spool.path().to_owned(),
                apps: apps.iter().map(|a| (*a).to_owned()).collect(),
                no_delete,
            }
        }

        async fn put(&self, key: &str, body: &[u8]) {
            self.store.put(key, body.to_vec(), "x").await.unwrap();
        }

        async fn keys(&self) -> Vec<String> {
            let mut all = Vec::new();
            for p in ["ready", "files", "export"] {
                all.extend(self.store.list(p).await.unwrap());
            }
            all
        }

        /// A ledger for workunit `id` (batch 3, app usnm_test) with one file per entry; the file
        /// objects are put unless `files_present` is false. Returns the ledger JSON.
        async fn publish(&self, id: i64, run: &str, files: &[(&str, &[u8])]) -> Value {
            let listed: Vec<Value> = files
                .iter()
                .enumerate()
                .map(|(i, (name, body))| {
                    json!({"name": format!("{id}_{name}"), "open_name": name, "index": i,
                           "present": true, "nbytes": body.len(), "md5": md5_of(body)})
                })
                .collect();
            for (name, body) in files {
                self.put(&format!("files/{id}-{run}/{id}_{name}"), body)
                    .await;
            }
            let l = ledger(id, run, listed);
            self.put(&format!("ready/{id}-{run}.json"), l.to_string().as_bytes())
                .await;
            l
        }
    }

    fn ledger(id: i64, run: &str, files: Vec<Value>) -> Value {
        json!({"schema": 2, "exported_at": 1, "run": run,
               "files_prefix": format!("files/{id}-{run}/"),
               "workunit": {"id": id, "name": format!("wu_{id}"), "app": "usnm_test", "batch": 3,
                            "error_mask": 0,
                            "canonical_result": {"id": id * 10, "name": "r", "host_id": 9,
                                                 "files": files}}})
    }

    fn read(p: std::path::PathBuf) -> Vec<u8> {
        fs::read(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
    }

    fn incoming_is_empty(spool: &Path) -> bool {
        fs::read_dir(spool.join(".incoming"))
            .unwrap()
            .next()
            .is_none()
    }

    #[tokio::test]
    async fn writes_the_spool_and_deletes_the_ledger_and_files() {
        let b = bucket();
        b.publish(7, "ab12", &[("result.json", b"{}"), ("text.txt", b"hello")])
            .await;
        b.publish(2, "cd34", &[("result.json", b"x")]).await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(
            (
                r.ledgers,
                r.files_written,
                r.ledgers_deleted,
                r.files_deleted
            ),
            (2, 3, 2, 3)
        );
        let d = b.spool.path().join("results/3");
        assert_eq!(read(d.join("wu_7__file_result.json")), b"{}");
        assert_eq!(read(d.join("wu_7__file_text.txt")), b"hello");
        assert_eq!(read(d.join("wu_7__host_id")), b"9\n");
        assert_eq!(read(d.join("wu_2__file_result.json")), b"x");
        assert!(incoming_is_empty(b.spool.path()));
        assert!(b.keys().await.is_empty(), "{:?}", b.keys().await);
    }

    #[tokio::test]
    async fn a_failed_workunit_leaves_an_error_marker() {
        let b = bucket();
        let mut l = ledger(4, "aa", vec![]);
        l["workunit"]["error_mask"] = json!(34);
        l["workunit"]["canonical_result"] = Value::Null;
        b.put("ready/4-aa.json", l.to_string().as_bytes()).await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!(
            (r.failed.len(), r.ledgers_deleted, r.files_deleted),
            (0, 1, 0)
        );
        assert_eq!(read(b.spool.path().join("results/3/wu_4_error")), b"0x22\n");
        assert!(b.keys().await.is_empty());
    }

    #[tokio::test]
    async fn no_delete_leaves_everything_and_a_later_pass_takes_it() {
        let b = bucket();
        b.publish(1, "aa", &[("result.json", b"{}")]).await;
        let before = b.keys().await;
        let r = pull_results_once(&b.cfg(&[], true), None).await.unwrap();
        assert_eq!((r.ledgers, r.ledgers_deleted, r.files_deleted), (1, 0, 0));
        assert_eq!(b.keys().await, before);
        assert!(b.spool.path().join("results/3/wu_1__host_id").exists());
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!(
            (r.files_written, r.files_skipped, r.ledgers_deleted),
            (0, 1, 1)
        );
        assert!(b.keys().await.is_empty());
    }

    #[tokio::test]
    async fn a_bad_md5_fails_only_that_workunit() {
        let b = bucket();
        b.publish(1, "aa", &[("result.json", b"good")]).await;
        // The object differs from what the ledger lists (same size).
        b.put("files/1-aa/1_result.json", b"evil").await;
        b.publish(2, "bb", &[("result.json", b"fine")]).await;
        // A short object is refused too.
        b.publish(3, "cc", &[("result.json", b"longer")]).await;
        b.put("files/3-cc/3_result.json", b"no").await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!(r.failed.len(), 2, "{:?}", r.failed);
        assert!(r.failed[0].contains("ready/1-aa.json") && r.failed[0].contains("md5"));
        assert!(r.failed[1].contains("ready/3-cc.json") && r.failed[1].contains("bytes"));
        let d = b.spool.path().join("results/3");
        assert!(!d.join("wu_1__file_result.json").exists());
        assert!(!d.join("wu_1__host_id").exists());
        assert!(d.join("wu_2__host_id").exists());
        assert!(incoming_is_empty(b.spool.path()));
        let mut keys = b.keys().await;
        keys.sort();
        assert_eq!(
            keys,
            [
                "files/1-aa/1_result.json",
                "files/3-cc/3_result.json",
                "ready/1-aa.json",
                "ready/3-cc.json"
            ]
        );
    }

    #[tokio::test]
    async fn a_missing_file_object_fails_the_workunit() {
        let b = bucket();
        b.publish(1, "aa", &[("result.json", b"x")]).await;
        let o = b.store.list_info("files").await.unwrap();
        b.store
            .delete_if_match(&o[0].path, &o[0].etag)
            .await
            .unwrap();
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!(r.failed.len(), 1);
        assert!(
            r.failed[0].contains("not in the results bucket"),
            "{:?}",
            r.failed
        );
        assert_eq!(b.keys().await, ["ready/1-aa.json"]);
    }

    #[tokio::test]
    async fn a_second_publication_of_a_workunit_is_fine() {
        let b = bucket();
        b.publish(5, "aa", &[("result.json", b"{}")]).await;
        b.publish(5, "bb", &[("result.json", b"{}")]).await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert!(r.failed.is_empty(), "{:?}", r.failed);
        assert_eq!(
            (
                r.ledgers,
                r.files_written,
                r.files_skipped,
                r.ledgers_deleted,
                r.files_deleted
            ),
            (2, 1, 1, 2, 2)
        );
        assert_eq!(
            read(b.spool.path().join("results/3/wu_5__file_result.json")),
            b"{}"
        );
        assert!(b.keys().await.is_empty());
    }

    #[tokio::test]
    async fn a_ledger_that_does_not_match_its_key_is_refused() {
        let b = bucket();
        b.publish(1, "aa", &[("result.json", b"x")]).await;
        // files_prefix of another publication.
        let mut l = ledger(2, "bb", vec![]);
        l["files_prefix"] = json!("files/9-bb/");
        b.put("ready/2-bb.json", l.to_string().as_bytes()).await;
        // Wrong schema, wrong workunit id, bad JSON.
        let mut l = ledger(3, "cc", vec![]);
        l["schema"] = json!(1);
        b.put("ready/3-cc.json", l.to_string().as_bytes()).await;
        let mut l = ledger(4, "dd", vec![]);
        l["workunit"]["id"] = json!(99);
        b.put("ready/4-dd.json", l.to_string().as_bytes()).await;
        b.put("ready/5-ee.json", b"{not json").await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!(r.failed.len(), 4, "{:?}", r.failed);
        assert!(r
            .failed
            .iter()
            .any(|f| f.contains("2-bb") && f.contains("files_prefix")));
        assert!(r
            .failed
            .iter()
            .any(|f| f.contains("3-cc") && f.contains("schema")));
        assert!(r
            .failed
            .iter()
            .any(|f| f.contains("4-dd") && f.contains("workunit id")));
        assert!(r
            .failed
            .iter()
            .any(|f| f.contains("5-ee") && f.contains("parsing")));
        let d = b.spool.path().join("results");
        assert!(d.join("3/wu_1__host_id").exists());
        assert!(!d.join("3/wu_2_no_output_files").exists());
        assert!(!d.join("3/wu_99_no_output_files").exists());
        let keys = b.keys().await;
        for k in [
            "ready/2-bb.json",
            "ready/3-cc.json",
            "ready/4-dd.json",
            "ready/5-ee.json",
        ] {
            assert!(keys.iter().any(|x| x == k), "{k}");
        }
        assert!(!keys.iter().any(|x| x == "ready/1-aa.json"));
    }

    #[tokio::test]
    async fn an_unsafe_name_in_a_ledger_is_refused_before_any_write() {
        let b = bucket();
        let f = json!({"name": "f", "open_name": "../escape", "index": 0, "present": true,
                       "nbytes": 1, "md5": md5_of(b"x")});
        b.put(
            "ready/1-aa.json",
            ledger(1, "aa", vec![f]).to_string().as_bytes(),
        )
        .await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!(r.failed.len(), 1);
        assert!(!b.spool.path().join("results").exists());
        assert_eq!(b.keys().await, ["ready/1-aa.json"]);
    }

    #[tokio::test]
    async fn export_and_junk_keys_are_ignored() {
        let b = bucket();
        b.put("export/cursor.json", br#"{"after":0,"until":0}"#)
            .await;
        b.put("ready/notes.txt", b"hi").await;
        b.put("ready/12-XYZ.json", b"{}").await;
        b.put("ready/x-ab.json", b"{}").await;
        b.put("ready/12.json", b"{}").await;
        b.put("ready/12-ab.json.bak", b"{}").await;
        b.put("ready/sub/1-aa.json", b"{}").await;
        b.publish(1, "aa", &[("result.json", b"x")]).await;
        let r = pull_results_once(&b.cfg(&[], false), None).await.unwrap();
        assert_eq!((r.ledgers, r.ignored, r.failed.len()), (1, 6, 0));
        let mut keys = b.keys().await;
        keys.sort();
        assert_eq!(
            keys,
            [
                "export/cursor.json",
                "ready/12-XYZ.json",
                "ready/12-ab.json.bak",
                "ready/12.json",
                "ready/notes.txt",
                "ready/sub/1-aa.json",
                "ready/x-ab.json"
            ]
        );
    }

    #[tokio::test]
    async fn the_app_filter_leaves_other_apps_alone() {
        let b = bucket();
        b.publish(1, "aa", &[("result.json", b"x")]).await;
        let mut l = ledger(2, "bb", vec![]);
        l["workunit"]["app"] = json!("other_app");
        b.put("ready/2-bb.json", l.to_string().as_bytes()).await;
        let r = pull_results_once(&b.cfg(&["usnm_test"], false), None)
            .await
            .unwrap();
        assert_eq!((r.ledgers, r.filtered, r.failed.len()), (1, 1, 0));
        assert!(b.spool.path().join("results/3/wu_1__host_id").exists());
        assert!(!b
            .spool
            .path()
            .join("results/3/wu_2_no_output_files")
            .exists());
        assert_eq!(b.keys().await, ["ready/2-bb.json"]);
        let r = pull_results_once(&b.cfg(&["other_app", "usnm_test"], false), None)
            .await
            .unwrap();
        assert_eq!((r.ledgers, r.filtered), (1, 0));
        assert!(b.keys().await.is_empty());
    }

    #[tokio::test]
    async fn limit_takes_the_lowest_workunit_ids_first() {
        let b = bucket();
        for id in [10, 9, 100] {
            b.publish(id, "aa", &[("result.json", b"x")]).await;
        }
        let r = pull_results_once(&b.cfg(&[], false), Some(2))
            .await
            .unwrap();
        assert_eq!(r.ledgers, 2);
        assert_eq!(b.keys().await.len(), 2, "{:?}", b.keys().await);
        assert!(b.store.exists("ready/100-aa.json").await.unwrap());
    }

    #[tokio::test]
    async fn a_failed_ledger_counts_against_the_limit() {
        let b = bucket();
        b.put("ready/1-aa.json", b"{not json").await;
        b.publish(2, "bb", &[("result.json", b"x")]).await;
        let r = pull_results_once(&b.cfg(&[], false), Some(1))
            .await
            .unwrap();
        assert_eq!((r.ledgers, r.failed.len()), (1, 1));
        assert!(b.store.exists("ready/2-bb.json").await.unwrap());
    }

    #[tokio::test]
    async fn a_missing_spool_is_refused_and_an_empty_bucket_is_fine() {
        let b = bucket();
        let mut c = b.cfg(&[], false);
        let r = pull_results_once(&c, None).await.unwrap();
        assert_eq!((r.ledgers, r.ignored), (0, 0));
        c.spool = b.spool.path().join("nope");
        let e = pull_results_once(&c, None).await.unwrap_err();
        assert!(e.to_string().contains("does not exist"), "{e:#}");
    }

    #[test]
    fn ledger_key_names() {
        assert_eq!(parse_key("ready/12-ab0f.json"), Some((12, "ab0f")));
        for bad in [
            "ready/-ab.json",
            "ready/1-.json",
            "ready/1-AB.json",
            "ready/1-ab-cd.json",
            "ready/1-g.json",
            "export/1-ab.json",
            "ready/99999999999999999999-ab.json",
        ] {
            assert_eq!(parse_key(bad), None, "{bad}");
        }
    }
}
