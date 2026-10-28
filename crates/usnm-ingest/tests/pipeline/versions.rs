//! Versions and their snapshots: the analyzer a version was built with, the run
//! item and batch list, staged index storage, and `prune`.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_core::text::Analyzer;
use usnm_ingest::release::Release;
use usnm_ingest::sink::JsonlSink;
use usnm_ingest::source;
use usnm_store::ObjectStore;

use crate::common::{
    by_id, env, fixture_pages, listed, loc_fixture, put_globe, quick_merges, raw_run, read_jsonl,
    write_archive, Env, Page,
};

#[tokio::test]
async fn a_version_without_these_common_word_pairs_is_rebuilt_in_full() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let base_archive = e.root.join("batch_fx_early_ver01.tar.gz");
    let delta_archive = e.root.join("batch_fx_late_ver01.tar.bz2");
    write_archive(&base_archive, &early, true, true);
    write_archive(&delta_archive, &late, false, false);
    let list = [listed("batch_fx_early_ver01", &base_archive, None)];
    source::enqueue(&e.state, &list).await.unwrap();
    e.worker("w1").run(None).await.unwrap();
    assert!(e.release(1, false).await.unwrap().full);
    // The published version names the pairs its indexes hold (05 §5.5.3).
    let mut pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], usnm_core::common_grams::VERSION);

    // A version published before the pairs (or with another list): the
    // next release rebuilds every index rather than add a delta to it.
    pointer.as_object_mut().unwrap().remove("common_grams");
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let list = [listed("batch_fx_late_ver01", &delta_archive, None)];
    source::enqueue(&e.state, &list).await.unwrap();
    e.worker("w2").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(
        p2.full,
        "a delta would mix indexes with and without the pairs"
    );
    assert_eq!(p2.indexes.len(), 1);
    let mut pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], usnm_core::common_grams::VERSION);

    // Pairs from a newer release than this code knows: rebuilt too.
    pointer["common_grams"] = (usnm_core::common_grams::VERSION + 1).into();
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let p3 = e.release(15, false).await.unwrap();
    assert!(p3.full, "pairs this release doesn't know");
    let pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], usnm_core::common_grams::VERSION);
}

/// A delta on a version built with an older analyzer folds its words as
/// that version's indexes do (#168): its pairs are at the published
/// version's, which the pointer and the build record keep naming, so every
/// index of the version tokenizes alike. A full rebuild moves the version
/// to the latest.
#[tokio::test]
async fn a_delta_folds_its_words_as_the_published_version_does() {
    let e = env().await;
    let pages = fixture_pages();
    // The market reports with fractions are on pages of March 1896.
    let split = NaiveDate::from_ymd_opt(1896, 3, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let base_archive = e.root.join("batch_fx_early_ver01.tar.gz");
    let delta_archive = e.root.join("batch_fx_late_ver01.tar.bz2");
    write_archive(&base_archive, &early, true, true);
    write_archive(&delta_archive, &late, false, false);
    let list = [listed("batch_fx_early_ver01", &base_archive, None)];
    source::enqueue(&e.state, &list).await.unwrap();
    e.worker("w1").run(None).await.unwrap();
    assert!(e.release(1, false).await.unwrap().full);

    // As if the base had been built before #168, with version 1.
    let mut pointer = e.reference_json("current.json").await;
    pointer["common_grams"] = 1.into();
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let list = [listed("batch_fx_late_ver01", &delta_archive, None)];
    source::enqueue(&e.state, &list).await.unwrap();
    e.worker("w2").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(
        !p2.full,
        "a version with pairs this release knows takes a delta"
    );
    let delta = &p2.indexes[1];
    // Every document's pairs are version 1's (`Env::index_as` checks).
    let docs = e.index_as(delta, Analyzer::V1);
    assert_eq!(
        docs.len(),
        loc_fixture("pages-delta-fixture-1").len() + {
            let base = loc_fixture("pages-base-fixture");
            base.values()
                .filter(|d| d["date"].as_str().unwrap() >= "1896-03-01")
                .count()
        }
    );
    let raw = by_id(read_jsonl(
        &e.root.join(format!("reference/indexes/{delta}.jsonl")),
    ));
    let market = &raw["sn99000001_1896-03-07_ed-1_seq-2"];
    assert!(market["text"].as_str().unwrap().contains("½ higher at 61¼"));
    let pairs = market["text_cg"].as_str().unwrap();
    assert!(
        pairs.contains("1\u{2044}2 higher at_611\u{2044}4 611\u{2044}4") && !pairs.contains('½'),
        "{pairs}"
    );
    let pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], 1);
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p2.index_version))
        .await;
    assert_eq!(manifest["build"]["features"]["common_grams"], 1);
    assert_eq!(manifest["build"]["features"]["ja_fold"], 1);

    // A full rebuild builds every index with the latest analyzer.
    let p3 = e.release(15, true).await.unwrap();
    assert!(p3.full);
    let pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], usnm_core::common_grams::VERSION);
    let docs = e.index(&p3.indexes[0]);
    assert!(docs.contains_key("sn99000001_1896-03-07_ed-1_seq-2"));
    let raw = by_id(read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", p3.indexes[0])),
    ));
    let pairs = raw["sn99000001_1896-03-07_ed-1_seq-2"]["text_cg"]
        .as_str()
        .unwrap()
        .to_owned();
    assert!(pairs.contains("½ higher at_61¼ 61¼"), "{pairs}");
}

#[tokio::test]
async fn index_storage_stages_a_fresh_base_without_changing_the_serving_version() {
    let e = env().await;
    let pages = fixture_pages();
    let archive = e.root.join("batch_fx_storage_ver01.tar.gz");
    write_archive(&archive, &pages.iter().collect::<Vec<_>>(), true, true);
    source::enqueue(
        &e.state,
        &[listed("batch_fx_storage_ver01", &archive, None)],
    )
    .await
    .unwrap();
    e.worker("w").run(None).await.unwrap();
    put_globe(&e, &[1, 2, 3, 300, 301]).await;
    let old = e.release_congress(1, false, true).await.unwrap();
    let current = e.reference.get("current.json").await.unwrap().unwrap();
    let release = Release {
        stage_index_storage: true,
        congress: true,
        congress_manual: false,
        ..e.releaser(2, false)
    };
    let mut sink = JsonlSink::new(e.root.join("new-account"));
    let staged = release.run(&mut sink).await.unwrap().unwrap();
    assert!(staged.full);
    assert_eq!(staged.indexes.len(), 1);
    assert!(!staged.indexes.iter().any(|i| old.indexes.contains(i)));
    assert_eq!(
        e.reference.get("current.json").await.unwrap().unwrap(),
        current
    );
    assert_eq!(
        e.state.current_version().await.unwrap().as_deref(),
        Some(old.index_version.as_str())
    );
    let pending = e
        .reference
        .get("index-storage-pending.json")
        .await
        .unwrap()
        .unwrap();
    let pointer: Value = serde_json::from_slice(&pending).unwrap();
    assert_eq!(pointer["index_version"], staged.index_version);
    assert_eq!(pointer["previous_version"], old.index_version);
    let new_congress = pointer["congress"]["indexes"][0].as_str().unwrap();
    assert_eq!(new_congress, "congress-20261002-1");
    assert!(
        e.root
            .join("new-account")
            .join(format!("{new_congress}.jsonl"))
            .exists(),
        "unchanged Congress pages must be rebuilt in the new account"
    );
    let kept = usnm_ingest::prune::kept(e.reference.as_ref(), &e.state)
        .await
        .unwrap();
    assert!(
        kept.indexes.contains(&staged.indexes[0]),
        "pending version survives prune"
    );
    e.reference
        .put("current.json", pending, "application/json")
        .await
        .unwrap();
    let kept = usnm_ingest::prune::kept(e.reference.as_ref(), &e.state)
        .await
        .unwrap();
    assert_eq!(kept.rollback.as_deref(), Some(old.index_version.as_str()));
    assert!(kept.indexes.contains(&old.indexes[0]));
    // Promoted, the staged version becomes the pipeline's current one
    // (`import-staged-run --current`) without an ordinary release.
    let (run, _) = e.state.run(&staged.index_version).await.unwrap().unwrap();
    let recorded = usnm_ingest::onprem::record_current(&e.state, e.reference.as_ref(), &run)
        .await
        .unwrap();
    assert_eq!(
        recorded.previous.as_deref(),
        Some(old.index_version.as_str())
    );
    assert_eq!(
        e.state.current_version().await.unwrap().as_deref(),
        Some(staged.index_version.as_str())
    );
}

/// `prune` against a real writer node (#245): four versions, a rebuild
/// that stopped after creating its index, and an index of another name.
/// The dry run lists the indexes of the versions before the rollback and
/// the stopped rebuild, with their sizes, and deletes nothing; `--delete`
/// deletes exactly those, and the serving version still searches.
/// Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn prune_deletes_what_no_kept_version_names_from_a_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::prune;
    use usnm_ingest::release::{lock_writer, unlock_writer};
    use usnm_ingest::sink::{IndexSink, MainLayout, QuickwitNode, QuickwitSink};
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);

    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7409, &meta, &root)
        .await
        .unwrap();

    // A base, a delta on it, then two full bases.
    for (day, full, batch) in [
        (1, false, Some(("batch_fx_early_ver01", &a))),
        (8, false, Some(("batch_fx_late_ver01", &b))),
        (15, true, None),
        (22, true, None),
    ] {
        if let Some((name, path)) = batch {
            source::enqueue(&e.state, &[listed(name, path, None)])
                .await
                .unwrap();
            e.worker("w").run(None).await.unwrap();
        }
        let r = e.releaser(day, full);
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        r.run(&mut sink).await.unwrap().unwrap();
    }
    // A rebuild stopped after it created its index, and an index of another name.
    let mut sink = QuickwitSink::new(&node.url, &root).unwrap();
    sink.create("pages-base-20261029-1", MainLayout::default())
        .await
        .unwrap();
    sink.create("other-index", MainLayout::default())
        .await
        .unwrap();
    drop(sink);

    let http = reqwest::Client::new();
    let reference: &dyn ObjectStore = e.reference.as_ref();
    let lease = lock_writer(&e.state, "pruner").await.unwrap();
    let dry = prune::prune(&e.state, reference, &node.url, false, &lease)
        .await
        .unwrap();
    assert_eq!(dry.kept.serving, "pages-v20261022-1");
    assert_eq!(dry.kept.rollback.as_deref(), Some("pages-v20261015-1"));
    let gone = [
        "pages-base-20261001-1",
        "pages-base-20261029-1",
        "pages-delta-20261008-1",
    ];
    let ids: Vec<&str> = dry.candidates.iter().map(|c| c.index_id.as_str()).collect();
    assert_eq!(ids, gone);
    for c in &dry.candidates {
        // The stopped rebuild has no splits; the versions' indexes do.
        assert_eq!(c.bytes > 0, c.index_id != "pages-base-20261029-1", "{c:?}");
    }
    assert_eq!(
        dry.total_bytes,
        dry.candidates.iter().map(|c| c.bytes).sum::<u64>()
    );
    // Left alone: the node's own telemetry indexes (`otel-…`) too.
    assert!(
        dry.other.contains(&"other-index".to_owned()),
        "{:?}",
        dry.other
    );
    assert!(!dry.other.iter().any(|i| prune::is_ours(i)));
    let before = prune::list_indexes(&http, &node.url).await.unwrap();

    let done = prune::prune(&e.state, reference, &node.url, true, &lease)
        .await
        .unwrap();
    unlock_writer(&e.state, "pruner", lease).await.unwrap();
    assert_eq!(done.deleted, gone);
    let after = prune::list_indexes(&http, &node.url).await.unwrap();
    let expected: Vec<String> = before
        .into_iter()
        .filter(|i| !gone.contains(&i.as_str()))
        .collect();
    assert_eq!(after, expected);
    assert!(after.contains(&"pages-base-20261015-1".to_owned()));
    assert!(after.contains(&"pages-base-20261022-1".to_owned()));
    // Quickwit deleted the split files with the index; the kept ones are there.
    let split_files = |id: &str| {
        std::fs::read_dir(qw.join("indexes").join(id))
            .map(|d| {
                d.flatten()
                    .filter(|f| f.path().extension().is_some_and(|x| x == "split"))
                    .count()
            })
            .unwrap_or(0)
    };
    assert_eq!(split_files("pages-base-20261001-1"), 0);
    assert_eq!(split_files("pages-delta-20261008-1"), 0);
    assert!(split_files("pages-base-20261022-1") > 0);
    // The serving version and its rollback still search every page.
    let fixture_docs = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .map(|f| loc_fixture(f).len() as u64)
        .sum::<u64>();
    for index in ["pages-base-20261022-1", "pages-base-20261015-1"] {
        let v: Value = http
            .post(format!("{}/api/v1/{index}/search", node.url))
            .json(&serde_json::json!({"query": "*", "max_hits": 0}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["num_hits"].as_u64(), Some(fixture_docs), "{index}: {v}");
    }
    node.stop().await.unwrap();
}

/// Curate one small real batch, then add `n` curated batches that copy its
/// parts and counts to paths as long as production's (a job replica's
/// owner id in every attempt path) and list every fixture title.
async fn synthetic_batches(e: &Env, n: usize) -> Vec<String> {
    let pages = fixture_pages();
    let mut one_per_title: BTreeMap<&str, &Page> = BTreeMap::new();
    for p in pages.iter().filter(|p| !p.text.is_empty()) {
        one_per_title.entry(p.lccn.as_str()).or_insert(p);
    }
    let seed: Vec<&Page> = one_per_title.into_values().collect();
    let a = e.root.join("batch_fx_seed_ver01.tar.gz");
    write_archive(&a, &seed, true, true);
    source::enqueue(&e.state, &[listed("batch_fx_seed_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let (template, _) = e.state.batch("batch_fx_seed").await.unwrap().unwrap();
    let c = template.curated.clone().unwrap();
    let part = e.curated.get(&c.parts[0]).await.unwrap().unwrap();
    let counts = e.curated.get(&c.counts).await.unwrap().unwrap();
    let mut names = Vec::new();
    for i in 0..n {
        let name = format!("xx_synthetic{i:05}");
        let prefix = format!(
            "pages/{name}/v01/20260929T101112123456Z-caj-usnm-backfill-4f7x2-kq9zd-1-0badf00d-a1"
        );
        let mut curated = c.clone();
        curated.parts = Vec::new();
        for k in 0..2 {
            let path = format!("{prefix}/part-{k:04}.parquet");
            e.curated
                .put(&path, part.clone(), "application/octet-stream")
                .await
                .unwrap();
            curated.parts.push(path);
        }
        curated.counts = format!("{prefix}/counts.json");
        e.curated
            .put(&curated.counts, counts.clone(), "application/json")
            .await
            .unwrap();
        let mut b = template.clone();
        b.id = name.clone();
        b.batch = name.clone();
        b.curated = Some(curated);
        assert!(e.state.create_batch(&b).await.unwrap());
        names.push(name);
    }
    names
}

/// A version's batch list grows with the corpus; its Cosmos run item must
/// not (Cosmos DB refuses items over 2 MB). The list lives in the version's
/// reference snapshot, and the next incremental release reads it from there.
#[tokio::test]
async fn run_items_stay_small_however_many_batches_a_version_has() {
    const N: usize = 3_000;
    let e = env().await;
    synthetic_batches(&e, N).await;
    let p1 = e.release(1, true).await.unwrap();
    let v1 = &p1.index_version;
    let item = serde_json::to_vec(&raw_run(&e, v1).await).unwrap().len();
    let list = e
        .reference
        .get(&format!("{v1}/batches.json"))
        .await
        .unwrap()
        .unwrap()
        .len();
    eprintln!(
        "{} batches: run item {item} bytes, batch list {list} bytes ({} per batch)",
        N + 1,
        list / (N + 1)
    );
    assert!(item < 64 * 1024, "run item is {item} bytes");
    // Inline, the list alone would be most of the way to the limit.
    assert!(list > 1_000_000, "batch list is {list} bytes");
    let (run, _) = e.state.run(v1).await.unwrap().unwrap();
    assert_eq!(run.batch_count, Some(N as u64 + 1));
    assert!(run.batches.is_none());

    // One more batch: the next release is a delta with only that batch.
    let pages = fixture_pages();
    let late: Vec<&Page> = pages
        .iter()
        .filter(|p| !p.text.is_empty())
        .rev()
        .take(5)
        .collect();
    let a = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &late, true, true);
    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(p2.indexes.len(), 2);
    assert_eq!(p2.docs, 5);
    let v2 = &p2.index_version;
    let listed2 = e.reference_json(&format!("{v2}/batches.json")).await;
    assert_eq!(listed2.as_array().unwrap().len(), N + 2);
    assert_eq!(raw_run(&e, v2).await["batch_count"], N + 2);
    assert!(serde_json::to_vec(&raw_run(&e, v2).await).unwrap().len() < 64 * 1024);
    // Nothing new since: no release.
    assert!(e.release(9, false).await.is_none());
}

/// Runs published before the batch list moved to the snapshot carry it
/// inline (and their snapshots have no `batches.json`). The next release
/// reads the inline list and writes the new format.
#[tokio::test]
async fn an_incremental_release_builds_on_a_run_with_an_inline_batch_list() {
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
    let v1 = &p1.index_version;

    // Rewrite the published run the way older releases stored it.
    let list = e.reference_json(&format!("{v1}/batches.json")).await;
    let mut old = raw_run(&e, v1).await;
    let doc = old.as_object_mut().unwrap();
    doc.remove("batch_count");
    doc.remove("batch_list");
    doc.insert("batches".into(), list);
    e.state.docs.upsert("index_runs", v1, &old).await.unwrap();
    std::fs::remove_file(e.root.join(format!("reference/{v1}/batches.json"))).unwrap();
    let (run, _) = e.state.run(v1).await.unwrap().unwrap();
    assert_eq!(run.batches.as_ref().map(Vec::len), Some(1));

    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    let want_delta = loc_fixture("pages-delta-fixture-1");
    assert_eq!(e.index(&p2.indexes[1]), want_delta);
    let item = raw_run(&e, &p2.index_version).await;
    assert!(item.get("batches").is_none());
    assert_eq!(item["batch_count"], 2);
    let list = e
        .reference_json(&format!("{}/batches.json", p2.index_version))
        .await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    // The older item keeps its inline list.
    assert!(raw_run(&e, v1).await["batches"].is_array());
}

/// A batch list that doesn't match its manifest entry stops the release.
#[tokio::test]
async fn a_batch_list_that_does_not_match_its_manifest_stops_the_release() {
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
    // Emptied, the published list would make every batch look new.
    e.reference
        .put(
            &format!("{}/batches.json", p1.index_version),
            b"[]".to_vec(),
            "application/json",
        )
        .await
        .unwrap();
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
    let mut sink = JsonlSink::new(e.root.join("idx"));
    let err = format!("{:#}", r.run(&mut sink).await.unwrap_err());
    assert!(err.contains("does not match its manifest entry"), "{err}");
}
