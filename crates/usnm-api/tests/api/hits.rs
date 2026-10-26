//! `/v1/hits`: order, marks and links, our own OCR, and a title's pages without a query.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::refdata::RefData;
use usnm_api::{app, AppState};
use usnm_core::time::date_from_day;
use usnm_search::PageDoc;
use usnm_store::LocalStore;

use crate::common::{
    config, fixture_backend, get, header_str, load_docs, refdata, state_with, temp_data_dir,
};

#[tokio::test]
async fn hits_are_sorted_marked_linked_and_paginated() {
    let s = state_with(None).await;
    let base = "/v1/hits?q=%22cross+of+gold%22&from=1896-01-01&to=1896-12-31&place=P00001&limit=5";
    let (status, _, page1) = get(&s, base).await;
    assert_eq!(status, StatusCode::OK, "{page1}");
    let items = page1["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let dates: Vec<&str> = items.iter().map(|i| i["date"].as_str().unwrap()).collect();
    let mut sorted = dates.clone();
    sorted.sort_unstable();
    assert_eq!(dates, sorted);
    let first = &items[0];
    assert!(first["snippets"][0].as_str().unwrap().contains("<mark>"));
    assert!(first["links"]["viewer"]
        .as_str()
        .unwrap()
        .starts_with("https://www.loc.gov/resource/sn99000001/1896-"));
    assert!(first["links"]["viewer"]
        .as_str()
        .unwrap()
        .ends_with("&q=cross+of+gold"));
    assert_eq!(page1["place"]["id"], "P00001");

    let cursor = page1["next_cursor"].as_str().unwrap();
    let (_, _, page2) = get(&s, &format!("{base}&cursor={cursor}")).await;
    let second_first = page2["items"][0]["doc_id"].as_str().unwrap();
    assert!(items.iter().all(|i| i["doc_id"] != second_first));
}

/// A page whose main-index document holds our OCR's Latin text (#203,
/// the snapshot's `ja_latin.json`) is marked as our OCR in hits, and its
/// viewer link has no highlight; the other hits are LoC's as before.
#[tokio::test]
async fn hits_on_our_latin_text_are_marked_as_our_ocr() {
    use sha2::Digest;
    let base = "/v1/hits?q=gold&place=P00001&limit=5";
    let (_, _, before) = get(&state_with(None).await, base).await;
    let ours = before["items"][0]["doc_id"].as_str().unwrap().to_owned();
    let theirs = before["items"][1]["doc_id"].as_str().unwrap().to_owned();

    // The fixture snapshot with `ja_latin.json` listing the first hit.
    let dir = temp_data_dir("ja-latin");
    let body = serde_json::to_vec(&json!({"pages": {&ours: {
        "batch": "batch_fx", "loc_text": "missing", "ocr_source": "usnm-ndlocr-lite",
        "ocr_engine": "ndlocr-lite 636d1cf",
    }}}))
    .unwrap();
    std::fs::write(dir.join("fixture-v1/ja_latin.json"), &body).unwrap();
    let manifest = dir.join("fixture-v1/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let digest: String = sha2::Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    m["files"].as_array_mut().unwrap().push(json!({
        "path": "ja_latin.json", "sha256": digest, "bytes": body.len(),
    }));
    std::fs::write(&manifest, m.to_string()).unwrap();
    let rd = RefData::load(&LocalStore::new(&dir)).await.unwrap();
    assert_eq!(rd.ja_latin.len(), 1);
    let s = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));

    let (status, _, after) = get(&s, base).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(
        after["total"], before["total"],
        "the same pages, counted the same"
    );
    let item = |id: &str| {
        after["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["doc_id"] == id)
            .unwrap()
            .clone()
    };
    let it = item(&ours);
    assert_eq!(
        it["ocr"],
        json!({"source": "usnm-ndlocr-lite", "engine": "ndlocr-lite 636d1cf"})
    );
    assert!(!it["links"]["viewer"].as_str().unwrap().contains("&q="));
    let it = item(&theirs);
    assert!(it.get("ocr").is_none());
    assert!(it["links"]["viewer"].as_str().unwrap().ends_with("&q=gold"));

    // When the version hides our copy (another batch's copy of the page
    // wins), its hits are that copy's, not ours.
    let hidden = serde_json::to_vec(&json!([{"doc_id": &ours, "batch": "batch_fx"}])).unwrap();
    std::fs::write(dir.join("fixture-v1/duplicates.json"), &hidden).unwrap();
    let digest: String = sha2::Sha256::digest(&hidden)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let files = m["files"].as_array_mut().unwrap();
    files.retain(|f| f["path"] != "duplicates.json");
    files.push(json!({"path": "duplicates.json", "sha256": digest, "bytes": hidden.len()}));
    std::fs::write(&manifest, m.to_string()).unwrap();
    let rd = RefData::load(&LocalStore::new(&dir)).await.unwrap();
    assert!(rd.ja_latin.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A page whose main-index document holds our own OCR in place of LoC's
/// text (12 §12.5 Step 9, the snapshot's `ocr_local_pages.bin`) is marked
/// as our OCR in hits, with its engine and a viewer link without
/// highlights; the other hits are LoC's as before. A copy the version hides
/// isn't marked.
#[tokio::test]
async fn hits_on_our_ocr_in_place_of_locs_are_marked_as_our_ocr() {
    use sha2::Digest;
    use usnm_state::ocr_local::{Builder, Engine, OCR_LOCAL_PAGES_FILE};
    let base = "/v1/hits?q=gold&place=P00001&limit=5";
    let (_, _, before) = get(&state_with(None).await, base).await;
    let ours = before["items"][0]["doc_id"].as_str().unwrap().to_owned();
    let theirs = before["items"][1]["doc_id"].as_str().unwrap().to_owned();

    let dir = temp_data_dir("ocr-local");
    let mut pages = Builder::default();
    pages
        .add(
            &ours,
            Engine {
                source: "usnm-tesseract".into(),
                engine: "tesseract/5.5.1 eng".into(),
            },
            1,
        )
        .unwrap();
    let body = pages.finish().encode().unwrap();
    std::fs::write(dir.join("fixture-v1").join(OCR_LOCAL_PAGES_FILE), &body).unwrap();
    let manifest = dir.join("fixture-v1/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let digest = |b: &[u8]| -> String {
        sha2::Sha256::digest(b)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    };
    m["files"].as_array_mut().unwrap().push(json!({
        "path": OCR_LOCAL_PAGES_FILE, "sha256": digest(&body), "bytes": body.len(),
    }));
    // The file names batches by their place in the version's list.
    m["built_from"] = json!({"batches": [
        {"batch": "batch_fx_early", "version": 1, "counts": "x"},
        {"batch": "batch_fx_late", "version": 1, "counts": "y"},
    ], "american_stories": {
        "version": 1, "years": 2, "parts": 5, "bytes": 100, "docs": 40, "only_american_stories": 3,
    }});
    std::fs::write(&manifest, m.to_string()).unwrap();
    let rd = RefData::load(&LocalStore::new(&dir)).await.unwrap();
    assert_eq!(rd.ocr_local.len(), 1);
    // The manifest's American Stories summary is kept for the status page.
    assert_eq!(
        rd.american_stories_built,
        Some(usnm_api::refdata::AmericanStoriesBuilt {
            years: 2,
            docs: 40,
            only_american_stories: 3,
        })
    );
    let s = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));

    let (status, _, after) = get(&s, base).await;
    assert_eq!(status, StatusCode::OK, "{after}");
    assert_eq!(after["total"], before["total"]);
    let item = |id: &str| {
        after["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["doc_id"] == id)
            .unwrap()
            .clone()
    };
    let it = item(&ours);
    assert_eq!(
        it["ocr"],
        json!({"source": "usnm-tesseract", "engine": "tesseract/5.5.1 eng"})
    );
    assert!(!it["links"]["viewer"].as_str().unwrap().contains("&q="));
    let it = item(&theirs);
    assert!(it.get("ocr").is_none());
    assert!(it["links"]["viewer"].as_str().unwrap().ends_with("&q=gold"));

    // Another batch's copy hidden changes nothing; ours hidden drops it.
    for (batch, kept) in [("batch_fx_early", 1), ("batch_fx_late", 0)] {
        let hidden = serde_json::to_vec(&json!([{"doc_id": &ours, "batch": batch}])).unwrap();
        std::fs::write(dir.join("fixture-v1/duplicates.json"), &hidden).unwrap();
        let files = m["files"].as_array_mut().unwrap();
        files.retain(|f| f["path"] != "duplicates.json");
        files.push(json!({
            "path": "duplicates.json", "sha256": digest(&hidden), "bytes": hidden.len(),
        }));
        std::fs::write(&manifest, m.to_string()).unwrap();
        let rd = RefData::load(&LocalStore::new(&dir)).await.unwrap();
        assert_eq!(rd.ocr_local.len(), kept, "{batch}");
    }

    // A file naming a batch the version doesn't have is refused.
    m["built_from"] = json!({"batches": [{"batch": "batch_fx_early", "version": 1}]});
    std::fs::write(&manifest, m.to_string()).unwrap();
    assert!(RefData::load(&LocalStore::new(&dir)).await.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The newest-first list is the oldest-first list reversed, including the
/// order of pages on the same day. `sort=oldest` is the default and shares its
/// canonical URL; anything else is refused.
#[tokio::test]
async fn hits_sort_oldest_or_newest() {
    let s = state_with(None).await;
    let base = "/v1/hits?q=gold&from=1895-01-01&to=1897-12-31&place=P00001";
    // Every page of the list, following the cursor.
    let all = |sort: &'static str| {
        let s = &s;
        async move {
            let mut ids = Vec::new();
            let mut cursor: Option<String> = None;
            loop {
                let mut uri = format!("{base}{sort}");
                if let Some(c) = &cursor {
                    uri.push_str(&format!("&cursor={c}"));
                }
                let (status, _, body) = get(s, &uri).await;
                assert_eq!(status, StatusCode::OK, "{body}");
                for i in body["items"].as_array().unwrap() {
                    ids.push(i["doc_id"].as_str().unwrap().to_owned());
                }
                match body["next_cursor"].as_str() {
                    Some(c) => cursor = Some(c.to_owned()),
                    None => break ids,
                }
            }
        }
    };
    let oldest = all("").await;
    assert!(oldest.len() > 1);
    assert_eq!(all("&sort=oldest").await, oldest);
    let mut reversed = oldest.clone();
    reversed.reverse();
    assert_eq!(all("&sort=newest").await, reversed);
    // Most mentions first (#126): the same pages in another order.
    let mut relevant = all("&sort=relevant").await;
    relevant.sort();
    let mut sorted = oldest.clone();
    sorted.sort();
    assert_eq!(relevant, sorted);

    let (_, plain, _) = get(&s, base).await;
    let (_, explicit, _) = get(&s, &format!("{base}&sort=oldest")).await;
    let (_, newest, _) = get(&s, &format!("{base}&sort=newest")).await;
    assert_eq!(
        header_str(&explicit, header::CONTENT_LOCATION),
        header_str(&plain, header::CONTENT_LOCATION)
    );
    assert!(header_str(&newest, header::CONTENT_LOCATION).contains("&sort=newest"));
    let (_, relevant, _) = get(&s, &format!("{base}&sort=relevant")).await;
    assert!(header_str(&relevant, header::CONTENT_LOCATION).contains("&sort=relevant"));

    let (status, _, body) = get(&s, &format!("{base}&sort=sideways")).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/bad-parameter");
}

#[tokio::test]
async fn hits_by_title_uses_lccn() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/v1/hits?q=gold&lccn=sn99000002&limit=3").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["title"]["lccn"], "sn99000002");
    assert!(body["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["lccn"] == "sn99000002"));
    let (status, _, _) = get(&s, "/v1/hits?q=gold&lccn=sn99000001,sn99000002").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&s, "/v1/hits?q=gold&lccn=sn99999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // With `place`, `lccn` is just a filter.
    let (status, _, body) = get(&s, "/v1/hits?q=gold&place=P00001&lccn=sn99000002").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["total"], 0);
}

async fn browsing_state(on: bool) -> Arc<AppState> {
    let mut cfg = config();
    cfg.browse_pages = on;
    Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ))
}

/// A title's pages without a query (#274): only with `USNM_BROWSE_PAGES`,
/// which `/v1/meta` reports; every page of the one title, by date, each
/// with its opening and an unhighlighted viewer link.
#[tokio::test]
async fn a_titles_pages_are_listed_without_a_query_only_when_switched_on() {
    let off = state_with(None).await;
    let (_, _, meta) = get(&off, "/v1/meta").await;
    assert_eq!(meta["browse_pages"], false);
    let (status, _, body) = get(&off, "/v1/hits?lccn=sn99000002").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body["detail"].as_str().unwrap().contains("`q`"), "{body}");

    let s = browsing_state(true).await;
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(meta["browse_pages"], true);
    let mut days: Vec<u32> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.lccn == "sn99000002")
        .map(|d| d.day)
        .collect();
    days.sort_unstable();
    let (status, _, body) = get(&s, "/v1/hits?lccn=sn99000002&limit=5").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["total"], days.len());
    assert_eq!(body["title"]["lccn"], "sn99000002");
    assert_eq!(body["place"], Value::Null);
    assert!(body["days"].as_u64().unwrap() > 0);
    let items = body["items"].as_array().unwrap();
    assert_eq!(items.len(), 5);
    let first = date_from_day(days[0]).to_string();
    assert_eq!(items[0]["date"], first.as_str());
    for i in items {
        assert_eq!(i["lccn"], "sn99000002");
        let snippets = i["snippets"].as_array().unwrap();
        assert_eq!(snippets.len(), 1, "{i}");
        assert!(!snippets[0].as_str().unwrap().contains("<mark>"), "{i}");
        assert!(i.get("matched_in").is_none());
        let viewer = i["links"]["viewer"].as_str().unwrap();
        assert!(!viewer.contains("&q="), "{viewer}");
    }
    // The next page carries on from the cursor, and newest first ends where
    // oldest first began.
    let cursor = body["next_cursor"].as_str().unwrap();
    let (status, _, next) = get(
        &s,
        &format!("/v1/hits?lccn=sn99000002&limit=5&cursor={cursor}"),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(next.get("days").is_none());
    let last = date_from_day(*days.last().unwrap()).to_string();
    let (_, _, newest) = get(&s, "/v1/hits?lccn=sn99000002&limit=1&sort=newest").await;
    assert_eq!(newest["items"][0]["date"], last.as_str());
    // Dates and the other filters narrow the list.
    let (_, _, narrowed) = get(
        &s,
        "/v1/hits?lccn=sn99000002&from=1896-01-01&to=1896-12-31&front=true&limit=50",
    )
    .await;
    let n = narrowed["total"].as_u64().unwrap();
    assert!(n > 0 && n < days.len() as u64, "{n}");
    assert!(narrowed["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|i| i["front_page"] == true && i["date"].as_str().unwrap().starts_with("1896")));

    // One title, no place, no relevance, nothing that needs a query.
    for uri in [
        "/v1/hits",
        "/v1/hits?lccn=sn99000001,sn99000002",
        "/v1/hits?lccn=sn99000002&place=P00002",
        "/v1/hits?place=P00002",
        "/v1/hits?lccn=sn99000002&sort=relevant",
        "/v1/hits?lccn=sn99000002&mode=all",
        "/v1/hits?lccn=sn99000002&bucket=year",
        "/v1/hits?lccn=sn99000002&source=congress",
    ] {
        let (status, _, body) = get(&s, uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
    let (status, _, _) = get(&s, "/v1/hits?lccn=sn99999999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get(&s, "/v1/hits?lccn=sn99999999&v=fixture-v1").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    // A stale `v` is redirected before the title is looked up: a title of
    // an older version may be gone from this one, and the client learns of
    // the new version from the redirect, not from a 404 at the old URL.
    let (status, headers, body) = get(&s, "/v1/hits?lccn=sn99999999&v=old-version").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{body}");
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    let location = header_str(&headers, header::LOCATION);
    assert!(
        location.contains("lccn=sn99999999") && location.ends_with("&v=fixture-v1"),
        "{location}"
    );
    // A search with `q` answers as before, under a key of its own.
    let (status, _, searched) = get(&s, "/v1/hits?q=gold&lccn=sn99000002&limit=5").await;
    assert_eq!(status, StatusCode::OK);
    assert!(searched["total"].as_u64().unwrap() < days.len() as u64);
    // A stale `v` is redirected to the current version's canonical URL.
    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .uri("/v1/hits?lccn=sn99000002&v=old-version")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::TEMPORARY_REDIRECT);
    let location = header_str(resp.headers(), header::LOCATION);
    assert!(
        location.contains("lccn=sn99000002") && !location.contains("q="),
        "{location}"
    );
}

/// A title's pages without a query go on to the last one however deep
/// (#274): their cursor is the last page's key, not an offset, so the
/// offset cap that stops a search's "next" doesn't apply. With the cap
/// lowered to a few pages, walking the cursors lists every page of the
/// title once, in order, same-day pages included, in both orders, while a
/// search of the same title stops at the cap.
#[tokio::test]
async fn browsing_a_titles_pages_reaches_the_last_page_past_the_offset_cap() {
    let mut cfg = config();
    cfg.browse_pages = true;
    cfg.max_offset = 4;
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let mut docs: Vec<PageDoc> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.lccn == "sn99000002")
        .collect();
    docs.sort_by_key(|d| (d.day, d.sort_key));
    let oldest: Vec<String> = docs.iter().map(|d| d.doc_id.clone()).collect();
    assert!(oldest.len() > 4 + 3, "too few pages to pass the cap");
    assert!(
        docs.windows(2).any(|w| w[0].day == w[1].day),
        "no two pages on one day"
    );
    let mut newest = oldest.clone();
    newest.reverse();
    for (sort, want) in [("oldest", &oldest), ("newest", &newest)] {
        let mut walked: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..=want.len() {
            let uri = match &cursor {
                None => format!("/v1/hits?lccn=sn99000002&limit=3&sort={sort}"),
                Some(c) => format!("/v1/hits?lccn=sn99000002&limit=3&sort={sort}&cursor={c}"),
            };
            let (status, _, body) = get(&s, &uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
            // The pages from this one on, as the backend counts them.
            assert_eq!(body["total"], want.len() - walked.len(), "{uri}");
            walked.extend(
                body["items"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|i| i["doc_id"].as_str().unwrap().to_owned()),
            );
            match body["next_cursor"].as_str() {
                Some(c) => cursor = Some(c.to_owned()),
                None => break,
            }
        }
        assert_eq!(&walked, want, "{sort}");
    }

    // A search of the same title stops at the cap.
    let (_, _, searched) = get(&s, "/v1/hits?q=the&lccn=sn99000002&limit=3").await;
    assert!(
        searched["total"].as_u64().unwrap() > 6,
        "{}",
        searched["total"]
    );
    let search_cursor = searched["next_cursor"].as_str().unwrap().to_owned();
    let (_, _, deeper) = get(
        &s,
        &format!("/v1/hits?q=the&lccn=sn99000002&limit=3&cursor={search_cursor}"),
    )
    .await;
    assert_eq!(deeper["next_cursor"], Value::Null, "{deeper}");

    // A cursor that isn't this list's, in this order, is refused.
    let (_, _, first) = get(&s, "/v1/hits?lccn=sn99000002&limit=3").await;
    let browse_cursor = first["next_cursor"].as_str().unwrap().to_owned();
    // A cursor carries no count a client could set: one with `n` is refused.
    let decoded: Value = serde_json::from_slice(
        &base64::Engine::decode(
            &base64::engine::general_purpose::URL_SAFE_NO_PAD,
            &browse_cursor,
        )
        .unwrap(),
    )
    .unwrap();
    let mut forged = decoded.clone();
    forged["n"] = json!(u64::MAX);
    let forged = base64::Engine::encode(
        &base64::engine::general_purpose::URL_SAFE_NO_PAD,
        serde_json::to_vec(&forged).unwrap(),
    );
    for uri in [
        format!("/v1/hits?lccn=sn99000002&cursor={forged}"),
        "/v1/hits?lccn=sn99000002&cursor=not-a-cursor".to_owned(),
        format!("/v1/hits?lccn=sn99000002&cursor={search_cursor}"),
        format!("/v1/hits?lccn=sn99000002&sort=newest&cursor={browse_cursor}"),
        format!("/v1/hits?q=the&lccn=sn99000002&cursor={browse_cursor}"),
    ] {
        let (status, _, body) = get(&s, &uri).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{uri}: {body}");
    }
}
