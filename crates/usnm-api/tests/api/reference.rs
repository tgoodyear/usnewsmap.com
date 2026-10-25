//! The reference-data and status endpoints: titles, meta, places, status and
//! versions.

use std::collections::HashMap;
use std::sync::Arc;

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use usnm_api::AppState;

use crate::common::{config, data_dir, fixture_backend, get, header_str, refdata, state_with};

/// The newspapers of the version (06 §6.3.11): every title in the snapshot,
/// by LCCN, with its place, years of publication, pages and languages,
/// cached and versioned like `/v1/places`.
#[tokio::test]
async fn titles_list_every_newspaper_versioned_like_places() {
    let s = state_with(None).await;
    let (status, headers, body) = get(&s, "/v1/titles").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["index_version"], "fixture-v1");
    let titles = body["titles"].as_array().unwrap();
    let lccns: Vec<&str> = titles.iter().map(|t| t["lccn"].as_str().unwrap()).collect();
    assert_eq!(
        lccns,
        [
            "sn99000001",
            "sn99000002",
            "sn99000003",
            "sn99000004",
            "sn99000005",
            "sn99000006"
        ]
    );
    assert_eq!(
        titles[1],
        json!({
            "lccn": "sn99000002",
            "title": "The Fixture Gazette 2",
            "place_id": "P00002",
            "place": "Fixture City B (New York area)",
            "state": "NY",
            "first_year": 1892,
            "last_year": 1897,
            "pages": 312,
            "first_page_date": "1895-01-05",
            "last_page_date": "1897-12-25",
            "languages": ["eng", "ger"],
        })
    );
    // The dates of its pages are the version's, apart from the years of
    // publication: every fixture title has pages from 1895 to 1897.
    assert_eq!(titles[0]["first_year"], 1891);
    assert_eq!(titles[0]["first_page_date"], "1895-01-05");
    // An open-ended run ("1896-current") has no last year.
    assert_eq!(titles[5]["first_year"], 1896);
    assert!(titles[5]["last_year"].is_null());
    // Every page of the version belongs to one title.
    let pages: u64 = titles.iter().map(|t| t["pages"].as_u64().unwrap()).sum();
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(meta["pages"], pages);
    assert_eq!(meta["titles"], titles.len());

    // Unversioned: short cache, Content-Location pins the version.
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=300"
    );
    assert_eq!(
        header_str(&headers, header::CONTENT_LOCATION),
        "/v1/titles?v=fixture-v1"
    );
    // Pinned: a day, with an ETag.
    let (status, headers, pinned) = get(&s, "/v1/titles?v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(pinned, body);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));
    // Another version: redirected to the serving one, never cached.
    let (status, headers, _) = get(&s, "/v1/titles?v=old-version").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    assert_eq!(
        header_str(&headers, header::LOCATION),
        "/v1/titles?v=fixture-v1"
    );
    // It takes no other parameter.
    let (status, _, body) = get(&s, "/v1/titles?q=gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["type"], "/errors/bad-parameter");
}

/// A paper that moved (#1066): the list keeps its place, place name and
/// state as the place where most of its pages were published, and gives its
/// places by date beside them; a paper in one place has no such list.
#[tokio::test]
async fn a_paper_that_moved_lists_its_places_by_date() {
    let mut rd = refdata().await;
    rd.titles.get_mut("sn99000002").unwrap().place_periods = serde_json::from_value(json!([
        {"place_id": "P00001", "to": "1896-06-30"},
        {"place_id": "P00002", "from": "1896-07-01"},
    ]))
    .unwrap();
    let s = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));
    let (status, _, list) = get(&s, "/v1/titles").await;
    assert_eq!(status, StatusCode::OK, "{list}");
    let titles = list["titles"].as_array().unwrap();
    let moved = &titles[1];
    assert_eq!(moved["lccn"], "sn99000002");
    // The majority place, as before.
    assert_eq!(
        (
            moved["place_id"].clone(),
            moved["place"].clone(),
            moved["state"].clone()
        ),
        (
            json!("P00002"),
            json!("Fixture City B (New York area)"),
            json!("NY")
        )
    );
    assert_eq!(
        moved["place_periods"],
        json!([
            {
                "place_id": "P00001", "place": "Fixture City A (Chicago area)",
                "state": "IL", "from": null, "to": "1896-06-30",
            },
            {
                "place_id": "P00002", "place": "Fixture City B (New York area)",
                "state": "NY", "from": "1896-07-01", "to": null,
            },
        ])
    );
    for (i, t) in titles.iter().enumerate() {
        assert_eq!(t.get("place_periods").is_some(), i == 1, "{t}");
    }
    // The same on its own.
    let (_, _, one) = get(&s, "/v1/titles/sn99000002").await;
    assert_eq!(one["place_periods"], moved["place_periods"]);
    let (_, _, one) = get(&s, "/v1/titles/sn99000001").await;
    assert!(one.get("place_periods").is_none());
    // A place counts the titles with pages in it, the paper in both.
    let (_, _, places) = get(&s, "/v1/places").await;
    let titles_in = |id: &str| {
        places["features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .unwrap()["properties"]["titles"]
            .clone()
    };
    let (_, _, plain) = get(&state_with(None).await, "/v1/places").await;
    let plain_in = |id: &str| {
        plain["features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["id"] == id)
            .unwrap()["properties"]["titles"]
            .clone()
    };
    assert_eq!(
        titles_in("P00001").as_u64(),
        plain_in("P00001").as_u64().map(|n| n + 1)
    );
    assert_eq!(titles_in("P00002"), plain_in("P00002"));
}

/// One newspaper: the list's entry with its page at the Library of
/// Congress; 404 for a title the version doesn't have, after a stale
/// version is redirected.
#[tokio::test]
async fn one_title_with_its_link_or_not_found() {
    let s = state_with(None).await;
    let (_, _, list) = get(&s, "/v1/titles").await;
    let (status, headers, body) = get(&s, "/v1/titles/sn99000003").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut want = list["titles"][2].clone();
    want["index_version"] = json!("fixture-v1");
    want["links"] = json!({"loc": "https://www.loc.gov/item/sn99000003/"});
    assert_eq!(body, want);
    assert_eq!(body["languages"], json!(["spa"]));
    assert_eq!(
        header_str(&headers, header::CONTENT_LOCATION),
        "/v1/titles/sn99000003?v=fixture-v1"
    );
    let (status, headers, _) = get(&s, "/v1/titles/sn99000003?v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        "public, max-age=86400"
    );
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));
    // A stale version is redirected first, even for a title this one lacks.
    for lccn in ["sn99000003", "sn12345678"] {
        let (status, headers, _) = get(&s, &format!("/v1/titles/{lccn}?v=old-version")).await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT, "{lccn}");
        assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
        assert_eq!(
            header_str(&headers, header::LOCATION),
            format!("/v1/titles/{lccn}?v=fixture-v1")
        );
    }
    // Unknown, or not an LCCN at all: not found, as problem details.
    for path in [
        "/v1/titles/sn12345678",
        "/v1/titles/sn12345678?v=fixture-v1",
        "/v1/titles/SN99000003",
        "/v1/titles/sn99000003x1234567",
        "/v1/titles/sn%2099000003",
    ] {
        let (status, headers, body) = get(&s, path).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        assert_eq!(
            header_str(&headers, header::CONTENT_TYPE),
            "application/problem+json"
        );
        assert_eq!(body["type"], "/errors/not-found", "{path}");
    }
    let (status, _, _) = get(&s, "/v1/titles/sn99000003?q=gold").await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
}

/// A snapshot without `title_pages.json` (published before releases wrote
/// it) still lists its titles, with their pages unknown.
#[tokio::test]
async fn titles_without_pages_per_title() {
    let mut rd = refdata().await;
    rd.title_pages = None;
    let s = Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd));
    let (status, _, body) = get(&s, "/v1/titles").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let titles = body["titles"].as_array().unwrap();
    assert_eq!(titles.len(), 6);
    assert!(titles.iter().all(|t| t["pages"].is_null()));
    let (_, _, one) = get(&s, "/v1/titles/sn99000001").await;
    assert!(one["pages"].is_null());
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
    assert_eq!(meta["limits"]["min_prefix_chars"], 5);
    assert_eq!(meta["limits"]["max_at_least_items"], 6);
    assert_eq!(meta["limits"]["max_ocr_variants"], 32);
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
    // Pages by year cover the version's dates and add up to its pages.
    let years = p["by_year"].as_array().unwrap();
    let year = |r: &Value| r["year"].as_i64().unwrap();
    assert_eq!(
        years.iter().map(year).collect::<Vec<_>>(),
        [1895, 1896, 1897]
    );
    let total: u64 = years.iter().map(|r| r["pages"].as_u64().unwrap()).sum();
    assert_eq!(total, p["pages"].as_u64().unwrap());
    // The fixtures search LoC's text alone.
    assert_eq!(
        p["sources"],
        json!({"american_stories": null, "ja_latin": null, "ocr_local": null})
    );
}
