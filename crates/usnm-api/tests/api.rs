//! End-to-end tests of the API against the synthetic fixture corpus.

use std::collections::{BTreeMap, HashMap};
use std::io::BufRead;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::{app, reload_if_changed, AppState, Engine, Loader};
use usnm_core::query::parse;
use usnm_core::text::tokenize;
use usnm_core::time::{date_from_day, day_number};
use usnm_search::memory::{eval, MemoryBackend};
use usnm_search::PageDoc;
use usnm_store::LocalStore;

fn data_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

fn load_docs(index_id: &str) -> Vec<PageDoc> {
    let path = data_dir().join("indexes").join(format!("{index_id}.jsonl"));
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

fn config() -> Config {
    let mut c = Config::from_lookup(|_| None).unwrap();
    c.data_dir = data_dir();
    c.reference_url = data_dir().display().to_string();
    c.cache_bytes = 16 * 1024 * 1024;
    // Tests without a client address share one bucket; rate limiting has its own test.
    c.rate_limit = None;
    c
}

async fn refdata() -> RefData {
    RefData::load(&LocalStore::new(data_dir())).await.unwrap()
}

fn fixture_backend() -> MemoryBackend {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        backend.add_index(id, load_docs(id));
    }
    backend
}

async fn state_with_cells(max_cells: usize) -> Arc<AppState> {
    let mut cfg = config();
    cfg.max_cells = max_cells;
    Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ))
}

async fn state_with(indexes: Option<Vec<String>>) -> Arc<AppState> {
    let backend = fixture_backend();
    let mut refdata = refdata().await;
    if let Some(ids) = indexes {
        refdata.current.indexes = ids;
    }
    Arc::new(AppState::new(config(), Arc::new(backend), refdata))
}

async fn get(state: &Arc<AppState>, uri: &str) -> (StatusCode, axum::http::HeaderMap, Value) {
    let resp = app(state.clone())
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, body)
}

fn header_str(h: &axum::http::HeaderMap, name: header::HeaderName) -> &str {
    h.get(name).and_then(|v| v.to_str().ok()).unwrap_or("")
}

/// Independent brute-force count over the fixture files.
fn oracle_count(q: &str, from: &str, to: &str, indexes: &[&str]) -> u64 {
    let node = parse(q).unwrap();
    let (f, t) = (
        day_number(chrono::NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap()),
        day_number(chrono::NaiveDate::parse_from_str(to, "%Y-%m-%d").unwrap()),
    );
    indexes
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text)))
        .count() as u64
}

/// American Stories' text (05 §5.5.4) is searched only for a version built
/// with it, and a hit says when its snippets come from it.
#[tokio::test]
async fn american_stories_text_is_searched_only_for_a_version_built_with_it() {
    let off = state_with(None).await;
    let mut rd = refdata().await;
    rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
    let on = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));
    let hits = |s: &Arc<AppState>, q: &'static str| {
        let s = s.clone();
        async move {
            let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}")).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            body["total"]["hits"].as_u64().unwrap()
        }
    };
    assert_eq!(hits(&off, "bimetallism").await, 0);
    assert!(hits(&on, "bimetallism").await > 0);
    assert!(hits(&on, "gold").await > hits(&off, "gold").await);

    let (status, _, body) = get(&on, "/v1/hits?q=bimetallism&place=P00001").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for item in items {
        assert_eq!(item["snippet_source"], "american_stories", "{item}");
        assert!(item["snippets"][0].as_str().unwrap().contains("<mark>"));
    }
    // Snippets from LoC's text say nothing about their source, and without
    // the flag no snippet comes from American Stories' text.
    for s in [&on, &off] {
        let (_, _, body) = get(s, "/v1/hits?q=council&place=P00001&limit=50").await;
        let items = body["items"].as_array().unwrap();
        assert!(items.iter().any(|i| i.get("snippet_source").is_none()));
    }
    let (_, _, body) = get(&off, "/v1/hits?q=council&place=P00001&limit=50").await;
    let items = body["items"].as_array().unwrap();
    assert!(items.iter().all(|i| i.get("snippet_source").is_none()));
}

/// With American Stories' text searched, each hit says which texts the
/// query matches, and the aggregate counts the pages only that text
/// matches; without it, neither key is there (05 §5.5.4).
#[tokio::test]
async fn hits_say_which_texts_match_and_the_aggregate_counts_american_stories_only() {
    let off = state_with(None).await;
    let mut rd = refdata().await;
    rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
    let on = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));

    // Every page of a place, and the aggregate's count for the same search.
    let (status, _, agg) = get(&on, "/v1/aggregate?q=gold").await;
    assert_eq!(status, StatusCode::OK, "{agg}");
    let only = agg["total"]["american_stories_only"].as_u64().unwrap();
    let total = agg["total"]["hits"].as_u64().unwrap();
    assert!(0 < only && only < total, "{only} of {total}");
    for edge in ["first", "last"] {
        assert!(agg["total"][edge]["matched_in"].is_array(), "{edge}");
    }
    let mut counted = BTreeMap::<String, u64>::new();
    for place in agg["places"]["id"].as_array().unwrap() {
        let mut cursor = String::new();
        loop {
            let uri = format!(
                "/v1/hits?q=gold&place={}&limit=50{cursor}",
                place.as_str().unwrap()
            );
            let (status, _, body) = get(&on, &uri).await;
            assert_eq!(status, StatusCode::OK, "{body}");
            for item in body["items"].as_array().unwrap() {
                let texts: Vec<&str> = item["matched_in"]
                    .as_array()
                    .unwrap_or_else(|| panic!("{item}"))
                    .iter()
                    .map(|t| t.as_str().unwrap())
                    .collect();
                *counted.entry(texts.join("+")).or_default() += 1;
                // Snippets from American Stories' text only when LoC's has no match.
                if item["snippet_source"] == "american_stories" {
                    assert_eq!(texts, ["american_stories"], "{item}");
                }
            }
            match body["next_cursor"].as_str() {
                Some(c) => cursor = format!("&cursor={c}"),
                None => break,
            }
        }
    }
    assert_eq!(counted.values().sum::<u64>(), total);
    assert_eq!(counted.get("american_stories"), Some(&only));
    assert!(counted.get("loc").is_some_and(|n| *n > 0));
    assert!(counted.get("loc+american_stories").is_some_and(|n| *n > 0));

    // A word only American Stories' text has: every page.
    let (_, _, agg) = get(&on, "/v1/aggregate?q=bimetallism").await;
    assert_eq!(agg["total"]["american_stories_only"], agg["total"]["hits"]);

    // Off: the keys aren't there.
    let (_, _, agg) = get(&off, "/v1/aggregate?q=gold").await;
    assert!(agg["total"].get("american_stories_only").is_none());
    assert!(agg["total"]["first"].get("matched_in").is_none());
    let (_, _, body) = get(&off, "/v1/hits?q=gold&place=P00001&limit=50").await;
    let items = body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    assert!(items.iter().all(|i| i.get("matched_in").is_none()));
}

#[tokio::test]
async fn health_and_meta() {
    let s = state_with(None).await;
    let (status, _, _) = get(&s, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
    let (status, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["index_version"], "fixture-v1");
    assert_eq!(meta["synthetic"], true);
    assert_eq!(meta["places"], 6);
    // Every page in the version: the sum of the baselines.
    let baselines: HashMap<String, Vec<(u32, u32)>> = serde_json::from_slice(
        &std::fs::read(data_dir().join("fixture-v1/baselines.json")).unwrap(),
    )
    .unwrap();
    let pages: u64 = baselines
        .values()
        .flatten()
        .map(|&(_, n)| u64::from(n))
        .sum();
    assert!(pages > 0);
    assert_eq!(meta["pages"], pages);
    // The language filter's choices: most pages first; the title in English
    // and German counts in both.
    assert_eq!(
        meta["languages"],
        json!([
            {"code": "eng", "name": "English", "titles": 4, "pages": 1248},
            {"code": "ger", "name": "German", "titles": 2, "pages": 624},
            {"code": "spa", "name": "Spanish", "titles": 1, "pages": 312},
        ])
    );
}

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
    // Searches without a language filter are unaffected.
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold").await;
    assert!(body["cube"]["baseline_ref"].is_string());
}

#[tokio::test]
async fn aggregate_counts_match_brute_force() {
    let s = state_with(None).await;
    let uri = "/v1/aggregate?q=%22cross+of+gold%22&from=1896-06-01&to=1896-12-31";
    let (status, headers, body) = get(&s, uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = oracle_count(
        r#""cross of gold""#,
        "1896-06-01",
        "1896-12-31",
        &["pages-base-fixture", "pages-delta-fixture-1"],
    );
    assert!(expected > 0);
    assert_eq!(body["total"]["hits"], expected);
    assert_eq!(body["bucket"]["unit"], "week");
    let series_sum: u64 = body["series"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    let cube_sum: u64 = body["cube"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    let places_sum: u64 = body["places"]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(series_sum, expected);
    assert_eq!(cube_sum, expected);
    assert_eq!(places_sum, expected);
    assert!(
        body["series"]["baseline"].as_array().unwrap().len()
            == body["bucket"]["count"].as_u64().unwrap() as usize
    );

    // The synthetic speech spreads east before west: P00001 first, P00006 last.
    let ids: Vec<&str> = body["places"]["id"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    let first: Vec<u64> = body["places"]["first_day"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .collect();
    let fd = |id: &str| first[ids.iter().position(|x| *x == id).unwrap()];
    assert!(fd("P00001") < fd("P00003") && fd("P00003") < fd("P00006"));

    // Unversioned: short cache, Content-Location pins the version.
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=300"
    );
    assert!(header_str(&headers, header::CONTENT_LOCATION).ends_with("&v=fixture-v1"));
}

#[tokio::test]
async fn version_pinning_and_redirects() {
    let s = state_with(None).await;
    let (status, headers, _) = get(&s, "/v1/aggregate?q=gold&v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));

    let (status, headers, _) = get(&s, "/v1/aggregate?q=Gold&v=old-version").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    let location = header_str(&headers, header::LOCATION);
    assert!(
        location.starts_with("/v1/aggregate?bucket=week&from=1895-01-01&q=gold&to=1897-12-31"),
        "{location}"
    );
    assert!(location.ends_with("&v=fixture-v1"));
}

#[tokio::test]
async fn index_set_isolation() {
    // A version naming only the base index must not see pages in the delta.
    let full = state_with(None).await;
    let base_only = state_with(Some(vec!["pages-base-fixture".into()])).await;
    let uri = "/v1/aggregate?q=fever&from=1895-01-01&to=1897-12-31";
    let (_, _, a) = get(&full, uri).await;
    let (_, _, b) = get(&base_only, uri).await;
    assert_eq!(
        a["total"]["hits"],
        oracle_count(
            "fever",
            "1895-01-01",
            "1897-12-31",
            &["pages-base-fixture", "pages-delta-fixture-1"]
        )
    );
    assert_eq!(
        b["total"]["hits"],
        oracle_count("fever", "1895-01-01", "1897-12-31", &["pages-base-fixture"])
    );
    assert!(a["total"]["hits"].as_u64() > b["total"]["hits"].as_u64());
}

#[tokio::test]
async fn problems_for_bad_requests() {
    let s = state_with(None).await;
    let (status, headers, body) = get(&s, "/v1/aggregate?q=text:gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/problem+json"
    );
    assert_eq!(body["type"], "/errors/query-syntax");
    assert_eq!(body["position"], 4);
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold&cachebust=1").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/bad-parameter");
    let (status, _, _) = get(&s, "/v1/hits?q=gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&s, "/v1/places?q=gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _, _) = get(&s, "/v1/hits?q=gold&place=P99999").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

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

/// Matching pages per day for some places, against a brute-force pass over
/// the fixture files and the aggregate's per-place totals.
#[tokio::test]
async fn days_per_place_match_brute_force() {
    let s = state_with(None).await;
    let search = "q=%22cross+of+gold%22&from=1896-01-01&to=1896-12-31";
    let (status, headers, body) = get(&s, &format!("/v1/days?{search}&place=P00006,P00001")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let places = body["places"].as_array().unwrap();
    // In the order asked.
    assert_eq!(places.len(), 2);
    assert_eq!(places[0]["id"], "P00006");
    assert_eq!(places[1]["id"], "P00001");

    let node = parse(r#""cross of gold""#).unwrap();
    let (from, to) = (
        day_number(chrono::NaiveDate::from_ymd_opt(1896, 1, 1).unwrap()),
        day_number(chrono::NaiveDate::from_ymd_opt(1896, 12, 31).unwrap()),
    );
    let (_, _, agg) = get(&s, &format!("/v1/aggregate?{search}")).await;
    let agg_ids: Vec<&str> = agg["places"]["id"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    for p in places {
        let id = p["id"].as_str().unwrap();
        let mut expected: BTreeMap<u32, u64> = BTreeMap::new();
        for d in ["pages-base-fixture", "pages-delta-fixture-1"]
            .iter()
            .flat_map(|i| load_docs(i))
            .filter(|d| d.place_id == id && d.day >= from && d.day <= to)
            .filter(|d| eval(&node, &tokenize(&d.text)))
        {
            *expected.entry(d.day).or_default() += 1;
        }
        assert!(!expected.is_empty(), "{id}");
        let days: Vec<u32> = p["days"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap() as u32)
            .collect();
        let hits: Vec<u64> = p["hits"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_u64().unwrap())
            .collect();
        assert_eq!(days, expected.keys().copied().collect::<Vec<_>>(), "{id}");
        assert_eq!(hits, expected.values().copied().collect::<Vec<_>>(), "{id}");
        // Same day numbers and totals as the aggregate's.
        let i = agg_ids.iter().position(|x| *x == id).unwrap();
        assert_eq!(agg["places"]["first_day"][i], days[0]);
        assert_eq!(agg["places"]["last_day"][i], *days.last().unwrap());
        assert_eq!(agg["places"]["hits"][i], hits.iter().sum::<u64>());
    }

    // Unversioned: short cache, Content-Location pins the version, by day
    // whatever `bucket` said, with the places in the order asked.
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=300"
    );
    let location = header_str(&headers, header::CONTENT_LOCATION);
    assert!(
        location.starts_with("/v1/days?bucket=day&from=1896-01-01&q="),
        "{location}"
    );
    assert!(
        location.ends_with("&place=P00006%2CP00001&v=fixture-v1"),
        "{location}"
    );
    let (_, h2, again) = get(
        &s,
        &format!("/v1/days?{search}&bucket=week&place=P00006,P00001"),
    )
    .await;
    assert_eq!(again, body);
    assert_eq!(header_str(&h2, header::CONTENT_LOCATION), location);

    // Pinned: cached for a day; another version redirects to this one.
    let (status, headers, _) =
        get(&s, &format!("/v1/days?{search}&place=P00001&v=fixture-v1")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    let (status, headers, _) = get(&s, &format!("/v1/days?{search}&place=P00001&v=old")).await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    let location = header_str(&headers, header::LOCATION);
    assert!(location.starts_with("/v1/days?bucket=day&"), "{location}");
    assert!(
        location.ends_with("&place=P00001&v=fixture-v1"),
        "{location}"
    );

    // A place without matches is listed with empty lists.
    let (status, _, body) = get(
        &s,
        &format!("/v1/days?{search}&lccn=sn99000001&place=P00002,P00001"),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        body["places"][0],
        json!({"id": "P00002", "days": [], "hits": []})
    );
    assert!(!body["places"][1]["days"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn days_problems_for_bad_requests() {
    let s = state_with(None).await;
    let many: Vec<String> = (1..=21).map(|i| format!("P{i:05}")).collect();
    for (query, status, kind) in [
        ("q=gold", StatusCode::BAD_REQUEST, "/errors/bad-parameter"),
        (
            "q=gold&place=",
            StatusCode::BAD_REQUEST,
            "/errors/bad-parameter",
        ),
        (
            "q=gold&place=P00001,P00002,P00001",
            StatusCode::BAD_REQUEST,
            "/errors/bad-parameter",
        ),
        (
            &format!("q=gold&place={}", many.join(",")),
            StatusCode::BAD_REQUEST,
            "/errors/bad-parameter",
        ),
        (
            "q=gold&place=P00001&limit=5",
            StatusCode::BAD_REQUEST,
            "/errors/bad-parameter",
        ),
        (
            "q=gold&place=P00001&bucket=decade",
            StatusCode::BAD_REQUEST,
            "/errors/bad-parameter",
        ),
        (
            "place=P00001",
            StatusCode::BAD_REQUEST,
            "/errors/bad-parameter",
        ),
        (
            "q=text:gold&place=P00001",
            StatusCode::BAD_REQUEST,
            "/errors/query-syntax",
        ),
        (
            "q=gold&place=P99999",
            StatusCode::NOT_FOUND,
            "/errors/not-found",
        ),
        (
            "q=gold&place=P00001,P%20OR%20x",
            StatusCode::NOT_FOUND,
            "/errors/not-found",
        ),
    ] {
        let (got, headers, body) = get(&s, &format!("/v1/days?{query}")).await;
        assert_eq!(got, status, "{query}: {body}");
        assert_eq!(body["type"], kind, "{query}");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/problem+json"
        );
    }
    // More day cells than the budget → 422, and nothing asked by day.
    let tiny = state_with_cells(5).await;
    let (status, _, body) = get(
        &tiny,
        "/v1/days?q=gold&from=1896-01-01&to=1896-12-31&place=P00001",
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["type"], "/errors/query-too-broad");
    assert!(body["detail"].as_str().unwrap().contains("days"), "{body}");
}

/// Newspapers and languages (#121) against a brute-force pass over the
/// fixture files: counts, order (most first, ties by key), the catalog's
/// names and places, and languages counted once per language a paper lists.
#[tokio::test]
async fn newspapers_and_languages_match_brute_force() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold&from=1895-01-01&to=1897-12-31").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let node = parse("gold").unwrap();
    let (f, t) = (
        day_number(chrono::NaiveDate::from_ymd_opt(1895, 1, 1).unwrap()),
        day_number(chrono::NaiveDate::from_ymd_opt(1897, 12, 31).unwrap()),
    );
    let mut papers: BTreeMap<String, u64> = BTreeMap::new();
    let mut langs: BTreeMap<String, u64> = BTreeMap::new();
    for d in ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text)))
    {
        *papers.entry(d.lccn.clone()).or_default() += 1;
        for l in d.language {
            *langs.entry(l).or_default() += 1;
        }
    }
    let ranked = |m: BTreeMap<String, u64>| {
        let mut v: Vec<(String, u64)> = m.into_iter().collect();
        v.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        v
    };
    let column = |v: &Value| -> Vec<Value> { v.as_array().unwrap().clone() };
    let got_papers: Vec<(String, u64)> = column(&body["papers"]["lccn"])
        .iter()
        .zip(column(&body["papers"]["hits"]))
        .map(|(l, h)| (l.as_str().unwrap().to_owned(), h.as_u64().unwrap()))
        .collect();
    let want_papers = ranked(papers);
    assert!(want_papers.len() > 1);
    assert_eq!(got_papers, want_papers);
    assert_eq!(body["total"]["papers"], want_papers.len());
    let got_langs: Vec<(String, u64)> = column(&body["languages"]["code"])
        .iter()
        .zip(column(&body["languages"]["hits"]))
        .map(|(l, h)| (l.as_str().unwrap().to_owned(), h.as_u64().unwrap()))
        .collect();
    let want_langs = ranked(langs);
    // Multilingual papers count in each of their languages.
    assert!(want_langs.iter().map(|l| l.1).sum::<u64>() > body["total"]["hits"].as_u64().unwrap());
    assert_eq!(got_langs, want_langs);
    // Names and places come from the catalog.
    let titles: Vec<Value> =
        serde_json::from_slice(&std::fs::read(data_dir().join("fixture-v1/titles.json")).unwrap())
            .unwrap();
    for (i, lccn) in column(&body["papers"]["lccn"]).iter().enumerate() {
        let t = titles.iter().find(|t| t["lccn"] == *lccn).unwrap();
        assert_eq!(body["papers"]["title"][i], t["name"]);
        assert_eq!(body["papers"]["place_id"][i], t["place_id"]);
    }
}

/// Distinct days (#127) against a brute-force pass: the whole search on
/// `/v1/aggregate`, one place on the first page of `/v1/hits` (not later ones).
#[tokio::test]
async fn distinct_days_match_brute_force() {
    let s = state_with(None).await;
    let node = parse("gold").unwrap();
    let (f, t) = (
        day_number(chrono::NaiveDate::from_ymd_opt(1895, 1, 1).unwrap()),
        day_number(chrono::NaiveDate::from_ymd_opt(1897, 12, 31).unwrap()),
    );
    let docs: Vec<PageDoc> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|i| load_docs(i))
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text)))
        .collect();
    let days = |place: Option<&str>| {
        docs.iter()
            .filter(|d| place.is_none_or(|p| d.place_id == p))
            .map(|d| d.day)
            .collect::<std::collections::BTreeSet<_>>()
            .len()
    };
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold&from=1895-01-01&to=1897-12-31").await;
    assert!(days(None) > 1);
    assert_eq!(body["total"]["days"], days(None));
    let base = "/v1/hits?q=gold&from=1895-01-01&to=1897-12-31&place=P00001&limit=5";
    let (status, _, first) = get(&s, base).await;
    assert_eq!(status, StatusCode::OK, "{first}");
    assert_eq!(first["days"], days(Some("P00001")));
    let cursor = first["next_cursor"].as_str().unwrap();
    let (_, _, next) = get(&s, &format!("{base}&cursor={cursor}")).await;
    assert!(next.get("days").is_none(), "{next}");
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

/// National and per-place first and last days, and the first and last pages,
/// against a brute-force pass over the fixture files.
#[tokio::test]
async fn first_and_last_mentions_match_brute_force() {
    let s = state_with(None).await;
    let docs: Vec<PageDoc> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|i| load_docs(i))
        .collect();
    let day = |iso: &str| day_number(chrono::NaiveDate::parse_from_str(iso, "%Y-%m-%d").unwrap());
    for (q, qs, from, to) in [
        ("gold", "gold", "1895-01-01", "1897-12-31"),
        (
            r#""cross of gold""#,
            "%22cross+of+gold%22",
            "1896-06-01",
            "1896-12-31",
        ),
        ("fever", "fever", "1896-07-01", "1896-09-30"),
        ("zyzzyva", "zyzzyva", "1895-01-01", "1897-12-31"),
    ] {
        let node = parse(q).unwrap();
        let (f, t) = (day(from), day(to));
        let matching: Vec<&PageDoc> = docs
            .iter()
            .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text)))
            .collect();
        let first = matching.iter().copied().min_by_key(|d| (d.day, d.sort_key));
        let last = matching.iter().copied().max_by_key(|d| (d.day, d.sort_key));
        let mut places: HashMap<&str, (u32, u32)> = HashMap::new();
        for d in &matching {
            let e = places.entry(&d.place_id).or_insert((d.day, d.day));
            e.0 = e.0.min(d.day);
            e.1 = e.1.max(d.day);
        }

        let uri = format!("/v1/aggregate?q={qs}&from={from}&to={to}");
        let (status, _, body) = get(&s, &uri).await;
        assert_eq!(status, StatusCode::OK, "{q}: {body}");
        let total = &body["total"];
        assert_eq!(total["first_day"], json!(first.map(|d| d.day)), "{q}");
        assert_eq!(total["last_day"], json!(last.map(|d| d.day)), "{q}");
        assert_eq!(
            total["first"]["doc_id"],
            json!(first.map(|d| &d.doc_id)),
            "{q}"
        );
        assert_eq!(
            total["last"]["doc_id"],
            json!(last.map(|d| &d.doc_id)),
            "{q}"
        );
        if let Some(d) = first {
            assert_eq!(total["first"]["place_id"], d.place_id.as_str());
            assert_eq!(total["first"]["date"], date_from_day(d.day).to_string());
            assert!(total["first"]["links"]["viewer"].is_string());
        }

        let column =
            |name: &str| -> Vec<Value> { body["places"][name].as_array().unwrap().clone() };
        let ids = column("id");
        assert_eq!(ids.len(), places.len(), "{q}");
        for (i, id) in ids.iter().enumerate() {
            let (lo, hi) = places[id.as_str().unwrap()];
            assert_eq!(column("first_day")[i], lo, "{q} {id}");
            assert_eq!(column("last_day")[i], hi, "{q} {id}");
        }
    }
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

#[tokio::test]
async fn coverage_matches_aggregate_baseline() {
    let s = state_with(None).await;
    let (_, _, agg) = get(
        &s,
        "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31&bucket=month",
    )
    .await;
    let baseline_ref = agg["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert!(baseline_ref.ends_with("&v=fixture-v1"));
    let (status, _, cov) = get(&s, &baseline_ref).await;
    assert_eq!(status, StatusCode::OK, "{cov}");
    let pages: u64 = cov["pages"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(Some(pages), agg["total"]["baseline_pages"].as_u64());
    // Baselines include empty-OCR pages, so they exceed the indexed page count.
    assert!(pages > 0);
}

#[tokio::test]
async fn front_filter_disables_inexact_baseline_and_api_alias_works() {
    let s = state_with(None).await;
    let (status, _, body) = get(&s, "/api/v1/aggregate?q=gold&front=true").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["series"]["baseline"].is_null());
    assert!(body["cube"]["baseline_ref"].is_null());
    // Links built on the /api/v1 mount stay on it.
    let (_, _, body) = get(&s, "/api/v1/aggregate?q=gold").await;
    let link = body["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    assert!(link.starts_with("/api/v1/coverage?"), "{link}");
    let (status, _, _) = get(&s, &link).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn coarsens_before_querying_an_oversized_cube() {
    // 6 places × 366 days and × 53 weeks exceed 240 cells; 6 × 12 months fit.
    let s = state_with_cells(240).await;
    let (status, _, body) = get(
        &s,
        "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31&bucket=day",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["bucket"]["unit"], "month");
    assert_eq!(body["coarsened"], true);
    let cube_sum: u64 = body["cube"]["h"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(cube_sum, body["total"]["hits"].as_u64().unwrap());
    // Even by year the cube would exceed the budget → 422, not a backend failure.
    let tiny = state_with_cells(5).await;
    let (status, _, body) = get(&tiny, "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31").await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["type"], "/errors/query-too-broad");
}

#[tokio::test]
async fn places_geojson_is_version_pinned() {
    let s = state_with(None).await;
    let (status, headers, _) = get(&s, "/v1/places?v=old").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(
        header_str(&headers, header::LOCATION),
        "/v1/places?v=fixture-v1"
    );
    let (status, headers, _) = get(&s, "/v1/places?v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    let (_, headers, body) = get(&s, "/v1/places").await;
    assert_eq!(
        header_str(&headers, header::CONTENT_LOCATION),
        "/v1/places?v=fixture-v1"
    );
    assert_eq!(body["type"], "FeatureCollection");
    assert_eq!(body["features"].as_array().unwrap().len(), 6);
    // Each place lists its titles' languages.
    let langs: Vec<(&str, &Value)> = body["features"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| (f["id"].as_str().unwrap(), &f["properties"]["languages"]))
        .collect();
    assert_eq!(
        langs,
        [
            ("P00001", &json!(["eng"])),
            ("P00002", &json!(["eng", "ger"])),
            ("P00003", &json!(["spa"])),
            ("P00004", &json!(["eng"])),
            ("P00005", &json!(["eng"])),
            ("P00006", &json!(["ger"])),
        ]
    );
    // And how many of its titles list each.
    assert_eq!(
        body["features"][1]["properties"]["language_titles"],
        json!({"eng": 1, "ger": 1})
    );
    assert_eq!(body["features"][1]["properties"]["titles"], 1);
    assert_eq!(body["features"][0]["geometry"]["type"], "Point");
}

/// Copy the fixture data dir to a fresh temp dir the test can modify.
fn temp_data_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    for sub in ["indexes", "fixture-v1"] {
        std::fs::create_dir_all(dir.join(sub)).unwrap();
        for entry in std::fs::read_dir(data_dir().join(sub)).unwrap() {
            let path = entry.unwrap().path();
            std::fs::copy(&path, dir.join(sub).join(path.file_name().unwrap())).unwrap();
        }
    }
    std::fs::copy(data_dir().join("current.json"), dir.join("current.json")).unwrap();
    dir
}

/// Copy a reference snapshot under a new version, as the stats job would publish it.
fn snapshot_copy(dir: &std::path::Path, from: &str, to: &str) {
    std::fs::create_dir_all(dir.join(to)).unwrap();
    for entry in std::fs::read_dir(dir.join(from)).unwrap() {
        let path = entry.unwrap().path();
        std::fs::copy(&path, dir.join(to).join(path.file_name().unwrap())).unwrap();
    }
    let manifest = dir.join(to).join("manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["index_version"] = to.into();
    std::fs::write(manifest, m.to_string()).unwrap();
}

#[tokio::test]
async fn hot_reload_swaps_reference_data_and_backend_together() {
    let dir = temp_data_dir("reload");
    let loader = Loader {
        reference: Arc::new(LocalStore::new(&dir)),
        engine: Engine::Memory {
            indexes_dir: dir.join("indexes"),
        },
    };
    let snapshot = loader.snapshot().await.unwrap();
    let s = Arc::new(AppState::with_loader(config(), snapshot, Some(loader)));
    assert!(
        !reload_if_changed(&s).await.unwrap(),
        "unchanged version must not reload"
    );

    // Publish a new version whose index set adds a delta the old snapshot never loaded.
    std::fs::copy(
        dir.join("indexes/pages-delta-fixture-1.jsonl"),
        dir.join("indexes/pages-delta-fixture-2.jsonl"),
    )
    .unwrap();
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    current["index_version"] = "fixture-v2".into();
    current["indexes"] = serde_json::json!(["pages-base-fixture", "pages-delta-fixture-2"]);

    // A version whose reference snapshot is still v1's is refused.
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();
    let err = reload_if_changed(&s).await.unwrap_err();
    assert!(err.contains("is for `fixture-v1`"), "{err}");
    assert_eq!(s.snapshot.load().refdata.version(), "fixture-v1");

    // With its own snapshot, it publishes.
    snapshot_copy(&dir, "fixture-v1", "fixture-v2");
    current["reference"] = "fixture-v2".into();
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();

    assert!(reload_if_changed(&s).await.unwrap());
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(meta["index_version"], "fixture-v2");
    let (status, _, body) = get(&s, "/v1/aggregate?q=fever").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["index_version"], "fixture-v2");
    assert!(body["total"]["hits"].as_u64().unwrap() > 0);
    let _ = std::fs::remove_dir_all(&dir);
}

fn persisted_files(dir: &std::path::Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir.join("fixture-v1/f6")) else {
        return Vec::new();
    };
    entries
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|x| x == "zst"))
        .collect()
}

#[tokio::test]
async fn slow_responses_persist_and_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("usnm-respcache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalStore::new(&dir));
    let mut cfg = config();
    cfg.persist_after = Duration::ZERO;
    let first = Arc::new(
        AppState::new(cfg.clone(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(store.clone()),
    );
    let uri = "/v1/aggregate?q=fever&v=fixture-v1";
    let (status, _, body) = get(&first, uri).await;
    assert_eq!(status, StatusCode::OK);
    let mut files = Vec::new();
    for _ in 0..100 {
        files = persisted_files(&dir);
        if !files.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(files.len(), 1, "one entry under {{version}}/f6/");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        !name.contains("fever"),
        "search text must not appear in paths"
    );

    // A fresh replica with an empty in-process cache and an empty index still
    // answers from the persisted entry: the body is identical.
    let restarted = Arc::new(
        AppState::new(cfg, Arc::new(MemoryBackend::new()), refdata().await)
            .with_response_store(store),
    );
    let (status, headers, again) = get(&restarted, uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, body);
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));

    // An entry that decompresses but isn't JSON is ignored and recomputed.
    std::fs::write(
        &files[0],
        zstd::encode_all(&b"{\"truncated"[..], 3).unwrap(),
    )
    .unwrap();
    let recomputing = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(Arc::new(LocalStore::new(&dir))),
    );
    let (status, _, mut fresh) = get(&recomputing, uri).await;
    assert_eq!(status, StatusCode::OK);
    // Recomputed, so only its timing may differ from the original.
    let mut original = body.clone();
    for v in [&mut fresh, &mut original] {
        v.as_object_mut().unwrap().remove("timing_ms");
    }
    // Serving the invalid entry would have parsed as `null` here.
    assert_eq!(fresh, original);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fast_responses_are_not_persisted() {
    let dir = std::env::temp_dir().join(format!("usnm-respfast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = config();
    cfg.persist_after = Duration::from_secs(3600);
    let s = Arc::new(
        AppState::new(cfg, Arc::new(fixture_backend()), refdata().await)
            .with_response_store(Arc::new(LocalStore::new(&dir))),
    );
    let (status, _, _) = get(&s, "/v1/aggregate?q=fever&v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(persisted_files(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

async fn get_from(
    state: &Arc<AppState>,
    uri: &str,
    xff: &str,
) -> (StatusCode, axum::http::HeaderMap) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-forwarded-for", xff)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (resp.status(), resp.headers().clone())
}

#[tokio::test]
async fn clients_over_their_rate_get_429() {
    let mut cfg = config();
    cfg.rate_limit = Some(usnm_api::config::RateLimit {
        per_minute: 1.try_into().unwrap(),
        burst: 2.try_into().unwrap(),
    });
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    for _ in 0..2 {
        assert_eq!(
            get_from(&s, "/v1/meta", "203.0.113.7").await.0,
            StatusCode::OK
        );
    }
    let (status, headers) = get_from(&s, "/api/v1/meta", "198.51.100.1, 203.0.113.7").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/problem+json"
    );
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    let retry: u64 = header_str(&headers, header::RETRY_AFTER).parse().unwrap();
    assert!((1..=60).contains(&retry));
    // Other clients and health probes are unaffected.
    assert_eq!(
        get_from(&s, "/v1/meta", "203.0.113.8").await.0,
        StatusCode::OK
    );
    assert_eq!(
        get_from(&s, "/healthz", "203.0.113.7").await.0,
        StatusCode::OK
    );
}

#[tokio::test]
async fn reference_files_must_match_the_manifest() {
    let dir = temp_data_dir("manifest");
    let store = LocalStore::new(&dir);
    assert!(RefData::load(&store).await.is_ok());

    let places = dir.join("fixture-v1/places.json");
    let mut text = std::fs::read_to_string(&places).unwrap();
    text = text.replacen("Fixture", "Fixturf", 1);
    std::fs::write(&places, text).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("manifest"), "{err}");

    // Ids from current.json become object paths, so they must be plain segments.
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(dir.join("current.json")).unwrap()).unwrap();
    current["reference"] = "../fixture-v1".into();
    std::fs::write(dir.join("current.json"), current.to_string()).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("not a valid id"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A built site: an index, a hashed asset with a precompressed copy, a favicon.
/// Each test names its own, since tests run in parallel.
fn site_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-site-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><div id=root></div>").unwrap();
    std::fs::write(dir.join("assets/index-abc.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("assets/index-abc.js.gz"), b"gzipped").unwrap();
    std::fs::write(dir.join("favicon.svg"), "<svg/>").unwrap();
    dir
}

fn site_dir_of(s: &AppState) -> PathBuf {
    s.config.site_dir.clone().unwrap()
}

async fn get_site(
    state: &Arc<AppState>,
    uri: &str,
    encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(e) = encoding {
        req = req.header(header::ACCEPT_ENCODING, e);
    }
    let resp = app(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn serves_the_site_alongside_the_api() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("serve"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));

    // The app shell, revalidated on every load, with the security headers.
    let (status, h, body) = get_site(&s, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("id=root"));
    assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
    assert!(header_str(&h, header::CONTENT_SECURITY_POLICY).contains("connect-src 'self'"));
    assert_eq!(header_str(&h, header::X_CONTENT_TYPE_OPTIONS), "nosniff");

    // The app's pages get the shell too, so a reload keeps working. The file
    // itself is served with or without a trailing slash (web/src/route.ts).
    for uri in ["/status", "/status/", "/index.html", "/index.html/"] {
        let (status, h, body) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(body.contains("id=root"), "{uri}");
        assert!(header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
    }

    // Other paths get the shell, where the app shows a not-found page, with a
    // 404; so do paths with a segment too long to be a file name, which the
    // filesystem would refuse (a 500 from the file service).
    let long = format!("/{}", "a".repeat(300));
    let long_nested = format!("/assets/{}/x", "b".repeat(256));
    for uri in [
        "/search/gold",
        "/does-not-exist",
        "/statuses",
        long.as_str(),
    ] {
        let (status, h, body) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(body.contains("id=root"), "{uri}");
        assert!(header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache", "{uri}");
    }
    let (status, _, _) = get_site(&s, &long_nested, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a long name under /assets/ is a plain 404"
    );
    // The limit is on the decoded name, as the file service decodes it: a
    // 100-byte file name sent as 300 bytes of escapes is still served.
    let name = format!("{}.txt", "d".repeat(96));
    std::fs::write(site_dir_of(&s).join(&name), "found").unwrap();
    let escaped: String = name.bytes().map(|b| format!("%{b:02X}")).collect();
    assert!(escaped.len() > 255);
    let (status, _, body) = get_site(&s, &format!("/{escaped}"), None).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "found"));
    // 253 bytes fits NAME_MAX, but `ServeDir` would also probe the 256-byte
    // `.gz` name for a client that accepts gzip.
    let (status, _, _) = get_site(&s, &format!("/{}.js", "e".repeat(250)), Some("gzip")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a name that can't take .gz is a plain 404"
    );
    let (status, _, _) = get_site(&s, &format!("/{}.js", "c".repeat(400)), None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a long file name is a plain 404"
    );

    // HEAD on an app route: GET's headers and status, no body.
    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/status")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let len = std::fs::metadata(s.config.site_dir.as_ref().unwrap().join("index.html"))
        .unwrap()
        .len();
    assert_eq!(
        header_str(resp.headers(), header::CONTENT_LENGTH),
        len.to_string()
    );
    assert!(header_str(resp.headers(), header::CONTENT_TYPE).starts_with("text/html"));
    assert!(resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());

    // Hashed assets: cached for a year, the precompressed copy when accepted.
    let (status, h, body) = get_site(&s, "/assets/index-abc.js", Some("gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "gzipped");
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "gzip");
    assert_eq!(
        header_str(&h, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    let (_, h, body) = get_site(&s, "/assets/index-abc.js", None).await;
    assert_eq!(body, "console.log(1)");
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "");

    let (status, h, _) = get_site(&s, "/favicon.svg", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&h, header::CACHE_CONTROL),
        "public, max-age=3600"
    );

    // A file path with a trailing slash is a missing file, not an app route.
    let (status, h, body) = get_site(&s, "/favicon.svg/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("id=root"));
    assert!(!header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
    for uri in ["/favicon.svg/", "/assets/missing.js"] {
        let resp = app(s.clone())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
        assert!(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty(),
            "{uri}"
        );
    }

    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(header_str(resp.headers(), header::CONTENT_TYPE).starts_with("text/html"));
    assert_eq!(
        header_str(resp.headers(), header::CACHE_CONTROL),
        "no-cache"
    );

    // Missing files are 404s, not the shell.
    for uri in ["/assets/gone-123.js", "/gone.txt"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "", "{uri}");
    }

    // The API is unchanged, and its unknown paths stay problem details.
    let (status, _, body) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["index_version"].is_string());
    let (status, h, _) = get(&s, "/v1/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        header_str(&h, header::CONTENT_TYPE),
        "application/problem+json"
    );
    let (status, _, _) = get(&s, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn search_permalinks_and_status_are_noindex() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("noindex"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let robots = header::HeaderName::from_static("x-robots-tag");
    for uri in [
        "/?q=x",
        "/?q=%22cross+of+gold%22&from=1896-06-01",
        "/status",
    ] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, robots.clone()), "noindex", "{uri}");
    }
    for uri in ["/", "/?t=1896-07-01", "/favicon.svg"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, robots.clone()), "", "{uri}");
    }
}

async fn get_host(state: &Arc<AppState>, host: &str, uri: &str) -> axum::response::Response {
    app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn www_redirects_to_the_apex() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("www"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let cases = [
        ("www.usnewsmap.com", "/", "https://usnewsmap.com/"),
        (
            "WWW.usnewsmap.com:443",
            "/?q=%22cross+of+gold%22&from=1896-06-01",
            "https://usnewsmap.com/?q=%22cross+of+gold%22&from=1896-06-01",
        ),
        (
            "www.usnewsmap.com",
            "/status",
            "https://usnewsmap.com/status",
        ),
        (
            "www.usnewsmap.com",
            "/v1/meta",
            "https://usnewsmap.com/v1/meta",
        ),
    ];
    for (host, uri, location) in cases {
        let resp = get_host(&s, host, uri).await;
        assert_eq!(resp.status(), StatusCode::MOVED_PERMANENTLY, "{host}{uri}");
        assert_eq!(header_str(resp.headers(), header::LOCATION), location);
        assert_eq!(
            header_str(resp.headers(), header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert!(header_str(resp.headers(), header::CONTENT_SECURITY_POLICY).contains("default-src"));
    }
    // HTTP/2 puts the host in the URI.
    let resp = get_site(&s, "https://www.usnewsmap.com/status?x=1", None).await;
    assert_eq!(resp.0, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        header_str(&resp.1, header::LOCATION),
        "https://usnewsmap.com/status?x=1"
    );
    // Other hosts are served as they are.
    for host in [
        "usnewsmap.com",
        "api.usnewsmap.com",
        "www.example.com",
        "127.0.0.1:8080",
    ] {
        let resp = get_host(&s, host, "/").await;
        assert_eq!(resp.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn without_a_site_only_the_api_answers() {
    let s = state_with(None).await;
    let (status, _, _) = get_site(&s, "/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get_site(&s, "/v1/meta", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn status_without_pipeline_state() {
    let state = state_with(None).await;
    for uri in ["/v1/status", "/api/v1/status"] {
        let (status, headers, body) = get(&state, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(
            header_str(&headers, header::CACHE_CONTROL),
            "public, max-age=30"
        );
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/json"
        );
        assert_eq!(body["schema"], 1);
        assert_eq!(body["published"]["index_version"], "fixture-v1");
        assert_eq!(body["published"]["indexes"].as_array().unwrap().len(), 2);
        assert_eq!(body["backfill"]["available"], false);
        assert_eq!(body["indexing"]["available"], false);
    }
}

#[tokio::test]
async fn versions_without_pipeline_state() {
    let state = state_with(None).await;
    for uri in ["/v1/versions", "/api/v1/versions"] {
        let (status, headers, body) = get(&state, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(
            header_str(&headers, header::CACHE_CONTROL),
            "public, max-age=30"
        );
        assert_eq!(body["schema"], 1);
        assert_eq!(body["available"], false);
        assert_eq!(body["serving"], "fixture-v1");
        assert_eq!(body["versions"], json!([]));
    }
}

#[tokio::test]
async fn status_has_pages_by_state_and_language() {
    let state = state_with(None).await;
    let (_, _, body) = get(&state, "/v1/status").await;
    let p = &body["published"];
    // Six fixture places, one per state, 312 pages each: ties go by name.
    let states = p["by_state"].as_array().unwrap();
    assert_eq!(states.len(), 6);
    assert_eq!(
        states[0],
        json!({"state": "CA", "name": "California", "places": 1, "titles": 1,
               "pages": 312, "percent": 16.7})
    );
    let names: Vec<&str> = states.iter().map(|r| r["name"].as_str().unwrap()).collect();
    assert_eq!(
        names,
        [
            "California",
            "Georgia",
            "Illinois",
            "Nebraska",
            "New York",
            "South Carolina"
        ]
    );
    let total: u64 = states.iter().map(|r| r["pages"].as_u64().unwrap()).sum();
    assert_eq!(total, p["pages"].as_u64().unwrap());
    assert_eq!(
        p["by_language"],
        json!({"pages_known": true, "multilingual_titles": 1, "multilingual_pages": 312,
               "rows": [{"code": "eng", "name": "English", "titles": 4, "pages": 1248,
                         "percent": 66.7},
                        {"code": "ger", "name": "German", "titles": 2, "pages": 624,
                         "percent": 33.3},
                        {"code": "spa", "name": "Spanish", "titles": 1, "pages": 312,
                         "percent": 16.7}]})
    );
}

#[tokio::test]
async fn a_snapshot_without_pages_per_title_still_loads() {
    let dir = temp_data_dir("no-title-pages");
    let store = LocalStore::new(&dir);
    // A snapshot from before releases wrote the file: not in the manifest.
    std::fs::remove_file(dir.join("fixture-v1/title_pages.json")).unwrap();
    let manifest = dir.join("fixture-v1/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    m["files"]
        .as_array_mut()
        .unwrap()
        .retain(|f| f["path"] != "title_pages.json");
    std::fs::write(&manifest, m.to_string()).unwrap();
    let rd = RefData::load(&store).await.unwrap();
    assert!(rd.title_pages.is_none());
    let lang = usnm_api::status::assemble::by_language(&rd);
    assert!(!lang.pages_known);
    assert_eq!((lang.rows[0].titles, lang.rows[0].pages), (4, None));
    assert_eq!(usnm_api::status::assemble::by_state(&rd).len(), 6);
    let _ = std::fs::remove_dir_all(&dir);

    // Listed in the manifest but altered: the version doesn't load.
    let dir = temp_data_dir("bad-title-pages");
    let store = LocalStore::new(&dir);
    let path = dir.join("fixture-v1/title_pages.json");
    let text = std::fs::read_to_string(&path)
        .unwrap()
        .replacen("312", "313", 1);
    std::fs::write(&path, text).unwrap();
    let err = RefData::load(&store).await.unwrap_err();
    assert!(err.contains("title_pages.json does not match"), "{err}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn api_responses_are_compressed_and_cors_is_limited_to_allowed_origins() {
    let mut cfg = config();
    cfg.allowed_origins = vec!["https://usnewsmap.com".to_owned()];
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let send = |origin: &'static str, encoding: Option<&'static str>| {
        let s = s.clone();
        async move {
            let mut req = Request::builder()
                .uri("/v1/meta")
                .header(header::ORIGIN, origin);
            if let Some(e) = encoding {
                req = req.header(header::ACCEPT_ENCODING, e);
            }
            app(s)
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap()
        }
    };
    let vary = |h: &axum::http::HeaderMap| {
        h.get_all(header::VARY)
            .iter()
            .map(|v| v.to_str().unwrap().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    };

    // Gzip when accepted, with the CORS header for an allowed origin.
    let resp = send("https://usnewsmap.com", Some("gzip")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers().clone();
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "gzip");
    assert_eq!(
        header_str(&h, header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "https://usnewsmap.com"
    );
    assert!(vary(&h).contains("accept-encoding"), "{}", vary(&h));
    assert!(vary(&h).contains("origin"), "{}", vary(&h));
    let gzipped = resp.into_body().collect().await.unwrap().to_bytes();
    let mut gz = flate2::read::GzDecoder::new(&gzipped[..]);
    let mut json = String::new();
    std::io::Read::read_to_string(&mut gz, &mut json).unwrap();
    let meta: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(meta["index_version"], "fixture-v1");

    // Identity when compression isn't asked for, or is refused in favour of identity.
    for encoding in [None, Some("identity"), Some("gzip;q=0")] {
        let resp = send("https://usnewsmap.com", encoding).await;
        assert_eq!(resp.status(), StatusCode::OK, "{encoding:?}");
        assert_eq!(
            header_str(resp.headers(), header::CONTENT_ENCODING),
            "",
            "{encoding:?}"
        );
    }

    // Another origin gets the response but no CORS grant.
    let resp = send("https://elsewhere.example", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        header_str(resp.headers(), header::ACCESS_CONTROL_ALLOW_ORIGIN),
        ""
    );
}

/// The fixture state with the Japanese pages' index (#139) published, and its
/// synthetic title in the catalog.
async fn ja_state() -> Arc<AppState> {
    let mut backend = fixture_backend();
    backend.add_index("pages-ja-fixture", load_docs("pages-ja-fixture"));
    let mut refdata = refdata().await;
    refdata.current.ja = Some(usnm_api::refdata::JaIndexes {
        indexes: vec!["pages-ja-fixture".into()],
        fold: usnm_core::ja::FOLD_VERSION,
        pages: 52,
    });
    refdata.titles.insert(
        "sn99000901".into(),
        serde_json::from_value(json!({
            "lccn": "sn99000901", "name": "Fixture Shimpo (Japanese)",
            "place_id": "P00003", "state": "CA", "languages": ["eng", "jpn"],
        }))
        .unwrap(),
    );
    Arc::new(AppState::new(config(), Arc::new(backend), refdata))
}

#[tokio::test]
async fn japanese_queries_search_the_japanese_pages() {
    let s = ja_state().await;
    let docs = load_docs("pages-ja-fixture");
    let printed_has = |w: &str| {
        let q = vec![usnm_core::ja::tokenize(w)];
        docs.iter()
            .filter(|d| !usnm_core::ja::find(d.printed.as_deref().unwrap(), &q).is_empty())
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
