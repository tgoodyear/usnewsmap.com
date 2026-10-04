//! The Japanese fixture index (#139): its documents are tokenized exactly as
//! usnm_core::ja does it, the memory backend finds Japanese words through
//! old-form folding, and snippets show the text as printed.

use std::io::BufRead;
use std::path::PathBuf;

use chrono::NaiveDate;
use usnm_core::ja;
use usnm_core::params::Filters;
use usnm_core::query::parse;
use usnm_search::memory::MemoryBackend;
use usnm_search::{HitsQuery, IndexSet, PageDoc, SearchBackend};

const JA: &str = "pages-ja-fixture";

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn docs() -> Vec<PageDoc> {
    let file =
        std::fs::File::open(root().join(format!("fixtures/data/indexes/{JA}.jsonl"))).unwrap();
    std::io::BufReader::new(file)
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

fn all_dates() -> Filters {
    let d = |s| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    Filters {
        from: d("1895-01-01"),
        to: d("1897-12-31"),
        states: vec![],
        lccns: vec![],
        langs: vec![],
        front_only: false,
    }
}

#[test]
fn fixture_text_is_the_rust_tokenization_of_the_printed_text() {
    let docs = docs();
    assert!(docs.len() > 40);
    for d in &docs {
        let printed = d.printed.as_deref().expect("printed");
        assert_eq!(d.text, ja::index_text(printed), "{}", d.doc_id);
        assert_eq!(d.ocr_source.as_deref(), Some("usnm-ndlocr-lite"));
    }
}

#[test]
fn the_japanese_template_indexes_like_the_main_one() {
    let read = |f: &str| std::fs::read_to_string(root().join("infra/quickwit").join(f)).unwrap();
    let settings = |t: &str| t[t.find("indexing_settings:").unwrap()..].to_owned();
    assert_eq!(
        settings(&read("pages-ja-index.yaml")),
        settings(&read("pages-index.yaml"))
    );
}

#[tokio::test]
async fn japanese_words_match_through_folding_and_snippets_show_the_printed_text() {
    let docs = docs();
    let mut mem = MemoryBackend::new();
    mem.add_index(JA, docs.clone());
    let set = IndexSet::new(vec![JA.to_owned()]);
    let f = all_dates();
    let page = HitsQuery {
        limit: 100,
        ..Default::default()
    };

    // 戦争 (modern) and 戰爭 (as printed) are the same search.
    let printed_has = |w: &str| {
        let q = vec![ja::tokenize(w)];
        docs.iter()
            .filter(|d| !ja::find(d.printed.as_deref().unwrap(), &q).is_empty())
            .count() as u64
    };
    let war = printed_has("戰爭");
    assert!(war > 0);
    for q in ["戦争", "戰爭"] {
        let hits = mem.hits(&set, &parse(q).unwrap(), &f, &page).await.unwrap();
        assert_eq!(hits.total, war, "{q}");
        for h in &hits.hits {
            assert!(
                h.snippets.iter().any(|s| s.contains("<mark>戰爭</mark>")),
                "{:?}",
                h.snippets
            );
            assert_eq!(h.ocr_source.as_deref(), Some("usnm-ndlocr-lite"));
        }
    }
    // Small kana fold: がつこう finds がっこう.
    let school = mem
        .hits(&set, &parse("がつこう").unwrap(), &f, &page)
        .await
        .unwrap();
    assert_eq!(school.total, printed_has("がっこう"));
    assert!(school.total > 0);
    // Latin words on Japanese pages fold as the main index does.
    let denver = mem
        .hits(&set, &parse("DENVER 日本").unwrap(), &f, &page)
        .await
        .unwrap();
    assert!(denver.total > 0 && denver.total < docs.len() as u64);
    // A phrase across a particle.
    let p = mem
        .hits(&set, &parse("\"米国と日本\"").unwrap(), &f, &page)
        .await
        .unwrap();
    assert_eq!(p.total, printed_has("米國と日本"));
}
