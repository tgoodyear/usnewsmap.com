//! A bundle unit inside BOINC's job container. No network. The engine is set up once; the pages
//! are read in order, and after each one its outputs and `ocr_checkpoint.json` are saved and
//! `fraction_done` is written, so a restarted task resumes at the next page. A page that fails
//! is returned as an error, and the rest still run.
use crate::{atomic, read};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use usnm_ocr_contract::boinc::{
    bundle::{
        Bundle, Outcome, PageResult, Results, Slice, CHECKPOINT_FILE, JOB_FILE, LAYOUTS_FILE,
        MAX_ERROR, MAX_JSON, MAX_LAYOUTS, MAX_PAGES_BYTES, MAX_TEXTS, PAGES_FILE, RESULTS_FILE,
        TEXTS_FILE, VERSION,
    },
    sha256, Job, MAX_ALTO, MAX_TEXT,
};
use usnm_ocr_worker::engine::Engine;

const FRACTION_FILE: &str = "fraction_done";
/// Finished pages' outputs, until the unit is assembled.
const DONE_DIR: &str = "ocr_done";
const OUTPUTS: [&str; 4] = [RESULTS_FILE, TEXTS_FILE, LAYOUTS_FILE, FRACTION_FILE];

/// Writes `fraction_done` readable by the BOINC wrapper (0644) from the moment it appears: the
/// container's user need not be the host's BOINC user.
fn progress(work: &Path, fraction: &str) -> anyhow::Result<()> {
    #[cfg(unix)]
    let mode = {
        use std::os::unix::fs::PermissionsExt;
        Some(std::fs::Permissions::from_mode(0o644))
    };
    #[cfg(not(unix))]
    let mode = None;
    crate::atomic_with_permissions(&work.join(FRACTION_FILE), fraction.as_bytes(), mode)
}

pub fn load(work: &Path) -> anyhow::Result<Bundle> {
    let bundle: Bundle = serde_json::from_slice(&read(&work.join(JOB_FILE), MAX_JSON)?)?;
    bundle.validate()?;
    Ok(bundle)
}

pub async fn run(work: &Path) -> anyhow::Result<()> {
    let bundle = load(work)?;
    let engine = crate::worker::tesseract(&bundle.pages[0].job.engine).await?;
    let check = |job: &Job| -> anyhow::Result<()> {
        crate::worker::check_models(&job.engine)?;
        crate::worker::check_route(&engine, job)
    };
    run_with(work, &bundle, &engine, &check).await
}

/// The pages done so far.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Checkpoint {
    v: u32,
    bundle_id: String,
    pages: Vec<PageResult>,
    texts: u64,
    layouts: u64,
}

fn done(work: &Path, index: usize, ext: &str) -> PathBuf {
    work.join(DONE_DIR).join(format!("{index}.{ext}"))
}

impl Checkpoint {
    fn new(bundle: &Bundle) -> Self {
        Self {
            v: VERSION,
            bundle_id: bundle.id.clone(),
            pages: vec![],
            texts: 0,
            layouts: 0,
        }
    }
    /// The saved checkpoint, if it belongs to this bundle and every finished page's outputs
    /// are still there with the bytes it recorded; otherwise none, and the unit starts over.
    fn load_valid(work: &Path, bundle: &Bundle) -> Option<Self> {
        let cp: Self =
            serde_json::from_slice(&read(&work.join(CHECKPOINT_FILE), MAX_JSON).ok()?).ok()?;
        if cp.v != VERSION || cp.bundle_id != bundle.id || cp.pages.len() > bundle.pages.len() {
            return None;
        }
        let (mut texts, mut layouts) = (0u64, 0u64);
        for (index, (entry, page)) in cp.pages.iter().zip(&bundle.pages).enumerate() {
            if entry.job_id != page.job.id {
                return None;
            }
            if entry.outcome != Outcome::Ok {
                continue;
            }
            let (result, text, alto) = (entry.result.as_ref()?, entry.text?, entry.alto?);
            let t = read(&done(work, index, "txt"), MAX_TEXT).ok()?;
            let a = read(&done(work, index, "xml"), MAX_ALTO).ok()?;
            if text
                != (Slice {
                    offset: texts,
                    bytes: t.len() as u64,
                })
                || alto
                    != (Slice {
                        offset: layouts,
                        bytes: a.len() as u64,
                    })
                || sha256(&t) != result.text_sha256
                || sha256(&a) != result.alto_sha256
            {
                return None;
            }
            texts += text.bytes;
            layouts += alto.bytes;
        }
        (texts == cp.texts && layouts == cp.layouts).then_some(cp)
    }
    fn save(&self, work: &Path) -> anyhow::Result<()> {
        atomic(&work.join(CHECKPOINT_FILE), &serde_json::to_vec(self)?)
    }
}

fn failed(job: &Job, error: &anyhow::Error) -> PageResult {
    let mut why = format!("{error:#}");
    if why.len() > MAX_ERROR {
        let mut end = MAX_ERROR;
        while !why.is_char_boundary(end) {
            end -= 1;
        }
        why.truncate(end);
    }
    PageResult {
        job_id: job.id.clone(),
        outcome: Outcome::Error,
        error: Some(why),
        result: None,
        text: None,
        alto: None,
    }
}

/// [`run`] with the engine and the per-page model and route check given, for tests.
pub(crate) async fn run_with(
    work: &Path,
    bundle: &Bundle,
    engine: &dyn Engine,
    check: &(dyn Fn(&Job) -> anyhow::Result<()> + Sync),
) -> anyhow::Result<()> {
    for name in OUTPUTS {
        let p = work.join(name);
        if p.exists() {
            std::fs::remove_file(p)?;
        }
    }
    let pages = read(&work.join(PAGES_FILE), MAX_PAGES_BYTES)?;
    ensure!(
        pages.len() as u64 == bundle.bytes(),
        "input pages size differs from the bundle"
    );
    let mut cp = match Checkpoint::load_valid(work, bundle) {
        Some(cp) => {
            eprintln!(
                "resumed from checkpoint: {} of {} pages already done",
                cp.pages.len(),
                bundle.pages.len()
            );
            cp
        }
        None => {
            let d = work.join(DONE_DIR);
            if d.exists() {
                std::fs::remove_dir_all(&d)?;
            }
            Checkpoint::new(bundle)
        }
    };
    let total = bundle.pages.len();
    for index in cp.pages.len()..total {
        let job = &bundle.pages[index].job;
        let read_page = async {
            let scan = bundle.scan(index, &pages)?;
            check(job)?;
            let scratch = tempfile::tempdir()?;
            let image = scratch.path().join("page");
            std::fs::write(&image, scan)?;
            crate::worker::ocr_page(engine, job, &image).await
        };
        let entry = match read_page.await {
            Ok((result, text, alto)) => {
                let (t, a) = (text.len() as u64, alto.len() as u64);
                if cp.texts + t > MAX_TEXTS as u64 || cp.layouts + a > MAX_LAYOUTS as u64 {
                    failed(job, &anyhow::anyhow!("bundle output limit reached"))
                } else {
                    atomic(&done(work, index, "txt"), text.as_bytes())?;
                    atomic(&done(work, index, "xml"), &alto)?;
                    let entry = PageResult {
                        job_id: job.id.clone(),
                        outcome: Outcome::Ok,
                        error: None,
                        result: Some(result),
                        text: Some(Slice {
                            offset: cp.texts,
                            bytes: t,
                        }),
                        alto: Some(Slice {
                            offset: cp.layouts,
                            bytes: a,
                        }),
                    };
                    cp.texts += t;
                    cp.layouts += a;
                    entry
                }
            }
            Err(e) => failed(job, &e),
        };
        if entry.outcome == Outcome::Error {
            eprintln!(
                "page {} failed: {}",
                job.id,
                entry.error.as_deref().unwrap_or("")
            );
        }
        cp.pages.push(entry);
        cp.save(work)?;
        let fraction = cp.pages.len() as f64 / total as f64;
        progress(work, &format!("{fraction:.6}\n"))?;
    }
    // Assemble the outputs from the finished pages, in order.
    let (mut texts, mut layouts) = (Vec::new(), Vec::new());
    for (index, entry) in cp.pages.iter().enumerate() {
        if entry.outcome == Outcome::Ok {
            texts.extend(read(&done(work, index, "txt"), MAX_TEXT)?);
            layouts.extend(read(&done(work, index, "xml"), MAX_ALTO)?);
        }
    }
    ensure!(
        texts.len() as u64 == cp.texts && layouts.len() as u64 == cp.layouts,
        "finished page outputs changed"
    );
    let results = Results {
        v: VERSION,
        bundle_id: bundle.id.clone(),
        pages: cp.pages,
    };
    atomic(&work.join(TEXTS_FILE), &texts)?;
    atomic(&work.join(LAYOUTS_FILE), &layouts)?;
    atomic(
        &work.join(RESULTS_FILE),
        &serde_json::to_vec_pretty(&results).context("results.json")?,
    )?;
    progress(work, "1.0\n")?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for name in OUTPUTS {
            std::fs::set_permissions(work.join(name), std::fs::Permissions::from_mode(0o644))?;
        }
    }
    let _ = std::fs::remove_file(work.join(CHECKPOINT_FILE));
    let _ = std::fs::remove_dir_all(work.join(DONE_DIR));
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use usnm_ocr_contract::boinc::{self, bundle::PageOutput, Job};
    use usnm_ocr_worker::engine::{EngineError, EngineOutput};

    /// A job for page `n` with scan bytes `scan(n)`.
    pub(crate) fn scan(n: u8) -> Vec<u8> {
        vec![n; 100 + n as usize]
    }
    pub(crate) fn job(n: u8) -> Job {
        let mut job: Job = serde_json::from_value(serde_json::json!({
            "v": 1, "id": "",
            "message": {"v": 1, "page": format!("sn00000001/1900-01-01/ed-1/seq-{n}"),
                "batch": "test_batch", "language": "eng", "reason": "bulk",
                "queued_at": "2026-10-24T12:00:00Z"},
            "batch_version": 1,
            "scan": {"url": "https://tile.loc.gov/x.jp2", "iiif_base": "https://tile.loc.gov/x",
                "sha256": boinc::sha256(&scan(n)), "bytes": scan(n).len(), "md5": null},
            "engine": {"image": format!("sha256:{}", "a".repeat(64)), "version": "5.5.0",
                "profile": "eng", "models": "sha256:0123456789ab", "psm": 3, "dpi": 300,
                "timeout_secs": 600}
        }))
        .unwrap();
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        job
    }

    pub(crate) fn alto(text: &str) -> Vec<u8> {
        crate::quality::alto_xml(1000, 1000, &[vec![vec![(10, 10, 200, 40, text, 0.9)]]])
            .into_bytes()
    }

    /// Reads the page's first byte back as its text; a page whose byte is in `fail` fails.
    #[derive(Debug, Default)]
    pub(crate) struct Fake {
        pub fail: Vec<u8>,
        pub calls: AtomicUsize,
    }
    #[async_trait::async_trait]
    impl Engine for Fake {
        fn source(&self) -> &str {
            "fake"
        }
        fn can_read(&self, _: &str) -> bool {
            true
        }
        async fn ocr(&self, image: &Path, _: &str, _: &Path) -> Result<EngineOutput, EngineError> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let n = std::fs::read(image).map_err(anyhow::Error::from)?[0];
            if self.fail.contains(&n) {
                return Err(anyhow::anyhow!("tesseract exited 1 on page {n}").into());
            }
            let text = format!("page{n}");
            Ok(EngineOutput {
                alto: alto(&text),
                text,
                page: None,
                layout_from: "ALTO",
                engine: "tesseract/5.5.0".into(),
                read: usnm_ocr_worker::engine::ReadInfo::local("fake"),
            })
        }
    }

    /// A bundle of pages `ns` staged in `work` as the BOINC client would.
    pub(crate) fn stage(work: &Path, ns: &[u8]) -> Bundle {
        let bundle = Bundle::new(ns.iter().map(|n| job(*n)).collect()).unwrap();
        let pages: Vec<u8> = ns.iter().flat_map(|n| scan(*n)).collect();
        std::fs::write(work.join(PAGES_FILE), pages).unwrap();
        std::fs::write(work.join(JOB_FILE), serde_json::to_vec(&bundle).unwrap()).unwrap();
        bundle
    }

    pub(crate) fn outputs(work: &Path) -> (Results, Vec<u8>, Vec<u8>) {
        (
            serde_json::from_slice(&std::fs::read(work.join(RESULTS_FILE)).unwrap()).unwrap(),
            std::fs::read(work.join(TEXTS_FILE)).unwrap(),
            std::fs::read(work.join(LAYOUTS_FILE)).unwrap(),
        )
    }

    fn ok(_: &Job) -> anyhow::Result<()> {
        Ok(())
    }

    #[tokio::test]
    async fn every_page_gets_its_own_result_and_a_failed_page_fails_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = stage(tmp.path(), &[1, 2, 3]);
        let engine = Fake {
            fail: vec![2],
            ..Default::default()
        };
        run_with(tmp.path(), &bundle, &engine, &ok).await.unwrap();
        let (results, texts, layouts) = outputs(tmp.path());
        assert_eq!(results.pages.len(), 3);
        assert_eq!(texts, b"page1page3");
        let out = |i: usize| results.page(&bundle, &bundle.pages[i].job, &texts, &layouts);
        let PageOutput::Ok { text, alto: a, .. } = out(0).unwrap() else {
            panic!("page 1 should be ok")
        };
        assert_eq!((text, a), (b"page1".to_vec(), alto("page1")));
        assert!(
            matches!(out(1).unwrap(), PageOutput::Failed(e) if e.contains("tesseract exited 1"))
        );
        assert!(matches!(out(2).unwrap(), PageOutput::Ok { .. }));
        assert_eq!(
            std::fs::read(tmp.path().join(FRACTION_FILE)).unwrap(),
            b"1.0\n"
        );
        assert!(!tmp.path().join(CHECKPOINT_FILE).exists());
        assert!(!tmp.path().join(DONE_DIR).exists());
    }

    /// The checkpoint a task stopped after `pages` pages leaves, from a full run's results.
    fn stopped_after(work: &Path, bundle: &Bundle, results: &Results, pages: usize) -> Checkpoint {
        let mut cp = Checkpoint::new(bundle);
        for i in 0..pages {
            let t = format!("page{}", i + 1);
            atomic(&done(work, i, "txt"), t.as_bytes()).unwrap();
            atomic(&done(work, i, "xml"), &alto(&t)).unwrap();
            cp.texts += t.len() as u64;
            cp.layouts += alto(&t).len() as u64;
            cp.pages.push(results.pages[i].clone());
        }
        cp.save(work).unwrap();
        cp
    }

    #[cfg(unix)]
    #[test]
    fn progress_is_readable_by_the_wrapper_from_the_first_page() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        progress(tmp.path(), "0.5\n").unwrap();
        let mode = std::fs::metadata(tmp.path().join(FRACTION_FILE))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o644);
    }

    #[tokio::test]
    async fn a_restarted_task_resumes_at_the_next_page() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = stage(tmp.path(), &[1, 2, 3]);
        run_with(tmp.path(), &bundle, &Fake::default(), &ok)
            .await
            .unwrap();
        let (full, _, _) = outputs(tmp.path());
        stopped_after(tmp.path(), &bundle, &full, 2);
        let again = Fake::default();
        run_with(tmp.path(), &bundle, &again, &ok).await.unwrap();
        // Only the third page is read again, and the result is the same.
        assert_eq!(again.calls.load(Ordering::SeqCst), 1);
        let (resumed, texts, _) = outputs(tmp.path());
        assert_eq!(texts, b"page1page2page3");
        assert_eq!(resumed.pages[..2], full.pages[..2]);
        // A checkpoint whose saved outputs changed is ignored: the unit starts over.
        stopped_after(tmp.path(), &bundle, &full, 1);
        atomic(&done(tmp.path(), 0, "txt"), b"PAGE1").unwrap();
        let fresh = Fake::default();
        run_with(tmp.path(), &bundle, &fresh, &ok).await.unwrap();
        assert_eq!(fresh.calls.load(Ordering::SeqCst), 3);
        // Another bundle's checkpoint is ignored too.
        let other = Bundle::new(vec![job(1), job(2), job(4)]).unwrap();
        let mut cp = stopped_after(tmp.path(), &bundle, &full, 2);
        cp.bundle_id = other.id;
        cp.save(tmp.path()).unwrap();
        let fresh = Fake::default();
        run_with(tmp.path(), &bundle, &fresh, &ok).await.unwrap();
        assert_eq!(fresh.calls.load(Ordering::SeqCst), 3);
    }

    #[tokio::test]
    async fn a_wrong_scan_or_refused_model_fails_that_page_and_a_short_input_fails_the_task() {
        let tmp = tempfile::tempdir().unwrap();
        let bundle = stage(tmp.path(), &[1, 2]);
        let mut pages = std::fs::read(tmp.path().join(PAGES_FILE)).unwrap();
        pages[0] ^= 0xff;
        std::fs::write(tmp.path().join(PAGES_FILE), &pages).unwrap();
        let refuse_two = |job: &Job| -> anyhow::Result<()> {
            anyhow::ensure!(
                !job.message.page.ends_with("seq-2"),
                "installed model checksum mismatch"
            );
            Ok(())
        };
        run_with(tmp.path(), &bundle, &Fake::default(), &refuse_two)
            .await
            .unwrap();
        let (results, _, _) = outputs(tmp.path());
        assert!(results.pages[0]
            .error
            .as_deref()
            .unwrap()
            .contains("checksum"));
        assert!(results.pages[1].error.as_deref().unwrap().contains("model"));
        pages.pop();
        std::fs::write(tmp.path().join(PAGES_FILE), &pages).unwrap();
        assert!(run_with(tmp.path(), &bundle, &Fake::default(), &ok)
            .await
            .is_err());
    }
}
