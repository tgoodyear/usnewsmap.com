//! Server side, at `spool-out`: a bundled page's share of its bundle's assimilated result.
//!
//! Stock `batch_collect_assimilator` writes the bundle's files as
//! `usnm_ocrb_<bundle>__file_{results.json,texts,layouts}` (or `_error`, `_no_output_files`).
//! For one page, [`split`] gives either the page's standard `result.json`, `text.txt` and
//! `layout.xml`, checked against its job, or why it has none. [`requeue`] then puts a page
//! that has none back to `reserved`, out of the bundle, so the next tick submits it alone.
use super::submit::bundle_dir;
use crate::{load, read, save, Lock, Stage};
use anyhow::{ensure, Context};
use std::path::Path;
use usnm_ocr_contract::boinc::bundle::{
    workunit, Bundle, PageOutput, Results, JOB_FILE, LAYOUTS_FILE, MAX_JSON, MAX_LAYOUTS,
    MAX_TEXTS, RESULTS_FILE, TEXTS_FILE,
};

/// The spool names of bundle `id`'s assimilated result.
pub fn spool_names(id: &str) -> [String; 5] {
    let wu = workunit(id);
    [
        format!("{wu}__file_{RESULTS_FILE}"),
        format!("{wu}__file_{TEXTS_FILE}"),
        format!("{wu}__file_{LAYOUTS_FILE}"),
        format!("{wu}_error"),
        format!("{wu}_no_output_files"),
    ]
}

/// Page `id`'s outputs from its bundle's spool in `spool`, or why it has none. Fails only
/// when the page isn't bundled or the bundle hasn't been assimilated.
pub fn split(root: &Path, spool: &Path, id: &str) -> anyhow::Result<PageOutput> {
    let record = load(root, id)?;
    let bundle_id = record.bundle.clone().context("job is not in a bundle")?;
    let bundle: Bundle = serde_json::from_slice(&read(
        &bundle_dir(root, &bundle_id)?.join(JOB_FILE),
        MAX_JSON,
    )?)?;
    bundle.validate()?;
    ensure!(
        bundle.id == bundle_id,
        "the server's bundle file is another bundle"
    );
    let [results, texts, layouts, error, no_output] =
        spool_names(&bundle_id).map(|n| spool.join(n));
    if error.exists() || no_output.exists() {
        return Ok(PageOutput::Failed(
            "the bundle's workunit failed on BOINC".into(),
        ));
    }
    ensure!(
        results.exists() && texts.exists() && layouts.exists(),
        "no assimilated result for bundle {bundle_id} under {} (not assimilated yet?)",
        spool.display()
    );
    let parsed = (|| -> anyhow::Result<(Results, Vec<u8>, Vec<u8>)> {
        Ok((
            serde_json::from_slice(&read(&results, MAX_JSON)?)?,
            read(&texts, MAX_TEXTS)?,
            read(&layouts, MAX_LAYOUTS)?,
        ))
    })();
    match parsed {
        // A result for another bundle, another version or too many pages fails every page
        // alone (and so requeues it), as an unreadable one does: the bundle won't get better.
        Ok((results, texts, layouts)) => Ok(results
            .page(&bundle, &record.job, &texts, &layouts)
            .unwrap_or_else(|e| {
                PageOutput::Failed(format!("the bundle's result is rejected: {e:#}"))
            })),
        Err(e) => Ok(PageOutput::Failed(format!(
            "the bundle's result is unreadable: {e:#}"
        ))),
    }
}

/// Puts bundled page `id` back to `reserved`, out of its bundle and marked so it is only ever
/// submitted alone. Returns the bundle it left.
pub fn requeue(root: &Path, id: &str, why: &str) -> anyhow::Result<String> {
    let _lock = Lock::take(root)?;
    let mut record = load(root, id)?;
    let bundle = record.bundle.clone().context("job is not in a bundle")?;
    ensure!(
        record.stage == Stage::Submitted,
        "only a submitted bundled page can be requeued"
    );
    record.stage = Stage::Reserved;
    record.bundle = None;
    record.unbundled = Some(bundle.clone());
    record.error = Some(format!("failed in bundle {bundle}: {why}; requeued alone"));
    save(root, &record)?;
    Ok(bundle)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::bundle::{
        submit::{submit, tests::Server},
        work::tests::{outputs, Fake},
    };
    use usnm_ocr_contract::boinc::{Job, ResultManifest};

    /// Runs the bundle the server submitted, as a host would, and spools its result the way
    /// the assimilator does. Returns the spool directory.
    async fn returned(s: &Server, bundle_id: &str, fail: Vec<u8>) -> std::path::PathBuf {
        let work = tempfile::tempdir().unwrap();
        let held = bundle_dir(&s.root, bundle_id).unwrap();
        std::fs::copy(held.join("pages"), work.path().join("pages")).unwrap();
        std::fs::copy(held.join(JOB_FILE), work.path().join(JOB_FILE)).unwrap();
        let bundle = crate::bundle::work::load(work.path()).unwrap();
        let ok = |_: &Job| Ok(());
        crate::bundle::work::run_with(
            work.path(),
            &bundle,
            &Fake {
                fail,
                ..Default::default()
            },
            &ok,
        )
        .await
        .unwrap();
        let (results, texts, layouts) = outputs(work.path());
        let spool = s.project.join("results/0");
        std::fs::create_dir_all(&spool).unwrap();
        let [r, t, l, _, _] = spool_names(bundle_id);
        std::fs::write(spool.join(r), serde_json::to_vec(&results).unwrap()).unwrap();
        std::fs::write(spool.join(t), texts).unwrap();
        std::fs::write(spool.join(l), layouts).unwrap();
        spool
    }

    #[tokio::test]
    async fn each_page_gets_its_own_checked_outputs_and_a_failed_one_goes_back_alone() {
        let (s, ids) = Server::new(&[1, 2, 3]);
        let bundle_id = submit(&s.root, &s.project, &ids, Some(106))
            .await
            .unwrap()
            .bundle
            .unwrap();
        let spool = returned(&s, &bundle_id, vec![2]).await;
        let PageOutput::Ok { result, text, .. } = split(&s.root, &spool, &ids[0]).unwrap() else {
            panic!("page 1 should be ok")
        };
        let manifest: ResultManifest = serde_json::from_slice(&result).unwrap();
        assert_eq!(manifest.job_id, ids[0]);
        assert_eq!(text, b"page1");
        let PageOutput::Failed(why) = split(&s.root, &spool, &ids[1]).unwrap() else {
            panic!("page 2 failed")
        };
        assert_eq!(requeue(&s.root, &ids[1], &why).unwrap(), bundle_id);
        let r = load(&s.root, &ids[1]).unwrap();
        assert_eq!(
            (r.stage, r.bundle, r.unbundled.as_deref()),
            (Stage::Reserved, None, Some(bundle_id.as_str()))
        );
        // It goes alone next time, and the other pages are untouched.
        let again = submit(&s.root, &s.project, &ids[1..2], Some(106))
            .await
            .unwrap();
        assert_eq!(again.alone, vec![ids[1].clone()]);
        assert_eq!(load(&s.root, &ids[2]).unwrap().stage, Stage::Submitted);
        // A page that isn't bundled any more has no share of the bundle.
        assert!(split(&s.root, &spool, &ids[1]).is_err());
    }

    #[tokio::test]
    async fn a_failed_or_tampered_bundle_requeues_its_pages() {
        let (s, ids) = Server::new(&[1, 2]);
        let bundle_id = submit(&s.root, &s.project, &ids, Some(106))
            .await
            .unwrap()
            .bundle
            .unwrap();
        let spool = s.project.join("results/1");
        std::fs::create_dir_all(&spool).unwrap();
        // Not assimilated yet: an error, nothing changes.
        assert!(split(&s.root, &spool, &ids[0]).is_err());
        let spool = returned(&s, &bundle_id, vec![]).await;
        // Changed text bytes: that page is rejected, and only that one.
        let [_, t, _, e, _] = spool_names(&bundle_id);
        let mut texts = std::fs::read(spool.join(&t)).unwrap();
        texts[0] = b'P';
        std::fs::write(spool.join(&t), &texts).unwrap();
        assert!(
            matches!(split(&s.root, &spool, &ids[0]).unwrap(), PageOutput::Failed(w) if w.contains("rejected"))
        );
        assert!(matches!(
            split(&s.root, &spool, &ids[1]).unwrap(),
            PageOutput::Ok { .. }
        ));
        // A result for another bundle fails every page, rather than holding them.
        let [r, ..] = spool_names(&bundle_id);
        let good = std::fs::read(spool.join(&r)).unwrap();
        let mut results: Results = serde_json::from_slice(&good).unwrap();
        results.bundle_id = "0".repeat(64);
        std::fs::write(spool.join(&r), serde_json::to_vec(&results).unwrap()).unwrap();
        for id in &ids {
            assert!(
                matches!(split(&s.root, &spool, id).unwrap(), PageOutput::Failed(w) if w.contains("rejected"))
            );
        }
        std::fs::write(spool.join(&r), good).unwrap();
        // A terminal BOINC failure fails every page.
        std::fs::write(spool.join(e), b"").unwrap();
        for id in &ids {
            assert!(matches!(
                split(&s.root, &spool, id).unwrap(),
                PageOutput::Failed(_)
            ));
        }
    }

    #[tokio::test]
    async fn spool_out_writes_a_single_page_spool_or_the_requeued_marker() {
        use crate::handoff::{get_spool, put_spool, Spool, Spooled};
        let (s, ids) = Server::new(&[1, 2]);
        let bundle_id = submit(&s.root, &s.project, &ids, Some(106))
            .await
            .unwrap()
            .bundle
            .unwrap();
        let spool = returned(&s, &bundle_id, vec![2]).await;
        let container = usnm_store::open(s.project.join("container").to_str().unwrap()).unwrap();
        let store = container.as_ref();
        assert_eq!(
            put_spool(store, Some(&s.root), &spool, &ids[0], Some(7), None)
                .await
                .unwrap(),
            Spooled::Files(3)
        );
        assert!(matches!(
            put_spool(store, Some(&s.root), &spool, &ids[1], Some(7), None)
                .await
                .unwrap(),
            Spooled::Requeued(_)
        ));
        // The hand-off side sees page 1 as an ordinary single-page spool, from the bundle's host.
        let down = s.project.join("down");
        assert_eq!(
            get_spool(store, &ids[0], &down).await.unwrap(),
            Spool::Files(Some(7))
        );
        let manifest: ResultManifest = serde_json::from_slice(
            &std::fs::read(down.join(format!("usnm_{}__file_result.json", ids[0]))).unwrap(),
        )
        .unwrap();
        let text = std::fs::read(down.join(format!("usnm_{}__file_text.txt", ids[0]))).unwrap();
        let alto = std::fs::read(down.join(format!("usnm_{}__file_layout.xml", ids[0]))).unwrap();
        manifest
            .matches(&load(&s.root, &ids[0]).unwrap().job, &text, &alto)
            .unwrap();
        // Page 2 has only the marker, until its own unit's result arrives.
        assert_eq!(
            get_spool(store, &ids[1], &s.project.join("down2"))
                .await
                .unwrap(),
            Spool::Requeued
        );
        // Without the server root (the old step), a bundled page has no spool of its own.
        assert!(put_spool(store, None, &spool, &ids[0], None, None)
            .await
            .is_err());
    }
}
