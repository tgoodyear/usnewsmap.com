//! Enqueue and curate: leases and retries, a real LoC archive, retained
//! archives, and the titles the catalog has, lacks or moves.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_ingest::release::Release;
use usnm_ingest::sink::JsonlSink;
use usnm_ingest::source;
use usnm_ingest::state::BatchStatus;
use usnm_store::{LocalStore, ObjectStore};

use crate::common::{env, fixture_pages, listed, sha256_file, write_archive, Page};

#[tokio::test]
async fn failed_and_leased_batches_are_retried_not_lost() {
    let e = env().await;
    let pages = fixture_pages();
    let few: Vec<&Page> = pages.iter().take(20).collect();
    let archive = e.root.join("batch_fx_small_ver01.tar.gz");
    write_archive(&archive, &few, true, true);

    // A wrong checksum fails the attempt and re-queues the batch.
    let bad = [listed(
        "batch_fx_small_ver01",
        &archive,
        Some("0".repeat(64)),
    )];
    source::enqueue(&e.state, &bad).await.unwrap();
    assert_eq!(e.worker("w1").run(None).await.unwrap(), 0);
    let (b, _) = e.state.batch("batch_fx_small").await.unwrap().unwrap();
    assert_eq!((b.status, b.attempts), (BatchStatus::Queued, 1));
    assert!(b.last_error.as_deref().unwrap().contains("sha256"));
    assert!(b.curated.is_none() && b.lease.is_none());

    // A claimed batch is invisible to other workers until its lease expires.
    let mut b2 = b.clone();
    b2.source_sha256 = None;
    let (_, etag) = e.state.batch("batch_fx_small").await.unwrap().unwrap();
    e.state.replace_batch(&b2, &etag).await.unwrap().unwrap();
    let w1 = e.worker("w1");
    let (claimed, _) = w1.claim().await.unwrap().unwrap();
    assert!(e.worker("w2").claim().await.unwrap().is_none());

    // A worker that lost its lease can't commit.
    let c = w1.curate(&claimed).await.unwrap();
    assert!(e
        .worker("w2")
        .commit("batch_fx_small", c.clone())
        .await
        .is_err());
    w1.commit("batch_fx_small", c).await.unwrap();
    let (b, _) = e.state.batch("batch_fx_small").await.unwrap().unwrap();
    assert_eq!(b.status, BatchStatus::Curated);
    assert_eq!(b.curated.unwrap().pages, 20);

    // A second release while the writer lock is held elsewhere is refused.
    e.state
        .lock(
            usnm_ingest::release::WRITER_LOCK,
            "someone-else",
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
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
    let mut sink = JsonlSink::new(e.root.join("idx"));
    assert!(r.run(&mut sink).await.is_err());
}

/// A real LoC bulk OCR archive (see `tests/data/README.md`): the layout,
/// compression and checksum as LoC publishes them.
#[tokio::test]
async fn curates_a_real_loc_archive() {
    let e = env().await;
    let archive =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/dlc_zurich_ver04.tar.bz2");
    let list = source::parse_list(
        format!(
            r#"{{"datasets": [{{"batch": "dlc_zurich_ver04", "url": "{}", "page_count": 4,
            "sha256": "ad36f7bb2ef915867460ac2b3c70f9aaa33709a9a99f554a49a9dc4fc35f3ba9"}}]}}"#,
            archive.display()
        )
        .as_bytes(),
    )
    .unwrap();
    source::enqueue(&e.state, &list).await.unwrap();
    assert_eq!(e.worker("w").run(None).await.unwrap(), 1);
    let (b, _) = e.state.batch("dlc_zurich").await.unwrap().unwrap();
    let c = b.curated.unwrap();
    assert_eq!(
        (c.version, c.pages, c.lccns.as_slice()),
        (4, 4, &["sn85042252".to_owned()][..])
    );
    assert_eq!(
        (c.first.as_str(), c.last.as_str()),
        ("1865-08-10", "1865-08-10")
    );
    let mut rows = Vec::new();
    let part = e.curated.get(&c.parts[0]).await.unwrap().unwrap();
    usnm_overlay::curated::read_part(part.into(), true, |r| {
        rows.push(r);
        Ok(())
    })
    .unwrap();
    let mut seqs: Vec<u16> = rows.iter().map(|r| r.key.seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert!(rows
        .iter()
        .all(|r| r.text.as_ref().is_some_and(|t| t.len() > 500)));
}

/// Workers that die mid-batch never record a failure; the attempt cap still
/// stops the batch being retried forever.
#[tokio::test]
async fn batches_abandoned_by_crashed_workers_fail_at_the_attempt_cap() {
    let e = env().await;
    let a = e.root.join("batch_fx_x_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(3).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_x_ver01", &a, None)])
        .await
        .unwrap();
    for _ in 0..usnm_ingest::worker::MAX_ATTEMPTS {
        let w = e.worker("crashy");
        let (mut b, _) = w.claim().await.unwrap().unwrap();
        // Crash: the lease simply runs out.
        let (_, etag) = e.state.batch(&b.batch).await.unwrap().unwrap();
        b.lease.as_mut().unwrap().until = Utc::now() - chrono::Duration::seconds(1);
        e.state.replace_batch(&b, &etag).await.unwrap().unwrap();
    }
    assert!(e.worker("w").claim().await.unwrap().is_none());
    let (b, _) = e.state.batch("batch_fx_x").await.unwrap().unwrap();
    assert_eq!(b.status, BatchStatus::Failed);
    assert!(b.last_error.unwrap().contains("abandoned"));
}

/// A batch whose title isn't in the catalog yet waits for a later release;
/// the batches that are ready publish without it.
#[tokio::test]
async fn batches_with_uncatalogued_titles_wait_for_a_later_release() {
    let e = env().await;
    let pages = fixture_pages();
    let ready: Vec<&Page> = pages
        .iter()
        .filter(|p| !p.text.is_empty())
        .take(20)
        .collect();
    let unknown = Page {
        lccn: "sn99999999".into(),
        date: ready[0].date,
        seq: 1,
        text: "armistice".into(),
    };
    let a = e.root.join("batch_fx_ready_ver01.tar.gz");
    let b = e.root.join("batch_fx_new_ver01.tar.gz");
    write_archive(&a, &ready, true, true);
    write_archive(&b, &[&unknown], true, true);
    source::enqueue(
        &e.state,
        &[
            listed("batch_fx_ready_ver01", &a, None),
            listed("batch_fx_new_ver01", &b, None),
        ],
    )
    .await
    .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p = e.release(1, false).await.unwrap();
    assert_eq!(p.pages, 20);
    // Nothing is ready to add until the title is catalogued.
    assert!(e.release(2, false).await.is_none());
}

/// With titles-sync unfinished, a full base, asked for or forced (there is
/// no published version yet), is refused; a delta goes ahead.
#[tokio::test]
async fn an_unfinished_titles_sync_holds_back_a_full_release_only() {
    let e = env().await;
    let pages = fixture_pages();
    let some: Vec<&Page> = pages
        .iter()
        .filter(|p| !p.text.is_empty())
        .take(20)
        .collect();
    let a = e.root.join("batch_fx_some_ver01.tar.gz");
    write_archive(&a, &some, true, true);
    source::enqueue(&e.state, &[listed("batch_fx_some_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let release = |full: bool, titles_left: Option<String>| Release {
        stage_index_storage: false,
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full,
        synthetic: true,
        now: Utc.with_ymd_and_hms(2026, 10, 1, 3, 0, 0).unwrap(),
        titles_left,
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
    let why = Some("LoC rate limited titles-sync with 5 of 9 titles left".to_owned());
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    // Not asked for, but forced: nothing is published yet.
    let err = release(false, why.clone())
        .run(&mut sink)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing was released"), "{err}");
    assert!(release(false, None).run(&mut sink).await.unwrap().is_some());
    let err = release(true, why.clone())
        .run(&mut sink)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("5 of 9 titles left"), "{err}");
    // A delta goes ahead (here with nothing new to add).
    assert!(release(false, why).run(&mut sink).await.unwrap().is_none());
}

/// A published title the catalog puts in another place (a merge) stays
/// where it was published until a full release, and a corrected point
/// rides a delta with the published place's id (04 §4.6).
#[tokio::test]
async fn a_title_moved_to_another_place_waits_for_a_full_release() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let (late_a, late_b) = late.split_at(late.len() / 2);
    let archives: Vec<(String, PathBuf)> = [("early", &early[..]), ("a", late_a), ("b", late_b)]
        .iter()
        .map(|(name, pages)| {
            let name = format!("batch_fx_{name}_ver01");
            let path = e.root.join(format!("{name}.tar.gz"));
            write_archive(&path, pages, true, true);
            (name, path)
        })
        .collect();
    let add = |i: usize| {
        let (name, path) = archives[i].clone();
        let e = &e;
        async move {
            source::enqueue(&e.state, &[listed(&name, &path, None)])
                .await
                .unwrap();
            e.worker("w").run(None).await.unwrap();
        }
    };
    let edit = |f: Box<dyn Fn(&mut Value)>, path: &'static str| {
        let e = &e;
        async move {
            let mut v = e.reference_json(path).await;
            f(&mut v);
            e.reference
                .put(path, serde_json::to_vec(&v).unwrap(), "application/json")
                .await
                .unwrap();
        }
    };
    add(0).await;
    let p1 = e.release(1, false).await.unwrap();

    // A corrected point for a published place: a delta, whose snapshot has
    // the new point under the same id.
    edit(
        Box::new(|v| v[0]["lat"] = serde_json::json!(41.5)),
        "catalog/places.json",
    )
    .await;
    add(1).await;
    let p2 = e.release(2, false).await.unwrap();
    assert!(!p2.full);
    assert_ne!(p2.index_version, p1.index_version);
    let places = e
        .reference_json(&format!("{}/places.json", p2.index_version))
        .await;
    let first = places
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "P00001")
        .unwrap();
    assert_eq!(first["lat"], 41.5);

    // A published title moved to another place: the delta keeps it where
    // it was published, so its new pages join its published ones.
    let moved = late_b
        .iter()
        .find(|p| !p.text.is_empty())
        .unwrap()
        .lccn
        .clone();
    let catalog = e.reference_json("catalog/titles.json").await;
    let at = catalog
        .as_array()
        .unwrap()
        .iter()
        .position(|t| t["lccn"] == moved.as_str())
        .unwrap();
    let was = catalog[at]["place_id"].as_str().unwrap().to_owned();
    let to = if was == "P00001" { "P00002" } else { "P00001" };
    edit(
        Box::new(move |v| v[at]["place_id"] = serde_json::json!(to)),
        "catalog/titles.json",
    )
    .await;
    add(2).await;
    let p3 = e.release(3, false).await.unwrap();
    assert!(!p3.full, "no full base on its own (#172)");
    let place_in = |titles: &Value| {
        titles
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["lccn"] == moved.as_str())
            .unwrap()["place_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let titles = e
        .reference_json(&format!("{}/titles.json", p3.index_version))
        .await;
    assert_eq!(place_in(&titles), was, "kept where it was published");
    let delta = e.index(p3.indexes.last().unwrap());
    let of_title: Vec<&Value> = delta
        .values()
        .filter(|d| d["lccn"] == moved.as_str())
        .collect();
    assert!(!of_title.is_empty());
    assert!(of_title.iter().all(|d| d["place_id"] == was.as_str()));

    // A full release moves it everywhere.
    let p4 = e.release(4, true).await.unwrap();
    assert!(p4.full);
    let titles = e
        .reference_json(&format!("{}/titles.json", p4.index_version))
        .await;
    assert_eq!(place_in(&titles), to);
    let docs = e.index(&p4.indexes[0]);
    let of_title: Vec<&Value> = docs
        .values()
        .filter(|d| d["lccn"] == moved.as_str())
        .collect();
    assert!(!of_title.is_empty());
    assert!(of_title.iter().all(|d| d["place_id"] == to));
}

/// A title the catalog places by date (#1066): a page is in the place of
/// its date in the documents, the baselines and the snapshot's titles and
/// places, once a full release applies the periods. A delta before that
/// keeps the title where it was published, so a version never counts a page
/// in one place and indexes it in another.
#[tokio::test]
async fn a_title_that_moved_has_each_page_in_the_place_of_its_date() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archives: Vec<(String, PathBuf)> = [("early", &early[..]), ("late", &late[..])]
        .iter()
        .map(|(name, pages)| {
            let name = format!("batch_fx_{name}_ver01");
            let path = e.root.join(format!("{name}.tar.gz"));
            write_archive(&path, pages, true, true);
            (name, path)
        })
        .collect();
    let add = |i: usize| {
        let (name, path) = archives[i].clone();
        let e = &e;
        async move {
            source::enqueue(&e.state, &[listed(&name, &path, None)])
                .await
                .unwrap();
            e.worker("w").run(None).await.unwrap();
        }
    };

    // sn99000002 (P00002, New York) was in P00001 (Illinois) up to 1896-06-30.
    let moved = "sn99000002";
    let last_day = NaiveDate::from_ymd_opt(1896, 6, 30).unwrap();
    let place_on = |lccn: &str, date: NaiveDate, periods: bool| {
        let own = lccn.trim_start_matches("sn99").parse::<u32>().unwrap();
        match (periods && lccn == moved && date <= last_day, own) {
            (true, _) => "P00001".to_owned(),
            (false, n) => format!("P{n:05}"),
        }
    };
    let baselines_of = |periods: bool| {
        let mut by: BTreeMap<String, BTreeMap<u32, u32>> = BTreeMap::new();
        for p in &pages {
            *by.entry(place_on(&p.lccn, p.date, periods))
                .or_default()
                .entry(usnm_core::time::day_number(p.date))
                .or_default() += 1;
        }
        by.into_iter()
            .map(|(k, v)| (k, v.into_iter().collect::<Vec<(u32, u32)>>()))
            .collect::<BTreeMap<_, _>>()
    };
    let snapshot_baselines =
        |v: Value| -> BTreeMap<String, Vec<(u32, u32)>> { serde_json::from_value(v).unwrap() };

    add(0).await;
    let p1 = e.release(1, false).await.unwrap();
    assert!(p1.full);

    // The catalog now gives the title its places by date. A delta keeps it
    // where it was published, in its pages and its counts.
    let mut titles = e.reference_json("catalog/titles.json").await;
    let at = titles
        .as_array()
        .unwrap()
        .iter()
        .position(|t| t["lccn"] == moved)
        .unwrap();
    titles[at]["place_periods"] = serde_json::json!([
        {"place_id": "P00001", "to": "1896-06-30"},
        {"place_id": "P00002", "from": "1896-07-01"},
    ]);
    e.reference
        .put(
            "catalog/titles.json",
            serde_json::to_vec(&titles).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    add(1).await;
    let p2 = e.release(2, false).await.unwrap();
    assert!(!p2.full);
    let v2 = &p2.index_version;
    let published = e.reference_json(&format!("{v2}/titles.json")).await;
    let record = published
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["lccn"] == moved)
        .unwrap();
    assert!(record.get("place_periods").is_none(), "{record}");
    assert_eq!(
        snapshot_baselines(e.reference_json(&format!("{v2}/baselines.json")).await),
        baselines_of(false)
    );
    for id in &p2.indexes {
        for d in e.index(id).values().filter(|d| d["lccn"] == moved) {
            assert_eq!(
                (&d["place_id"], &d["state"]),
                (&"P00002".into(), &"NY".into())
            );
        }
    }

    // A full release applies them: the pages up to 1896-06-30 are in
    // Illinois's place, the rest in New York's.
    let p3 = e.release(3, true).await.unwrap();
    assert!(p3.full);
    let v3 = &p3.index_version;
    let docs = e.index(&p3.indexes[0]);
    let (mut early_docs, mut late_docs) = (0, 0);
    for d in docs.values() {
        let lccn = d["lccn"].as_str().unwrap();
        let date: NaiveDate = d["date"].as_str().unwrap().parse().unwrap();
        let id = place_on(lccn, date, true);
        let ordinal: u64 = id.trim_start_matches('P').parse().unwrap();
        assert_eq!(d["place_id"], id.as_str(), "{}", d["doc_id"]);
        assert_eq!(d["place_shard"], ordinal % 8, "{}", d["doc_id"]);
        if lccn == moved {
            let state = if id == "P00001" { "IL" } else { "NY" };
            assert_eq!(d["state"], state, "{}", d["doc_id"]);
            if id == "P00001" {
                early_docs += 1;
            } else {
                late_docs += 1;
            }
        }
    }
    assert!(early_docs > 0 && late_docs > 0, "{early_docs} {late_docs}");
    // The counts are the same pages by the same places, empty ones too.
    assert_eq!(
        snapshot_baselines(e.reference_json(&format!("{v3}/baselines.json")).await),
        baselines_of(true)
    );
    let published = e.reference_json(&format!("{v3}/titles.json")).await;
    let record = published
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["lccn"] == moved)
        .unwrap();
    assert_eq!(record["place_id"], "P00002");
    assert_eq!(record["place_periods"], titles[at]["place_periods"]);
    // Every page is still counted once, by title as by place.
    let want_pages: Value = serde_json::from_slice(
        &std::fs::read(crate::common::fixtures().join("fixture-v1/title_pages.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        e.reference_json(&format!("{v3}/title_pages.json")).await,
        want_pages
    );
    let places = e.reference_json(&format!("{v3}/places.json")).await;
    let ids: Vec<&str> = places
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert!(
        ids.contains(&"P00001") && ids.contains(&"P00002"),
        "{ids:?}"
    );
}

/// With a raw store (`USNM_RETAIN_RAW`), curation keeps each archive byte for
/// byte with a manifest, and a batch queued again (`enqueue --force`) is
/// curated from that copy, the source gone, to the same pages.
#[tokio::test]
async fn retained_archives_are_kept_and_read_back() {
    let e = env().await;
    let pages = fixture_pages();
    let all: Vec<&Page> = pages.iter().collect();
    let archive = e.root.join("batch_fx_raw_ver01.tar.bz2");
    write_archive(&archive, &all, false, false);
    let original = std::fs::read(&archive).unwrap();
    let sha = sha256_file(&archive);
    let raw: Arc<dyn ObjectStore> = Arc::new(LocalStore::new(e.root.join("raw")));
    let worker = |owner: &str| usnm_ingest::worker::Worker {
        raw: Some(raw.clone()),
        ..e.worker(owner)
    };
    let list = [listed("batch_fx_raw_ver01", &archive, Some(sha.clone()))];
    source::enqueue(&e.state, &list).await.unwrap();
    assert_eq!(worker("w1").run(None).await.unwrap(), 1);
    let first = e.state.batch("batch_fx_raw").await.unwrap().unwrap().0;
    let kept = raw
        .get("batch_fx_raw_ver01/batch_fx_raw_ver01.tar.bz2")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(kept, original, "byte for byte");
    let m: Value = serde_json::from_slice(
        &raw.get("batch_fx_raw_ver01/manifest.json")
            .await
            .unwrap()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(m["sha256"], sha.as_str());
    assert_eq!(m["bytes"], original.len() as u64);
    assert_eq!(m["source_url"], archive.to_str().unwrap());

    // Queued again and curated with the source gone: from the retained copy.
    let report = source::requeue(&e.state, &list).await.unwrap();
    assert_eq!(report.requeued, 1);
    // Without --force the curated batch stays as it is.
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().unchanged, 1);
    std::fs::remove_file(&archive).unwrap();
    assert_eq!(worker("w2").run(None).await.unwrap(), 1);
    let second = e.state.batch("batch_fx_raw").await.unwrap().unwrap().0;
    let (a, b) = (first.curated.unwrap(), second.curated.unwrap());
    assert_ne!(a.parts, b.parts, "a new attempt");
    assert_eq!(
        (a.pages, a.ok_pages, a.source_sha256),
        (b.pages, b.ok_pages, b.source_sha256)
    );
}
