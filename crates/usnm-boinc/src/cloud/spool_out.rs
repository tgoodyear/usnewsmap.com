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
}

impl fmt::Display for Report {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "handed off {}, already done {}, incomplete {}, not handled {}, failed {}",
            self.handed_off.len(),
            self.already,
            self.incomplete.len(),
            self.unsupported.len(),
            self.failed.len()
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
                continue;
            }
            let mut seen_here: BTreeMap<String, ()> = BTreeMap::new();
            for path in sorted_entries(&batch)? {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if !is_real_file(&path)? {
                    continue;
                }
                let Some(rest) = name.strip_prefix("usnm_") else {
                    continue;
                };
                if let Some(bundle) = rest.strip_prefix("ocrb_") {
                    if let Some(b) = bundle.get(..64).filter(|b| boinc::is_sha256(b)) {
                        let wu = format!("usnm_ocrb_{b}");
                        if !report.unsupported.contains(&wu) {
                            report.unsupported.push(wu);
                        }
                    }
                    continue;
                }
                let Some(job) = rest.get(..64).filter(|j| boinc::is_sha256(j)) else {
                    continue;
                };
                let Some(suffix) = SUFFIXES.iter().find(|s| &rest[64..] == **s) else {
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
    let fingerprint = fingerprint(dir, wu, host_id, cfg.trust.as_deref())?;
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
) -> anyhow::Result<String> {
    let mut h = Sha256::new();
    for (name, _) in handoff::spool_names(wu) {
        let p = dir.join(&name);
        if p.exists() {
            let bytes = fs::read(&p).with_context(|| p.display().to_string())?;
            h.update((name.len() as u64).to_be_bytes());
            h.update(name.as_bytes());
            h.update((bytes.len() as u64).to_be_bytes());
            h.update(&bytes);
        }
    }
    h.update(format!("host:{host_id:?}").as_bytes());
    if let Some(t) = trust {
        h.update(fs::read(t).with_context(|| t.display().to_string())?);
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
}
