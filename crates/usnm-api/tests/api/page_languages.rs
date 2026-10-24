//! Page languages (#289): the `lang` filter, its baselines and the version flag.

use std::collections::BTreeMap;
use std::io::BufRead;
use std::sync::Arc;

use axum::http::StatusCode;
use serde_json::{json, Value};
use usnm_api::refdata::RefData;
use usnm_api::AppState;
use usnm_core::text::tokenize;
use usnm_search::memory::{eval, MemoryBackend};
use usnm_search::PageDoc;
use usnm_store::LocalStore;

use crate::common::{config, data_dir, get, parse, refdata, state_with, temp_data_dir, FIXTURE};
use crate::page_language;

/// The fixture docs with their page languages (`fixtures/page_language.rs`).
fn page_language_docs(index_id: &str) -> Vec<PageDoc> {
    let path = data_dir().join("indexes").join(format!("{index_id}.jsonl"));
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| {
            let mut doc: Value = serde_json::from_str(&l.unwrap()).unwrap();
            page_language::add(&mut doc);
            serde_json::from_value(doc).unwrap()
        })
        .collect()
}

/// The fixture version with page languages: its documents have them, its
/// `current.json` says `page_language: version`, and its snapshot has
/// `page_language_baselines.json`, counted from the documents (each page
/// once, by its own languages).
async fn page_language_state(version: u32, side: Option<&str>) -> (Arc<AppState>, Vec<PageDoc>) {
    use sha2::Digest;
    let mut backend = MemoryBackend::new();
    let mut docs = Vec::new();
    for id in ["pages-base-fixture", "pages-delta-fixture-1"] {
        let d = page_language_docs(id);
        docs.extend(d.clone());
        backend.add_index(id, d);
    }
    let mut sets: BTreeMap<Vec<String>, BTreeMap<String, BTreeMap<u32, u32>>> = BTreeMap::new();
    for d in &docs {
        *sets
            .entry(d.page_language.clone())
            .or_default()
            .entry(d.place_id.clone())
            .or_default()
            .entry(d.day)
            .or_default() += 1;
    }
    let body = serde_json::to_vec(&json!({"sets": sets
        .into_iter()
        .map(|(languages, places)| json!({
            "languages": languages,
            "baselines": places
                .into_iter()
                .map(|(p, days)| (p, days.into_iter().collect::<Vec<_>>()))
                .collect::<BTreeMap<_, _>>(),
        }))
        .collect::<Vec<_>>()}))
    .unwrap();
    let dir = temp_data_dir(&format!(
        "page-language-{version}-{}",
        side.unwrap_or("default")
    ));
    std::fs::write(dir.join("fixture-v1/page_language_baselines.json"), &body).unwrap();
    let manifest = dir.join("fixture-v1/manifest.json");
    let mut m: Value = serde_json::from_str(&std::fs::read_to_string(&manifest).unwrap()).unwrap();
    let digest: String = sha2::Sha256::digest(&body)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    m["files"].as_array_mut().unwrap().push(json!({
        "path": "page_language_baselines.json", "sha256": digest, "bytes": body.len(),
    }));
    std::fs::write(&manifest, m.to_string()).unwrap();
    let current = dir.join("current.json");
    let mut c: Value = serde_json::from_str(&std::fs::read_to_string(&current).unwrap()).unwrap();
    c["page_language"] = version.into();
    std::fs::write(&current, c.to_string()).unwrap();
    let rd = RefData::load(&LocalStore::new(&dir)).await.unwrap();
    let mut cfg = config();
    if let Some(side) = side {
        cfg.page_language_filter = match side {
            "match" => usnm_search::PageLanguageSide::Match,
            _ => usnm_search::PageLanguageSide::Smaller,
        };
    }
    (Arc::new(AppState::new(cfg, Arc::new(backend), rd)), docs)
}

/// On a version with page languages at the API's version, `lang=` keeps
/// the pages written in any of the languages, a page in two found by
/// either; the baselines count pages by theirs, and the counts by language
/// are by page language. `/v1/meta` says so.
#[tokio::test]
async fn lang_filters_by_page_language_on_a_version_with_them() {
    let (s, docs) = page_language_state(usnm_core::page_language::VERSION, None).await;
    let (status, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(meta["language_filter"], "page");
    let pages_in = |l: &str| {
        docs.iter()
            .filter(|d| d.page_language.iter().any(|x| x == l))
            .count()
    };
    let langs = meta["languages"].as_array().unwrap();
    for l in langs {
        let code = l["code"].as_str().unwrap();
        assert_eq!(l["pages"], pages_in(code) as u64, "{code}");
    }
    // Most pages first: English, then German.
    assert_eq!(langs[0]["code"], "eng");
    assert_eq!(langs[1]["code"], "ger");

    let q = "gold";
    let node = parse(q).unwrap();
    let matching = |l: &[&str]| -> Vec<&PageDoc> {
        docs.iter()
            .filter(|d| {
                d.page_language.iter().any(|x| l.contains(&x.as_str()))
                    && eval(&node, &tokenize(&d.text, FIXTURE))
            })
            .collect()
    };
    for lang in ["eng", "ger", "spa", "fre", "ger,spa"] {
        let (status, _, body) = get(&s, &format!("/v1/aggregate?q={q}&lang={lang}")).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        let l: Vec<&str> = lang.split(',').collect();
        let want = matching(&l);
        assert_eq!(body["total"]["hits"], want.len() as u64, "{lang}");
        assert_eq!(body["languages"]["by"], "page", "{lang}");
        // A matching page counts in each of its own languages.
        let mut by_lang: BTreeMap<String, u64> = BTreeMap::new();
        for d in &want {
            for x in &d.page_language {
                *by_lang.entry(x.clone()).or_default() += 1;
            }
        }
        let got: BTreeMap<String, u64> = body["languages"]["code"]
            .as_array()
            .unwrap()
            .iter()
            .zip(body["languages"]["hits"].as_array().unwrap())
            .map(|(c, h)| (c.as_str().unwrap().to_owned(), h.as_u64().unwrap()))
            .collect();
        assert_eq!(got, by_lang, "{lang}");
        // The baseline: the pages written in the languages, each once.
        let baseline = docs
            .iter()
            .filter(|d| d.page_language.iter().any(|x| l.contains(&x.as_str())))
            .count() as u64;
        assert_eq!(body["total"]["baseline_pages"], baseline, "{lang}");
    }
    // Without `lang`, an English query's baseline is the English pages (#237).
    let (_, _, body) = get(&s, "/v1/aggregate?q=gold").await;
    assert_eq!(body["baseline"]["languages"], json!(["eng"]));
    assert_eq!(body["total"]["baseline_pages"], pages_in("eng") as u64);
}

/// Rendered as the complement or as the match side, a page-language filter
/// finds the same pages.
#[tokio::test]
async fn both_sides_of_a_page_language_filter_answer_alike() {
    let (smaller, _) =
        page_language_state(usnm_core::page_language::VERSION, Some("smaller")).await;
    let (matching, _) = page_language_state(usnm_core::page_language::VERSION, Some("match")).await;
    let eng = vec!["eng".to_owned()];
    let side = |s: &Arc<AppState>| s.snapshot.load().refdata.index_set().language_match(&eng);
    assert!(matches!(
        side(&smaller),
        Some(usnm_search::LanguageMatch::NotPageSets(_))
    ));
    assert_eq!(
        side(&matching),
        Some(usnm_search::LanguageMatch::Page(eng.clone()))
    );
    for uri in [
        "/v1/aggregate?q=gold&lang=eng",
        "/v1/aggregate?q=railroad&lang=eng,ger",
        "/v1/aggregate?q=cotton&lang=ger",
        "/v1/hits?q=gold&lang=eng&limit=20",
    ] {
        let (_, _, mut a) = get(&smaller, uri).await;
        let (_, _, mut b) = get(&matching, uri).await;
        for body in [&mut a, &mut b] {
            if let Some(o) = body.as_object_mut() {
                o.remove("timing_ms");
            }
        }
        assert_eq!(a, b, "{uri}");
    }
}

/// The Japanese index set uses page languages too on such a version, so a
/// Japanese search's `lang=`, counts and baselines agree (its pages carry
/// the languages the baselines count them in); never on a title-language
/// version.
#[tokio::test]
async fn the_japanese_index_set_follows_the_version_s_language_mode() {
    let ja =
        json!({"indexes": ["pages-ja-fixture"], "fold": usnm_core::ja::FOLD_VERSION, "pages": 1});
    let mut rd = refdata().await;
    rd.current.ja = serde_json::from_value(ja.clone()).unwrap();
    assert!(rd.ja_index_set().unwrap().page_languages().is_none());
    // The page-language version's snapshot, its pointer naming a Japanese index.
    let _ = page_language_state(usnm_core::page_language::VERSION, Some("ja")).await;
    let dir = std::env::temp_dir().join(format!(
        "usnm-page-language-{}-ja-{}",
        usnm_core::page_language::VERSION,
        std::process::id()
    ));
    let current = dir.join("current.json");
    let mut c: Value = serde_json::from_str(&std::fs::read_to_string(&current).unwrap()).unwrap();
    c["ja"] = ja;
    std::fs::write(&current, c.to_string()).unwrap();
    let rd = RefData::load(&LocalStore::new(&dir)).await.unwrap();
    let set = rd.ja_index_set().unwrap();
    assert!(set.page_languages().is_some());
    assert_eq!(set.language_field(), "page_language");
    let ger = vec!["ger".to_owned()];
    assert_eq!(
        set.language_match(&ger),
        Some(usnm_search::LanguageMatch::Page(ger.clone()))
    );
}

/// Page languages of another version than the API's are left alone:
/// `lang=` means title language, as before the setting.
#[tokio::test]
async fn page_languages_of_another_version_are_not_used() {
    let (s, _) = page_language_state(usnm_core::page_language::VERSION + 1, None).await;
    let (_, _, meta) = get(&s, "/v1/meta").await;
    assert_eq!(meta["language_filter"], "title");
    let (status, _, body) = get(&s, "/v1/aggregate?q=gold&lang=ger").await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(body["languages"].get("by").is_none(), "{body}");
    // The title-language baseline: P00002 and P00006.
    assert_eq!(body["total"]["baseline_pages"], 2 * 312);
    // And the fixture version itself: title language.
    let (_, _, meta) = get(&state_with(None).await, "/v1/meta").await;
    assert_eq!(meta["language_filter"], "title");
}
