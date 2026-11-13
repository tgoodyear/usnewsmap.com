//! `cloud spool-out`: hands the spool that `cloud pull` wrote to the hand-off store, as the
//! bridge's `spool-out` step does for the stock assimilator's spool, by calling
//! `handoff::put_spool` for each finished single-page job. Dev cloud server only; nothing in
//! the running bridge calls it. See docs/boinc/server/bridge-steps.md, "Hand the cloud spool
//! off".
//!
//! What it reads: `results/<batch>/usnm_<job id>__file_{result.json,text.txt,layout.xml}`,
//! `usnm_<job id>_error`, `usnm_<job id>_no_output_files` and the `usnm_<job id>__host_id`
//! sidecar. A job is finished when it has an error or no-output marker, or all three result
//! files and the host id sidecar (`cloud pull` writes that last), which is the completeness
//! test `batch-inventory.py` applies plus the sidecar.
//!
//! What it writes: only the hand-off store (through `put_spool`) and a ledger under
//! `<spool>/.handed-off/<workunit>.json` holding a fingerprint of what was handed off. It never
//! deletes or renames a spool file; the stock flow does not either at this point (later,
//! `archive_history.py bridge` moves drained jobs' spool files).
use super::pull::sync_dir;
use crate::handoff::{self, Spooled};
use anyhow::{bail, ensure, Context};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fmt, fs,
    path::{Path, PathBuf},
};
use usnm_ocr_contract::boinc;

pub struct Config {
    pub spool: PathBuf,
    /// The hand-off store (`handoff put-spool --to-url`); not needed for a dry run.
    pub to_url: Option<String>,
    /// A `gold/trust.json` to put with each job, as the bridge does when the share has one.
    pub trust: Option<PathBuf>,
    pub dry_run: bool,
    /// Hand off again even where the ledger says it was done.
    pub redo: bool,
}

#[derive(Debug, Default)]
pub struct Report {
    pub handed_off: Vec<String>,
    pub already: usize,
    pub incomplete: Vec<String>,
    pub unsupported: Vec<String>,
    pub failed: Vec<String>,
    /// Entries under `results/` that are no job's spool file and no bundle's (a workunit with
    /// another naming, a stray file), so "nothing recognised" shows apart from "nothing to do".
    pub ignored: usize,
    /// The first few of them (`MAX_IGNORED_EXAMPLES`), as `<batch>/<name>`.
    pub ignored_examples: Vec<String>,
}

/// How many ignored names the report keeps to show.
pub const MAX_IGNORED_EXAMPLES: usize = 5;

impl Report {
    fn ignore(&mut self, batch: &Path, name: &str) {
        self.ignored += 1;
        if self.ignored_examples.len() < MAX_IGNORED_EXAMPLES {
            let b = batch.file_name().and_then(|n| n.to_str()).unwrap_or("?");
            self.ignored_examples.push(format!("{b}/{name}"));
        }
    }
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "handed off {}, already done {}, incomplete {}, not handled {}, failed {}, ignored {}",
            self.handed_off.len(),
            self.already,
            self.incomplete.len(),
            self.unsupported.len(),
            self.failed.len(),
            self.ignored
        )?;
        for (label, list) in [
            ("handed off", &self.handed_off),
            ("incomplete", &self.incomplete),
            ("not handled", &self.unsupported),
            ("failed", &self.failed),
        ] {
            for w in list {
                write!(f, "\n  {label}: {w}")?;
            }
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct Ledger {
    fingerprint: String,
    outcome: String,
}

/// The limit `put_spool` puts on the trust file (`MAX_TRUST` in handoff.rs, not public).
const MAX_TRUST: usize = 16 << 20;

/// The file kinds a job's spool can hold, with the host id sidecar.
const SUFFIXES: [&str; 6] = [
    "__file_result.json",
    "__file_text.txt",
    "__file_layout.xml",
    "_error",
    "_no_output_files",
    "__host_id",
];

/// One job's files in one batch directory.
#[derive(Default)]
struct Unit {
    dir: Option<PathBuf>,
    dirs: usize,
    suffixes: Vec<&'static str>,
}

pub async fn spool_out_once(cfg: &Config) -> anyhow::Result<Report> {
    ensure!(
        cfg.spool.is_dir(),
        "--spool-dir {} does not exist",
        cfg.spool.display()
    );
    let store = match (&cfg.to_url, cfg.dry_run) {
        (Some(u), false) => Some(usnm_store::open(u)?),
        (None, false) => bail!("--to-url is required unless --dry-run"),
        (_, true) => None,
    };
    let mut report = Report::default();
    // job id -> where its files are. A bundle's workunit (`usnm_ocrb_`) and any other name are
    // not a single-page job's and are listed as not handled.
    let mut units: BTreeMap<String, Unit> = BTreeMap::new();
    let results = cfg.spool.join("results");
    if results.is_dir() {
        for batch in sorted_entries(&results)? {
            if !is_real_dir(&batch)? {
                report.ignore(
                    &results,
                    &batch.file_name().unwrap_or_default().to_string_lossy(),
                );
                continue;
            }
            let mut seen_here: BTreeMap<String, ()> = BTreeMap::new();
            for path in sorted_entries(&batch)? {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    report.ignore(
                        &batch,
                        &path.file_name().unwrap_or_default().to_string_lossy(),
                    );
                    continue;
                };
                if !is_real_file(&path)? {
                    report.ignore(&batch, name);
                    continue;
                }
                let Some(rest) = name.strip_prefix("usnm_") else {
                    report.ignore(&batch, name);
                    continue;
                };
                if let Some(bundle) = rest.strip_prefix("ocrb_") {
                    if let Some(b) = bundle.get(..64).filter(|b| boinc::is_sha256(b)) {
                        let wu = format!("usnm_ocrb_{b}");
                        if !report.unsupported.contains(&wu) {
                            report.unsupported.push(wu);
                        }
                    } else {
                        report.ignore(&batch, name);
                    }
                    continue;
                }
                let Some(job) = rest.get(..64).filter(|j| boinc::is_sha256(j)) else {
                    report.ignore(&batch, name);
                    continue;
                };
                let Some(suffix) = SUFFIXES.iter().find(|s| &rest[64..] == **s) else {
                    report.ignore(&batch, name);
                    continue;
                };
                let u = units.entry(job.to_owned()).or_default();
                if seen_here.insert(job.to_owned(), ()).is_none() {
                    u.dirs += 1;
                }
                u.dir = Some(batch.clone());
                u.suffixes.push(suffix);
            }
        }
    }
    for (id, unit) in units {
        let wu = format!("usnm_{id}");
        if unit.dirs > 1 {
            // As batch-inventory.py: one job, one spool.
            report
                .failed
                .push(format!("{wu}: files in more than one batch directory"));
            continue;
        }
        let dir = unit.dir.expect("a unit has a directory");
        let has = |s: &str| unit.suffixes.contains(&s);
        let marker = has("_error") || has("_no_output_files");
        let files = has("__file_result.json") && has("__file_text.txt") && has("__file_layout.xml");
        if !(marker || (files && has("__host_id"))) {
            report.incomplete.push(wu);
            continue;
        }
        match hand_off(cfg, store.as_deref(), &dir, &id, &wu).await {
            Ok(Outcome::Done) => report.handed_off.push(wu),
            Ok(Outcome::Already) => report.already += 1,
            Err(e) => report.failed.push(format!("{wu}: {e:#}")),
        }
    }
    Ok(report)
}

enum Outcome {
    Done,
    Already,
}

async fn hand_off(
    cfg: &Config,
    store: Option<&dyn usnm_store::ObjectStore>,
    dir: &Path,
    id: &str,
    wu: &str,
) -> anyhow::Result<Outcome> {
    let host_id = read_host_id(&dir.join(format!("{wu}__host_id")))?;
    // The destination is part of the fingerprint, so a job handed to one store is handed to
    // another, and a ledger from a different --to-url never skips a delivery.
    let fingerprint = fingerprint(
        dir,
        wu,
        host_id,
        cfg.trust.as_deref(),
        cfg.to_url.as_deref().unwrap_or(""),
    )?;
    let ledger_path = cfg.spool.join(".handed-off").join(format!("{wu}.json"));
    if !cfg.redo {
        if let Ok(bytes) = fs::read(&ledger_path) {
            if serde_json::from_slice::<Ledger>(&bytes).is_ok_and(|l| l.fingerprint == fingerprint)
            {
                return Ok(Outcome::Already);
            }
        }
    }
    let Some(store) = store else {
        // Dry run: say it would be handed off, touch nothing.
        return Ok(Outcome::Done);
    };
    let mut keep: Vec<String> = handoff::spool_names(wu)
        .into_iter()
        .map(|(n, _)| n)
        .filter(|n| dir.join(n).exists())
        .collect();
    if host_id.is_some() {
        keep.push("host_id".to_owned());
    }
    remove_stale(store, id, &keep).await?;
    let spooled = handoff::put_spool(store, None, dir, id, host_id, cfg.trust.as_deref()).await?;
    let outcome = match spooled {
        Spooled::Files(n) => format!("files:{n}"),
        Spooled::Requeued(why) => format!("requeued:{why}"),
    };
    // After the store has the objects, so a crash before this line repeats the (idempotent,
    // overwriting) put and loses nothing.
    let root = &cfg.spool;
    fs::create_dir_all(ledger_path.parent().unwrap())?;
    sync_dir(root)?;
    crate::atomic(
        &ledger_path,
        &serde_json::to_vec(&Ledger {
            fingerprint,
            outcome,
        })?,
    )?;
    Ok(Outcome::Done)
}

/// `put_spool` only writes; it never removes. An object left by an earlier hand-off that the
/// spool no longer produces (a marker the job outgrew, result files a marker replaced, a host id
/// that went away) would still be copied by `get_spool` and read by `collect`, which would take
/// the job for a terminal failure or the wrong host. The writer is bridge code and stays as it
/// is; here those objects are removed through the store before the new put.
///
/// Only this job's own names under `spool/<id>/` can go: the five spool names `put_spool`
/// writes and `host_id`, and only those the current spool will not write (`keep`). Anything
/// else under the prefix, and `gold/trust.json` (which is outside the prefix and put_spool's own
/// to handle), is left alone. A store that cannot list or delete, or an object that changed
/// during removal, fails the job with a message, and no ledger entry is written.
async fn remove_stale(
    store: &dyn usnm_store::ObjectStore,
    id: &str,
    keep: &[String],
) -> anyhow::Result<()> {
    // The prefix is built from nothing but a checked job id, so it cannot widen.
    ensure!(boinc::is_sha256(id), "invalid job id {id:?}");
    let prefix = format!("spool/{id}/");
    let wu = format!("usnm_{id}");
    let mut ours: Vec<String> = handoff::spool_names(&wu)
        .into_iter()
        .map(|(n, _)| n)
        .collect();
    ours.push("host_id".to_owned());
    let stale: Vec<String> = ours
        .into_iter()
        .filter(|n| !keep.contains(n))
        .map(|n| format!("{prefix}{n}"))
        .collect();
    let listed = store
        .list_info(&format!("spool/{id}"))
        .await
        .map_err(|e| anyhow::anyhow!("cannot list {prefix} in the hand-off store ({e})"))?;
    for o in listed {
        if !stale.contains(&o.path) {
            continue;
        }
        debug_assert!(o.path.starts_with(&prefix));
        match store.delete_if_match(&o.path, &o.etag).await {
            Ok(true) => {}
            Ok(false) => bail!("the stale {} changed while it was being removed; run again", o.path),
            Err(e) => bail!(
                "the hand-off store holds {} from an earlier hand-off that the spool no longer \
                 produces, and the store cannot delete it ({e}); remove that object, then run again",
                o.path
            ),
        }
    }
    Ok(())
}

/// The sidecar's decimal host id. 0 (what `cloud pull` writes when the server gave none) and a
/// missing sidecar are no host id, as the bridge passes none when the database has no
/// canonical result.
fn read_host_id(path: &Path) -> anyhow::Result<Option<u64>> {
    if !path.try_exists()? {
        return Ok(None);
    }
    let text = fs::read_to_string(path).with_context(|| path.display().to_string())?;
    let n: u64 = text.trim().parse().context("bad host id sidecar")?;
    Ok(if n == 0 { None } else { Some(n) })
}

/// Over the names `put_spool` reads, the host id and the trust file, so a change to any of
/// them is handed off again.
fn fingerprint(
    dir: &Path,
    wu: &str,
    host_id: Option<u64>,
    trust: Option<&Path>,
    destination: &str,
) -> anyhow::Result<String> {
    let mut h = Sha256::new();
    h.update(format!("to:{destination}\n").as_bytes());
    for (name, max) in handoff::spool_names(wu) {
        let p = dir.join(&name);
        if p.exists() {
            // The limit `put_spool` applies, so an oversized output is this job's failure,
            // also in a dry run, and nothing unbounded is read.
            let bytes = crate::read(&p, max).with_context(|| p.display().to_string())?;
            h.update((name.len() as u64).to_be_bytes());
            h.update(name.as_bytes());
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(&bytes);
        }
    }
    h.update(format!("host:{host_id:?}").as_bytes());
    if let Some(t) = trust {
        h.update(crate::read(t, MAX_TRUST).with_context(|| t.display().to_string())?);
    }
    Ok(h.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

fn sorted_entries(dir: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let mut v = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<Vec<_>, _>>()?;
    v.sort();
    Ok(v)
}

fn is_real_dir(p: &Path) -> anyhow::Result<bool> {
    Ok(fs::symlink_metadata(p)?.is_dir())
}

fn is_real_file(p: &Path) -> anyhow::Result<bool> {
    Ok(fs::symlink_metadata(p)?.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const ID2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    struct Fx {
        spool: tempfile::TempDir,
        store: tempfile::TempDir,
    }

    impl Fx {
        fn new() -> Fx {
            Fx {
                spool: tempfile::tempdir().unwrap(),
                store: tempfile::tempdir().unwrap(),
            }
        }
        fn put(&self, batch: &str, name: &str, body: &str) {
            let d = self.spool.path().join("results").join(batch);
            fs::create_dir_all(&d).unwrap();
            fs::write(d.join(name), body).unwrap();
        }
        fn success(&self, id: &str, host: &str) {
            for (suffix, body) in [
                ("__file_result.json", "{}"),
                ("__file_text.txt", "text"),
                ("__file_layout.xml", "<alto/>"),
                ("__host_id", host),
            ] {
                self.put("7", &format!("usnm_{id}{suffix}"), body);
            }
        }
        fn cfg(&self, dry: bool) -> Config {
            Config {
                spool: self.spool.path().to_owned(),
                to_url: (!dry).then(|| self.store.path().to_str().unwrap().to_owned()),
                trust: None,
                dry_run: dry,
                redo: false,
            }
        }
        fn object(&self, path: &str) -> Option<Vec<u8>> {
            fs::read(self.store.path().join(path)).ok()
        }
    }

    #[tokio::test]
    async fn a_finished_job_is_handed_off_and_read_back_by_the_collect_side() {
        let fx = Fx::new();
        fx.success(ID, "42\n");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.failed.len()), (1, 0), "{r}");
        assert_eq!(
            fx.object(&format!("spool/{ID}/usnm_{ID}__file_text.txt"))
                .unwrap(),
            b"text"
        );
        assert_eq!(fx.object(&format!("spool/{ID}/host_id")).unwrap(), b"42");
        // `get_spool` is what the hand-off job's collect calls.
        let store = usnm_store::open(fx.store.path().to_str().unwrap()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let got = handoff::get_spool(store.as_ref(), ID, out.path())
            .await
            .unwrap();
        assert_eq!(got, handoff::Spool::Files(Some(42)));
        assert_eq!(
            fs::read(out.path().join(format!("usnm_{ID}__file_result.json"))).unwrap(),
            b"{}"
        );
        // The spool is untouched: no file was deleted or renamed.
        assert!(fx
            .spool
            .path()
            .join(format!("results/7/usnm_{ID}__file_result.json"))
            .exists());
        assert!(fx
            .spool
            .path()
            .join(format!(".handed-off/usnm_{ID}.json"))
            .exists());
    }

    #[tokio::test]
    async fn a_repeat_run_hands_off_nothing_again() {
        let fx = Fx::new();
        fx.success(ID, "42");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        fs::remove_dir_all(fx.store.path().join("spool")).unwrap();
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.already), (0, 1), "{r}");
        assert!(fx.object(&format!("spool/{ID}/host_id")).is_none());
        // --redo puts it again.
        let mut cfg = fx.cfg(false);
        cfg.redo = true;
        let r = spool_out_once(&cfg).await.unwrap();
        assert_eq!(r.handed_off.len(), 1);
        assert!(fx.object(&format!("spool/{ID}/host_id")).is_some());
    }

    #[tokio::test]
    async fn a_crash_before_the_ledger_repeats_the_put_and_loses_nothing() {
        let fx = Fx::new();
        fx.success(ID, "42");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        // As if the process died after the store write and before the ledger write.
        fs::remove_dir_all(fx.spool.path().join(".handed-off")).unwrap();
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.failed.len()), (1, 0), "{r}");
        assert_eq!(fx.object(&format!("spool/{ID}/host_id")).unwrap(), b"42");
        // A changed sidecar is handed off again.
        fx.put("7", &format!("usnm_{ID}__host_id"), "43");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!(r.handed_off.len(), 1);
        assert_eq!(fx.object(&format!("spool/{ID}/host_id")).unwrap(), b"43");
    }

    #[tokio::test]
    async fn the_ledger_is_per_destination() {
        let fx = Fx::new();
        fx.success(ID, "42");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        let other = tempfile::tempdir().unwrap();
        let mut cfg = fx.cfg(false);
        cfg.to_url = Some(other.path().to_str().unwrap().to_owned());
        let r = spool_out_once(&cfg).await.unwrap();
        assert_eq!((r.handed_off.len(), r.already), (1, 0), "{r}");
        assert_eq!(
            fs::read(other.path().join(format!("spool/{ID}/host_id"))).unwrap(),
            b"42"
        );
        // The same destination again is skipped.
        let r = spool_out_once(&cfg).await.unwrap();
        assert_eq!((r.handed_off.len(), r.already), (0, 1), "{r}");
    }

    #[tokio::test]
    async fn a_host_id_that_goes_away_leaves_no_stale_object() {
        let fx = Fx::new();
        fx.success(ID, "42");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        fx.put("7", &format!("usnm_{ID}__host_id"), "0");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.failed.len()), (1, 0), "{r}");
        let store = usnm_store::open(fx.store.path().to_str().unwrap()).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert_eq!(
            handoff::get_spool(store.as_ref(), ID, out.path())
                .await
                .unwrap(),
            handoff::Spool::Files(None)
        );
    }

    fn remove_local(fx: &Fx, id: &str, suffixes: &[&str]) {
        for s in suffixes {
            let _ = fs::remove_file(fx.spool.path().join(format!("results/7/usnm_{id}{s}")));
        }
    }

    async fn read_back(fx: &Fx, id: &str) -> (handoff::Spool, Vec<String>) {
        let store = usnm_store::open(fx.store.path().to_str().unwrap()).unwrap();
        let out = tempfile::tempdir().unwrap();
        let got = handoff::get_spool(store.as_ref(), id, out.path())
            .await
            .unwrap();
        let mut names: Vec<String> = fs::read_dir(out.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        (got, names)
    }

    #[tokio::test]
    async fn a_marker_replaced_by_success_files_leaves_no_marker_behind() {
        let fx = Fx::new();
        fx.put("7", &format!("usnm_{ID}_error"), "0x2\n");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        remove_local(&fx, ID, &["_error"]);
        fx.success(ID, "42");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.failed.len()), (1, 0), "{r}");
        let (got, names) = read_back(&fx, ID).await;
        assert_eq!(got, handoff::Spool::Files(Some(42)));
        assert!(names.iter().all(|n| !n.ends_with("_error")), "{names:?}");
        assert_eq!(names.len(), 3, "{names:?}");
    }

    #[tokio::test]
    async fn success_files_replaced_by_a_marker_leave_no_result_files_behind() {
        let fx = Fx::new();
        fx.success(ID, "42");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        remove_local(
            &fx,
            ID,
            &[
                "__file_result.json",
                "__file_text.txt",
                "__file_layout.xml",
                "__host_id",
            ],
        );
        fx.put("7", &format!("usnm_{ID}_error"), "0x2\n");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.failed.len()), (1, 0), "{r}");
        let (got, names) = read_back(&fx, ID).await;
        assert_eq!(got, handoff::Spool::Files(None));
        assert_eq!(names, [format!("usnm_{ID}_error")]);
    }

    #[tokio::test]
    async fn one_marker_replaced_by_the_other_leaves_only_the_new_one() {
        let fx = Fx::new();
        fx.put("7", &format!("usnm_{ID}_error"), "0x2\n");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        remove_local(&fx, ID, &["_error"]);
        fx.put("7", &format!("usnm_{ID}_no_output_files"), "");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        let (_, names) = read_back(&fx, ID).await;
        assert_eq!(names, [format!("usnm_{ID}_no_output_files")]);
    }

    #[tokio::test]
    async fn removal_stays_inside_the_jobs_own_names_and_prefix() {
        let fx = Fx::new();
        fx.put("7", &format!("usnm_{ID}_error"), "0x2\n");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        // Another job's objects, the trust object and an unrelated object under the prefix.
        let other = fx.store.path().join(format!("spool/{ID2}"));
        fs::create_dir_all(&other).unwrap();
        fs::write(other.join(format!("usnm_{ID2}_error")), "x").unwrap();
        fs::create_dir_all(fx.store.path().join("gold")).unwrap();
        fs::write(fx.store.path().join("gold/trust.json"), "{}").unwrap();
        fs::write(fx.store.path().join(format!("spool/{ID}/requeued")), "n").unwrap();
        remove_local(&fx, ID, &["_error"]);
        fx.put("7", &format!("usnm_{ID}_no_output_files"), "");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        assert!(other.join(format!("usnm_{ID2}_error")).exists());
        assert!(fx.store.path().join("gold/trust.json").exists());
        assert!(fx
            .store
            .path()
            .join(format!("spool/{ID}/requeued"))
            .exists());
        assert!(!fx
            .store
            .path()
            .join(format!("spool/{ID}/usnm_{ID}_error"))
            .exists());
        // A malformed or empty id cannot form a prefix.
        let store = usnm_store::open(fx.store.path().to_str().unwrap()).unwrap();
        for bad in ["", "..", "a/b", &ID[..63]] {
            assert!(
                remove_stale(store.as_ref(), bad, &[]).await.is_err(),
                "{bad:?}"
            );
        }
        assert!(other.join(format!("usnm_{ID2}_error")).exists());
    }

    #[tokio::test]
    async fn an_oversized_output_or_trust_file_is_a_job_failure() {
        let fx = Fx::new();
        fx.success(ID, "42");
        fx.put(
            "7",
            &format!("usnm_{ID}__file_result.json"),
            &"x".repeat(64 * 1024 + 1),
        );
        for dry in [false, true] {
            let r = spool_out_once(&fx.cfg(dry)).await.unwrap();
            assert_eq!((r.handed_off.len(), r.failed.len()), (0, 1), "{r}");
        }
        assert!(fs::read_dir(fx.store.path()).unwrap().next().is_none());
        fx.success(ID, "42");
        let trust = fx.spool.path().join("trust.json");
        fs::write(&trust, vec![b' '; (16 << 20) + 1]).unwrap();
        for dry in [false, true] {
            let mut cfg = fx.cfg(dry);
            cfg.trust = Some(trust.clone());
            let r = spool_out_once(&cfg).await.unwrap();
            assert_eq!((r.handed_off.len(), r.failed.len()), (0, 1), "{r}");
        }
        assert!(fs::read_dir(fx.store.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn a_dry_run_lists_and_touches_nothing() {
        let fx = Fx::new();
        fx.success(ID, "42");
        let r = spool_out_once(&fx.cfg(true)).await.unwrap();
        assert_eq!(r.handed_off, [format!("usnm_{ID}")]);
        assert!(!fx.spool.path().join(".handed-off").exists());
        assert!(fs::read_dir(fx.store.path()).unwrap().next().is_none());
        // A real run needs a store.
        let mut cfg = fx.cfg(true);
        cfg.dry_run = false;
        assert!(spool_out_once(&cfg).await.is_err());
    }

    #[tokio::test]
    async fn unfinished_jobs_are_left_alone() {
        let fx = Fx::new();
        fx.put("7", &format!("usnm_{ID}__file_result.json"), "{}");
        fx.put("7", &format!("usnm_{ID}__file_text.txt"), "t");
        // All three files but no host id sidecar yet (cloud pull writes it last).
        for s in ["__file_result.json", "__file_text.txt", "__file_layout.xml"] {
            fx.put("7", &format!("usnm_{ID2}{s}"), "x");
        }
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.incomplete.len()), (0, 2), "{r}");
        assert!(fx.object(&format!("spool/{ID}/host_id")).is_none());
        assert!(fs::read_dir(fx.store.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn error_and_no_output_markers_go_without_a_host_id() {
        let fx = Fx::new();
        fx.put("7", &format!("usnm_{ID}_error"), "0x2\n");
        fx.put("7", &format!("usnm_{ID2}_no_output_files"), "");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.failed.len()), (2, 0), "{r}");
        assert_eq!(
            fx.object(&format!("spool/{ID}/usnm_{ID}_error")).unwrap(),
            b"0x2\n"
        );
        assert!(fx.object(&format!("spool/{ID}/host_id")).is_none());
        let store = usnm_store::open(fx.store.path().to_str().unwrap()).unwrap();
        let out = tempfile::tempdir().unwrap();
        assert_eq!(
            handoff::get_spool(store.as_ref(), ID, out.path())
                .await
                .unwrap(),
            handoff::Spool::Files(None)
        );
    }

    #[tokio::test]
    async fn host_id_zero_is_no_host_id() {
        let fx = Fx::new();
        fx.success(ID, "0\n");
        spool_out_once(&fx.cfg(false)).await.unwrap();
        assert!(fx.object(&format!("spool/{ID}/host_id")).is_none());
    }

    #[tokio::test]
    async fn bundles_other_names_and_split_jobs_are_not_handed_off() {
        let fx = Fx::new();
        fx.put("7", &format!("usnm_ocrb_{ID}__file_results.json"), "{}");
        fx.put("7", "usnm_quality_123__file_out", "x");
        fx.put("7", "other.txt", "x");
        // One job in two batch directories.
        fx.put("7", &format!("usnm_{ID2}_error"), "0x2\n");
        fx.put("8", &format!("usnm_{ID2}_no_output_files"), "");
        let r = spool_out_once(&fx.cfg(false)).await.unwrap();
        assert_eq!(r.unsupported, [format!("usnm_ocrb_{ID}")]);
        assert_eq!(r.ignored, 2, "{r}");
        assert!(
            r.ignored_examples.contains(&"7/other.txt".to_owned()),
            "{:?}",
            r.ignored_examples
        );
        assert_eq!((r.handed_off.len(), r.failed.len()), (0, 1), "{r}");
        assert!(fs::read_dir(fx.store.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn a_missing_spool_root_is_refused() {
        let fx = Fx::new();
        let mut cfg = fx.cfg(true);
        cfg.spool = fx.spool.path().join("nope");
        assert!(spool_out_once(&cfg).await.is_err());
    }

    #[tokio::test]
    async fn names_it_does_not_recognise_are_counted_and_shown_also_in_a_dry_run() {
        let fx = Fx::new();
        // The dev server's workunit names are not `usnm_<64 hex>`.
        for i in 0..8 {
            fx.put("0", &format!("load123_{i}__file_out"), "x");
            fx.put("0", &format!("load123_{i}__host_id"), "5");
        }
        fx.put("0", "usnm_short__file_result.json", "x");
        for dry in [true, false] {
            let r = spool_out_once(&fx.cfg(dry)).await.unwrap();
            assert_eq!(r.handed_off.len(), 0, "{r}");
            assert_eq!(r.ignored, 17, "{r}");
            assert_eq!(r.ignored_examples.len(), MAX_IGNORED_EXAMPLES);
            assert!(r.to_string().contains("ignored 17"), "{r}");
        }
        // With a recognised job beside them, both counts show.
        fx.success(ID, "42");
        let r = spool_out_once(&fx.cfg(true)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.ignored), (1, 17), "{r}");
        // A spool with nothing in it has nothing ignored.
        let empty = Fx::new();
        let r = spool_out_once(&empty.cfg(true)).await.unwrap();
        assert_eq!((r.handed_off.len(), r.ignored), (0, 0));
    }
}
