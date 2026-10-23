//! Pages that ship in two batches (04 §4.7): indexed and counted once, the
//! copies a delta hides, and the same on a real Quickwit writer.

use std::collections::BTreeMap;
use std::path::Path;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_ingest::release::Release;

use crate::common::{
    curate, env, fixture_pages, fixtures, listed, loc_fixture, quick_merges, raw_run, read_jsonl,
    sha256_file, write_archive, Page,
};

/// Pages that ship in two batches are indexed and counted once (04 §4.7).
/// Of a page's copies the one kept has text, then comes from the batch whose
/// name sorts first. A delta can't drop a copy a published index holds, so
/// the snapshot lists it for searches to hide.
#[tokio::test]
async fn pages_in_two_batches_are_kept_once() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, Some(sha256_file(&path)))
    };

    // Week 1, a base: the early pages, and 25 of them again in a batch whose
    // name sorts later, so the early batch's copies are kept.
    curate(
        &e,
        vec![
            archive("batch_fx_early_ver01", &early),
            archive("batch_zz_ver01", &early[..25]),
        ],
    )
    .await;
    // `usnm-ingest duplicates` measures them before any release.
    let report = usnm_ingest::dedup::report(&e.state, e.curated.as_ref())
        .await
        .unwrap();
    assert_eq!(report["duplicate_pages"], 25);
    assert_eq!(report["distinct_pages"], early.len());
    assert_eq!(
        report["pairs"],
        serde_json::json!([{"kept": "batch_fx_early", "other": "batch_zz", "pages": 25}])
    );
    let p1 = e.release(1, false).await.unwrap();
    assert!(p1.full);
    let want_base = loc_fixture("pages-base-fixture");
    let base = e.index(&p1.indexes[0]);
    assert_eq!(base, want_base, "each page once");
    assert_eq!(p1.docs, want_base.len() as u64);
    assert_eq!(p1.pages, early.len() as u64);
    let v1 = &p1.index_version;
    assert_eq!(raw_run(&e, v1).await["duplicate_pages"], 25);
    let manifest = e.reference_json(&format!("{v1}/manifest.json")).await;
    assert_eq!(manifest["duplicate_pages"], 25);
    // A base indexes one copy of everything, so nothing needs hiding.
    assert_eq!(
        e.reference_json(&format!("{v1}/duplicates.json")).await,
        serde_json::json!([])
    );
    let title_pages: BTreeMap<String, u64> =
        serde_json::from_value(e.reference_json(&format!("{v1}/title_pages.json")).await).unwrap();
    assert_eq!(title_pages.values().sum::<u64>(), early.len() as u64);

    // Week 2, a delta: the late pages, and a batch whose name sorts first
    // with 10 more early pages (already published) and 10 of the late ones.
    let again_early: Vec<&Page> = early[30..40].to_vec();
    let mut aa = again_early.clone();
    aa.extend(&late[..10]);
    curate(
        &e,
        vec![
            archive("batch_aa_ver01", &aa),
            archive("batch_fx_late_ver01", &late),
        ],
    )
    .await;
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(p2.pages, pages.len() as u64);
    let v2 = &p2.index_version;
    assert_eq!(raw_run(&e, v2).await["duplicate_pages"], 25 + 10 + 10);
    // The delta has every late page once, plus batch_aa's copies of the 10
    // early pages: they win, and the base's copies are hidden.
    let want_delta = loc_fixture("pages-delta-fixture-1");
    let delta = e.index(p2.indexes.last().unwrap());
    let early_ids: Vec<String> = again_early
        .iter()
        .map(|p| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq))
        .filter(|id| want_base.contains_key(id))
        .collect();
    assert!(!early_ids.is_empty());
    let mut want = want_delta.clone();
    want.extend(
        early_ids
            .iter()
            .map(|id| (id.clone(), want_base[id].clone())),
    );
    assert_eq!(delta, want);
    let hidden = e.reference_json(&format!("{v2}/duplicates.json")).await;
    let want_hidden: Vec<Value> = early_ids
        .iter()
        .map(|id| serde_json::json!({"doc_id": id, "batch": "batch_fx_early"}))
        .collect();
    let mut got: Vec<Value> = hidden.as_array().unwrap().clone();
    got.sort_by_key(|h| h["doc_id"].as_str().unwrap().to_owned());
    let mut want_hidden = want_hidden;
    want_hidden.sort_by_key(|h| h["doc_id"].as_str().unwrap().to_owned());
    assert_eq!(got, want_hidden);
    // Every page counts once: the snapshot matches the fixture's.
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

    // The API loads the copies to hide with the snapshot.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.duplicate_pages, Some(45));
    assert_eq!(refdata.hidden.len(), early_ids.len());
    assert_eq!(refdata.pages, pages.len() as u64);
    assert!(refdata.index_set().hides(&early_ids[0], "batch_fx_early"));

    // A full release keeps the same copies and needs to hide none.
    let p3 = e.release(15, true).await.unwrap();
    let mut both = want_base.clone();
    both.extend(want_delta);
    assert_eq!(e.index(&p3.indexes[0]), both);
    let v3 = &p3.index_version;
    assert_eq!(
        e.reference_json(&format!("{v3}/duplicates.json")).await,
        serde_json::json!([])
    );
    let kept: BTreeMap<String, String> = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", p3.indexes[0])),
    )
    .into_iter()
    .map(|d| {
        (
            d["doc_id"].as_str().unwrap().to_owned(),
            d["batch"].as_str().unwrap().to_owned(),
        )
    })
    .collect();
    assert!(early_ids.iter().all(|id| kept[id] == "batch_aa"));
}

/// A version released before duplicates were handled indexed every copy, so
/// the first delta on top of it hides every copy it doesn't keep.
#[tokio::test]
async fn a_delta_on_an_older_version_hides_every_extra_copy() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, Some(sha256_file(&path)))
    };
    curate(
        &e,
        vec![
            archive("batch_fx_early_ver01", &early),
            archive("batch_zz_ver01", &early[..25]),
        ],
    )
    .await;
    let p1 = e.release(1, false).await.unwrap();
    // As an older release wrote it: no list of hidden copies.
    std::fs::remove_file(
        e.root
            .join(format!("reference/{}/duplicates.json", p1.index_version)),
    )
    .unwrap();

    curate(&e, vec![archive("batch_fx_late_ver01", &late)]).await;
    let p2 = e.release(8, false).await.unwrap();
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", p2.index_version))
        .await;
    let base = e.index(&p1.indexes[0]);
    let want: Vec<Value> = early[..25]
        .iter()
        .map(|p| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq))
        .filter(|id| base.contains_key(id))
        .map(|id| serde_json::json!({"doc_id": id, "batch": "batch_zz"}))
        .collect();
    assert!(!want.is_empty());
    assert_eq!(hidden, Value::Array(want));
}

/// Searches on Quickwit hide the copies a delta couldn't drop: the base
/// holds one copy of a page and the delta the copy that wins, and every
/// count and hit list sees the page once. Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn quickwit_searches_hide_the_copies_a_delta_could_not_drop() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_core::params::Filters;
    use usnm_core::query::parse;
    use usnm_core::time::{BucketSpec, BucketUnit};
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    use usnm_search::quickwit::QuickwitBackend;
    use usnm_search::{HitsQuery, IndexSet, SearchBackend};

    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let early: Vec<&Page> = pages.iter().filter(|p| p.date < split).collect();
    let again: Vec<&Page> = early[30..40].to_vec();
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, None)
    };
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7393, &meta, &root)
        .await
        .unwrap();
    let mut published = Vec::new();
    for (day, batch) in [
        (1, archive("batch_fx_early_ver01", &early)),
        (8, archive("batch_aa_ver01", &again)),
    ] {
        curate(&e, vec![batch]).await;
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
            american_stories: false,
            ja_latin: false,
            ocr_local: false,
            congress: false,
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
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", p.index_version))
        .await;
    let hidden: Vec<(String, String)> = hidden
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["doc_id"].as_str().unwrap().to_owned(),
                h["batch"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(!hidden.is_empty());

    let backend = QuickwitBackend::new(&node.url, std::time::Duration::from_secs(30)).unwrap();
    let both = IndexSet::new(p.indexes.clone());
    let deduped = both.clone().hiding(hidden.clone());
    let from = NaiveDate::from_ymd_opt(1895, 1, 1).unwrap();
    let to = NaiveDate::from_ymd_opt(1897, 6, 30).unwrap();
    let f = Filters {
        from,
        to,
        states: vec![],
        lccns: vec![],
        langs: vec![],
        front_only: false,
        series: Vec::new(),
    };
    let spec = BucketSpec::new(BucketUnit::Year, from, to);
    let every = parse("the OR a OR of OR and").unwrap();
    let with = backend.summary(&both, &every, &f, &spec).await.unwrap();
    let without = backend.summary(&deduped, &every, &f, &spec).await.unwrap();
    assert_eq!(with.total_hits - without.total_hits, hidden.len() as u64);
    let indexed: u64 = ["pages-base-fixture"]
        .iter()
        .map(|f| read_jsonl(&fixtures().join(format!("indexes/{f}.jsonl"))).len() as u64)
        .sum();
    assert_eq!(
        p.docs,
        hidden.len() as u64,
        "the delta holds the winning copies"
    );
    assert!(without.total_hits <= indexed);

    // A hidden page's hit list shows it once, from the batch that won.
    let (doc_id, _) = &hidden[0];
    let key = usnm_core::ids::PageKey::from_doc_id(doc_id).unwrap();
    let day = Filters {
        from: key.date,
        to: key.date,
        lccns: vec![key.lccn.clone()],
        ..f.clone()
    };
    let page = HitsQuery {
        lccn: Some(key.lccn.clone()),
        limit: 50,
        ..HitsQuery::default()
    };
    let ids = |set: &IndexSet| {
        let (backend, day, page, every) = (&backend, &day, &page, &every);
        let set = set.clone();
        async move {
            backend
                .hits(&set, every, day, page)
                .await
                .unwrap()
                .hits
                .into_iter()
                .filter(|h| h.doc_id == *doc_id)
                .count()
        }
    };
    assert_eq!(ids(&both).await, 2);
    assert_eq!(ids(&deduped).await, 1);
    node.stop().await.unwrap();
}
