//! Congress pages (#229, phase 0).

use std::sync::Arc;

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use usnm_api::config::Config;
use usnm_api::refdata::RefData;
use usnm_api::{reload_if_changed, AppState, Engine, Loader};
use usnm_store::LocalStore;

use crate::common::{
    config, fixture_backend, get, header_str, load_docs, oracle_count, parse, refdata, state_with,
    temp_data_dir,
};

const CONGRESS: &str = "congress-fixture";

/// The fixture version with the Congress fixture index named in its pointer.
async fn congress_state_with(cfg: Config) -> Arc<AppState> {
    let mut backend = fixture_backend();
    backend.add_index(CONGRESS, load_docs(CONGRESS));
    Arc::new(AppState::new(
        cfg,
        Arc::new(backend),
        congress_refdata().await,
    ))
}

/// The fixture version's reference data, its pointer naming the Congress
/// fixture index and its series.
async fn congress_refdata() -> RefData {
    let mut refdata = refdata().await;
    let d = |s: &str| chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    let series = |name: &str, pages: u64, from: &str, to: &str| usnm_api::refdata::CongressSeries {
        series: name.into(),
        pages,
        from: d(from),
        to: d(to),
    };
    refdata.current.congress = Some(usnm_api::refdata::CongressIndexes {
        indexes: vec![CONGRESS.into()],
        common_grams: refdata.current.common_grams,
        series: vec![
            series("annals", 20, "1820-02-01", "1820-03-29"),
            series("register", 21, "1832-12-10", "1833-02-27"),
            series("globe", 78, "1850-01-07", "1850-09-30"),
            series("serialset", 72, "1896-02-03", "1897-11-29"),
        ],
    });
    refdata
}

async fn congress_state() -> Arc<AppState> {
    congress_state_with(config()).await
}

/// Congress fixture pages whose text holds `phrase`, by brute force.
fn congress_oracle(phrase: &str, from: &str, to: &str, series: Option<&str>) -> u64 {
    let needle = format!(" {phrase} ");
    load_docs(CONGRESS)
        .iter()
        .filter(|d| {
            let date = usnm_core::time::date_from_day(d.day).to_string();
            date.as_str() >= from
                && date.as_str() <= to
                && series.is_none_or(|s| d.congress.series.as_deref() == Some(s))
                && format!(" {} ", d.text).contains(&needle)
        })
        .count() as u64
}

#[tokio::test]
async fn congress_searches_count_congress_pages_at_their_own_place() {
    let s = congress_state().await;
    // 1850 is before the fixture newspapers' first year (1895): a Congress
    // search is clamped to the Congress series' dates, not the newspapers'.
    let (st, _, body) = get(
        &s,
        "/v1/aggregate?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&bucket=month&source=congress",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let want = congress_oracle("fugitive slave", "1850-01-01", "1850-12-31", None);
    assert!(want > 0);
    assert_eq!(body["total"]["hits"], want);
    assert_eq!(body["places"]["id"], json!(["PC001"]));
    assert_eq!(body["bucket"]["from"], "1850-01-01");
    // No Congress baselines yet, and no newspapers.
    assert!(body["series"]["baseline"].is_null());
    assert!(body["baseline"].is_null());
    assert_eq!(body["total"]["papers"], 0);
    assert_eq!(body["papers"]["lccn"], json!([]));
    assert!(body["query"]["canonical"]
        .as_str()
        .unwrap()
        .ends_with("&source=congress&to=1850-12-31"));
}

#[tokio::test]
async fn the_series_filter_counts_one_series() {
    let s = congress_state().await;
    let q = "/v1/aggregate?q=gold&mode=phrase&from=1789-01-01&to=1994-12-31&bucket=year&source=congress";
    let (st, _, all) = get(&s, q).await;
    assert_eq!(st, StatusCode::OK, "{all}");
    let (_, _, serial) = get(&s, &format!("{q}&series=serialset")).await;
    let (_, _, globe) = get(&s, &format!("{q}&series=globe")).await;
    let want = |series| congress_oracle("gold", "1789-01-01", "1994-12-31", series);
    assert_eq!(all["total"]["hits"], want(None));
    assert_eq!(serial["total"]["hits"], want(Some("serialset")));
    assert_eq!(globe["total"]["hits"], want(Some("globe")));
    assert!(want(Some("serialset")) > 0);
}

#[tokio::test]
async fn newspaper_searches_never_see_congress_pages() {
    let s = congress_state().await;
    let (st, _, body) = get(
        &s,
        "/v1/aggregate?q=%22free+silver%22&from=1896-01-01&to=1896-12-31&bucket=month",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"]["hits"],
        oracle_count(
            "\"free silver\"",
            "1896-01-01",
            "1896-12-31",
            &["pages-base-fixture", "pages-delta-fixture-1"]
        )
    );
    assert!(!body["places"]["id"]
        .as_array()
        .unwrap()
        .contains(&json!("PC001")));
    assert!(!body["query"]["canonical"]
        .as_str()
        .unwrap()
        .contains("source"));
    // `source=news` is the same search, under the same cache key.
    let (_, _, explicit) = get(
        &s,
        "/v1/aggregate?q=%22free+silver%22&from=1896-01-01&to=1896-12-31&bucket=month&source=news",
    )
    .await;
    assert_eq!(explicit["query"]["canonical"], body["query"]["canonical"]);
}

#[tokio::test]
async fn congress_hits_carry_their_series_citation_and_link() {
    let s = congress_state().await;
    let (st, _, body) = get(
        &s,
        "/v1/hits?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress&place=PC001&limit=5",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    // The Congress place isn't in the newspapers' catalog, but is echoed.
    assert_eq!(
        body["place"],
        json!({"id": "PC001", "name": "Congress", "state": "DC"})
    );
    let items = body["items"].as_array().unwrap();
    assert!(!items.is_empty());
    for it in items {
        assert_eq!(it["place_id"], "PC001");
        assert_eq!(it["front_page"], false);
        assert_eq!(it["congress"]["series"], "globe");
        assert_eq!(it["congress"]["congress"], 31);
        assert!(it["title"].as_str().unwrap().starts_with("Fixture Globe"));
        assert!(it["links"]["viewer"]
            .as_str()
            .unwrap()
            .starts_with("https://example.org/fixture/congress/globe/"));
        assert!(it["snippets"][0].as_str().unwrap().contains("<mark>"));
        // Who made the text goes with the citation, not in `ocr`, which
        // the site shows as our Japanese OCR.
        assert!(it.get("ocr").is_none(), "{it}");
        assert_eq!(it["congress"]["text_source"], "fixture");
        assert!(it["congress"].get("text_engine").is_none(), "{it}");
    }
    // A newspaper search still refuses the Congress place.
    let (st, _, _) = get(&s, "/v1/hits?q=gold&place=PC001").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn congress_searches_answer_422_where_they_cannot_run() {
    let unsupported = |st: StatusCode, body: &Value| {
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
        assert_eq!(body["type"], "/errors/unsupported");
    };
    let s = congress_state().await;
    // Both at once needs two result blocks, which come in phase 1.
    let (st, _, body) = get(&s, "/v1/aggregate?q=gold&source=all").await;
    unsupported(st, &body);
    // Congress pages have no Japanese.
    let (st, _, body) = get(&s, "/v1/aggregate?q=%E6%97%A5%E6%9C%AC&source=congress").await;
    unsupported(st, &body);
    // A version without a Congress set.
    let plain = state_with(None).await;
    let (st, _, body) = get(&plain, "/v1/aggregate?q=gold&source=congress").await;
    unsupported(st, &body);
    // Switched off by the setting.
    let mut cfg = config();
    cfg.congress_search = false;
    let off = congress_state_with(cfg).await;
    let (st, _, body) = get(&off, "/v1/aggregate?q=gold&source=congress").await;
    unsupported(st, &body);
    let (_, _, meta) = get(&off, "/v1/meta").await;
    assert!(meta["congress"].is_null());
    // Newspaper-only filters are refused under Congress.
    let (st, _, _) = get(&s, "/v1/aggregate?q=gold&source=congress&front=true").await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
}

/// A stale `v` is redirected before a Congress search is refused, so a page
/// pinned to a version with Congress pages learns that the version changed
/// even when the new one can't search them.
#[tokio::test]
async fn a_stale_version_is_redirected_before_congress_is_refused() {
    let plain = state_with(None).await;
    let q = "q=gold&from=1895-01-01&to=1897-12-31&source=congress";
    for path in [
        format!("/v1/aggregate?{q}"),
        format!("/v1/hits?{q}&place=PC001"),
        format!("/v1/days?{q}&place=PC001"),
    ] {
        let (st, headers, body) = get(&plain, &format!("{path}&v=old-version")).await;
        assert_eq!(st, StatusCode::TEMPORARY_REDIRECT, "{path}: {body}");
        let location = header_str(&headers, header::LOCATION);
        assert!(location.contains("&source=congress"), "{location}");
        assert!(location.ends_with("&v=fixture-v1"), "{location}");
        // The serving version refuses it, pinned to it or not.
        for uri in [path.clone(), format!("{path}&v=fixture-v1")] {
            let (st, _, body) = get(&plain, &uri).await;
            assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {body}");
        }
    }
}

#[tokio::test]
async fn current_json_congress_series_are_validated() {
    use usnm_store::ObjectStore as _;
    let dir = tempfile::tempdir().unwrap();
    let store = LocalStore::new(dir.path());
    let base = json!({
        "index_version": "v1", "indexes": ["pages-base-1"], "reference": "v1",
        "bounds": {"from": "1895-01-01", "to": "1897-12-31"}, "published_at": "2026-10-05T00:00:00Z",
    });
    let with_series = |series: Value, indexes: Value| {
        let mut v = base.clone();
        v["congress"] = json!({"indexes": indexes, "series": [series]});
        v
    };
    let globe =
        |from: &str, to: &str| json!({"series": "globe", "pages": 3, "from": from, "to": to});
    let read = |v: Value| {
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
            usnm_api::refdata::read_current(store).await
        }
    };
    let ok = globe("1833-12-02", "1873-03-03");
    assert!(read(with_series(ok.clone(), json!(["congress-1"])))
        .await
        .is_ok());
    // One day is a span.
    let day = globe("1850-01-01", "1850-01-01");
    assert!(read(with_series(day, json!(["congress-1"]))).await.is_ok());
    // Requests clamp their dates to a series' span, so it can't run backwards.
    let backwards = globe("1873-03-03", "1833-12-02");
    let err = read(with_series(backwards, json!(["congress-1"])))
        .await
        .unwrap_err();
    assert!(err.contains("ends before it starts"), "{err}");
    let mut unknown = ok.clone();
    unknown["series"] = json!("hansard");
    let err = read(with_series(unknown, json!(["congress-1"])))
        .await
        .unwrap_err();
    assert!(err.contains("unknown Congress series"), "{err}");
    for bad in [json!([]), json!(["../escape"])] {
        assert!(
            read(with_series(ok.clone(), bad.clone())).await.is_err(),
            "{bad}"
        );
    }
}

#[tokio::test]
async fn meta_lists_congress_series_and_its_place() {
    let s = congress_state().await;
    let (st, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(st, StatusCode::OK);
    let series: Vec<&str> = meta["congress"]["series"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["series"].as_str().unwrap())
        .collect();
    assert_eq!(series, ["annals", "register", "globe", "serialset"]);
    assert_eq!(meta["congress"]["place"]["id"], "PC001");
    assert_eq!(meta["congress"]["place"]["name"], "Congress");
    // A version without Congress pages says so.
    let (_, _, plain) = get(&state_with(None).await, "/v1/meta").await;
    assert!(plain["congress"].is_null());
}

#[tokio::test]
async fn days_take_the_congress_place_for_congress_searches_only() {
    let s = congress_state().await;
    let (st, _, body) = get(
        &s,
        "/v1/days?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress&place=PC001",
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["places"][0]["id"], "PC001");
    let hits: u64 = body["places"][0]["hits"]
        .as_array()
        .unwrap()
        .iter()
        .map(|h| h.as_u64().unwrap())
        .sum();
    let want = congress_oracle("fugitive slave", "1850-01-01", "1850-12-31", None);
    assert!(want > 0);
    assert_eq!(hits, want);
    // A newspaper search doesn't know the place.
    let (st, _, _) = get(&s, "/v1/days?q=gold&place=PC001").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _, _) = get(&s, "/v1/days?q=gold&place=PC001&source=news").await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

/// Switching Congress search off refuses Congress searches before any
/// cache is asked, so responses cached while it was on aren't served.
#[tokio::test]
async fn congress_responses_cached_before_the_switch_goes_off_are_refused() {
    let s = congress_state().await;
    let range = "q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress";
    let uris = [
        format!("/v1/aggregate?{range}"),
        format!("/v1/hits?{range}&place=PC001"),
        format!("/v1/days?{range}&place=PC001"),
    ];
    let mut keys = Vec::new();
    for uri in &uris {
        let (st, _, body) = get(&s, uri).await;
        assert_eq!(st, StatusCode::OK, "{uri}: {body}");
        if let Some(canonical) = body["query"]["canonical"].as_str() {
            keys.push(format!("fixture-v1|aggregate|{canonical}"));
        }
    }
    assert!(s.cache.contains_key(&keys[0]), "{keys:?}");
    // The same version, switched off.
    let serving = s.snapshot.load_full();
    let mut refdata = congress_refdata().await;
    refdata.congress_search = false;
    s.snapshot.store(Arc::new(usnm_api::Snapshot {
        refdata,
        backend: serving.backend.clone(),
        congress_dropped: false,
    }));
    for uri in &uris {
        let (st, _, body) = get(&s, uri).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{uri}: {body}");
        assert_eq!(body["type"], "/errors/unsupported");
    }
    // Still cached: only refused.
    assert!(s.cache.contains_key(&keys[0]));
    // Newspaper searches answer as before.
    let (st, _, _) = get(&s, "/v1/aggregate?q=fever").await;
    assert_eq!(st, StatusCode::OK);
}

/// Without series a Congress set has no dates of its own, so a Congress
/// search would be clamped to the newspapers': refused instead.
#[tokio::test]
async fn a_congress_set_without_series_is_not_searched() {
    let mut backend = fixture_backend();
    backend.add_index(CONGRESS, load_docs(CONGRESS));
    let mut refdata = congress_refdata().await;
    if let Some(c) = refdata.current.congress.as_mut() {
        c.series.clear();
    }
    let s = Arc::new(AppState::new(config(), Arc::new(backend), refdata));
    let (st, _, body) = get(
        &s,
        "/v1/aggregate?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress",
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["type"], "/errors/unsupported");
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert!(meta["congress"].is_null());
}

/// A state loaded from `dir` as the API loads one, with Congress search on
/// or off.
async fn loaded_state(dir: &std::path::Path, congress_search: bool) -> Arc<AppState> {
    let loader = Loader {
        reference: Arc::new(LocalStore::new(dir)),
        engine: Engine::Memory {
            indexes_dir: dir.join("indexes"),
        },
        congress_search,
    };
    let snapshot = loader.snapshot().await.unwrap();
    let mut cfg = config();
    cfg.congress_search = congress_search;
    Arc::new(AppState::with_loader(cfg, snapshot, None))
}

/// A Congress index that can't be loaded only takes the Congress pages out
/// of a version: its newspapers still publish. Switched off, the Congress
/// index isn't read at all.
#[tokio::test]
async fn a_broken_congress_index_never_holds_back_the_newspapers() {
    let dir = temp_data_dir("congress-load");
    let pointer = dir.join("current.json");
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(&pointer).unwrap()).unwrap();
    current["congress"] = json!({
        "indexes": ["congress-missing"],
        "series": [{"series": "globe", "pages": 78, "from": "1850-01-07", "to": "1850-09-30"}],
    });
    std::fs::write(&pointer, current.to_string()).unwrap();
    let congress =
        "/v1/aggregate?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress";
    for on in [true, false] {
        let s = loaded_state(&dir, on).await;
        let (st, _, body) = get(&s, "/v1/aggregate?q=fever").await;
        assert_eq!(st, StatusCode::OK, "{on}: {body}");
        assert!(body["total"]["hits"].as_u64().unwrap() > 0);
        let (st, _, body) = get(&s, congress).await;
        assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{on}: {body}");
        let (_, _, meta) = get(&s, "/v1/meta").await;
        assert!(meta["congress"].is_null(), "{on}");
    }

    // With its index there, the version searches its Congress pages too.
    current["congress"]["indexes"] = json!([CONGRESS]);
    std::fs::write(&pointer, current.to_string()).unwrap();
    let s = loaded_state(&dir, true).await;
    let (st, _, body) = get(&s, congress).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"]["hits"],
        congress_oracle("fugitive slave", "1850-01-01", "1850-12-31", None)
    );
    // Switched off, the index was never loaded.
    let s = loaded_state(&dir, false).await;
    let filters = usnm_core::params::Filters {
        from: chrono::NaiveDate::from_ymd_opt(1789, 1, 1).unwrap(),
        to: chrono::NaiveDate::from_ymd_opt(1994, 12, 31).unwrap(),
        states: vec![],
        lccns: vec![],
        langs: vec![],
        front_only: false,
        series: vec![],
    };
    let page = usnm_search::HitsQuery {
        limit: 1,
        ..Default::default()
    };
    let read = s
        .snapshot
        .load_full()
        .backend
        .hits(
            &usnm_search::IndexSet::new(vec![CONGRESS.into()]),
            &parse("slave").unwrap(),
            &filters,
            &page,
        )
        .await;
    assert!(read.is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A Congress set dropped at load comes back at the next refresh of the same
/// version once it can be loaded, without republishing anything else; until
/// then the refresh changes nothing (#229).
#[tokio::test]
async fn a_dropped_congress_set_comes_back_at_the_next_refresh() {
    let dir = temp_data_dir("congress-retry");
    let pointer = dir.join("current.json");
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(&pointer).unwrap()).unwrap();
    current["congress"] = json!({
        "indexes": ["congress-late"],
        "series": [{"series": "globe", "pages": 78, "from": "1850-01-07", "to": "1850-09-30"}],
    });
    std::fs::write(&pointer, current.to_string()).unwrap();
    let loader = || Loader {
        reference: Arc::new(LocalStore::new(&dir)),
        engine: Engine::Memory {
            indexes_dir: dir.join("indexes"),
        },
        congress_search: true,
    };
    let snapshot = loader().snapshot().await.unwrap();
    assert!(snapshot.congress_dropped);
    let s = Arc::new(AppState::with_loader(config(), snapshot, Some(loader())));
    let congress =
        "/v1/aggregate?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress";

    // Still missing: the refresh keeps the snapshot as it is.
    let before = Arc::as_ptr(&s.snapshot.load_full());
    assert!(!reload_if_changed(&s).await.unwrap());
    assert_eq!(Arc::as_ptr(&s.snapshot.load_full()), before);
    let (st, _, _) = get(&s, congress).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY);

    // The index arrives: the next refresh brings the Congress pages back.
    std::fs::copy(
        dir.join("indexes").join(format!("{CONGRESS}.jsonl")),
        dir.join("indexes").join("congress-late.jsonl"),
    )
    .unwrap();
    assert!(reload_if_changed(&s).await.unwrap());
    assert!(!s.snapshot.load_full().congress_dropped);
    let (st, _, body) = get(&s, congress).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["total"]["hits"],
        congress_oracle("fugitive slave", "1850-01-01", "1850-12-31", None)
    );
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(meta["congress"]["place"]["id"], "PC001");
    // Nothing left to bring back: the next refresh is a no-op again.
    assert!(!reload_if_changed(&s).await.unwrap());
    let _ = std::fs::remove_dir_all(&dir);
}

/// A same-version `current.json` whose `page_language` changed isn't
/// loaded, not even to bring back a dropped Congress set: the caches key on
/// the version, so the language mode is fixed for it (#289).
#[tokio::test]
async fn a_changed_page_language_under_the_same_version_is_not_loaded() {
    let dir = temp_data_dir("congress-retry-page-language");
    let pointer = dir.join("current.json");
    let mut current: Value =
        serde_json::from_str(&std::fs::read_to_string(&pointer).unwrap()).unwrap();
    current["congress"] = json!({
        "indexes": ["congress-late"],
        "series": [{"series": "globe", "pages": 78, "from": "1850-01-07", "to": "1850-09-30"}],
    });
    std::fs::write(&pointer, current.to_string()).unwrap();
    let loader = || Loader {
        reference: Arc::new(LocalStore::new(&dir)),
        engine: Engine::Memory {
            indexes_dir: dir.join("indexes"),
        },
        congress_search: true,
    };
    let snapshot = loader().snapshot().await.unwrap();
    assert!(snapshot.congress_dropped);
    assert_eq!(snapshot.refdata.current.page_language, None);
    let s = Arc::new(AppState::with_loader(config(), snapshot, Some(loader())));
    std::fs::copy(
        dir.join("indexes").join(format!("{CONGRESS}.jsonl")),
        dir.join("indexes").join("congress-late.jsonl"),
    )
    .unwrap();

    // The Congress set could come back, but page_language changed: no reload.
    let mut changed = current.clone();
    changed["page_language"] = json!(1);
    std::fs::write(&pointer, changed.to_string()).unwrap();
    let before = Arc::as_ptr(&s.snapshot.load_full());
    assert!(!reload_if_changed(&s).await.unwrap());
    assert_eq!(Arc::as_ptr(&s.snapshot.load_full()), before);
    assert!(s.snapshot.load_full().congress_dropped);

    // The pointer restored: the Congress set comes back, in title language.
    std::fs::write(&pointer, current.to_string()).unwrap();
    assert!(reload_if_changed(&s).await.unwrap());
    let now = s.snapshot.load_full();
    assert!(!now.congress_dropped);
    assert_eq!(now.refdata.current.page_language, None);
    let _ = std::fs::remove_dir_all(&dir);
}
