//! Our OCR's Latin text on the pages LoC ships without text (`--ja-latin`, #203).

use chrono::NaiveDate;
use serde_json::Value;
use usnm_core::text::Analyzer;
use usnm_ingest::release::{Published, Release};
use usnm_ingest::sink::JsonlSink;
use usnm_store::ObjectStore;

use crate::common::{
    curate, curate_early_and_late, env, fixture_pages, listed, put_overlay_part, read_jsonl,
    write_archive, Env, JaRow, Page,
};

impl Env {
    /// A release with or without our OCR's Latin text (`--ja-latin`, #203).
    async fn release_latin(&self, day: u32, full: bool, ja_latin: bool) -> Option<Published> {
        self.release_opts(day, full, false, ja_latin).await.unwrap()
    }
}

/// Our OCR's Latin text on the pages LoC ships without text (#203,
/// `--ja-latin`): the release's new main index takes the pages no index of
/// the version holds yet, each counted once (the baselines had them since
/// #139). A delta keeps the earlier ones, and hides one a new batch now has
/// from LoC; with no new batch, new Latin text of ours gets a delta of its
/// own, and OCR with no Latin text gets an overlay-only release; a full
/// release without the setting leaves them out. A page curation had takes our text in place of
/// LoC's where ours reads more words (here, an empty one).
#[tokio::test]
async fn our_ocr_latin_text_reaches_the_main_index_once() {
    let e = env().await;
    let (early, late, early_pages, late_pages) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let p0 = pages
        .iter()
        .find(|p| p.date < split && !p.text.is_empty())
        .unwrap();
    let p1 = pages
        .iter()
        .find(|p| p.lccn == p0.lccn && p.date == p0.date && p.seq != p0.seq && !p.text.is_empty())
        .expect("two pages with text on one day");
    // An early page LoC has no text for, which curation has (an `empty` page).
    let blank = pages
        .iter()
        .find(|p| p.date < split && p.text.is_empty())
        .expect("an empty early page");
    // A late page with LoC's text, which our OCR also read as a page missing from the early batch.
    let late_page = pages
        .iter()
        .find(|p| p.date >= split && !p.text.is_empty())
        .unwrap();
    let id = |p: &Page| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq);
    let at = |seq: u16| format!("{}_{}_ed-1_seq-{seq}", p0.lccn, p0.date);
    let row = |lccn, date, seq, loc_text, text| JaRow {
        lccn,
        date,
        seq,
        batch: "batch_fx_early",
        loc_text,
        text,
        ocred_at: 1,
    };
    let ads = "Moritz Drug Co\n2001 Larimer St., Denver\n米國と日本の戰爭";
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(&p0.lccn, p0.date, 90, "missing", ads),
            // Japanese only: no main-index document.
            row(&p0.lccn, p0.date, 91, "missing", "東京の新聞"),
            // Curation has these from LoC: no document of ours.
            row(
                &p0.lccn,
                p0.date,
                p1.seq,
                "missing",
                "Larimer Street, Denver, Colorado",
            ),
            row(
                &p0.lccn,
                p0.date,
                p0.seq,
                "garbled",
                "Larimer Street, Denver, Colorado",
            ),
            row(
                &late_page.lccn,
                late_page.date,
                late_page.seq,
                "missing",
                "Sunshine Grocery, Denver",
            ),
            // Ours reads words where LoC has none: its document takes our text.
            row(
                &blank.lccn,
                blank.date,
                blank.seq,
                "empty",
                "The relocation center held a meeting",
            ),
        ],
    )
    .await;

    // A base with the setting: the two pages curation doesn't have get our
    // Latin text, and so does the empty one, in its own document.
    let v1 = e.release_latin(5, true, true).await.expect("base");
    let base = e.index(&v1.indexes[0]);
    let early_docs = early_pages
        - pages
            .iter()
            .filter(|p| p.date < split && p.text.is_empty())
            .count();
    assert_eq!(base.len(), early_docs + 3, "{:?}", v1);
    assert_eq!(
        base[&id(blank)]["text"],
        "The relocation center held a meeting"
    );
    let doc = &base[&at(90)];
    assert_eq!(doc["text"], "Moritz Drug Co\n2001 Larimer St., Denver");
    // Its batch is the one whose archive had no text for it (hidden copies name it).
    let raw = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", v1.indexes[0])),
    );
    let raw = raw.iter().find(|d| d["doc_id"] == at(90).as_str()).unwrap();
    assert_eq!(raw["batch"], "batch_fx_early");
    assert_eq!(base[&id(late_page)]["text"], "Sunshine Grocery, Denver");
    assert!(!base.contains_key(&at(91)));
    // LoC's copies, untouched.
    assert_eq!(base[&id(p1)]["text"], p1.text.as_str());
    assert_eq!(base[&id(p0)]["text"], p0.text.as_str());
    let v = &v1.index_version;
    let latin = e.reference_json(&format!("{v}/ja_latin.json")).await;
    assert_eq!(
        latin["pages"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        {
            let mut want = vec![at(90), id(late_page), id(blank)];
            want.sort();
            want
        }
        .iter()
        .collect::<Vec<_>>()
    );
    let frozen = e.reference_json(&format!("{v}/credits.json")).await;
    let current = e.reference_json("current.json").await;
    let ja_index = e.index(current["ja"]["indexes"][0].as_str().unwrap());
    let unique: std::collections::BTreeSet<_> = ja_index
        .keys()
        .chain(latin["pages"].as_object().unwrap().keys())
        .collect();
    assert_eq!(frozen["japanese"]["pages"], unique.len() as u64);
    assert_eq!(
        frozen["japanese"]["by_engine"][0]["pages"],
        unique.len() as u64
    );
    assert_eq!(latin["pages"][at(90)]["ocr_engine"], "ndlocr-lite 636d1cf");
    assert_eq!(latin["pages"][at(90)]["loc_text"], "missing");
    assert_eq!(latin["pages"][id(blank)]["loc_text"], "empty");
    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(
        record["latin"],
        serde_json::json!({"added": 3, "pages": 3, "hidden": 0, "rule": usnm_ingest::ocr_ja::LATIN_VERSION})
    );
    // Counted once each: the three pages curation never had (90, 91 and the
    // late one, in no counts.json of this version).
    assert_eq!(record["added_to_baselines"], 3);
    assert_eq!(v1.pages, early_pages as u64 + 3);
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert_eq!(
        manifest["build"]["features"]["ja_latin"],
        usnm_ingest::ocr_ja::LATIN_VERSION
    );

    // New OCR alone, with Latin text on a page no index holds: a delta of
    // just that page, though no batch is new, and the base kept.
    put_overlay_part(
        &e,
        "ocr-ja/pages/b.parquet",
        &[row(
            &p0.lccn,
            p0.date,
            92,
            "missing",
            "SUNSHINE GROCERY 1851 Larimer Street",
        )],
    )
    .await;
    let v2 = e.release_latin(6, false, true).await.expect("Latin delta");
    assert!(!v2.full);
    assert_eq!(v2.indexes.len(), 2);
    assert_eq!(v2.indexes[0], v1.indexes[0]);
    let delta = e.index(&v2.indexes[1]);
    assert_eq!(delta.keys().collect::<Vec<_>>(), vec![&at(92)]);
    assert_eq!(
        delta[&at(92)]["text"],
        "SUNSHINE GROCERY 1851 Larimer Street"
    );
    assert_eq!(v2.docs, 1);
    assert_eq!(v2.pages, v1.pages + 1);
    let v = &v2.index_version;
    let latin2 = e.reference_json(&format!("{v}/ja_latin.json")).await;
    assert_eq!(latin2["pages"].as_object().unwrap().len(), 4);
    assert_eq!(latin2["pages"][at(92)]["loc_text"], "missing");
    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(
        record["latin"],
        serde_json::json!({"added": 1, "pages": 4, "hidden": 0, "rule": usnm_ingest::ocr_ja::LATIN_VERSION})
    );
    let hidden = e.reference_json(&format!("{v}/duplicates.json")).await;
    assert_eq!(hidden, serde_json::json!([]));
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v.as_str());
    assert_eq!(
        current["indexes"],
        serde_json::to_value(&v2.indexes).unwrap()
    );
    assert!(current["ja"]["indexes"][0].is_string());
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert!(manifest["build"]["templates"]["pages"].is_object());
    assert!(manifest["build"]["templates"]["pages-ja"].is_object());
    // Nothing new since: nothing to release.
    assert!(e.release_latin(7, false, true).await.is_none());

    // OCR with no Latin text: a new Japanese index, no main index, the same Latin pages.
    put_overlay_part(
        &e,
        "ocr-ja/pages/c.parquet",
        &[row(&p0.lccn, p0.date, 93, "missing", "米國の新聞")],
    )
    .await;
    let v3 = e.release_latin(8, false, true).await.expect("overlay-only");
    assert_eq!(v3.indexes, v2.indexes);
    assert_eq!(v3.pages, v2.pages + 1);
    let latin3 = e
        .reference_json(&format!("{}/ja_latin.json", v3.index_version))
        .await;
    assert_eq!(latin3, latin2);

    // A delta with the late batch: the late page now has LoC's text, so our
    // copy in the base is hidden and it counts once. Pages 90 and 92 are in
    // the indexes the version keeps.
    curate(&e, late).await;
    let v4 = e.release_latin(9, false, true).await.expect("delta");
    assert!(!v4.full);
    assert_eq!(v4.indexes.len(), 3);
    let delta = e.index(&v4.indexes[2]);
    assert_eq!(delta[&id(late_page)]["text"], late_page.text.as_str());
    assert!(!delta.contains_key(&at(90)), "already in the base");
    assert!(!delta.contains_key(&at(92)), "already in the first delta");
    let v = &v4.index_version;
    let latin4 = e.reference_json(&format!("{v}/ja_latin.json")).await;
    assert_eq!(latin4["pages"].as_object().unwrap().len(), 4);
    let hidden = e.reference_json(&format!("{v}/duplicates.json")).await;
    assert_eq!(
        hidden,
        serde_json::json!([{"doc_id": id(late_page), "batch": "batch_fx_early"}])
    );
    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(
        record["latin"],
        serde_json::json!({"added": 0, "pages": 4, "hidden": 1, "rule": usnm_ingest::ocr_ja::LATIN_VERSION})
    );
    // Every fixture page, and 90 to 93 once each.
    assert_eq!(v4.pages, (early_pages + late_pages) as u64 + 4);
    let title_pages = e.reference_json(&format!("{v}/title_pages.json")).await;
    let total: u64 = title_pages
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(total, v4.pages);

    // A full release without the setting: LoC's text only, and no record.
    let v5 = e.release_latin(10, true, false).await.expect("full");
    let docs: usize = v5.indexes.iter().map(|i| e.index(i).len()).sum();
    let fixture_docs = pages.iter().filter(|p| !p.text.is_empty()).count();
    assert_eq!(docs, fixture_docs);
    assert_eq!(v5.pages, v4.pages);
    assert!(e
        .reference
        .get(&format!("{}/ja_latin.json", v5.index_version))
        .await
        .unwrap()
        .is_none());
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", v5.index_version))
        .await;
    assert_eq!(hidden, serde_json::json!([]));
}

/// The setting turned on with no new batch and no new OCR (#203): the
/// pages LoC ships without text that our OCR read Latin text on get a
/// delta of just those pages, rather than waiting for LoC's next batch.
/// Our text in place of LoC's (an empty page here) waits for a release
/// that indexes the page's batch. Once published, nothing is left to
/// release, and a run finds that out without reading the curated parts,
/// though a page our OCR read is left out for a copy of LoC's.
#[tokio::test]
async fn our_latin_text_gets_a_delta_without_a_new_batch() {
    let e = env().await;
    let (early, _, early_pages, _) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let blank = pages
        .iter()
        .find(|p| p.date < split && p.text.is_empty())
        .expect("an empty early page");
    let row = |seq, loc_text, text| JaRow {
        lccn: &blank.lccn,
        date: blank.date,
        seq,
        batch: "batch_fx_early",
        loc_text,
        text,
        ocred_at: 1,
    };
    // A page curation has with LoC's text, that our OCR read as missing
    // from the batch's archive: no document of ours, ever.
    let with_text = pages
        .iter()
        .find(|p| p.date < split && !p.text.is_empty())
        .unwrap();
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(
                90,
                "missing",
                "Moritz Drug Co, 2001 Larimer Street, eggs ½ dozen",
            ),
            row(91, "missing", "東京の新聞"),
            row(blank.seq, "empty", "The relocation center held a meeting"),
            JaRow {
                lccn: &with_text.lccn,
                date: with_text.date,
                seq: with_text.seq,
                batch: "batch_fx_early",
                loc_text: "missing",
                text: "Larimer Street, Denver, Colorado",
                ocred_at: 1,
            },
        ],
    )
    .await;
    let id = |seq: u16| format!("{}_{}_ed-1_seq-{seq}", blank.lccn, blank.date);
    let v1 = e.release_latin(5, true, false).await.expect("base");
    assert!(!e.index(&v1.indexes[0]).contains_key(&id(90)));
    assert!(e.release_latin(6, false, false).await.is_none());
    // As if the base had been built before #168, with analyzer version 1.
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

    let v2 = e.release_latin(7, false, true).await.expect("Latin delta");
    assert!(!v2.full);
    assert_eq!(v2.indexes.len(), 2);
    assert_eq!(v2.indexes[0], v1.indexes[0]);
    // Its pairs are version 1's, like the base's (`Env::index_as` checks).
    let delta = e.index_as(&v2.indexes[1], Analyzer::V1);
    assert_eq!(delta.keys().collect::<Vec<_>>(), vec![&id(90)]);
    let text = "Moritz Drug Co, 2001 Larimer Street, eggs ½ dozen";
    assert_eq!(delta[&id(90)]["text"], text);
    assert_ne!(
        usnm_core::common_grams::index_text(text, Analyzer::V1),
        usnm_core::common_grams::index_text(text, Analyzer::LATEST),
        "a text the two analyzers fold apart"
    );
    assert_eq!(e.reference_json("current.json").await["common_grams"], 1);
    assert_eq!(v2.docs, 1);
    // Pages 90 and 91 were counted since the base.
    assert_eq!(v2.pages, v1.pages);
    assert_eq!(v1.pages, early_pages as u64 + 2);
    let v = &v2.index_version;
    let latin = e.reference_json(&format!("{v}/ja_latin.json")).await;
    assert_eq!(
        latin["pages"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        vec![&id(90)]
    );
    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(
        record["latin"],
        serde_json::json!({"added": 1, "pages": 1, "hidden": 0, "rule": usnm_ingest::ocr_ja::LATIN_VERSION})
    );
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert_eq!(
        manifest["build"]["features"]["ja_latin"],
        usnm_ingest::ocr_ja::LATIN_VERSION
    );
    assert_eq!(manifest["build"]["features"]["common_grams"], 1);
    assert_eq!(manifest["build"]["features"]["ja_fold"], 1);
    // With the curated parts gone, a run that read them would fail.
    let parts = e.root.join("curated/pages");
    let moved = e.root.join("parts-away");
    std::fs::rename(&parts, &moved).unwrap();
    assert!(e.release_latin(8, false, true).await.is_none());
    std::fs::rename(&moved, &parts).unwrap();
}

/// The setting turned on where every page our OCR read Latin text on is
/// left out for a copy of LoC's (#203): one overlay-only release records
/// that the overlay was weighed, and later runs release nothing without
/// reading the curated parts.
#[tokio::test]
async fn latin_text_with_nothing_to_add_is_weighed_once() {
    let e = env().await;
    let (early, _, _, _) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let with_text = pages
        .iter()
        .find(|p| p.date < split && !p.text.is_empty())
        .unwrap();
    let row = |seq, text| JaRow {
        lccn: &with_text.lccn,
        date: with_text.date,
        seq,
        batch: "batch_fx_early",
        loc_text: "missing",
        text,
        ocred_at: 1,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(with_text.seq, "Larimer Street, Denver, Colorado"),
            row(91, "東京の新聞"),
        ],
    )
    .await;
    let v1 = e.release_latin(5, true, false).await.expect("base");
    let v2 = e.release_latin(6, false, true).await.expect("overlay-only");
    assert_eq!(v2.indexes, v1.indexes);
    assert_eq!(v2.pages, v1.pages);
    let v = &v2.index_version;
    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(
        record["latin"],
        serde_json::json!({"added": 0, "pages": 0, "hidden": 0, "rule": usnm_ingest::ocr_ja::LATIN_VERSION})
    );
    assert!(e
        .reference
        .get(&format!("{v}/ja_latin.json"))
        .await
        .unwrap()
        .is_none());
    // With the curated parts gone, a run that read them would fail.
    let parts = e.root.join("curated/pages");
    let moved = e.root.join("parts-away");
    std::fs::rename(&parts, &moved).unwrap();
    assert!(e.release_latin(7, false, true).await.is_none());
    std::fs::rename(&moved, &parts).unwrap();
}

/// A page LoC had no text for, indexed with ours in its own document
/// (#203), that a later batch ships with LoC's text: the later copy wins,
/// and the snapshot hides ours, so the page counts once.
#[tokio::test]
async fn a_copy_with_our_text_that_loses_to_locs_is_hidden() {
    superseded_ocr_copy("empty").await;
}

#[tokio::test]
async fn a_superseded_latin_only_copy_loses_its_credits_but_japanese_text_does_not() {
    superseded_ocr_copy("mixed").await;
}

async fn superseded_ocr_copy(reason: &str) {
    let e = env().await;
    let (early, _, early_pages, _) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let blank = pages
        .iter()
        .find(|p| p.date < split && p.text.is_empty())
        .expect("an empty early page");
    let id = format!("{}_{}_ed-1_seq-{}", blank.lccn, blank.date, blank.seq);
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[JaRow {
            lccn: &blank.lccn,
            date: blank.date,
            seq: blank.seq,
            batch: "batch_fx_early",
            loc_text: reason,
            text: "The relocation center held a meeting",
            ocred_at: 1,
        }],
    )
    .await;
    let v1 = e.release_latin(5, true, true).await.expect("base");
    assert_eq!(
        e.index(&v1.indexes[0])[&id]["text"],
        "The relocation center held a meeting"
    );
    assert_eq!(v1.pages, early_pages as u64);

    // A later batch has the page with LoC's text.
    let again = Page {
        lccn: blank.lccn.clone(),
        date: blank.date,
        seq: blank.seq,
        text: "The council met on Tuesday and voted for the new school.".into(),
    };
    let path = e.root.join("batch_fx_again_ver01.tar.gz");
    write_archive(&path, &[&again], false, true);
    curate(&e, vec![listed("batch_fx_again_ver01", &path, None)]).await;
    let v2 = e.release_latin(6, false, true).await.expect("delta");
    assert_eq!(e.index(&v2.indexes[1])[&id]["text"], again.text.as_str());
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", v2.index_version))
        .await;
    assert_eq!(
        hidden,
        serde_json::json!([{"doc_id": id, "batch": "batch_fx_early"}])
    );
    assert_eq!(v2.pages, early_pages as u64, "the page counts once");
    let frozen = e
        .reference
        .get(&format!("{}/credits.json", v2.index_version))
        .await
        .unwrap();
    let rd = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert!(!rd.ja_latin.contains_key(&id));
    if reason == "mixed" {
        // English-only mixed text never entered the Japanese index, and
        // its main-index copy is hidden: no serving OCR credit remains.
        assert!(frozen.is_none());
        assert!(usnm_api::status::assemble::sources(&rd).ocr_ja.is_none());
    } else {
        // The Japanese index still serves this reading independently.
        let frozen: Value = serde_json::from_slice(&frozen.unwrap()).unwrap();
        assert_eq!(frozen["japanese"]["pages"], 1);
        assert_eq!(frozen["engines"][0]["key"], "ndlocr-lite 636d1cf");
        assert_eq!(
            usnm_api::status::assemble::sources(&rd)
                .ocr_ja
                .unwrap()
                .pages,
            1
        );
    }
}

/// A later batch that ships a page with no text of LoC's doesn't take over
/// from our copy (#203): neither a page our OCR read because LoC's archive
/// had no text for it, nor an empty page given our text, is hidden, and the
/// later copy gets no document, so each page is still searchable and counts
/// once.
#[tokio::test]
async fn a_copy_without_locs_text_does_not_hide_ours() {
    let e = env().await;
    let (early, _, early_pages, _) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let blank = pages
        .iter()
        .find(|p| p.date < split && p.text.is_empty())
        .expect("an empty early page");
    let row = |seq, loc_text, text| JaRow {
        lccn: &blank.lccn,
        date: blank.date,
        seq,
        batch: "batch_fx_early",
        loc_text,
        text,
        ocred_at: 1,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(90, "missing", "Moritz Drug Co, 2001 Larimer Street"),
            row(blank.seq, "empty", "The relocation center held a meeting"),
        ],
    )
    .await;
    let v1 = e.release_latin(5, true, true).await.expect("base");
    let id = |seq: u16| format!("{}_{}_ed-1_seq-{seq}", blank.lccn, blank.date);
    let base = e.index(&v1.indexes[0]);
    assert!(base.contains_key(&id(90)) && base.contains_key(&id(blank.seq)));
    assert_eq!(v1.pages, early_pages as u64 + 1);

    // A batch whose name sorts first ships both pages, empty: its copy of
    // the empty page wins the plan's tie, but has nothing to search.
    let empty = |seq| Page {
        lccn: blank.lccn.clone(),
        date: blank.date,
        seq,
        text: String::new(),
    };
    let (p90, pb) = (empty(90), empty(blank.seq));
    let path = e.root.join("batch_fx_again_ver01.tar.gz");
    write_archive(&path, &[&p90, &pb], false, true);
    curate(&e, vec![listed("batch_fx_again_ver01", &path, None)]).await;
    let v2 = e.release_latin(6, false, true).await.expect("delta");
    let delta = e.index(&v2.indexes[1]);
    assert!(!delta.contains_key(&id(90)) && !delta.contains_key(&id(blank.seq)));
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", v2.index_version))
        .await;
    assert_eq!(hidden, serde_json::json!([]));
    let record = e
        .reference_json(&format!("{}/ocr_ja.json", v2.index_version))
        .await;
    assert_eq!(record["latin"]["hidden"], 0);
    assert_eq!(record["latin"]["pages"], 2);
    // Page 90 is counted now from the new batch's counts, not ours; the
    // empty page twice over is one page.
    assert_eq!(v2.pages, early_pages as u64 + 1, "each page counts once");
}

/// Our OCR's Latin text in a version laid out by decade (#203 with #123):
/// its documents carry the `decade` field like LoC's, in the partitioned
/// base and in a tagged delta, so a date-limited search that skips the
/// other decades' splits still finds them.
#[tokio::test]
async fn our_latin_text_follows_the_decade_layout() {
    let e = env().await;
    let (early, late, _, _) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let blank = pages
        .iter()
        .find(|p| p.date < split && p.text.is_empty())
        .expect("an empty early page");
    let row = |seq, loc_text, text| JaRow {
        lccn: &blank.lccn,
        date: blank.date,
        seq,
        batch: "batch_fx_early",
        loc_text,
        text,
        ocred_at: 1,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(90, "missing", "Moritz Drug Co, 2001 Larimer Street"),
            row(blank.seq, "empty", "The relocation center held a meeting"),
        ],
    )
    .await;
    let release = |day, full| Release {
        partition_decade: true,
        single_text_field: false,
        page_language: false,
        split_docs: None,
        ja_latin: true,
        ocr_local: false,
        congress: false,
        congress_manual: false,
        ..e.releaser(day, full)
    };
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    let v1 = release(5, true).run(&mut sink).await.unwrap().unwrap();
    assert_eq!(
        e.reference_json("current.json").await["decades"],
        usnm_core::decade::VERSION
    );
    let decade = usnm_core::decade::of_date(blank.date);
    let id = |seq: u16| format!("{}_{}_ed-1_seq-{seq}", blank.lccn, blank.date);
    let base = e.index(&v1.indexes[0]);
    assert!(base.values().all(|d| d.get("decade").is_some()));
    assert_eq!(base[&id(90)]["decade"], decade);
    assert_eq!(base[&id(90)]["text"], "Moritz Drug Co, 2001 Larimer Street");
    assert_eq!(base[&id(blank.seq)]["decade"], decade);

    // A delta on it is tagged: its new page of ours has the field too.
    put_overlay_part(
        &e,
        "ocr-ja/pages/b.parquet",
        &[row(92, "missing", "SUNSHINE GROCERY 1851 Larimer Street")],
    )
    .await;
    curate(&e, late).await;
    let v2 = release(6, false).run(&mut sink).await.unwrap().unwrap();
    assert!(!v2.full);
    let delta = e.index(&v2.indexes[1]);
    assert!(delta.values().all(|d| d.get("decade").is_some()));
    assert_eq!(delta[&id(92)]["decade"], decade);

    // So is a delta of only our Latin text, with no new batch.
    put_overlay_part(
        &e,
        "ocr-ja/pages/c.parquet",
        &[row(93, "missing", "Pacific Citizen, 1945 Larimer Street")],
    )
    .await;
    let v3 = release(7, false).run(&mut sink).await.unwrap().unwrap();
    assert_eq!(v3.indexes.len(), 3);
    let delta = e.index(&v3.indexes[2]);
    assert_eq!(delta.keys().collect::<Vec<_>>(), vec![&id(93)]);
    assert_eq!(delta[&id(93)]["decade"], decade);
    assert_eq!(
        e.reference_json("current.json").await["decades"],
        usnm_core::decade::VERSION
    );
}

/// Copies without LoC's text don't cost a page our text (#203, review of
/// #250): a page LoC's archive had no text for in one batch and an empty
/// copy in another keeps our document (and the empty copy gets none), and
/// an empty page with copies in two batches gets our text on the copy the
/// version keeps, whichever batch's copy we read. One document each.
#[tokio::test]
async fn copies_without_locs_text_keep_our_text_once() {
    let e = env().await;
    let (early, _, early_pages, _) = curate_early_and_late(&e).await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let blank = pages
        .iter()
        .find(|p| p.date < split && p.text.is_empty())
        .expect("an empty early page");
    // Another batch, sorting first, with both pages and no text for either.
    let empty = |seq| Page {
        lccn: blank.lccn.clone(),
        date: blank.date,
        seq,
        text: String::new(),
    };
    let (p90, pb) = (empty(90), empty(blank.seq));
    let path = e.root.join("batch_fx_again_ver01.tar.gz");
    write_archive(&path, &[&p90, &pb], false, true);
    let mut both = early;
    both.push(listed("batch_fx_again_ver01", &path, None));
    curate(&e, both).await;
    let row = |seq, loc_text, text| JaRow {
        lccn: &blank.lccn,
        date: blank.date,
        seq,
        batch: "batch_fx_early",
        loc_text,
        text,
        ocred_at: 1,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(90, "missing", "Moritz Drug Co, 2001 Larimer Street"),
            // We read the early batch's copy; the plan keeps batch_fx_again's.
            row(blank.seq, "empty", "The relocation center held a meeting"),
        ],
    )
    .await;
    let v = e.release_latin(5, true, true).await.expect("base");
    let id = |seq: u16| format!("{}_{}_ed-1_seq-{seq}", blank.lccn, blank.date);
    let raw = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", v.indexes[0])),
    );
    let docs = |seq| {
        raw.iter()
            .filter(|d| d["doc_id"] == id(seq).as_str())
            .collect::<Vec<_>>()
    };
    let d90 = docs(90);
    assert_eq!(d90.len(), 1, "one document for page 90");
    assert_eq!(d90[0]["text"], "Moritz Drug Co, 2001 Larimer Street");
    assert_eq!(d90[0]["batch"], "batch_fx_early");
    let db = docs(blank.seq);
    assert_eq!(db.len(), 1, "one document for the empty page");
    assert_eq!(db[0]["text"], "The relocation center held a meeting");
    assert_eq!(db[0]["batch"], "batch_fx_again");
    let latin = e
        .reference_json(&format!("{}/ja_latin.json", v.index_version))
        .await;
    assert_eq!(latin["pages"][id(blank.seq)]["batch"], "batch_fx_again");
    assert_eq!(latin["pages"][id(90)]["batch"], "batch_fx_early");
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", v.index_version))
        .await;
    assert_eq!(hidden, serde_json::json!([]));
    // The early pages, and page 90 from the other batch's counts; the empty
    // page's two copies are one page.
    assert_eq!(v.pages, early_pages as u64 + 1);
}
