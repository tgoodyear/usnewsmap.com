//! Matching pages per day, distinct days, and first and last mentions, against
//! brute-force passes over the fixture files.

use std::collections::{BTreeMap, HashMap};

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use usnm_core::text::tokenize;
use usnm_core::time::{date_from_day, day_number};
use usnm_search::memory::eval;
use usnm_search::PageDoc;

use crate::common::{get, header_str, load_docs, parse, state_with, state_with_cells, FIXTURE};

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
            .filter(|d| eval(&node, &tokenize(&d.text, FIXTURE)))
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
        .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text, FIXTURE)))
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
            .filter(|d| d.day >= f && d.day <= t && eval(&node, &tokenize(&d.text, FIXTURE)))
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
