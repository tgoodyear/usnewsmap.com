//! Our OCR beside LoC's: the Japanese overlay and its own index, and the rule
//! that gives a page our text in place of LoC's (`--ocr-local`, 12 §12.5 Step 9).

use chrono::NaiveDate;
use serde_json::Value;
use usnm_core::text::Analyzer;
use usnm_ingest::release::Release;
use usnm_ingest::sink::JsonlSink;
use usnm_store::ObjectStore;

use crate::common::{
    by_id, curate, curate_early_and_late, env, fixture_pages, fixtures, garble, listed, local_row,
    prose, put_overlay_part, put_overlay_part_with_engine, raw_run, read_jsonl, sha256_file,
    write_archive, JaRow, Page, JA_LATIN,
};

/// Our Japanese OCR (#139): a release indexes the overlay's pages in their own
/// index, newest copy of each page, only for batches in the version, and adds
/// the pages curation never had to the baselines (not the ones it had).
#[tokio::test]
async fn a_japanese_only_unknown_engine_blocks_publication() {
    let e = env().await;
    let (early, _, _, _) = curate_early_and_late(&e).await;
    curate(&e, early).await;
    let pages = fixture_pages();
    let p = &pages[0];
    put_overlay_part_with_engine(
        &e,
        "ocr-ja/pages/unknown.parquet",
        &[JaRow {
            lccn: &p.lccn,
            date: p.date,
            seq: 90,
            batch: "batch_fx_early",
            loc_text: "missing",
            text: "米國と日本の戰爭",
            ocred_at: 1,
        }],
        "uncredited/1 japanese",
    )
    .await;
    let error = e.release_with(5, true, false).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("missing OCR credit"),
        "{error:#}"
    );
    assert!(e.reference.get("current.json").await.unwrap().is_none());
}

#[tokio::test]
async fn a_release_indexes_the_japanese_ocr_and_counts_missing_pages() {
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

    let p0 = pages.iter().find(|p| !p.text.is_empty()).unwrap();
    // Another page of the same title and day that LoC gave text, in the same batch.
    let p1 = pages
        .iter()
        .find(|p| p.lccn == p0.lccn && p.date == p0.date && p.seq != p0.seq && !p.text.is_empty())
        .expect("two pages with text on one day");
    let same_day = pages
        .iter()
        .filter(|p| p.lccn == p0.lccn && p.date == p0.date)
        .count();
    let row = |seq, batch, loc_text, text, at| JaRow {
        lccn: &p0.lccn,
        date: p0.date,
        seq,
        batch,
        loc_text,
        text,
        ocred_at: at,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(90, "batch_fx_ja", "missing", "古い読み", 1),
            row(p0.seq, "batch_fx_ja", "garbled", "日本", 1),
            row(91, "batch_elsewhere", "missing", "東京", 1),
            // In the version's batch; a newer copy below is from a batch outside it.
            row(92, "batch_fx_ja", "missing", "大阪", 1),
            // Marked missing (from some archive), but curation has it with text: counted already.
            row(p1.seq, "batch_fx_ja", "missing", "平和", 1),
        ],
    )
    .await;
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.2.parquet",
        &[
            row(90, "batch_fx_ja", "missing", "米國と日本の戰爭", 2),
            row(92, "batch_elsewhere", "missing", "外", 5),
        ],
    )
    .await;

    let published = e.release(5, true).await.expect("published");
    let current = e.reference_json("current.json").await;
    let ja_id = format!(
        "pages-ja-{}",
        published.index_version.trim_start_matches("pages-v")
    );
    assert_eq!(current["ja"]["indexes"], serde_json::json!([ja_id]));
    assert_eq!(current["ja"]["pages"], 4);
    assert_eq!(current["ja"]["fold"], usnm_core::ja::FOLD_VERSION);
    let credits = e
        .reference_json(&format!("{}/credits.json", published.index_version))
        .await;
    assert_eq!(credits["engines"][0]["key"], "ndlocr-lite 636d1cf");
    assert_eq!(credits["japanese"]["pages"], 4);
    assert_eq!(credits["japanese"]["by_engine"][0]["pages"], 4);
    let rd = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(
        usnm_api::status::assemble::sources(&rd)
            .ocr_ja
            .unwrap()
            .pages,
        4
    );

    let ja = e.index(&ja_id);
    assert_eq!(ja.len(), 4, "{:?}", ja.keys());
    // The out-of-version copy didn't hide the in-version one.
    assert_eq!(
        ja[&format!("{}_{}_ed-1_seq-92", p0.lccn, p0.date)]["printed"],
        "大阪"
    );
    let missing = &ja[&format!("{}_{}_ed-1_seq-90", p0.lccn, p0.date)];
    // The newest copy wins, and its text is the folded tokens of the printed text.
    assert_eq!(missing["printed"], "米國と日本の戰爭");
    assert_eq!(
        missing["text"],
        usnm_core::ja::index_text("米國と日本の戰爭", Analyzer::LATEST)
    );
    assert_eq!(missing["ocr_engine"], "ndlocr-lite 636d1cf");

    // The missing pages join the baselines once; the garbled one, and the one
    // curation had in spite of its label, were counted already.
    let v = &published.index_version;
    let titles: Vec<Value> =
        serde_json::from_value(e.reference_json(&format!("{v}/titles.json")).await).unwrap();
    let place = titles
        .iter()
        .find(|t| t["lccn"] == p0.lccn.as_str())
        .unwrap()["place_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let baselines = e.reference_json(&format!("{v}/baselines.json")).await;
    let day = usnm_core::time::day_number(p0.date);
    let at_day = baselines[&place]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d[0] == day)
        .unwrap()[1]
        .as_u64()
        .unwrap();
    assert_eq!(at_day, same_day as u64 + 2);
    let title_pages = e.reference_json(&format!("{v}/title_pages.json")).await;
    let fixture_title_pages: Value = serde_json::from_slice(
        &std::fs::read(fixtures().join("fixture-v1/title_pages.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        title_pages[&p0.lccn],
        fixture_title_pages[&p0.lccn].as_u64().unwrap() + 2
    );
    // The release's page total is the snapshot's.
    let snapshot_total: u64 = title_pages
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(published.pages, snapshot_total);

    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(record["added_to_baselines"], 2);
    // The four pages kept have short test texts: all near-blank (no other kind is listed).
    assert_eq!(record["kinds"]["all"]["near_blank"], 4);
    assert!(record["kinds"]["all"].get("japanese").is_none());
    assert_eq!(record["kinds"]["by_loc_text"]["garbled"]["near_blank"], 1);
    assert_eq!(record["skipped"], 1);
    assert_eq!(record["parts"].as_array().unwrap().len(), 2);
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert!(manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "ocr_ja.json"));
    assert_eq!(
        manifest["built_from"]["ocr_ja"].as_array().unwrap().len(),
        2
    );
}

/// New Japanese OCR with no new batches (#139): an incremental release
/// publishes it on the same main indexes, with a new Japanese index and
/// snapshot; with neither new batches nor new OCR, nothing is released.
#[tokio::test]
async fn new_japanese_ocr_alone_is_released_on_the_same_indexes() {
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

    // A base with the first OCR part.
    put_overlay_part(&e, "ocr-ja/pages/a.parquet", &[row(90, "米國と日本の戰爭")]).await;
    let v1 = e.release(5, true).await.expect("base");
    // No new batches, no new OCR: nothing to release.
    assert!(e.release(6, false).await.is_none());

    // New OCR arrives: released on the same main indexes.
    put_overlay_part(&e, "ocr-ja/pages/b.parquet", &[row(91, "東京の新聞")]).await;
    let v2 = e.release(6, false).await.expect("overlay-only release");
    assert!(!v2.full);
    assert_eq!(v2.indexes, v1.indexes, "no new main index");
    assert_eq!(v2.docs, 0);
    assert_eq!(v2.pages, v1.pages + 1);
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v2.index_version.as_str());
    assert_eq!(current["indexes"], serde_json::json!(v1.indexes));
    let ja_id = format!(
        "pages-ja-{}",
        v2.index_version.trim_start_matches("pages-v")
    );
    assert_eq!(current["ja"]["indexes"], serde_json::json!([ja_id]));
    assert_eq!(e.index(&ja_id).len(), 2);
    let run = raw_run(&e, &v2.index_version).await;
    assert_eq!(run["new_index"], ja_id.as_str());
    // Its build record lists only the template it applied (#161).
    assert!(run["build"]["templates"].get("pages").is_none(), "{run}");
    assert!(run["build"]["templates"]["pages-ja"]["sha256"].is_string());
    let manifest = e
        .reference_json(&format!("{}/manifest.json", v2.index_version))
        .await;
    assert_eq!(
        manifest["build"]["templates"]["pages-ja"]["yaml"],
        usnm_ingest::ocr_ja::JA_TEMPLATE
    );
    assert!(manifest["build"]["templates"].get("pages").is_none());
    let record = e
        .reference_json(&format!("{}/ocr_ja.json", v2.index_version))
        .await;
    assert_eq!(record["added_to_baselines"], 2);

    // And again nothing, until something changes.
    assert!(e.release(7, false).await.is_none());
}

/// OCR that changed without a page to index doesn't make an overlay-only
/// release: the run would name a Japanese index it never wrote.
#[tokio::test]
async fn japanese_ocr_with_nothing_to_index_waits() {
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
    let p0 = pages.iter().find(|p| !p.text.is_empty()).unwrap();
    e.release(5, true).await.expect("base");
    // A part whose only page is from a batch outside the version: changed, nothing to index.
    put_overlay_part(
        &e,
        "ocr-ja/pages/x.parquet",
        &[JaRow {
            lccn: &p0.lccn,
            date: p0.date,
            seq: 90,
            batch: "batch_elsewhere",
            loc_text: "missing",
            text: "東京",
            ocred_at: 1,
        }],
    )
    .await;
    assert!(e.release(6, false).await.is_none());
}

/// With `--ocr-local` (12 §12.5 Step 9) a release gives a page our OCR's
/// text in place of LoC's where rule v1 prefers it, on the copy the version
/// keeps, only for the batches it indexes: a garbled page and one LoC has
/// no text for take ours; a clean one, a short reading, a page the Japanese
/// overlay has and a revoked node's reading keep LoC's. The version records
/// the rule, the parts and the verdicts in `ocr_local.json`, and the pages
/// in `ocr_local_pages.bin`, which a delta carries forward, less a copy
/// another batch's now wins over, and the API marks those hits as ours.
/// A full release that searches all the texts in one field then keeps LoC's
/// text of the pages ours replaced it on beside ours (#925): `text_loc`, and
/// a third text in `text_all`, on those pages only.
#[tokio::test]
async fn our_ocr_replaces_locs_text_where_rule_v1_prefers_it() {
    use usnm_ingest::ocr_local::{pages, pages_path};
    use usnm_state::ocr_local::{OcrLocalPages, OCR_LOCAL_FILE, OCR_LOCAL_PAGES_FILE};
    let e = env().await;
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (mut early, mut late): (Vec<Page>, Vec<Page>) =
        fixture_pages().into_iter().partition(|p| p.date < split);
    let ours_of = |v: &[Page], skip: usize| -> Vec<usize> {
        v.iter()
            .enumerate()
            .filter(|(_, p)| p.lccn == "sn99000001" && !p.text.is_empty())
            .map(|(i, _)| i)
            .skip(skip)
            .collect()
    };
    let sel = ours_of(&early, 0);
    // Pages of prose (the fixture's word lists name too few function words
    // for the metric to decide their language).
    let clean: Vec<String> = (0..7).map(prose).collect();
    let (a, b, c, d, j, f) = (sel[0], sel[1], sel[2], sel[3], sel[4], sel[5]);
    // LoC's text as the archives will have it.
    early[a].text = garble(&clean[0]);
    early[b].text = clean[1].clone();
    early[c].text = format!("{}\n{}", clean[2], garble(&clean[6]));
    early[d].text = String::new();
    early[j].text = garble(&clean[4]);
    early[f].text = garble(&clean[5]);
    let g = ours_of(&late, 0)[0];
    let late_clean = prose(7);
    late[g].text = garble(&late_clean);
    let id = |p: &Page| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq);
    let early_path = e.root.join("batch_fx_early_ver01.tar.gz");
    write_archive(&early_path, &early.iter().collect::<Vec<_>>(), true, true);
    let late_path = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&late_path, &late.iter().collect::<Vec<_>>(), false, true);
    curate(&e, vec![listed("batch_fx_early_ver01", &early_path, None)]).await;

    // Two imports. Page a has worse readings the lower stored rate passes
    // over; f's only reading is from a node later revoked.
    let fx = "batch_fx_early";
    let lccn = "sn99000001";
    let run1 = [
        local_row(&early[a], fx, &clean[0], &early[a].text, "node1", 10),
        // The same page twice in one part (a message delivered twice):
        // the chosen row's own text, not the part's last for the page.
        local_row(
            &early[a],
            fx,
            &garble(&clean[0]),
            &early[a].text,
            "node1",
            10,
        ),
        local_row(
            &early[b],
            fx,
            &garble(&clean[1]),
            &early[b].text,
            "node1",
            10,
        ),
        local_row(&early[c], fx, &clean[2], &early[c].text, "node1", 10),
        local_row(&early[d], fx, &clean[3], "", "node1", 10),
        local_row(&early[j], fx, &clean[4], &early[j].text, "node1", 10),
    ];
    let run2 = [
        local_row(
            &early[a],
            fx,
            &garble(&clean[0]),
            &early[a].text,
            "node1",
            20,
        ),
        local_row(&early[f], fx, &clean[5], &early[f].text, "node-bad", 20),
        local_row(
            &late[g],
            "batch_fx_late",
            &late_clean,
            &late[g].text,
            "node1",
            20,
        ),
    ];
    let mut part_bytes = 0;
    for (run, rows) in [
        ("20261011T000000Z-aaaa", &run1[..]),
        ("20261012T000000Z-bbbb", &run2[..]),
    ] {
        let body = pages::encode(rows).unwrap();
        part_bytes += body.len();
        e.curated
            .put(&pages_path(lccn, run), body, "application/octet-stream")
            .await
            .unwrap();
    }
    e.curated
        .put(
            "ocr-local/revoked/20261013T000000Z.json",
            serde_json::to_vec(&serde_json::json!({
                "node": "node-bad", "reason": "a bad model", "reference": "#296",
            }))
            .unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    // The Japanese overlay has page j, with more words than LoC's: it wins.
    let ja_text = format!("{JA_LATIN}\n{JA_LATIN}");
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[JaRow {
            lccn,
            date: early[j].date,
            seq: early[j].seq,
            batch: fx,
            loc_text: "garbled",
            text: &ja_text,
            ocred_at: 1,
        }],
    )
    .await;

    let release = |day, full| {
        let r = Release {
            ja_latin: true,
            ocr_local: true,
            congress: false,
            congress_manual: false,
            ..e.releaser(day, full)
        };
        let root = e.root.clone();
        async move {
            let mut sink = JsonlSink::new(root.join("reference/indexes"));
            r.run(&mut sink).await.unwrap().unwrap()
        }
    };
    let v1 = release(5, true).await;
    let docs = e.index(&v1.indexes[0]);
    let text = |p: &Page| docs[&id(p)]["text"].as_str().unwrap().to_owned();
    assert_eq!(
        text(&early[a]),
        clean[0],
        "garbled: ours, the lower stored rate"
    );
    assert_eq!(text(&early[b]), clean[1], "clean: LoC's");
    assert_eq!(text(&early[c]), early[c].text, "a short reading: LoC's");
    assert_eq!(text(&early[d]), clean[3], "no LoC text: ours");
    assert_eq!(
        text(&early[j]),
        usnm_ingest::ocr_ja::latin_text(&ja_text),
        "the Japanese overlay's"
    );
    assert_eq!(text(&early[f]), early[f].text, "revoked: LoC's");

    let v = &v1.index_version;
    let record = e.reference_json(&format!("{v}/{OCR_LOCAL_FILE}")).await;
    assert_eq!(record["rule"]["version"], 1);
    assert_eq!(record["rule"]["margin"], 0.05);
    assert_eq!(record["metric"]["version"], usnm_core::damage::VERSION);
    assert_eq!(record["parts"]["count"], 2);
    assert_eq!(record["parts"]["bytes"], part_bytes);
    let p = &record["pages"];
    assert_eq!(p["considered"], 6, "{record}");
    assert_eq!(p["replaced"], 2, "{record}");
    assert_eq!(
        p["kept"],
        serde_json::json!({"margin": 1, "head": 1, "japanese_ocr": 1, "revoked": 1}),
        "{record}"
    );
    assert_eq!(p["by_engine"]["tesseract/5.5.1 eng"]["replaced"], 2);
    assert_eq!(p["by_language"]["eng"]["considered"], 4);
    assert_eq!(record["version"]["pages"], 2);
    assert_eq!(record["revocations"][0]["reason"], "a bad model");
    assert!(record["estimate_published"].is_null(), "a full base");
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    let credits = e.reference_json(&format!("{v}/credits.json")).await;
    assert_eq!(credits["schema"], 1);
    let credit = credits["engines"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["key"] == "tesseract/5.5.1 eng")
        .unwrap();
    assert_eq!(credit["engine"]["author"], "Tesseract OCR project");
    assert_eq!(credit["models"][0]["name"], "tessdata/eng.traineddata");
    assert_eq!(credit["models"].as_array().unwrap().len(), 1);
    assert_eq!(credit["models"][0]["fetched"], "2026-10-11");
    assert_eq!(manifest["built_from"]["ocr_local"], record["parts"]);
    assert_eq!(manifest["build"]["features"]["ocr_local"], 1);
    let bin = |v: &str| {
        let e = &e;
        let v = v.to_owned();
        async move {
            let bytes = e
                .reference
                .get(&format!("{v}/{OCR_LOCAL_PAGES_FILE}"))
                .await
                .unwrap()
                .unwrap();
            let batches = e.reference_json(&format!("{v}/batches.json")).await;
            let names: Vec<String> = batches
                .as_array()
                .unwrap()
                .iter()
                .map(|b| b["batch"].as_str().unwrap().to_owned())
                .collect();
            (OcrLocalPages::decode(&bytes, names.len()).unwrap(), names)
        }
    };
    let (pages1, names1) = bin(v).await;
    assert_eq!(pages1.len(), 2);
    for p in [&early[a], &early[d]] {
        let entry = pages1.get(&id(p)).expect("ours");
        assert_eq!(names1[usize::from(entry.batch)], fx);
        assert_eq!(pages1.engine(entry).unwrap().source, "usnm-tesseract");
    }
    assert!(pages1.get(&id(&early[b])).is_none());

    // A delta: the late batch, and a batch sorting first whose copy of
    // page a has clean LoC text, so the plan keeps that copy and hides ours.
    let again = Page {
        text: clean[0].clone(),
        ..early[a].clone()
    };
    let again_path = e.root.join("batch_fx_again_ver01.tar.gz");
    write_archive(&again_path, &[&again], false, true);
    curate(
        &e,
        vec![
            listed("batch_fx_late_ver01", &late_path, None),
            listed("batch_fx_again_ver01", &again_path, None),
        ],
    )
    .await;
    let v2 = release(6, false).await;
    assert!(!v2.full);
    let delta = e.index(&v2.indexes[1]);
    assert_eq!(delta[&id(&late[g])]["text"], late_clean.as_str());
    assert_eq!(delta[&id(&again)]["text"], clean[0].as_str());
    let v = &v2.index_version;
    let hidden = e.reference_json(&format!("{v}/duplicates.json")).await;
    assert!(hidden
        .as_array()
        .unwrap()
        .contains(&serde_json::json!({"doc_id": id(&early[a]), "batch": fx})));
    let (pages2, names2) = bin(v).await;
    assert_eq!(pages2.len(), 2, "d carried, a dropped, g added");
    assert!(pages2.get(&id(&early[a])).is_none());
    assert_eq!(
        names2[usize::from(pages2.get(&id(&early[d])).unwrap().batch)],
        fx
    );
    assert_eq!(
        names2[usize::from(pages2.get(&id(&late[g])).unwrap().batch)],
        "batch_fx_late"
    );
    let record = e.reference_json(&format!("{v}/{OCR_LOCAL_FILE}")).await;
    assert_eq!(
        record["version"],
        serde_json::json!({"pages": 2, "carried": 2, "dropped": 1, "added": 1}),
        "{record}"
    );
    // Of the published batch: a, d and j by the stored scores (f revoked,
    // b and c kept).
    assert_eq!(record["estimate_published"], 3, "{record}");
    assert_eq!(
        record["pages"]["kept"]["margin"], 1,
        "again's copy: {record}"
    );

    // The API marks the hits on our text, not the hidden copy's.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.ocr_local.len(), 2);
    assert!(refdata.ocr_local.get(&id(&late[g])).is_some());
    let frozen = e.reference_json(&format!("{v}/credits.json")).await;
    assert_eq!(refdata.ocr_credits, Some(frozen));

    // A full release with the one-field layout: on a page ours replaced
    // LoC's text on, `text` is ours, `text_loc` LoC's, and `text_all` holds
    // ours and then LoC's. No other page has `text_loc`: not a page whose
    // LoC text is kept (a, whose copy another batch keeps, and b), not the
    // page the Japanese overlay's Latin text replaced LoC's on (j), and not
    // one LoC had no text for (d).
    let r3 = Release {
        ja_latin: true,
        ocr_local: true,
        congress: false,
        congress_manual: false,
        single_text_field: true,
        ..e.releaser(7, true)
    };
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    let v3 = r3.run(&mut sink).await.unwrap().unwrap();
    assert!(v3.full);
    assert_eq!(e.reference_json("current.json").await["text_layout"], 3);
    let (pages3, _) = bin(&v3.index_version).await;
    assert_eq!(pages3.len(), 2, "d and g");
    let docs3: std::collections::BTreeMap<String, Value> = v3
        .indexes
        .iter()
        .filter(|i| !i.starts_with("pages-ja-"))
        .flat_map(|i| {
            by_id(read_jsonl(
                &e.root.join(format!("reference/indexes/{i}.jsonl")),
            ))
        })
        .collect();
    let original = |p: &Page| docs3[&id(p)].get("text_loc").and_then(Value::as_str);
    assert_eq!(docs3[&id(&late[g])]["text"], late_clean.as_str());
    assert_eq!(original(&late[g]), Some(late[g].text.as_str()));
    for p in [
        &early[a], &early[b], &early[c], &early[d], &early[f], &early[j],
    ] {
        assert_eq!(original(p), None, "{}", id(p));
    }
    assert_eq!(docs3[&id(&early[d])]["text"], clean[3].as_str());
    for (doc_id, doc) in &docs3 {
        let words = doc["text_all"].as_str().unwrap();
        let want = usnm_core::text_layout::index_texts(
            std::iter::once(doc["text"].as_str().unwrap())
                .chain(doc["text_as"].as_str())
                .chain(doc["text_loc"].as_str()),
            Analyzer::LATEST,
        );
        assert_eq!(words, want.0, "{doc_id}");
        assert_eq!(doc["text_all_cg"].as_str().unwrap(), want.1, "{doc_id}");
        assert!(doc.get("text_cg").is_none(), "{doc_id}");
        // Only a page of our OCR's has a LoC original beside it.
        assert!(
            doc.get("text_loc").is_none() || pages3.get(doc_id).is_some(),
            "{doc_id}"
        );
    }
    // The gap sits between ours and LoC's: the first word of LoC's text
    // is 33 positions after the last of ours.
    let doc = &docs3[&id(&late[g])];
    let gap = usnm_core::text_layout::GAP_POSITIONS;
    let words: Vec<&str> = doc["text_all"].as_str().unwrap().split(' ').collect();
    let ours = usnm_core::common_grams::index_words(&late_clean, Analyzer::LATEST)
        .split(' ')
        .count();
    assert!(words[ours..ours + gap].iter().all(|w| *w == "_"));
    assert_ne!(words[ours + gap], "_");
    // The page counts once: one document.
    assert_eq!(docs3.keys().filter(|k| **k == id(&late[g])).count(), 1);
}
