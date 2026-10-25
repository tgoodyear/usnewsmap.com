//! American Stories' text (05 §5.5.4) and the text layouts: searched only for a
//! version built with it, switched off, one field for both texts, and the cache
//! keys and persisted responses of each state.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::StatusCode;
use serde_json::{json, Value};
use usnm_api::refdata::RefData;
use usnm_api::AppState;
use usnm_core::text_layout::TextLayout;
use usnm_search::memory::MemoryBackend;
use usnm_search::PageDoc;
use usnm_store::LocalStore;

use crate::common::{
    config, data_dir, fixture_backend, get, ja_state_with, load_docs, persisted_files, refdata,
    state_with,
};

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

/// A state on a version built with American Stories' text, searching it or
/// not (`USNM_AMERICAN_STORIES_SEARCH`).
pub(crate) async fn american_stories_state(search: bool) -> Arc<AppState> {
    let mut rd = refdata().await;
    rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
    let mut cfg = config();
    cfg.american_stories_search = search;
    Arc::new(AppState::new(cfg, Arc::new(fixture_backend()), rd))
}

/// The search endpoints, and the reference-data ones a search links to.
const AMERICAN_STORIES_URIS: [&str; 6] = [
    "/v1/aggregate?q=gold",
    "/v1/aggregate?q=bimetallism",
    "/v1/aggregate?q=gold+-bryan&from=1896-01-01&to=1896-12-31",
    "/v1/hits?q=gold&place=P00001&limit=50",
    "/v1/hits?q=council&place=P00001&limit=50",
    "/v1/days?q=gold&place=P00001,P00006",
];

/// With `USNM_AMERICAN_STORIES_SEARCH=false`, a version built with American
/// Stories' text answers exactly as if `current.json` had no
/// `american_stories`: LoC's text alone, no `matched_in`, no
/// `snippet_source`, no `american_stories_only` (05 §5.5.4).
#[tokio::test]
async fn the_setting_switches_american_stories_text_off() {
    let switched = american_stories_state(false).await;
    let on = american_stories_state(true).await;
    let loc = state_with(None).await;
    let strip = |mut v: Value| {
        if let Some(o) = v.as_object_mut() {
            o.remove("timing_ms");
        }
        v
    };
    for uri in AMERICAN_STORIES_URIS {
        let (status, _, off) = get(&switched, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {off}");
        let (_, _, without) = get(&loc, uri).await;
        assert_eq!(strip(off.clone()), strip(without), "{uri}");
        let (_, _, with) = get(&on, uri).await;
        assert_ne!(strip(off), strip(with), "{uri}");
    }
    let (_, _, agg) = get(&switched, "/v1/aggregate?q=bimetallism").await;
    assert_eq!(agg["total"]["hits"], 0);
    let (_, _, agg) = get(&switched, "/v1/aggregate?q=gold").await;
    assert!(agg["total"].get("american_stories_only").is_none());
    assert!(agg["total"]["first"].get("matched_in").is_none());

    // `/v1/meta` says whether searches cover the text.
    for (s, searched) in [(&switched, false), (&on, true), (&loc, false)] {
        let (_, _, meta) = get(s, "/v1/meta").await;
        assert_eq!(meta["american_stories"], searched);
    }
}

/// The keys of every response cached in process, after the cache settles.
async fn cached_keys(state: &Arc<AppState>) -> std::collections::BTreeSet<String> {
    state.cache.run_pending_tasks().await;
    state.cache.iter().map(|(k, _)| (*k).clone()).collect()
}

/// Search responses computed with American Stories' text switched off are
/// cached under keys of their own, so neither state serves the other's;
/// with it on the keys are as before the setting, and reference-data
/// responses (places, titles, coverage) share their keys.
#[tokio::test]
async fn search_cache_keys_mark_american_stories_switched_off() {
    let (on, off) = (
        american_stories_state(true).await,
        american_stories_state(false).await,
    );
    let (_, _, agg) = get(&on, "/v1/aggregate?q=gold").await;
    let coverage = agg["cube"]["baseline_ref"].as_str().unwrap().to_owned();
    let mut uris: Vec<&str> = AMERICAN_STORIES_URIS.to_vec();
    uris.extend(["/v1/places", "/v1/titles", coverage.as_str()]);
    for s in [&on, &off] {
        for uri in &uris {
            let (status, _, body) = get(s, uri).await;
            assert_eq!(status, StatusCode::OK, "{uri}: {body}");
        }
    }
    let on_keys = cached_keys(&on).await;
    let off_keys = cached_keys(&off).await;
    // On: `{index_version}|{endpoint}|{canonical}`, unchanged.
    let canonical = agg["query"]["canonical"].as_str().unwrap();
    assert!(on_keys.contains(&format!("fixture-v1|aggregate|{canonical}")));
    assert!(on_keys.iter().all(|k| !k.contains("american_stories")));
    assert!(on_keys.iter().any(|k| k.contains("|places|")));
    assert!(on_keys.iter().any(|k| k.contains("|titles|")));
    assert!(on_keys.iter().any(|k| k.contains("|coverage|")));
    // Off: the same keys, the searches' marked.
    let expected: std::collections::BTreeSet<String> = on_keys
        .iter()
        .map(|k| {
            if k.contains("|places|") || k.contains("|titles|") || k.contains("|coverage|") {
                k.clone()
            } else {
                format!("{k}{}", usnm_api::refdata::AMERICAN_STORIES_OFF_KEY)
            }
        })
        .collect();
    assert_eq!(off_keys, expected);
    for endpoint in ["aggregate", "hits", "days"] {
        assert!(
            off_keys
                .iter()
                .any(|k| k.contains(&format!("|{endpoint}|"))
                    && k.ends_with("|american_stories=off")),
            "{endpoint}"
        );
    }

    // Japanese queries search the Japanese pages, which have no American
    // Stories text: their keys stay as they were.
    let mut cfg = config();
    cfg.american_stories_search = false;
    let ja = ja_state_with(cfg, true).await;
    for uri in ["/v1/aggregate?q=%E6%88%A6%E4%BA%89", "/v1/aggregate?q=gold"] {
        let (status, _, body) = get(&ja, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {body}");
    }
    let ja_keys: Vec<String> = cached_keys(&ja)
        .await
        .into_iter()
        .filter(|k| k.contains("|aggregate|"))
        .collect();
    assert_eq!(ja_keys.len(), 2, "{ja_keys:?}");
    for k in &ja_keys {
        let japanese = k.contains("%E6%88%A6");
        assert_eq!(k.ends_with("|american_stories=off"), !japanese, "{k}");
    }

    // On a version without the text the setting changes nothing, and
    // neither do the keys.
    let mut cfg = config();
    cfg.american_stories_search = false;
    let plain = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let (_, _, _) = get(&plain, "/v1/aggregate?q=gold").await;
    assert!(cached_keys(&plain)
        .await
        .contains(&format!("fixture-v1|aggregate|{canonical}")));
}

/// A state on a version whose main indexes search both texts in one field
/// (`text_layout` 2, 05 §5.5.7), with `USNM_AMERICAN_STORIES_SEARCH` as
/// `search` says.
async fn single_field_state(search: bool) -> Arc<AppState> {
    let mut rd = refdata().await;
    rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
    rd.current.text_layout = Some(usnm_core::text_layout::TextLayout::Single.version());
    let mut cfg = config();
    cfg.american_stories_search = search;
    Arc::new(AppState::new(cfg, Arc::new(fixture_backend()), rd))
}

/// One field for both texts answers as a field per text with both
/// searched: the same pages, snippets and `matched_in`, but no
/// `total.american_stories_only`, which needs LoC's text on its own. The
/// setting can't leave American Stories' text out, so it changes nothing,
/// cache keys included.
#[tokio::test]
async fn one_text_field_answers_as_both_texts_without_the_american_stories_only_count() {
    let single = single_field_state(true).await;
    let switched = single_field_state(false).await;
    let on = american_stories_state(true).await;
    let strip = |mut v: Value| {
        if let Some(o) = v.as_object_mut() {
            o.remove("timing_ms");
            if let Some(t) = o.get_mut("total").and_then(Value::as_object_mut) {
                t.remove("american_stories_only");
            }
        }
        v
    };
    for uri in AMERICAN_STORIES_URIS {
        let (status, _, got) = get(&single, uri).await;
        assert_eq!(status, StatusCode::OK, "{uri}: {got}");
        if uri.starts_with("/v1/aggregate") {
            assert!(got["total"].get("american_stories_only").is_none(), "{uri}");
        }
        let (_, _, want) = get(&on, uri).await;
        assert_eq!(strip(got.clone()), strip(want), "{uri}");
        let (_, _, off) = get(&switched, uri).await;
        assert_eq!(strip(off), strip(got), "{uri}");
    }
    let (_, _, agg) = get(&single, "/v1/aggregate?q=bimetallism").await;
    assert!(agg["total"]["hits"].as_u64() > Some(0));
    assert_eq!(
        agg["total"]["first"]["matched_in"],
        json!(["american_stories"])
    );
    for s in [&single, &switched] {
        let (_, _, meta) = get(s, "/v1/meta").await;
        assert_eq!(meta["american_stories"], true);
        assert_eq!(meta["text_layout"], 2);
        assert!(cached_keys(s)
            .await
            .iter()
            .all(|k| !k.contains("american_stories")));
    }
    let (_, _, meta) = get(&on, "/v1/meta").await;
    assert_eq!(meta["text_layout"], 1);
}

/// A state on a version whose main indexes search every text in one field,
/// `layout` 2 or 3 (05 §5.5.7), over the fixture pages as
/// `fixtures/loc_original.rs` has them: on some pages our OCR replaced
/// LoC's text, so `text` is ours (LoC's with its second half first) and
/// `text_loc` LoC's original, which only layout 3 searches (#925).
pub(crate) async fn original_text_state(layout: TextLayout) -> Arc<AppState> {
    let mut backend = MemoryBackend::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        let docs: Vec<PageDoc> = load_docs(id)
            .into_iter()
            .map(|d| {
                let mut v = serde_json::to_value(&d).unwrap();
                crate::loc_original::add(&mut v);
                serde_json::from_value(v).unwrap()
            })
            .collect();
        backend.add_index(id, docs);
    }
    let mut rd = refdata().await;
    rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
    rd.current.text_layout = Some(layout.version());
    Arc::new(AppState::new(config(), Arc::new(backend), rd))
}

/// A fixture page our OCR replaced LoC's text on, with no American Stories'
/// text, and two words that follow each other in LoC's original text and
/// not in ours, as a query (a phrase LoC's text has intact).
pub(crate) fn original_only_phrase() -> (PageDoc, String) {
    let words = |t: &str| -> Vec<String> {
        t.split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>()
    };
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        for d in load_docs(id) {
            let mut v = serde_json::to_value(&d).unwrap();
            if d.text_as.is_some() || !crate::loc_original::replaced(&v) {
                continue;
            }
            crate::loc_original::add(&mut v);
            let ours = words(v["text"].as_str().unwrap());
            let original = words(&d.text);
            for pair in original.windows(2) {
                let plain = pair
                    .iter()
                    .all(|w| w.len() > 2 && w.chars().all(char::is_alphabetic));
                if plain && !ours.windows(2).any(|o| o == pair) {
                    return (d, pair.join(" "));
                }
            }
        }
    }
    panic!("no replaced fixture page has a phrase of LoC's text that ours lacks");
}

/// On the layout that keeps LoC's original text (3, #925), a phrase LoC's
/// text of a replaced page has and ours lacks finds the page, and the hit
/// says it matched there, with the snippet from that text; layout 2 and
/// the page counts of a word in both texts are unchanged, as a page counts
/// once.
#[tokio::test]
async fn the_original_text_of_a_replaced_page_is_searched_and_reported() {
    let (doc, phrase) = original_only_phrase();
    let three = original_text_state(TextLayout::SingleWithLoc).await;
    let two = original_text_state(TextLayout::Single).await;
    let uri = format!(
        "/v1/hits?q=%22{}%22&lccn={}&limit=50",
        phrase.replace(' ', "+"),
        doc.lccn
    );
    let ids = |body: &Value| -> Vec<String> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| i["doc_id"].as_str().unwrap().to_owned())
            .collect()
    };
    // Layout 2 has only ours, in which the words are the other way round.
    let (status, _, body) = get(&two, &uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(!ids(&body).contains(&doc.doc_id), "{phrase}: {body}");
    // Layout 3 finds the page through LoC's text, and says so.
    let (status, _, body) = get(&three, &uri).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(ids(&body).contains(&doc.doc_id), "{phrase}: {body}");
    let item = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["doc_id"] == doc.doc_id.as_str())
        .unwrap();
    assert_eq!(item["matched_in"], json!(["loc_original"]), "{item}");
    assert_eq!(item["snippet_source"], "loc_original", "{item}");
    assert!(item["snippets"][0].as_str().unwrap().contains("<mark>"));

    // A word in both texts of the page: found once, and in both.
    let word = phrase.split(' ').next().unwrap();
    let uri = format!("/v1/hits?q={word}&lccn={}&limit=50", doc.lccn);
    let (_, _, body) = get(&three, &uri).await;
    let item = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["doc_id"] == doc.doc_id.as_str())
        .unwrap();
    assert_eq!(item["matched_in"], json!(["loc", "loc_original"]), "{item}");
    assert!(item.get("snippet_source").is_none(), "{item}");
    assert_eq!(
        ids(&body).iter().filter(|i| **i == doc.doc_id).count(),
        1,
        "{body}"
    );
    let (_, _, body2) = get(&two, &uri).await;
    assert_eq!(ids(&body), ids(&body2));
    // Layout 2 never names LoC's original text.
    assert!(!body2.to_string().contains("loc_original"), "{body2}");

    // The counts: a word the two texts share counts each page once, the
    // same as on layout 2; the phrase adds the page.
    for q in ["gold", word] {
        let (_, _, a3) = get(&three, &format!("/v1/aggregate?q={q}")).await;
        let (_, _, a2) = get(&two, &format!("/v1/aggregate?q={q}")).await;
        assert_eq!(a3["total"]["hits"], a2["total"]["hits"], "{q}");
        assert!(a3["total"].get("american_stories_only").is_none());
    }
    let q = format!("%22{}%22", phrase.replace(' ', "+"));
    let (_, _, a3) = get(&three, &format!("/v1/aggregate?q={q}")).await;
    let (_, _, a2) = get(&two, &format!("/v1/aggregate?q={q}")).await;
    assert!(a3["total"]["hits"].as_u64() > a2["total"]["hits"].as_u64());

    // The version says which layout it has.
    let (_, _, meta) = get(&three, "/v1/meta").await;
    assert_eq!(meta["text_layout"], 3);
    assert_eq!(meta["american_stories"], true);
}

/// A version with a text layout newer than the API's isn't loaded: its
/// fields may not be the ones a search would name.
#[tokio::test]
async fn a_text_layout_this_api_does_not_know_is_not_loaded() {
    let store = LocalStore::new(data_dir());
    let mut current = refdata().await.current;
    current.common_grams = Some(usnm_core::common_grams::VERSION);
    current.american_stories = Some(usnm_core::american_stories::VERSION);
    for known in [None, Some(1), Some(2), Some(3)] {
        current.text_layout = known;
        assert!(
            RefData::load_for(&store, current.clone()).await.is_ok(),
            "{known:?}"
        );
    }
    let mut newer = current.clone();
    newer.text_layout = Some(usnm_core::text_layout::TextLayout::LATEST.version() + 1);
    let err = RefData::load_for(&store, newer).await.unwrap_err();
    assert!(err.contains("text_layout"), "{err}");
    // One field for both texts holds words folded by the version's
    // analyzer, and American Stories' text of its version: both must be
    // ones the API knows, or queries would silently miss.
    for layout in [2, 3] {
        for (grams, stories) in [
            (None, Some(usnm_core::american_stories::VERSION)),
            (Some(usnm_core::common_grams::VERSION + 1), None),
            (
                Some(usnm_core::common_grams::VERSION),
                Some(usnm_core::american_stories::VERSION + 1),
            ),
        ] {
            let mut c = current.clone();
            c.text_layout = Some(layout);
            c.common_grams = grams;
            c.american_stories = stories;
            let err = RefData::load_for(&store, c.clone()).await.unwrap_err();
            assert!(err.contains(&format!("text_layout {layout}")), "{err}");
            // A field per text still loads, as before.
            c.text_layout = None;
            assert!(RefData::load_for(&store, c).await.is_ok());
        }
    }
}

/// The persistent cache too: a response persisted with American Stories'
/// text on is never read with it off, and the other way round.
#[tokio::test]
async fn persisted_responses_stay_with_their_american_stories_state() {
    let dir = std::env::temp_dir().join(format!("usnm-respcache-as-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store: Arc<LocalStore> = Arc::new(LocalStore::new(&dir));
    let state = |search: bool| {
        let store = store.clone();
        async move {
            let mut rd = refdata().await;
            rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
            let mut cfg = config();
            cfg.persist_after = Duration::ZERO;
            cfg.american_stories_search = search;
            Arc::new(AppState::new(cfg, Arc::new(fixture_backend()), rd).with_response_store(store))
        }
    };
    let wait_for = |n: usize| {
        let dir = dir.clone();
        async move {
            for _ in 0..100 {
                if persisted_files(&dir).len() == n {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            assert_eq!(persisted_files(&dir).len(), n);
        }
    };
    let uri = "/v1/aggregate?q=gold&v=fixture-v1";
    let (status, _, body) = get(&state(true).await, uri).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["total"]["american_stories_only"].is_u64());
    wait_for(1).await;
    // Off, with nothing in process: computed, not read, and persisted apart.
    let (status, _, body) = get(&state(false).await, uri).await;
    assert_eq!(status, StatusCode::OK);
    assert!(
        body["total"].get("american_stories_only").is_none(),
        "{body}"
    );
    wait_for(2).await;
    // Each state reads its own entry back, with an empty index.
    for (search, only) in [(true, true), (false, false)] {
        let mut rd = refdata().await;
        rd.current.american_stories = Some(usnm_core::american_stories::VERSION);
        let mut cfg = config();
        cfg.american_stories_search = search;
        let restarted = Arc::new(
            AppState::new(cfg, Arc::new(MemoryBackend::new()), rd)
                .with_response_store(store.clone()),
        );
        let (status, _, body) = get(&restarted, uri).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["total"]["american_stories_only"].is_u64(), only);
        assert!(body["total"]["hits"].as_u64().unwrap() > 0);
    }
    let _ = std::fs::remove_dir_all(&dir);
}
