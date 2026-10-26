//! `/v1/aggregate`: counts against a brute-force pass, wildcards and OCR spellings,
//! bad requests, and the baselines (`/v1/coverage`).

use std::collections::BTreeMap;

use axum::http::{header, StatusCode};
use serde_json::Value;
use usnm_core::text::tokenize;
use usnm_core::time::day_number;
use usnm_search::memory::eval;

use crate::common::{
    data_dir, get, header_str, load_docs, oracle_count, parse, state_with, state_with_cells,
    FIXTURE,
};

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
    // A prefix needs 5 letters: a shorter one would walk too many words.
    let (status, headers, body) = get(&s, "/v1/aggregate?q=gold+silv*").await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/problem+json"
    );
    assert_eq!(body["type"], "/errors/query-syntax");
    assert_eq!(body["detail"], "prefix searches need at least 5 letters");
    assert_eq!(body["position"], 5);
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold+silve*").await;
    assert_eq!(status, StatusCode::OK, "{body}");
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

/// One state's pages: `/v1/hits` with a single `state` and no place, as many
/// as the search's total for that state; two states are refused.
#[tokio::test]
async fn hits_list_one_states_pages() {
    let s = state_with(None).await;
    let (status, _, agg) = get(&s, "/v1/aggregate?q=gold&state=IL").await;
    assert_eq!(status, StatusCode::OK, "{agg}");
    let (status, _, hits) = get(&s, "/v1/hits?q=gold&state=IL").await;
    assert_eq!(status, StatusCode::OK, "{hits}");
    assert_eq!(hits["total"], agg["total"]["hits"]);
    assert!(hits["place"].is_null());
    let (status, _, _) = get(&s, "/v1/hits?q=gold&state=IL,NY").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// Wildcards inside words (#124): counted like the reference, one cache key
/// per spelling, marked in snippets, and refused with a 400 where they
/// can't work.
#[tokio::test]
async fn wildcard_words_count_mark_and_refuse() {
    let s = state_with(None).await;
    let all = ["pages-base-fixture", "pages-delta-fixture-1"];
    let (status, headers, body) = get(
        &s,
        "/v1/aggregate?q=CONVE*%3FTION&from=1895-01-01&to=1897-12-31",
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let expected = oracle_count("conve?*tion", "1895-01-01", "1897-12-31", &all);
    assert!(expected > 0);
    assert_eq!(body["total"]["hits"], expected);
    assert_eq!(
        expected,
        oracle_count("convention", "1895-01-01", "1897-12-31", &all)
    );
    assert!(
        header_str(&headers, header::CONTENT_LOCATION).contains("&q=conve%3F*tion&"),
        "{}",
        header_str(&headers, header::CONTENT_LOCATION)
    );

    let (status, _, page) = get(&s, "/v1/hits?q=silve%2Ar&place=P00001&limit=3").await;
    assert_eq!(status, StatusCode::OK, "{page}");
    let snippet = page["items"][0]["snippets"][0].as_str().unwrap();
    assert!(
        snippet.to_lowercase().contains("<mark>silver</mark>"),
        "{snippet}"
    );

    for (q, detail) in [
        ("pres%3Fdent", "wildcards must follow at least 5 letters"),
        ("%2Agold", "wildcards must follow at least 5 letters"),
        ("%22presi%3Fent+lincoln%22", "inside quotes"),
        ("presi%3Fent~1", "not both"),
    ] {
        let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["type"], "/errors/query-syntax", "{q}");
        assert!(
            body["detail"].as_str().unwrap().contains(detail),
            "{q}: {body}"
        );
        assert!(body["hint"].is_string(), "{q}");
    }
    // A question mark that ends a word is still punctuation.
    let (status, _, body) = get(&s, "/v1/aggregate?q=convention%3F&mode=phrase").await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

/// `word~ocr` and `N OF (...)` (#124): counted, in canonical form, and
/// refused with a syntax error where they can't work.
#[tokio::test]
async fn ocr_spellings_and_at_least_n_count_and_refuse() {
    let s = state_with(None).await;
    let all = ["pages-base-fixture", "pages-delta-fixture-1"];
    let (from, to) = ("1895-01-01", "1897-12-31");
    for (q, node, canonical) in [
        ("Gold~OCR", "gold~ocr", "gold%7Eocr"),
        (
            "2+OF+(silver+gold+bryan)",
            "2 OF (bryan gold silver)",
            "2+OF+%28bryan+gold+silver%29",
        ),
    ] {
        let (status, headers, body) =
            get(&s, &format!("/v1/aggregate?q={q}&from={from}&to={to}")).await;
        assert_eq!(status, StatusCode::OK, "{q}: {body}");
        let expected = oracle_count(node, from, to, &all);
        assert!(expected > 0, "{q}");
        assert_eq!(body["total"]["hits"], expected, "{q}");
        assert!(
            header_str(&headers, header::CONTENT_LOCATION).contains(&format!("&q={canonical}&")),
            "{q}: {}",
            header_str(&headers, header::CONTENT_LOCATION)
        );
    }
    assert!(oracle_count("gold~ocr", from, to, &all) > oracle_count("gold", from, to, &all));
    for (q, detail) in [
        ("gold~ocr*", "not both"),
        ("%22cross+of+gold%22~ocr", "not a phrase"),
        // The fixture's analyzer (V1) folds `ﷺ` to four words and `½` to
        // `1⁄2`: not one index token, so not one term-set member.
        ("%EF%B7%BA~ocr", "single word of letters and digits"),
        ("%C2%BD~ocr", "single word of letters and digits"),
        ("2+OF+(gold+silver)", "use AND"),
        ("2+OF+(gold+OR+silver+bryan)", "takes words and phrases"),
        ("2+OF+(gold-silver+bryan+coin)", "quote it as a phrase"),
        ("9+OF+(gold+silver+bryan)", "N from 2 to 5"),
    ] {
        let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}")).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{q}");
        assert_eq!(body["type"], "/errors/query-syntax", "{q}");
        assert!(
            body["detail"].as_str().unwrap().contains(detail),
            "{q}: {body}"
        );
    }
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
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text, FIXTURE)))
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
