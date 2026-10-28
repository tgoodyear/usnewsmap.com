//! American Stories' text beside LoC's (`--american-stories`, 05 §5.5.4) and the
//! ways a full release lays the text out (`--single-text-field`, `--split-docs`).

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{TimeZone, Utc};
use serde_json::Value;
use usnm_core::text::Analyzer;
use usnm_core::text_layout::TextLayout;
use usnm_ingest::release::{Published, Release};
use usnm_ingest::sink::JsonlSink;
use usnm_store::ObjectStore;

use crate::common::{
    by_id, curate, curate_early_and_late, env, fixture_pages, fixtures, listed, loc_fixture,
    put_american_stories, put_overlay_part, quick_merges, read_jsonl, sha256_file, write_archive,
    Env, JaRow, Page,
};

/// A fixture index as it is, American Stories' text included (05 §5.5.4).
fn fixture(index_id: &str) -> BTreeMap<String, Value> {
    by_id(read_jsonl(
        &fixtures().join(format!("indexes/{index_id}.jsonl")),
    ))
}

impl Env {
    /// A main index laid out with one searched field for all the texts (05
    /// §5.5.7) as `layout` has them: each document's `text_all` and
    /// `text_all_cg` checked against its stored texts and returned without
    /// them, as the fixtures hold. None has a pairs field of one text, and
    /// only layout 3 may have `text_loc` (#925).
    fn index_single(&self, id: &str, layout: TextLayout) -> BTreeMap<String, Value> {
        let mut docs = by_id(read_jsonl(
            &self.root.join(format!("reference/indexes/{id}.jsonl")),
        ));
        for (doc_id, doc) in &mut docs {
            let o = doc.as_object_mut().unwrap();
            assert!(
                o.get("text_cg").is_none() && o.get("text_as_cg").is_none(),
                "{doc_id}"
            );
            let words = o.remove("text_all").unwrap_or_else(|| panic!("{doc_id}"));
            let pairs = o
                .remove("text_all_cg")
                .unwrap_or_else(|| panic!("{doc_id}"));
            assert!(
                layout.keeps_loc_original() || doc.get("text_loc").is_none(),
                "{doc_id}"
            );
            let want = usnm_core::text_layout::index_texts(
                std::iter::once(doc["text"].as_str().unwrap())
                    .chain(doc["text_as"].as_str())
                    .chain(doc["text_loc"].as_str()),
                Analyzer::LATEST,
            );
            assert_eq!(
                (words, pairs),
                (Value::from(want.0), Value::from(want.1)),
                "{doc_id}"
            );
        }
        docs
    }

    /// A release with American Stories' text, and `--single-text-field` as
    /// `single` says.
    async fn release_single(&self, day: u32, full: bool, single: bool) -> Published {
        let r = Release {
            american_stories: true,
            single_text_field: single,
            page_language: false,
            split_docs: None,
            ..self.releaser(day, full)
        };
        let mut sink = JsonlSink::new(self.root.join("reference/indexes"));
        r.run(&mut sink).await.unwrap().unwrap()
    }
}

/// With `--american-stories` (#218, 05 §5.5.4) the release reproduces the
/// fixtures in full: `text_as` and its pairs exactly on the pages American
/// Stories covers, and the pages only it has text for indexed with an empty
/// `text`. The version says so in `current.json` and its build record, and
/// records the parts it read. Without the setting, the next release is as
/// before.
#[tokio::test]
async fn american_stories_text_is_indexed_beside_locs() {
    let e = env().await;
    let (early, late, early_pages, late_pages) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    let p1 = e.release_with(1, false, true).await.unwrap().unwrap();
    assert!(p1.full);
    let want_base = fixture("pages-base-fixture");
    assert_eq!(e.index(&p1.indexes[0]), want_base);
    assert_eq!(p1.docs, want_base.len() as u64);
    assert_eq!(
        p1.pages, early_pages as u64,
        "the baselines count every page already"
    );
    let only = want_base.values().filter(|d| d["text"] == "").count();
    let with_as = want_base
        .values()
        .filter(|d| d.get("text_as").is_some())
        .count();
    assert!(only > 0 && with_as > only, "{only} {with_as}");
    assert_eq!(
        loc_fixture("pages-base-fixture").len() + only,
        want_base.len()
    );

    let v1 = &p1.index_version;
    let current = e.reference_json("current.json").await;
    assert_eq!(
        current["american_stories"],
        usnm_core::american_stories::VERSION
    );
    let manifest = e.reference_json(&format!("{v1}/manifest.json")).await;
    let built = &manifest["built_from"]["american_stories"];
    assert_eq!(built["docs"], with_as);
    assert_eq!(built["only_american_stories"], only);
    assert_eq!(built["version"], usnm_core::american_stories::VERSION);
    assert_eq!(
        manifest["build"]["features"]["american_stories"],
        usnm_core::american_stories::VERSION
    );
    assert!(manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "american_stories.json"));
    let record = e
        .reference_json(&format!("{v1}/american_stories.json"))
        .await;
    let parts = record["parts"].as_array().unwrap();
    assert_eq!(built["parts"], parts.len());
    assert!(!parts.is_empty());
    assert!(parts
        .iter()
        .all(|p| p["sha256"].as_str().unwrap().len() == 64 && p["bytes"].as_u64() > Some(0)));
    assert_eq!(record["docs"], with_as);

    // A delta keeps writing the text.
    curate(&e, late).await;
    let p2 = e.release_with(8, false, true).await.unwrap().unwrap();
    assert!(!p2.full);
    assert_eq!(
        e.index(p2.indexes.last().unwrap()),
        fixture("pages-delta-fixture-1")
    );
    assert_eq!(p2.pages, (early_pages + late_pages) as u64);
    let v2 = &p2.index_version;
    let current = e.reference_json("current.json").await;
    assert_eq!(
        current["american_stories"],
        usnm_core::american_stories::VERSION
    );
    // The snapshot is the fixture's: no page counts twice.
    for f in [
        "baselines.json",
        "title_pages.json",
        "title_dates.json",
        "language_baselines.json",
    ] {
        let want: Value =
            serde_json::from_slice(&std::fs::read(fixtures().join("fixture-v1").join(f)).unwrap())
                .unwrap();
        assert_eq!(e.reference_json(&format!("{v2}/{f}")).await, want, "{f}");
    }
    // The API searches the text.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert!(refdata.index_set().american_stories());

    // Without the setting, a full release is LoC's text alone again, and
    // the version doesn't claim the text.
    let p3 = e.release_with(15, true, false).await.unwrap().unwrap();
    let mut both = loc_fixture("pages-base-fixture");
    both.extend(loc_fixture("pages-delta-fixture-1"));
    assert_eq!(e.index(&p3.indexes[0]), both);
    let current = e.reference_json("current.json").await;
    assert!(current.get("american_stories").is_none(), "{current}");
    let v3 = &p3.index_version;
    let manifest = e.reference_json(&format!("{v3}/manifest.json")).await;
    assert!(manifest["built_from"].get("american_stories").is_none());
    assert!(manifest["build"]["features"]
        .get("american_stories")
        .is_none());
    assert!(e
        .reference
        .get(&format!("{v3}/american_stories.json"))
        .await
        .unwrap()
        .is_none());
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert!(!refdata.index_set().american_stories());
}

/// Turning the setting on for a version built without it rebuilds every
/// index, so the version can say all of them have the text. Until then,
/// text in the curated store changes nothing: the documents are LoC's.
#[tokio::test]
async fn a_version_without_american_stories_is_rebuilt_in_full_when_it_is_turned_on() {
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    let p1 = e.release(1, false).await.unwrap();
    assert_eq!(
        e.index(&p1.indexes[0]),
        loc_fixture("pages-base-fixture"),
        "off: the text in the store is not read"
    );
    let current = e.reference_json("current.json").await;
    assert!(current.get("american_stories").is_none(), "{current}");

    curate(&e, late).await;
    let p2 = e.release_with(8, false, true).await.unwrap().unwrap();
    assert!(
        p2.full,
        "a delta would mix indexes with and without the text"
    );
    assert_eq!(p2.indexes.len(), 1);
    let mut both = fixture("pages-base-fixture");
    both.extend(fixture("pages-delta-fixture-1"));
    assert_eq!(e.index(&p2.indexes[0]), both);
    let current = e.reference_json("current.json").await;
    assert_eq!(
        current["american_stories"],
        usnm_core::american_stories::VERSION
    );

    // A published version with another version of the text is rebuilt too.
    let mut pointer = current;
    pointer["american_stories"] = serde_json::json!(usnm_core::american_stories::VERSION + 1);
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let pages = fixture_pages();
    let extra: Vec<&Page> = pages.iter().take(3).collect();
    let path = e.root.join("batch_zz_ver01.tar.gz");
    write_archive(&path, &extra, false, true);
    curate(&e, vec![listed("batch_zz_ver01", &path, None)]).await;
    let p3 = e.release_with(9, false, true).await.unwrap().unwrap();
    assert!(p3.full);
}

/// A full release with `--single-text-field` searches all the texts in one
/// field (05 §5.5.7): the documents have `text_all` and `text_all_cg` and
/// no pairs field of one text, and the version says so (layout 3, the
/// latest, #925). Deltas follow the published version whatever the setting
/// says; a full release without it goes back to a field per text.
#[tokio::test]
async fn one_text_field_is_built_by_a_full_release_and_followed_by_deltas() {
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    let p1 = e.release_single(1, true, true).await;
    assert!(p1.full);
    assert_eq!(
        e.index_single(&p1.indexes[0], TextLayout::LATEST),
        fixture("pages-base-fixture")
    );
    let current = e.reference_json("current.json").await;
    assert_eq!(current["text_layout"], 3);
    assert_eq!(
        current["american_stories"],
        usnm_core::american_stories::VERSION
    );
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p1.index_version))
        .await;
    assert_eq!(manifest["build"]["features"]["text_layout"], 3);
    let yaml = manifest["build"]["templates"]["pages"]["yaml"]
        .as_str()
        .unwrap();
    assert!(yaml.contains("name: text_all,") && !yaml.contains("name: text_cg,"));
    assert!(yaml.contains("name: text_loc,"), "{yaml}");
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    let set = refdata.index_set();
    assert_eq!(set.text_layout(), TextLayout::SingleWithLoc);
    assert!(set.american_stories() && !set.counts_american_stories_only());

    // A delta without the setting is laid out as the version is.
    curate(&e, late).await;
    let p2 = e.release_single(8, false, false).await;
    assert!(!p2.full);
    assert_eq!(
        e.index_single(p2.indexes.last().unwrap(), TextLayout::LATEST),
        fixture("pages-delta-fixture-1")
    );
    assert_eq!(e.reference_json("current.json").await["text_layout"], 3);

    // Leaving American Stories' text out can't be done in a delta here:
    // a full base, with a field per text again.
    let pages = fixture_pages();
    let extra: Vec<&Page> = pages.iter().take(3).collect();
    let path = e.root.join("batch_zz_ver01.tar.gz");
    write_archive(&path, &extra, false, true);
    curate(&e, vec![listed("batch_zz_ver01", &path, None)]).await;
    let p3 = e.release_with(9, false, false).await.unwrap().unwrap();
    assert!(p3.full, "the indexes it keeps would hold the text");
    let current = e.reference_json("current.json").await;
    assert!(current.get("text_layout").is_none(), "{current}");
    assert!(current.get("american_stories").is_none(), "{current}");
    let mut both = loc_fixture("pages-base-fixture");
    both.extend(loc_fixture("pages-delta-fixture-1"));
    assert_eq!(e.index(&p3.indexes[0]), both);

    // On a field per text, the setting waits for a full release.
    let more: Vec<&Page> = pages.iter().skip(3).take(3).collect();
    let path = e.root.join("batch_zy_ver01.tar.gz");
    write_archive(&path, &more, false, true);
    curate(&e, vec![listed("batch_zy_ver01", &path, None)]).await;
    let r = Release {
        single_text_field: true,
        page_language: false,
        ..e.releaser(10, false)
    };
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    let p4 = r.run(&mut sink).await.unwrap().unwrap();
    assert!(!p4.full);
    assert!(e
        .reference_json("current.json")
        .await
        .get("text_layout")
        .is_none());
    e.index(p4.indexes.last().unwrap());
}

/// A published version laid out in a way the release doesn't know (a
/// newer one) can't take a delta: the release builds a full base.
#[tokio::test]
async fn a_text_layout_the_release_does_not_know_is_rebuilt_in_full() {
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    e.release_single(1, true, true).await;
    let mut pointer = e.reference_json("current.json").await;
    pointer["text_layout"] = serde_json::json!(TextLayout::LATEST.version() + 1);
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    curate(&e, late).await;
    let p = e.release_single(8, false, true).await;
    assert!(p.full);
    assert_eq!(
        e.reference_json("current.json").await["text_layout"],
        TextLayout::LATEST.version()
    );
    let mut both = fixture("pages-base-fixture");
    both.extend(fixture("pages-delta-fixture-1"));
    assert_eq!(e.index_single(&p.indexes[0], TextLayout::LATEST), both);
}

/// A published version with one field for both texts (layout 2, before
/// #925) takes its deltas in layout 2, whatever `--single-text-field` now
/// builds: the documents have no `text_loc`, `text_all` holds two texts,
/// the index's mapping has no `text_loc`, and `current.json` still says 2.
#[tokio::test]
async fn a_published_layout_2_version_takes_deltas_in_layout_2() {
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    e.release_single(1, true, true).await;
    // The version as a release before #925 would have published it.
    let mut pointer = e.reference_json("current.json").await;
    pointer["text_layout"] = serde_json::json!(TextLayout::Single.version());
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    curate(&e, late).await;
    // Asking for the latest layout changes nothing for a delta.
    let p = e.release_single(8, false, true).await;
    assert!(!p.full);
    assert_eq!(
        e.index_single(p.indexes.last().unwrap(), TextLayout::Single),
        fixture("pages-delta-fixture-1")
    );
    let current = e.reference_json("current.json").await;
    assert_eq!(current["text_layout"], 2, "{current}");
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p.index_version))
        .await;
    assert_eq!(manifest["build"]["features"]["text_layout"], 2);
    let yaml = manifest["build"]["templates"]["pages"]["yaml"]
        .as_str()
        .unwrap();
    assert!(yaml.contains("name: text_all,") && !yaml.contains("name: text_loc,"));
    // The API reads the version as layout 2.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.index_set().text_layout(), TextLayout::Single);
}

/// A full release with `--split-docs` (#283) creates its main index with
/// that split target and records it in the manifest; a delta keeps the
/// template's, whatever the setting says. The documents and `current.json`
/// are as without it.
#[tokio::test]
async fn a_split_target_is_taken_by_a_full_base_only() {
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    let release = |day, full| Release {
        split_docs: Some(120_000),
        ..e.releaser(day, full)
    };
    let build = |p: &Published| {
        let path = format!("{}/manifest.json", p.index_version);
        let e = &e;
        async move { e.reference_json(&path).await["build"].clone() }
    };
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));

    curate(&e, early).await;
    let p1 = release(1, true).run(&mut sink).await.unwrap().unwrap();
    assert!(p1.full);
    let b1 = build(&p1).await;
    assert_eq!(b1["features"]["split_docs"], 120_000);
    let yaml = b1["templates"]["pages"]["yaml"].as_str().unwrap();
    assert!(yaml.contains("  split_num_docs_target: 120000\n"), "{yaml}");
    assert_eq!(e.index(&p1.indexes[0]), loc_fixture("pages-base-fixture"));
    let current = e.reference_json("current.json").await;
    assert!(current.get("split_docs").is_none(), "{current}");

    curate(&e, late).await;
    let p2 = release(8, false).run(&mut sink).await.unwrap().unwrap();
    assert!(!p2.full);
    let b2 = build(&p2).await;
    assert!(b2["features"].get("split_docs").is_none(), "{b2}");
    let yaml = b2["templates"]["pages"]["yaml"].as_str().unwrap();
    assert!(yaml.contains("  split_num_docs_target: 30000\n"), "{yaml}");
    assert_eq!(
        e.index(p2.indexes.last().unwrap()),
        loc_fixture("pages-delta-fixture-1")
    );
}

/// With the setting on and no American Stories text written yet (no
/// finished year), nothing is released: the version would claim text it
/// doesn't have.
#[tokio::test]
async fn the_setting_needs_american_stories_text_in_the_store() {
    let e = env().await;
    let (early, ..) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    // A part of a year the writer hasn't finished doesn't count.
    e.curated
        .put(
            "american-stories/pages/sn99000001/1895-000.parquet",
            usnm_ingest::american_stories::encode_part(&[]).unwrap(),
            "application/octet-stream",
        )
        .await
        .unwrap();
    let err = e.release_with(1, false, true).await.unwrap_err();
    assert!(
        format!("{err:#}").contains("american-stories-write"),
        "{err:#}"
    );
    assert!(e.reference.get("current.json").await.unwrap().is_none());
    // Without the setting the same release goes ahead.
    assert!(e.release(1, false).await.unwrap().full);
}

/// A page only American Stories has text for is a document, so a copy of
/// it in a later batch that wins (its name sorts first) hides the published
/// one, as a copy with LoC's text would (04 §4.7).
#[tokio::test]
async fn a_page_with_only_american_stories_text_is_kept_once() {
    let e = env().await;
    let (early, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    let p1 = e.release_with(1, false, true).await.unwrap().unwrap();
    let want_base = fixture("pages-base-fixture");
    let pages = fixture_pages();
    let id = |p: &Page| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq);
    let only: Vec<&Page> = pages
        .iter()
        .filter(|p| want_base.get(&id(p)).is_some_and(|d| d["text"] == ""))
        .take(2)
        .collect();
    assert_eq!(only.len(), 2);

    let path = e.root.join("batch_aa_ver01.tar.gz");
    write_archive(&path, &only, false, true);
    curate(&e, vec![listed("batch_aa_ver01", &path, None)]).await;
    let p2 = e.release_with(8, false, true).await.unwrap().unwrap();
    assert!(!p2.full);
    assert_eq!(p2.pages, p1.pages, "each page counts once");
    let delta = e.index(p2.indexes.last().unwrap());
    let want: BTreeMap<String, Value> = only
        .iter()
        .map(|p| (id(p), want_base[&id(p)].clone()))
        .collect();
    assert_eq!(delta, want);
    let v2 = &p2.index_version;
    let mut hidden: Vec<Value> = e
        .reference_json(&format!("{v2}/duplicates.json"))
        .await
        .as_array()
        .unwrap()
        .clone();
    hidden.sort_by_key(|h| h["doc_id"].as_str().unwrap().to_owned());
    let mut want_hidden: Vec<Value> = only
        .iter()
        .map(|p| serde_json::json!({"doc_id": id(p), "batch": "batch_fx_early"}))
        .collect();
    want_hidden.sort_by_key(|h| h["doc_id"].as_str().unwrap().to_owned());
    assert_eq!(hidden, want_hidden);

    // A full release keeps batch_aa's copies and hides nothing.
    let p3 = e.release_with(15, true, true).await.unwrap().unwrap();
    assert_eq!(e.index(&p3.indexes[0]), want_base);
    assert_eq!(
        e.reference_json(&format!("{}/duplicates.json", p3.index_version))
            .await,
        serde_json::json!([])
    );
}

/// A release of new Japanese OCR alone keeps the published main indexes,
/// and with them their American Stories text: `current.json` still says so.
#[tokio::test]
async fn a_japanese_ocr_release_keeps_american_stories() {
    let e = env().await;
    let pages = fixture_pages();
    let path = e.root.join("batch_fx_ja_ver01.tar.gz");
    let all: Vec<&Page> = pages.iter().collect();
    write_archive(&path, &all, false, true);
    curate(
        &e,
        vec![listed("batch_fx_ja_ver01", &path, Some(sha256_file(&path)))],
    )
    .await;
    put_american_stories(&e).await;
    let p0 = pages.iter().find(|p| !p.text.is_empty()).unwrap();
    let row = |seq, text| JaRow {
        lccn: &p0.lccn,
        date: p0.date,
        seq,
        batch: "batch_fx_ja",
        loc_text: "missing",
        text,
        ocred_at: 1,
    };
    put_overlay_part(&e, "ocr-ja/pages/a.parquet", &[row(90, "米國と日本の戰爭")]).await;
    let v1 = e.release_with(5, true, true).await.unwrap().unwrap();
    put_overlay_part(&e, "ocr-ja/pages/b.parquet", &[row(91, "東京の新聞")]).await;
    let v2 = e.release_with(6, false, true).await.unwrap().unwrap();
    assert!(!v2.full);
    assert_eq!(v2.indexes, v1.indexes, "no new main index");
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v2.index_version.as_str());
    assert_eq!(
        current["american_stories"],
        usnm_core::american_stories::VERSION
    );
    // It built no main index, so it read no American Stories text.
    let manifest = e
        .reference_json(&format!("{}/manifest.json", v2.index_version))
        .await;
    assert!(manifest["built_from"].get("american_stories").is_none());
}

/// The documents with American Stories' text go into a real Quickwit
/// writer under the strict mapping, and both texts are searchable. Runs
/// when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn releases_american_stories_into_a_quickwit_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7401, &meta, &root)
        .await
        .unwrap();
    let mut published = Vec::new();
    for (day, list) in [(1, early), (8, late)] {
        curate(&e, list).await;
        let r = Release {
            stage_index_storage: false,
            state: e.state.clone(),
            curated: e.curated.clone(),
            reference: e.reference.clone(),
            owner: "releaser".into(),
            full: false,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
            american_stories: true,
            ja_latin: false,
            ocr_local: false,
            congress: false,
            congress_manual: false,
            partition_decade: false,
            single_text_field: false,
            page_language: false,
            split_docs: None,
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    assert_eq!(p.indexes.len(), 2);
    let current = e.reference_json("current.json").await;
    assert_eq!(
        current["american_stories"],
        usnm_core::american_stories::VERSION
    );
    let http = reqwest::Client::new();
    let count = |q: String| {
        let url = format!("{}/api/v1/{}/search", node.url, p.indexes.join(","));
        let http = http.clone();
        async move {
            let v: Value = http
                .post(url)
                .json(&serde_json::json!({"query": q, "max_hits": 0}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["num_hits"].as_u64().unwrap_or_else(|| panic!("{v}"))
        }
    };
    let docs: Vec<Value> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|f| fixture(f).into_values())
        .collect();
    assert_eq!(count("*".into()).await, docs.len() as u64);
    let has = |field: &str, phrase: &str| {
        docs.iter()
            .filter(|d| d[field].as_str().is_some_and(|t| t.contains(phrase)))
            .count() as u64
    };
    let want = has("text_as", "cross of gold");
    assert!(want > 0);
    assert_eq!(count("text_as:\"cross of gold\"".into()).await, want);
    // The pairs field, as the API queries a phrase with a common word.
    let pairs = usnm_core::common_grams::index_text("cross of gold", Analyzer::LATEST);
    assert_eq!(
        count(format!(
            "text_as_cg:\"{pairs}\" AND text_as:\"cross of gold\""
        ))
        .await,
        want
    );
    node.stop().await.unwrap();
}

/// One searched field for all the texts (05 §5.5.7) goes into a real
/// Quickwit writer under the strict mapping: a base and a delta with
/// `text_all` and `text_all_cg`, `text`, `text_as` and `text_loc` stored
/// (layout 3, #925), and a phrase can't span the two texts. Runs when
/// `QUICKWIT_BIN` is set.
#[tokio::test]
async fn releases_one_text_field_into_a_quickwit_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    let e = env().await;
    let (early, late, ..) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7405, &meta, &root)
        .await
        .unwrap();
    let mut published = Vec::new();
    for (day, full, list) in [(1, true, early), (8, false, late)] {
        curate(&e, list).await;
        let r = Release {
            american_stories: true,
            single_text_field: true,
            page_language: false,
            ..e.releaser(day, full)
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    assert_eq!(p.indexes.len(), 2);
    assert_eq!(e.reference_json("current.json").await["text_layout"], 3);
    let http = reqwest::Client::new();
    let search = |q: String| {
        let url = format!("{}/api/v1/{}/search", node.url, p.indexes.join(","));
        let http = http.clone();
        async move {
            let v: Value = http
                .post(url)
                .json(&serde_json::json!({"query": q, "max_hits": 1}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v
        }
    };
    let count = |q: String| {
        let s = search(q);
        async move {
            let v = s.await;
            v["num_hits"].as_u64().unwrap_or_else(|| panic!("{v}"))
        }
    };
    let docs: Vec<Value> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|f| fixture(f).into_values())
        .collect();
    assert_eq!(count("*".into()).await, docs.len() as u64);
    let has = |phrase: &str| {
        docs.iter()
            .filter(|d| {
                ["text", "text_as"].iter().any(|f| {
                    d[*f]
                        .as_str()
                        .is_some_and(|t| t.to_lowercase().contains(phrase))
                })
            })
            .count() as u64
    };
    let want = has("cross of gold");
    assert!(want > 0);
    assert_eq!(count("text_all:\"cross of gold\"".into()).await, want);
    let pairs = usnm_core::common_grams::index_text("cross of gold", Analyzer::LATEST);
    assert_eq!(
        count(format!(
            "text_all_cg:\"{pairs}\" AND text_all:cross AND text_all:gold"
        ))
        .await,
        want
    );
    // A word only American Stories' headlines have.
    assert_eq!(
        count("text_all:bimetallism".into()).await,
        has("bimetallism")
    );
    // The texts are stored for snippets, and not searched on their own.
    let hit = search("text_all:bimetallism".into()).await;
    assert!(hit["hits"][0]["text_as"].as_str().is_some(), "{hit}");
    assert!(hit["hits"][0].get("text_all").is_none(), "{hit}");
    let refused = search("text:gold".into()).await;
    assert!(refused.get("num_hits").is_none(), "{refused}");
    // No phrase across the end of LoC's text and the start of American
    // Stories': a word near the end of one page's LoC text that its
    // American Stories text lacks, and one near the start of that text that
    // LoC's lacks, are within the largest slop a query may have of each
    // other but for the gap.
    let tokens = |d: &Value, f: &str| {
        usnm_core::text::tokenize(d[f].as_str().unwrap_or_default(), Analyzer::LATEST)
    };
    let (id, a, b) = docs
        .iter()
        .find_map(|d| {
            let (loc, american) = (tokens(d, "text"), tokens(d, "text_as"));
            let a = loc.iter().rev().take(10).find(|w| !american.contains(w))?;
            let b = american.iter().take(10).find(|w| !loc.contains(w))?;
            Some((d["doc_id"].as_str()?.to_owned(), a.clone(), b.clone()))
        })
        .expect("a page with words only near the end of LoC's text and the start of the other");
    let slop = usnm_core::query::MAX_SLOP;
    assert_eq!(
        count(format!("text_all:\"{a} {b}\"~{slop} AND doc_id:{id}")).await,
        0
    );
    assert_eq!(
        count(format!("text_all:\"{b} {a}\"~{slop} AND doc_id:{id}")).await,
        0
    );
    assert_eq!(
        count(format!("text_all:{a} AND text_all:{b} AND doc_id:{id}")).await,
        1
    );
    node.stop().await.unwrap();
}
