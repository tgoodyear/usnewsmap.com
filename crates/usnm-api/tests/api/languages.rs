//! The `lang` filter by title language and its baselines, and Japanese queries.

use std::sync::Arc;

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use usnm_api::AppState;
use usnm_store::LocalStore;

use crate::common::{
    config, fixture_backend, get, header_str, ja_state, load_docs, refdata, state_with, FIXTURE,
};

/// `lang` keeps the pages of titles that list any of the given languages; a
/// title in two languages is found by either.
#[tokio::test]
async fn lang_filters_by_any_title_language() {
    let s = state_with(None).await;
    let places = |lang: &str| {
        let s = &s;
        let lang = lang.to_owned();
        async move {
            let uri = format!("/v1/aggregate?q=%22cross+of+gold%22&lang={lang}");
            let (status, _, body) = get(s, &uri).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let ids: Vec<String> = body["places"]["id"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap().to_owned())
                .collect();
            (ids, body)
        }
    };
    let (ger, body) = places("ger").await;
    assert_eq!(ger, ["P00002", "P00006"]);
    assert!(body["query"]["canonical"]
        .as_str()
        .unwrap()
        .contains("lang=ger&"));
    let (eng, _) = places("eng").await;
    assert_eq!(eng, ["P00001", "P00002", "P00004", "P00005"]);
    // Any of the languages, in canonical (sorted) order.
    let (both, body) = places("SPA,ger").await;
    assert_eq!(both, ["P00002", "P00003", "P00006"]);
    assert!(body["query"]["canonical"]
        .as_str()
        .unwrap()
        .contains("lang=ger%2Cspa&"));
    let (none, body) = places("fre").await;
    assert!(none.is_empty());
    assert_eq!(body["total"]["hits"], 0);
}

/// Under a language filter the baseline is the pages of the titles that list
/// any of the languages, each page once: the title in English and German is in
/// both counts but not twice in their union. Every fixture title has 312 pages.
#[tokio::test]
async fn lang_filter_keeps_exact_baselines() {
    let s = state_with(None).await;
    let baseline = |lang: &'static str, extra: &'static str| {
        let s = &s;
        async move {
            let uri = format!("/v1/aggregate?q=gold&lang={lang}{extra}");
            let (status, _, body) = get(s, &uri).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            let series: u64 = body["series"]["baseline"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_u64().unwrap())
                .sum();
            assert_eq!(body["total"]["baseline_pages"], series);
            (series, body)
        }
    };
    assert_eq!(baseline("eng", "").await.0, 4 * 312);
    // P00002 (English and German) and P00006 (German).
    assert_eq!(baseline("ger", "").await.0, 2 * 312);
    assert_eq!(baseline("spa", "").await.0, 312);
    // Together: P00006 added to the four English titles, P00002 once.
    assert_eq!(baseline("eng,ger", "").await.0, 5 * 312);
    // Lowercased, de-duplicated and ordered like the search itself.
    assert_eq!(baseline("GER,eng,ger", "").await.0, 5 * 312);
    assert_eq!(baseline("fre", "").await.0, 0);
    // With a state filter: only that state's places (P00002 is in New York).
    assert_eq!(baseline("ger", "&state=NY").await.0, 312);

    // The link names the same scope, and the coverage cube sums to the baseline.
    let (pages, body) = baseline("ger", "").await;
    let link = body["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert!(link.contains("lang=ger"), "{link}");
    let (status, _, cov) = get(&s, &link).await;
    assert_eq!(status, StatusCode::OK, "{cov}");
    let covered: u64 = cov["pages"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(covered, pages);
    assert_eq!(cov["places"], json!(["P00002", "P00006"]));
    // Without the filter it is every page.
    let (status, _, all) = get(&s, "/v1/coverage?bucket=year").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(all["places"].as_array().unwrap().len(), 6);
    // Other filters still have no exact baseline, with or without a language.
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold&lang=ger&front=true").await;
    assert!(body["series"]["baseline"].is_null());
    assert!(body["cube"]["baseline_ref"].is_null());
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold&lang=ger&lccn=sn99000002").await;
    assert!(body["series"]["baseline"].is_null());
}

/// Without `lang`, a query whose words are English is compared with the pages
/// of titles that list English (#237), while its hits still count every
/// matching page: the fixture's German (P00006) and Spanish (P00003) titles
/// print the same English text, so they match but aren't in the baseline.
#[tokio::test]
async fn an_english_query_is_compared_with_english_pages() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["baseline"],
        json!({"languages": ["eng"], "why": "query_language"})
    );
    // P00001, P00002 (English and German), P00004 and P00005.
    assert_eq!(body["total"]["baseline_pages"], 4 * 312);
    let series: u64 = body["series"]["baseline"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(series, 4 * 312);
    // The canonical request has no `lang`: the choice follows from the query.
    assert!(!body["query"]["canonical"]
        .as_str()
        .unwrap()
        .contains("lang="));
    let link = body["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert!(link.contains("lang=eng"), "{link}");
    let (status, _, cov) = get(&s, &link).await;
    assert_eq!(status, StatusCode::OK, "{cov}");
    assert_eq!(
        cov["places"],
        json!(["P00001", "P00002", "P00004", "P00005"])
    );
    // The hits are not filtered: the German and Spanish titles' matches count.
    let ids = body["places"]["id"].as_array().unwrap();
    assert!(ids.contains(&json!("P00003")) && ids.contains(&json!("P00006")));
    let (_, _, filtered) = get(&s, "/v1/aggregate?q=gold&lang=eng").await;
    assert!(
        body["total"]["hits"].as_u64().unwrap() > filtered["total"]["hits"].as_u64().unwrap(),
        "{body}"
    );
    // A phrase of English words too.
    let (_, _, body) = get(&s, "/v1/aggregate?q=%22cross+of+gold%22").await;
    assert_eq!(body["baseline"]["languages"], json!(["eng"]));
}

/// An explicit `lang` decides the baseline, whatever the query's words.
#[tokio::test]
async fn a_language_filter_still_decides_the_baseline() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold&lang=ger").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["baseline"],
        json!({"languages": ["ger"], "why": "filter"})
    );
    assert_eq!(body["total"]["baseline_pages"], 2 * 312);
    let (_, _, body) = get(&s, "/v1/aggregate?q=convention&lang=eng,spa").await;
    assert_eq!(
        body["baseline"],
        json!({"languages": ["eng", "spa"], "why": "filter"})
    );
    assert_eq!(body["total"]["baseline_pages"], 5 * 312);
}

/// A query the word lists can't call English keeps every page as its
/// baseline: "convention" is as common in French, and a prefix matches
/// words the lists can't vouch for.
#[tokio::test]
async fn a_query_of_uncertain_language_keeps_every_page() {
    let s = state_with(None).await;
    for q in ["convention", "silve*", "convention+OR+gold"] {
        let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert!(body["total"]["hits"].as_u64().unwrap() > 0, "{q}");
        assert_eq!(
            body["baseline"],
            json!({"languages": [], "why": "all"}),
            "{q}"
        );
        assert_eq!(body["total"]["baseline_pages"], 6 * 312, "{q}");
        let link = body["cube"]["baseline_ref"].as_str().unwrap();
        assert!(!link.contains("lang="), "{link}");
    }
    // Filters without exact baselines have no baseline to describe.
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold&front=true").await;
    assert!(body["baseline"].is_null());
    assert!(body["series"]["baseline"].is_null());
}

/// A version published before baselines were kept per language serves the
/// language filter without them, as it always did.
#[tokio::test]
async fn lang_filter_on_an_older_snapshot_has_no_baseline() {
    let mut refdata = refdata().await;
    refdata.language_baselines = None;
    let s = Arc::new(AppState::new(
        config(),
        Arc::new(fixture_backend()),
        refdata,
    ));
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold&lang=ger").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["series"]["baseline"].is_null());
    assert!(body["cube"]["baseline_ref"].is_null());
    assert!(body["total"]["baseline_pages"].is_null());
    let (status, _, _) = get(&s, "/v1/coverage?lang=ger").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // A request for another version is redirected, not rejected.
    let (status, headers, _) = get(&s, "/v1/coverage?lang=ger&v=pages-v-other").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    // Searches without a language filter are unaffected: an English query
    // keeps every page as its baseline, as before #237.
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold").await;
    let link = body["cube"]["baseline_ref"].as_str().unwrap();
    assert!(!link.contains("lang="), "{link}");
    assert_eq!(body["baseline"], json!({"languages": [], "why": "all"}));
    assert_eq!(body["total"]["baseline_pages"], 6 * 312);
}

#[tokio::test]
async fn japanese_queries_search_the_japanese_pages() {
    let s = ja_state().await;
    let docs = load_docs("pages-ja-fixture");
    let printed_has = |w: &str| {
        let q = vec![usnm_core::ja::tokenize(w, FIXTURE)];
        docs.iter()
            .filter(|d| !usnm_core::ja::find(d.printed.as_deref().unwrap(), &q, FIXTURE).is_empty())
            .count() as u64
    };
    let war = printed_has("戰爭");
    assert!(war > 0);
    // Modern and printed forms are one search, on the Japanese pages only.
    for q in ["%E6%88%A6%E4%BA%89", "%E6%88%B0%E7%88%AD"] {
        let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        assert_eq!(body["total"]["hits"], war, "{body}");
        assert_eq!(body["places"]["id"], json!(["P00003"]));
        // The relative rate compares with pages of titles that list Japanese.
        let r = body["cube"]["baseline_ref"].as_str().unwrap();
        assert!(r.contains("lang=jpn"), "{r}");
        assert_eq!(
            body["baseline"],
            json!({"languages": ["jpn"], "why": "query_language"})
        );
    }
    // Hits: our OCR is marked, snippets show the printed form, and the LoC
    // viewer link has no highlight (LoC has no text for these pages).
    let (status, _, body) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ja"]["indexes"], json!(["pages-ja-fixture"]));
    assert_eq!(body["ja"]["pages"], 52);
    let (status, _, body) = get(&s, "/v1/hits?q=%E6%88%A6%E4%BA%89&place=P00003").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert_eq!(body["total"], war);
    assert_eq!(
        items.len() as u64,
        war.min(50),
        "a first page of the matches"
    );
    for it in items {
        assert_eq!(it["ocr"]["source"], "usnm-ndlocr-lite");
        assert_eq!(it["ocr"]["engine"], "ndlocr-lite 636d1cf");
        assert_eq!(it["title"], "Fixture Shimpo (Japanese)");
        assert!(it["snippets"][0]
            .as_str()
            .unwrap()
            .contains("<mark>戰爭</mark>"));
        assert!(!it["links"]["viewer"].as_str().unwrap().contains("&q="));
    }
    // A Latin query is untouched: the main indexes, LoC's text, highlighted viewer links.
    let (_, _, body) = get(&s, "/v1/hits?q=gold&place=P00001").await;
    let it = &body["items"][0];
    assert!(it.get("ocr").is_none());
    assert!(it["links"]["viewer"].as_str().unwrap().contains("&q=gold"));
}

#[tokio::test]
async fn a_japanese_query_on_a_version_without_japanese_pages_is_refused() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/v1/aggregate?q=%E6%97%A5%E6%9C%AC").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    let (status, _, _) = get(&s, "/v1/hits?q=%E6%97%A5%E6%9C%AC&place=P00003").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    // Latin searches work as before.
    let (status, _, _) = get(&s, "/v1/aggregate?q=gold").await;
    assert_eq!(status, StatusCode::OK);
}

/// `current.json`'s Japanese index ids are checked like the main ones (#139).
#[tokio::test]
async fn current_json_japanese_index_ids_are_validated() {
    use usnm_store::ObjectStore as _;
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStore::new(dir.path());
    let base = json!({
        "index_version": "v1", "indexes": ["pages-base-1"], "reference": "v1",
        "bounds": {"from": "1895-01-01", "to": "1897-12-31"}, "published_at": "2026-10-05T00:00:00Z",
    });
    let put = |v: Value| {
        let store = &store;
        async move {
            store
                .put(
                    "current.json",
                    serde_json::to_vec(&v).unwrap(),
                    "application/json",
                )
                .await
                .unwrap();
        }
    };
    let mut ok = base.clone();
    ok["ja"] = json!({"indexes": ["pages-ja-1"], "fold": 1, "pages": 3});
    put(ok).await;
    assert!(usnm_api::refdata::read_current(&store).await.is_ok());
    for bad in [json!(["../escape"]), json!(["a/b"]), json!([])] {
        let mut v = base.clone();
        v["ja"] = json!({"indexes": bad, "fold": 1});
        put(v).await;
        assert!(
            usnm_api::refdata::read_current(&store).await.is_err(),
            "{bad}"
        );
    }
}
