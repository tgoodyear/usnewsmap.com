//! The Congress set (`--congress`, #229): the Globe and the Serial Set in an
//! index set of their own.

use std::path::Path;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_ingest::release::Release;
use usnm_store::ObjectStore;

use crate::common::{
    curate, env, fixture_pages, fixtures, listed, put_globe, quick_merges, raw_run, write_archive,
    Env, Page,
};

/// Serial Set pages curated from the recorded GovInfo responses (#229
/// phase 2), as `congress-serialset` writes them: volume 33's two
/// documents, their MODS and the page texts pdftotext gave.
async fn put_serialset(e: &Env) -> usnm_ingest::congress::curated::Counts {
    use usnm_ingest::congress::{curated, serialset};
    let dir = fixtures().join("../congress/govinfo");
    let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
    let urls = serialset::parse_urlset(&read("SERIALSET_1820_sitemap.xml")).unwrap();
    let plan = serialset::plan(urls);
    let volume = plan.volumes.iter().find(|v| v.serial == 33).unwrap();
    let at = Utc.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap();
    let mut rows = Vec::new();
    for d in &volume.documents {
        let mods = serialset::parse_mods(&read(&format!("{}-mods.xml", d.id.id))).unwrap();
        let text = String::from_utf8(read(&format!("{}-pages.txt", d.id.id))).unwrap();
        let pages = serialset::split_pages(&text);
        rows.extend(serialset::document_rows(&d.id, &mods, &pages, at).unwrap());
    }
    curated::write_attempt_from(
        e.curated.as_ref(),
        serialset::SERIES,
        serialset::SOURCE,
        &volume.volume,
        &rows,
        at,
        volume.lastmod(),
        Some(volume.fingerprint()),
    )
    .await
    .unwrap()
}

/// `--congress` (#229): the curated Congress pages get an index set of
/// their own with their own baselines, kept while they don't change,
/// rebuilt (even without new batches) when they do, and left out without
/// the setting. The newspapers' baselines never count them.
#[tokio::test]
async fn congress_pages_get_an_index_set_of_their_own() {
    use usnm_ingest::congress;
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    curate(&e, vec![listed("batch_fx_early_ver01", &a, None)]).await;
    let first = put_globe(&e, &[1, 2, 3, 300, 301]).await;
    assert_eq!((first.pages, first.indexable), (5, 4));
    let day = |y, m, d| usnm_core::time::day_number(NaiveDate::from_ymd_opt(y, m, d).unwrap());

    // The first release builds the set.
    let p1 = e.release_congress(1, false, true).await.unwrap();
    let v1 = &p1.index_version;
    let current = e.reference_json("current.json").await;
    let id1 = "congress-20261001-1";
    assert_eq!(
        current["congress"],
        serde_json::json!({
            "indexes": [id1],
            "common_grams": usnm_core::common_grams::VERSION,
            "series": [{"series": "globe", "pages": 5, "from": "1837-12-04", "to": "1838-03-29"}],
        })
    );
    let docs = e.index(id1);
    assert_eq!(docs.len(), 4);
    let p300 = &docs["unt-metadc29274_p300"];
    assert_eq!(p300["place_id"], congress::PLACE_ID);
    assert_eq!(p300["series"], "globe");
    assert_eq!(p300["date"], "1838-03-29");
    assert_eq!(p300["chamber"], "house");
    assert_eq!(p300["states"], serde_json::json!(["AL"]));
    assert_eq!(
        e.reference_json(&format!("{v1}/{}", congress::release::BASELINES_FILE))
            .await,
        serde_json::json!({"globe": [[day(1837, 12, 4), 3], [day(1838, 3, 29), 2]]})
    );
    let record = e
        .reference_json(&format!("{v1}/{}", congress::release::FILE))
        .await;
    assert_eq!(record["index"], id1);
    assert_eq!(record["indexed"], 4);
    assert_eq!(record["units"][0]["unit"], "metadc29274");
    let baselines = e.reference_json(&format!("{v1}/baselines.json")).await;
    assert!(baselines.get(congress::PLACE_ID).is_none(), "{baselines}");
    let manifest = e.reference_json(&format!("{v1}/manifest.json")).await;
    assert_eq!(manifest["built_from"]["congress"]["built"], true);
    assert_eq!(manifest["build"]["features"]["congress"], congress::VERSION);
    assert_eq!(
        manifest["build"]["templates"]["congress"]["yaml"],
        congress::TEMPLATE
    );
    let files: Vec<&str> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|f| f["path"].as_str())
        .collect();
    assert!(
        files.contains(&congress::release::BASELINES_FILE),
        "{files:?}"
    );
    // The API loads it and searches it with `source=congress`.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert!(refdata.congress_index_set().is_some());
    assert_eq!(
        refdata.congress_bounds(),
        Some((
            NaiveDate::from_ymd_opt(1837, 12, 4).unwrap(),
            NaiveDate::from_ymd_opt(1838, 3, 29).unwrap()
        ))
    );

    // Nothing new anywhere: nothing to release.
    assert!(e.release_congress(2, false, true).await.is_none());

    // A delta of newspapers keeps the set as it is, with its files.
    curate(&e, vec![listed("batch_fx_late_ver01", &b, None)]).await;
    let p2 = e.release_congress(8, false, true).await.unwrap();
    assert!(!p2.full);
    let v2 = &p2.index_version;
    let current = e.reference_json("current.json").await;
    assert_eq!(current["congress"]["indexes"], serde_json::json!([id1]));
    let manifest = e.reference_json(&format!("{v2}/manifest.json")).await;
    assert_eq!(manifest["built_from"]["congress"]["built"], false);
    assert!(manifest["build"]["templates"].get("congress").is_none());
    assert_eq!(manifest["build"]["features"]["congress"], congress::VERSION);
    for f in [congress::release::FILE, congress::release::BASELINES_FILE] {
        assert_eq!(
            e.reference.get(&format!("{v2}/{f}")).await.unwrap(),
            e.reference.get(&format!("{v1}/{f}")).await.unwrap(),
            "{f}"
        );
    }

    // New Congress pages without new batches: the set alone is rebuilt.
    let second = put_globe(&e, &[1, 2, 3, 300, 301, 541, 800]).await;
    assert_eq!(second.attempt, 2);
    let p3 = e.release_congress(15, false, true).await.unwrap();
    assert_eq!(p3.indexes, p2.indexes, "no new main index");
    assert_eq!(p3.docs, 0);
    let current = e.reference_json("current.json").await;
    let id3 = "congress-20261015-1";
    assert_eq!(current["congress"]["indexes"], serde_json::json!([id3]));
    assert_eq!(
        current["congress"]["series"][0],
        serde_json::json!({"series": "globe", "pages": 7, "from": "1837-12-04", "to": "1838-07-18"})
    );
    assert_eq!(e.index(id3).len(), 6);
    let run = raw_run(&e, &p3.index_version).await;
    assert_eq!(run["new_index"], id3);

    // Without the setting the version has no Congress set.
    let p4 = e.release_congress(22, true, false).await.unwrap();
    let current = e.reference_json("current.json").await;
    assert!(current.get("congress").is_none(), "{current}");
    let v4 = &p4.index_version;
    assert!(e
        .reference
        .get(&format!("{v4}/{}", congress::release::FILE))
        .await
        .unwrap()
        .is_none());
}

/// The Serial Set (#229 phase 2) joins the Globe in the one Congress set:
/// its own series in the documents, the baselines and `current.json`, so
/// `series=serialset` finds its pages alone; a new Serial Set attempt
/// rebuilds the set with the Globe's pages kept.
#[tokio::test]
async fn serial_set_pages_join_the_globe_in_the_congress_set() {
    use usnm_ingest::congress;
    let e = env().await;
    let pages = fixture_pages();
    let early: Vec<&Page> = pages
        .iter()
        .filter(|p| p.date < NaiveDate::from_ymd_opt(1897, 7, 1).unwrap())
        .collect();
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    curate(&e, vec![listed("batch_fx_early_ver01", &a, None)]).await;
    let globe = put_globe(&e, &[1, 2, 3, 300, 301]).await;
    let serial = put_serialset(&e).await;
    assert_eq!((serial.pages, serial.indexable), (6, 4));
    assert_eq!(
        serial.source_lastmod.as_deref(),
        Some("2024-11-06T03:15:00.159Z")
    );
    let day = |y, m, d| usnm_core::time::day_number(NaiveDate::from_ymd_opt(y, m, d).unwrap());

    let p1 = e.release_congress(1, false, true).await.unwrap();
    let v1 = &p1.index_version;
    let id1 = "congress-20261001-1";
    let current = e.reference_json("current.json").await;
    assert_eq!(current["congress"]["indexes"], serde_json::json!([id1]));
    assert_eq!(
        current["congress"]["series"],
        serde_json::json!([
            {"series": "globe", "pages": 5, "from": "1837-12-04", "to": "1838-03-29"},
            {"series": "serialset", "pages": 6, "from": "1820-01-13", "to": "1820-01-20"},
        ])
    );
    let docs = e.index(id1);
    assert_eq!(docs.len() as u64, globe.indexable + serial.indexable);
    let p3 = &docs["SERIALSET-00033_00_00-009-0042-0000_p3"];
    assert_eq!(p3["series"], "serialset");
    assert_eq!(p3["place_id"], congress::PLACE_ID);
    assert_eq!(p3["date"], "1820-01-13");
    assert_eq!(p3["chamber"], "house");
    assert_eq!(p3["citation"], "H. Doc. 16-42");
    assert_eq!(p3["package_id"], "SERIALSET-00033_00_00-009-0042-0000");
    assert_eq!(p3["ocr_source"], "govinfo");
    assert!(p3["link"].as_str().unwrap().ends_with(".pdf#page=3"));
    assert!(p3["text"]
        .as_str()
        .unwrap()
        .contains("in relation to the illicit introduction"));
    // Same-day documents keep their order in the volume, under the Globe's.
    let key = |id: &str| docs[id]["sort_key"].as_u64().unwrap();
    assert!(
        key("SERIALSET-00033_00_00-009-0042-0000_p3") > key("unt-metadc29274_p300"),
        "series order"
    );
    let serialset_docs = docs.values().filter(|d| d["series"] == "serialset").count();
    assert_eq!(serialset_docs as u64, serial.indexable);
    let baselines = e
        .reference_json(&format!("{v1}/{}", congress::release::BASELINES_FILE))
        .await;
    assert_eq!(
        baselines["serialset"],
        serde_json::json!([[day(1820, 1, 13), 3], [day(1820, 1, 20), 3]])
    );
    let record = e
        .reference_json(&format!("{v1}/{}", congress::release::FILE))
        .await;
    let units: Vec<(&str, &str)> = record["units"]
        .as_array()
        .unwrap()
        .iter()
        .map(|u| (u["series"].as_str().unwrap(), u["unit"].as_str().unwrap()))
        .collect();
    assert_eq!(
        units,
        [("globe", "metadc29274"), ("serialset", "00033_00_00")]
    );
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(
        refdata.congress_bounds(),
        Some((
            NaiveDate::from_ymd_opt(1820, 1, 13).unwrap(),
            NaiveDate::from_ymd_opt(1838, 3, 29).unwrap()
        ))
    );

    // Nothing new: nothing to release.
    assert!(e.release_congress(2, false, true).await.is_none());
    // The fetch writes the volume again: the set is rebuilt, the Globe kept.
    let again = put_serialset(&e).await;
    assert_eq!(again.attempt, 2);
    e.release_congress(8, false, true).await.unwrap();
    let current = e.reference_json("current.json").await;
    let id2 = "congress-20261008-1";
    assert_eq!(current["congress"]["indexes"], serde_json::json!([id2]));
    assert_eq!(e.index(id2).len(), docs.len());
}

/// Leaving out a published Congress set (#229) is a change to publish,
/// even with no new batches: with `--congress` off, and with no curated
/// Congress page with text left. The version keeps the main indexes, has
/// no `congress` block or files, and its run names no index it didn't
/// write. Prune keeps the old Congress index while its version is the
/// rollback, and not after.
#[tokio::test]
async fn a_published_congress_set_is_removed_without_new_batches() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    curate(&e, vec![listed("batch_fx_early_ver01", &a, None)]).await;
    put_globe(&e, &[1, 2, 3, 300, 301]).await;

    let p1 = e.release_congress(1, false, true).await.unwrap();
    let id1 = "congress-20261001-1";
    let current = e.reference_json("current.json").await;
    assert_eq!(current["congress"]["indexes"], serde_json::json!([id1]));

    // --congress off, no new batch: a version on the same indexes without it.
    let p2 = e.release_congress(8, false, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(p2.indexes, p1.indexes, "no new main index");
    assert_eq!(p2.docs, 0);
    assert_no_congress(&e, &p2.index_version).await;
    assert!(kept_indexes(&e).await.contains(id1), "v1 is the rollback");
    // Once removed, nothing more to release.
    assert!(e.release_congress(9, false, false).await.is_none());
    // A newer version with v2 as its rollback: prune drops the old index.
    curate(&e, vec![listed("batch_fx_late_ver01", &b, None)]).await;
    let p3 = e.release_congress(10, false, false).await.unwrap();
    assert_eq!(p3.indexes.len(), 2);
    assert!(!kept_indexes(&e).await.contains(id1));

    // Back on, the set is built again; then the curated listing has no
    // page with text (an attempt of the item's textless page only).
    let p4 = e.release_congress(15, false, true).await.unwrap();
    assert_eq!(p4.indexes, p3.indexes);
    let id4 = "congress-20261015-1";
    let current = e.reference_json("current.json").await;
    assert_eq!(current["congress"]["indexes"], serde_json::json!([id4]));
    let empty = put_globe(&e, &[2]).await;
    assert_eq!((empty.pages, empty.indexable), (1, 0));
    let p5 = e.release_congress(22, false, true).await.unwrap();
    assert_eq!(p5.indexes, p4.indexes, "no new main index");
    assert_no_congress(&e, &p5.index_version).await;
    assert!(kept_indexes(&e).await.contains(id4), "v4 is the rollback");
    assert!(e.release_congress(23, false, true).await.is_none());
}

/// What prune keeps now.
async fn kept_indexes(e: &Env) -> std::collections::BTreeSet<String> {
    usnm_ingest::prune::kept(e.reference.as_ref(), &e.state)
        .await
        .unwrap()
        .indexes
}

/// `v` is the serving version and has no Congress set, and its run names
/// no index it didn't write (there are no Japanese pages).
async fn assert_no_congress(e: &Env, v: &str) {
    use usnm_ingest::congress;
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v);
    assert!(current.get("congress").is_none(), "{current}");
    for f in [congress::release::FILE, congress::release::BASELINES_FILE] {
        assert!(
            e.reference
                .get(&format!("{v}/{f}"))
                .await
                .unwrap()
                .is_none(),
            "{f}"
        );
    }
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert!(
        manifest["built_from"].get("congress").is_none(),
        "{manifest}"
    );
    assert!(manifest["build"]["features"].get("congress").is_none());
    // A JSONL sink lays out nothing; a writer node's layout has no Congress index.
    let layout: Vec<&str> = manifest["indexes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|l| l["index_id"].as_str())
        .collect();
    assert!(
        !layout.iter().any(|i| i.starts_with("congress-")),
        "{layout:?}"
    );
    let run = raw_run(e, v).await;
    assert_eq!(run["new_index"], "", "{run}");
    assert_eq!(run["status"], "published");
}

/// The Congress index in a Quickwit writer node (#229): built with its own
/// template, merged and sealed like the others, laid out in the manifest,
/// and searchable by series, chamber and phrase. Runs when `QUICKWIT_BIN`
/// is set.
#[tokio::test]
async fn congress_pages_are_indexed_in_a_quickwit_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    let e = env().await;
    let pages = fixture_pages();
    let all: Vec<&Page> = pages.iter().collect();
    let a = e.root.join("batch_fx_all_ver01.tar.gz");
    write_archive(&a, &all, true, true);
    curate(&e, vec![listed("batch_fx_all_ver01", &a, None)]).await;
    let counts = put_globe(&e, &[1, 2, 3, 300, 301, 541, 800]).await;
    let serial = put_serialset(&e).await;

    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7421, &meta, &root)
        .await
        .unwrap();
    let r = Release {
        congress: true,
        ..e.releaser(1, false)
    };
    let mut sink = QuickwitSink::new(&node.url, &root)
        .unwrap()
        .watching(&node)
        .merges(quick_merges());
    let p = r.run(&mut sink).await.unwrap().unwrap();
    let id = "congress-20261001-1";
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p.index_version))
        .await;
    let layout = manifest["indexes"].as_array().unwrap();
    let c = layout
        .iter()
        .find(|l| l["index_id"] == id)
        .expect("laid out");
    assert_eq!(
        c["docs"].as_u64(),
        Some(counts.indexable + serial.indexable)
    );

    let http = reqwest::Client::new();
    let count = |q: &'static str| {
        let url = format!("{}/api/v1/{id}/search", node.url);
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
            v["num_hits"].as_u64().unwrap()
        }
    };
    assert_eq!(count("*").await, counts.indexable + serial.indexable);
    assert_eq!(count("series:globe").await, counts.indexable);
    assert_eq!(count("series:serialset").await, serial.indexable);
    assert_eq!(
        count("series:IN [globe serialset]").await,
        counts.indexable + serial.indexable
    );
    assert_eq!(count("series:annals").await, 0);
    // The Serial Set's text, `¬` line-end hyphens joined (`compa¬` / `rative`,
    // `APPROPRIA¬` / `TIONS`), and its chamber. The title page names the
    // subject too.
    assert_eq!(
        count("series:serialset AND text:\"illicit introduction of slaves\"").await,
        2
    );
    assert_eq!(count("series:serialset AND text:comparative").await, 1);
    assert_eq!(count("series:serialset AND text:appropriations").await, 1);
    assert_eq!(count("text:appropria").await, 0);
    assert_eq!(
        count("series:serialset AND chamber:house").await,
        serial.indexable
    );
    assert_eq!(count("text:\"price of public lands\"").await, 1);
    assert_eq!(count("chamber:house AND text:duel").await, 1);
    node.stop().await.unwrap();
}
