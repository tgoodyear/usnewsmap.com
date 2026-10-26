//! The release itself: the fixtures reproduced as a base and a delta, and a
//! release that crashes, loses the writer lock or fails.

use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_ingest::docs::MemoryDocs;
use usnm_ingest::release::Release;
use usnm_ingest::sink::JsonlSink;
use usnm_ingest::source;
use usnm_ingest::state::State;
use usnm_store::ObjectStore;

use crate::common::{
    env, fixture_pages, fixtures, listed, loc_fixture, raw_run, sha256_file, write_archive, Page,
};

#[tokio::test]
async fn reproduces_the_fixture_corpus_as_a_base_and_a_delta() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let base_archive = e.root.join("batch_fx_early_ver01.tar.gz");
    let delta_archive = e.root.join("batch_fx_late_ver01.tar.bz2");
    write_archive(&base_archive, &early, true, true);
    write_archive(&delta_archive, &late, false, false);

    // Week 1: one batch → a base index.
    let list = [listed(
        "batch_fx_early_ver01",
        &base_archive,
        Some(sha256_file(&base_archive)),
    )];
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().new, 1);
    assert_eq!(e.worker("w1").run(None).await.unwrap(), 1);
    let p1 = e.release(1, false).await.unwrap();
    assert!(p1.full, "the first release is a base");
    assert_eq!(p1.indexes, ["pages-base-20261001-1"]);

    let want_base = loc_fixture("pages-base-fixture");
    assert_eq!(e.index("pages-base-20261001-1"), want_base);
    assert_eq!(p1.docs, want_base.len() as u64);

    // Nothing new (a weekly run when LoC published nothing): the listing
    // queues nothing, the worker finds nothing to claim, and nothing is released.
    let again = source::enqueue(&e.state, &list).await.unwrap();
    assert_eq!((again.new, again.unchanged), (0, 1));
    assert_eq!(e.worker("w1").run(None).await.unwrap(), 0);
    assert!(e.release(2, false).await.is_none());

    // Week 2: the second batch → a delta on top of the base.
    let list = [listed("batch_fx_late_ver01", &delta_archive, None)];
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().new, 1);
    assert_eq!(e.worker("w2").run(None).await.unwrap(), 1);
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(
        p2.indexes,
        ["pages-base-20261001-1", "pages-delta-20261008-1"]
    );
    let want_delta = loc_fixture("pages-delta-fixture-1");
    assert_eq!(e.index("pages-delta-20261008-1"), want_delta);

    // The reference snapshot matches the fixture's, file for file.
    let v = &p2.index_version;
    for f in [
        "baselines.json",
        "language_baselines.json",
        "titles.json",
        "places.json",
        "title_pages.json",
        "title_dates.json",
    ] {
        let want: Value =
            serde_json::from_slice(&std::fs::read(fixtures().join("fixture-v1").join(f)).unwrap())
                .unwrap();
        assert_eq!(e.reference_json(&format!("{v}/{f}")).await, want, "{f}");
    }
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v.as_str());
    assert_eq!(current["previous_version"], p1.index_version.as_str());
    assert_eq!(current["backend"], "memory");
    assert_eq!(current["bounds"]["from"], "1895-01-05");
    assert_eq!(current["synthetic"], true);
    // Stamped when the pointer was written, not when the run started (#119):
    // the same instant the run records as its publish.
    assert_ne!(current["published_at"], "2026-10-08T03:00:00Z");
    let run_published: chrono::DateTime<Utc> =
        serde_json::from_value(raw_run(&e, v).await["published_at"].clone()).unwrap();
    assert_eq!(
        current["published_at"],
        run_published.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );

    // The run item points at the batch list in the snapshot instead of
    // carrying it, and the manifest checksums the list like its other files.
    let item = raw_run(&e, v).await;
    assert!(item.get("batches").is_none(), "{item}");
    assert_eq!(item["batch_count"], 2);
    assert_eq!(item["batch_list"], format!("{v}/batches.json"));
    let listed = e.reference_json(&format!("{v}/batches.json")).await;
    assert_eq!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["batch"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["batch_fx_early", "batch_fx_late"]
    );
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert!(manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "batches.json"));
    // What built it (#161): the manifest has the templates in full, the run
    // item their checksums only.
    let build = &manifest["build"];
    assert_eq!(
        build["features"]["common_grams"],
        usnm_core::common_grams::VERSION
    );
    assert_eq!(
        build["templates"]["pages"]["yaml"],
        usnm_ingest::sink::INDEX_TEMPLATE
    );
    assert_eq!(
        item["build"]["templates"]["pages"]["sha256"],
        build["templates"]["pages"]["sha256"]
    );
    assert!(item["build"]["templates"]["pages"].get("yaml").is_none());
    assert_eq!(item["build"]["full"], build["full"]);
    // The memory sink has no engine to name.
    assert_eq!(build["engine"], serde_json::Value::Null);

    // The API loads it: checksums, manifest and version pairing all hold.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.version(), v);
    assert_eq!(refdata.places.len(), 6);
    // Pages per title count the same pages as the baselines.
    let title_pages = refdata.title_pages.as_ref().unwrap();
    assert_eq!(title_pages.len(), 6);
    assert_eq!(title_pages.values().sum::<u64>(), refdata.pages);
    // And each title's first and last page, within the version's bounds.
    let title_dates = refdata.title_dates.as_ref().unwrap();
    assert_eq!(title_dates.len(), 6);
    let first = title_dates.values().map(|d| d.first).min().unwrap();
    assert_eq!(first.to_string(), current["bounds"]["from"]);
    assert!(title_dates.values().all(|d| d.first <= d.last));

    // A full release (compaction) rebuilds one base with every page.
    let p3 = e.release(15, true).await.unwrap();
    assert_eq!(p3.indexes, ["pages-base-20261015-1"]);
    let mut both = want_base.clone();
    both.extend(want_delta.clone());
    assert_eq!(e.index("pages-base-20261015-1"), both);

    // Published indexes and snapshots are never rewritten.
    assert!(e
        .reference
        .get(&format!("{}/manifest.json", p1.index_version))
        .await
        .unwrap()
        .is_some());
}

/// A release that crashed after writing `current.json` but before recording
/// the publish in Cosmos: the next release treats `current.json` as the
/// truth, repairs the state and builds on the version that is live.
#[tokio::test]
async fn recovers_from_a_crash_between_publish_and_bookkeeping() {
    use usnm_ingest::state::RunStatus;
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    source::enqueue(&e.state, &[listed("batch_fx_early_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p1 = e.release(1, false).await.unwrap();
    // A publish well before the restart, so the restart's time can't match it.
    let mut pointer = e.reference_json("current.json").await;
    pointer["published_at"] = "2026-10-01T04:00:00Z".into();
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();

    // Undo the bookkeeping, as if the process died right after current.json.
    let (mut run, etag) = e.state.run(&p1.index_version).await.unwrap().unwrap();
    run.status = RunStatus::Building;
    run.published_at = None;
    e.state.update_run(&run, &etag).await.unwrap();
    e.state
        .set_current_version("pages-v20260901-1")
        .await
        .unwrap();

    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert_eq!(
        p2.indexes,
        ["pages-base-20261001-1", "pages-delta-20261008-1"]
    );
    assert_eq!(
        e.state
            .run(&p1.index_version)
            .await
            .unwrap()
            .unwrap()
            .0
            .status,
        RunStatus::Published
    );
    // The repaired run takes current.json's time, not the restart's.
    let repaired = e.state.run(&p1.index_version).await.unwrap().unwrap().0;
    assert_eq!(
        repaired
            .published_at
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        Some("2026-10-01T04:00:00Z".to_owned())
    );
    assert_eq!(
        e.state.current_version().await.unwrap().as_deref(),
        Some(p2.index_version.as_str())
    );
}

/// Records documents and claims to be Quickwit: stands in for a backend switch.
struct OtherBackend(u64);

#[async_trait::async_trait]
impl usnm_ingest::sink::IndexSink for OtherBackend {
    async fn create(
        &mut self,
        _index_id: &str,
        _: usnm_ingest::sink::MainLayout,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn add(&mut self, _doc: &Value) -> anyhow::Result<()> {
        self.0 += 1;
        Ok(())
    }
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        anyhow::ensure!(self.0 == expected);
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "quickwit"
    }
}

/// Deltas only stack on indexes in the same engine: switching backend
/// rebuilds everything into a new base there.
#[tokio::test]
async fn a_backend_switch_forces_a_full_release() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    source::enqueue(&e.state, &[listed("batch_fx_early_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    e.release(1, false).await.unwrap(); // memory
    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let r = Release {
        stage_index_storage: false,
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: false,
        synthetic: true,
        now: Utc.with_ymd_and_hms(2026, 10, 8, 3, 0, 0).unwrap(),
        titles_left: None,
        american_stories: false,
        ja_latin: false,
        ocr_local: false,
        congress: false,
        congress_manual: false,
        partition_decade: false,
        single_text_field: false,
        page_language: false,
        split_docs: None,
    };
    let mut sink = OtherBackend(0);
    let p = r.run(&mut sink).await.unwrap().unwrap();
    assert!(p.full);
    assert_eq!(p.indexes, ["pages-base-20261008-1"]);
    assert_eq!(sink.0, p.docs);
    assert_eq!(
        e.reference_json("current.json").await["backend"],
        "quickwit"
    );
}

/// A release that no longer holds the writer lock stops without publishing.
#[tokio::test]
async fn a_release_that_loses_the_writer_lock_does_not_publish() {
    let e = env().await;
    let a = e.root.join("batch_fx_x_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(40).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_x_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let r = Release {
        stage_index_storage: false,
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: false,
        synthetic: true,
        now: Utc::now(),
        titles_left: None,
        american_stories: false,
        ja_latin: false,
        ocr_local: false,
        congress: false,
        congress_manual: false,
        partition_decade: false,
        single_text_field: false,
        page_language: false,
        split_docs: None,
    };
    let lease = r.lock().await.unwrap();
    // Another writer takes the lock over (e.g. after this one stalled).
    let taken = serde_json::json!({
        "id": "quickwit-writer", "kind": "quickwit-writer", "owner": "someone-else",
        "until": Utc::now() + chrono::Duration::hours(1),
    });
    e.state
        .docs
        .upsert("ops", "quickwit-writer", &taken)
        .await
        .unwrap();
    let mut sink = JsonlSink::new(e.root.join("idx"));
    assert!(r.run_held(&mut sink, &lease).await.is_err());
    assert!(e.reference.get("current.json").await.unwrap().is_none());
}

/// The pipeline state with writes of the release's progress item failing,
/// as when Cosmos throttles or drops one request.
struct NoProgress(MemoryDocs, Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl usnm_ingest::docs::DocStore for NoProgress {
    async fn get(
        &self,
        c: &str,
        pk: &str,
        id: &str,
    ) -> anyhow::Result<Option<usnm_ingest::docs::Versioned>> {
        self.0.get(c, pk, id).await
    }
    async fn create(&self, c: &str, pk: &str, doc: &Value) -> anyhow::Result<Option<String>> {
        self.0.create(c, pk, doc).await
    }
    async fn replace(
        &self,
        c: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>> {
        self.0.replace(c, pk, doc, etag).await
    }
    async fn upsert(&self, c: &str, pk: &str, doc: &Value) -> anyhow::Result<()> {
        if pk == "release-progress" {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("Cosmos upsert in `ops` returned 503 Service Unavailable");
        }
        self.0.upsert(c, pk, doc).await
    }
    async fn list(
        &self,
        c: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<usnm_ingest::docs::Versioned>> {
        self.0.list(c, field, values).await
    }
}

/// Recording progress for the status page never fails a release.
#[tokio::test]
async fn a_release_publishes_when_its_progress_cannot_be_recorded() {
    let mut e = env().await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    e.state = State::new(Arc::new(NoProgress(
        MemoryDocs::default(),
        attempts.clone(),
    )));
    let a = e.root.join("batch_fx_p_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(40).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_p_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    // The commit records when the batch was curated.
    let (b, _) = e.state.batch("batch_fx_p").await.unwrap().unwrap();
    assert!(b.curated_at.is_some_and(|t| t == b.updated_at));
    let p = e.release(1, true).await.unwrap();
    assert!(p.docs > 0);
    assert!(e.reference.get("current.json").await.unwrap().is_some());
    // The release did try (the first report is immediate).
    assert!(attempts.load(std::sync::atomic::Ordering::SeqCst) > 0);
}

/// A sink whose final check fails, as a writer that died mid-merge does.
struct FailingSink;

#[async_trait::async_trait]
impl usnm_ingest::sink::IndexSink for FailingSink {
    async fn create(
        &mut self,
        _index_id: &str,
        _: usnm_ingest::sink::MainLayout,
    ) -> anyhow::Result<()> {
        Ok(())
    }
    async fn add(&mut self, _doc: &Value) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&mut self, _expected: u64) -> anyhow::Result<()> {
        anyhow::bail!("the writer exited")
    }
    fn backend(&self) -> &'static str {
        "memory"
    }
}

/// A release that fails records the run as failed, with when it failed, for the status page.
#[tokio::test]
async fn a_failed_release_records_when_it_failed() {
    let e = env().await;
    let a = e.root.join("batch_fx_fail_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(20).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_fail_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 9, 3, 0, 0).unwrap();
    let r = Release {
        stage_index_storage: false,
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: true,
        synthetic: true,
        now,
        titles_left: None,
        american_stories: false,
        ja_latin: false,
        ocr_local: false,
        congress: false,
        congress_manual: false,
        partition_decade: false,
        single_text_field: false,
        page_language: false,
        split_docs: None,
    };
    let before = Utc::now();
    let err = r.run(&mut FailingSink).await.unwrap_err();
    assert!(format!("{err:#}").contains("the writer exited"));
    let (run, _) = e
        .state
        .run("pages-v20261009-1")
        .await
        .unwrap()
        .expect("the run is recorded");
    assert_eq!(run.status, usnm_state::state::RunStatus::Failed);
    let failed_at = run.failed_at.expect("failed_at is recorded");
    assert!(
        failed_at >= before,
        "failed_at is when it failed, not when it started"
    );
    assert!(run.last_error.unwrap().contains("the writer exited"));
}
