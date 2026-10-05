//! Spike S-2: the Quickwit backend must return exactly what the in-memory
//! reference backend returns on the fixture corpus.
//!
//! Runs only when `QUICKWIT_URL` points at a Quickwit 0.9 node that has the
//! fixture indexes loaded (`scripts/quickwit-fixtures.sh` does both); otherwise
//! every test returns early. CI runs it in the `quickwit` job.

use std::io::BufRead;
use std::path::PathBuf;
use std::time::Duration;

use chrono::NaiveDate;
use usnm_core::params::Filters;
use usnm_core::query::{build, parse, Mode, Node};
use usnm_core::time::{BucketSpec, BucketUnit};
use usnm_search::memory::MemoryBackend;
use usnm_search::quickwit::QuickwitBackend;
use usnm_search::{
    CubeCell, HitSort, HitsQuery, IndexSet, PageDoc, SearchBackend, SearchError, Summary,
};

const INDEXES: [&str; 2] = ["pages-base-fixture", "pages-delta-fixture-1"];
/// The Japanese pages (#139), searched on their own.
const JA_INDEX: &str = "pages-ja-fixture";

fn quickwit() -> Option<QuickwitBackend> {
    let url = std::env::var("QUICKWIT_URL")
        .ok()
        .filter(|u| !u.is_empty())?;
    Some(QuickwitBackend::new(&url, Duration::from_secs(30)).expect("client"))
}

fn memory() -> MemoryBackend {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data/indexes");
    let mut m = MemoryBackend::new();
    for id in INDEXES.iter().chain([&JA_INDEX]) {
        let file = std::fs::File::open(dir.join(format!("{id}.jsonl"))).expect("fixture");
        let docs: Vec<PageDoc> = std::io::BufReader::new(file)
            .lines()
            .map(|l| serde_json::from_str(&l.expect("line")).expect("doc"))
            .collect();
        m.add_index(id, docs);
    }
    m
}

fn d(s: &str) -> NaiveDate {
    NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
}

fn filters(from: &str, to: &str) -> Filters {
    Filters {
        from: d(from),
        to: d(to),
        states: vec![],
        lccns: vec![],
        langs: vec![],
        front_only: false,
    }
}

fn queries() -> Vec<(&'static str, Node)> {
    vec![
        ("term", parse("gold").unwrap()),
        ("phrase", parse(r#""cross of gold""#).unwrap()),
        (
            "phrase with stop word",
            parse(r#""board of health""#).unwrap(),
        ),
        ("sloppy phrase", parse(r#""fever port"~4"#).unwrap()),
        ("and", parse("free silver").unwrap()),
        ("or", parse("(yellow OR orator)").unwrap()),
        ("not", parse("gold -standard").unwrap()),
        ("prefix", parse("silv*").unwrap()),
        (
            "mode any",
            build("bankers orator", Some(Mode::Any), 0, 0).unwrap(),
        ),
        (
            "mode near",
            build("speaker convention", Some(Mode::Near), 3, 0).unwrap(),
        ),
        ("no match", parse("zyzzyva").unwrap()),
    ]
}

fn filter_cases() -> Vec<(&'static str, Filters)> {
    let all = filters("1895-01-01", "1897-12-31");
    let mut states = all.clone();
    states.states = vec!["GA".into(), "SC".into()];
    let mut lccn = all.clone();
    lccn.lccns = vec!["sn99000002".into()];
    let mut front = all.clone();
    front.front_only = true;
    let mut lang = all.clone();
    lang.langs = vec!["eng".into()];
    vec![
        ("all", all),
        ("1896 H2", filters("1896-06-01", "1896-12-31")),
        ("states", states),
        ("lccn", lccn),
        ("front pages", front),
        ("language", lang),
    ]
}

fn specs(f: &Filters) -> Vec<BucketSpec> {
    let mut out = vec![
        BucketSpec::new(BucketUnit::Year, f.from, f.to),
        BucketSpec::new(BucketUnit::Month, f.from, f.to),
        BucketSpec::new(BucketUnit::Week, f.from, f.to),
    ];
    // Day buckets over a short window, with a start that isn't a week boundary.
    out.push(BucketSpec::new(
        BucketUnit::Day,
        d("1896-07-03"),
        d("1896-09-17"),
    ));
    out
}

fn sorted(mut s: Summary) -> Summary {
    s.places.sort_by(|a, b| a.place_id.cmp(&b.place_id));
    s
}

fn sorted_cells(mut c: Vec<CubeCell>) -> Vec<CubeCell> {
    c.sort_by(|a, b| (&a.place_id, a.bucket).cmp(&(&b.place_id, b.bucket)));
    c
}

#[tokio::test]
async fn summaries_and_cubes_match_the_reference_backend() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let set = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    let mut checked = 0;
    for (qname, q) in queries() {
        for (fname, f) in filter_cases() {
            for spec in specs(&f) {
                let mut f = f.clone();
                // Day specs use their own window.
                f.from = spec.from;
                f.to = spec.to;
                let ctx = format!("{qname} / {fname} / {:?}", spec.unit);
                let want = sorted(mem.summary(&set, &q, &f, &spec).await.unwrap());
                let got = sorted(qw.summary(&set, &q, &f, &spec).await.expect(&ctx));
                assert_eq!(got, want, "summary: {ctx}");

                let all: Vec<u8> = (0..8).collect();
                let want = sorted_cells(mem.cube(&set, &q, &f, &spec, &all).await.unwrap());
                let got = sorted_cells(qw.cube(&set, &q, &f, &spec, &all).await.expect(&ctx));
                assert_eq!(got, want, "cube: {ctx}");

                // Sharded cubes must partition the full cube exactly.
                let mut parts = Vec::new();
                for shards in [vec![0u8, 2, 4, 6], vec![1, 3, 5, 7]] {
                    parts.extend(qw.cube(&set, &q, &f, &spec, &shards).await.expect(&ctx));
                }
                assert_eq!(sorted_cells(parts), want, "sharded cube: {ctx}");
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "only {checked} cases ran");
}

#[tokio::test]
async fn hits_pages_match_the_reference_backend() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let set = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    let f = filters("1895-01-01", "1897-12-31");
    for (qname, q) in queries() {
        for selector in [
            HitsQuery {
                place_id: Some("P00001".into()),
                ..Default::default()
            },
            HitsQuery {
                lccn: Some("sn99000004".into()),
                ..Default::default()
            },
            HitsQuery {
                place_id: Some("P00001".into()),
                sort: HitSort::Newest,
                ..Default::default()
            },
            // Every match: how the aggregate finds the first and last page.
            HitsQuery::default(),
            HitsQuery {
                sort: HitSort::Newest,
                ..Default::default()
            },
        ] {
            for offset in [0, 5, 30] {
                let page = HitsQuery {
                    offset,
                    limit: 7,
                    ..selector.clone()
                };
                let ctx = format!("{qname} / {selector:?} / offset {offset}");
                let want = mem.hits(&set, &q, &f, &page).await.unwrap();
                let got = qw.hits(&set, &q, &f, &page).await.expect(&ctx);
                assert_eq!(got.total, want.total, "total: {ctx}");
                // Distinct days (#127), on the first page only. Quickwit's is a
                // HyperLogLog estimate, exact at the fixtures' size.
                assert_eq!(got.days, want.days, "days: {ctx}");
                assert_eq!(got.days.is_some(), offset == 0, "days: {ctx}");
                let key = |h: &usnm_search::Hit| {
                    (
                        h.doc_id.clone(),
                        h.day,
                        h.lccn.clone(),
                        h.place_id.clone(),
                        h.edition,
                        h.seq,
                        h.front_page,
                        // Both build snippets from the page text (#126).
                        h.snippets.clone(),
                    )
                };
                assert_eq!(
                    got.hits.iter().map(key).collect::<Vec<_>>(),
                    want.hits.iter().map(key).collect::<Vec<_>>(),
                    "hits: {ctx}"
                );
                // Snippets: highlighted and safe (text escaped, only <mark> tags).
                for h in &got.hits {
                    for s in &h.snippets {
                        let stripped = s.replace("<mark>", "").replace("</mark>", "");
                        assert!(
                            !stripped.contains('<') && !stripped.contains('>'),
                            "unescaped snippet: {s}"
                        );
                    }
                }
                if qname == "phrase" && !got.hits.is_empty() {
                    assert!(got
                        .hits
                        .iter()
                        .any(|h| h.snippets.iter().any(|s| s.contains("<mark>"))));
                }
            }
        }
    }
}

/// "Most mentions first" (#126): Quickwit's score weighs rare words per
/// split, so its order isn't the reference's; the pages listed are the same.
#[tokio::test]
async fn relevant_hits_list_the_same_pages() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let set = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    let f = filters("1895-01-01", "1897-12-31");
    for (qname, q) in queries() {
        let mut pages = Vec::new();
        for backend in [&mem as &dyn SearchBackend, &qw] {
            let mut all = Vec::new();
            let mut offset = 0;
            loop {
                let page = HitsQuery {
                    sort: HitSort::Relevant,
                    offset,
                    limit: 500,
                    ..Default::default()
                };
                let got = backend.hits(&set, &q, &f, &page).await.expect(qname);
                let n = got.hits.len();
                all.extend(got.hits.into_iter().map(|h| (h.doc_id, h.snippets)));
                offset += n;
                if n == 0 || offset as u64 >= got.total {
                    break;
                }
            }
            all.sort();
            pages.push(all);
        }
        assert_eq!(pages[1], pages[0], "relevant: {qname}");
    }
}

#[tokio::test]
async fn fuzzy_terms_match_the_reference_backend_or_are_refused() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let set = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    let f = filters("1895-01-01", "1897-12-31");
    let spec = BucketSpec::new(BucketUnit::Month, f.from, f.to);
    // OCR-style misspellings of words in the corpus.
    for (q, fuzzy) in [("silvr", 1), ("fevre", 2), ("convention", 1)] {
        let node = build(q, Some(Mode::All), 0, fuzzy).unwrap();
        match qw.summary(&set, &node, &f, &spec).await {
            Ok(got) => {
                assert!(
                    qw.capabilities().fuzzy,
                    "fuzzy query ran but capability is off"
                );
                let want = mem.summary(&set, &node, &f, &spec).await.unwrap();
                assert_eq!(sorted(got), sorted(want), "fuzzy {q}~{fuzzy}");
            }
            Err(SearchError::Unsupported(_)) => assert!(!qw.capabilities().fuzzy),
            Err(e) => panic!("fuzzy {q}~{fuzzy}: {e}"),
        }
    }
}

#[tokio::test]
async fn prepare_accepts_published_indexes_and_refuses_missing_ones() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let set = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    qw.prepare(&set).await.expect("fixture indexes");
    let missing = IndexSet::new(vec![
        "pages-base-fixture".into(),
        "pages-delta-missing".into(),
    ]);
    assert!(matches!(
        qw.prepare(&missing).await,
        Err(SearchError::Backend(_))
    ));
}

/// Japanese words, phrases, folding, mixed Latin, NEAR and filters on the
/// Japanese index (#139): counts, series, hits and snippets as the memory
/// backend has them. Also checks that Quickwit's query grammar takes bare
/// CJK terms and phrases, and the `whitespace` tokenizer keeps positions.
#[tokio::test]
async fn japanese_searches_match_the_reference_backend() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let set = IndexSet::new(vec![JA_INDEX.to_owned()]);
    let all = filters("1895-01-01", "1897-12-31");
    let mut jpn = filters("1896-01-01", "1896-12-31");
    jpn.langs = vec!["jpn".into()];
    let mut ca = filters("1895-01-01", "1897-12-31");
    ca.states = vec!["CA".into()];
    let queries: Vec<(&str, Node)> = vec![
        ("char", parse("年").unwrap()),
        ("word", parse("戦争").unwrap()),
        ("old form", parse("戰爭").unwrap()),
        ("small kana", parse("がつこう").unwrap()),
        ("katakana", parse("ニュース").unwrap()),
        ("phrase", parse("\"米国と日本\"").unwrap()),
        ("and", parse("東京 選挙").unwrap()),
        ("or", parse("戦争 OR 選挙").unwrap()),
        ("not", parse("日本 -戦争").unwrap()),
        ("mixed latin", parse("denver 日本").unwrap()),
        ("punctuation", parse("東京、平和").unwrap()),
        ("near", build("米国 日本", Some(Mode::Near), 3, 0).unwrap()),
        ("any", build("戦争 選挙", Some(Mode::Any), 0, 0).unwrap()),
    ];
    let mut nonzero = 0;
    for (qname, q) in &queries {
        for (fname, f) in [("all", &all), ("jpn 1896", &jpn), ("CA", &ca)] {
            for spec in specs(f) {
                let mut f = f.clone();
                // Day specs use their own window.
                f.from = spec.from;
                f.to = spec.to;
                let ctx = format!("{qname} / {fname} / {:?}", spec.unit);
                let want = sorted(mem.summary(&set, q, &f, &spec).await.unwrap());
                let got = sorted(qw.summary(&set, q, &f, &spec).await.expect(&ctx));
                assert_eq!(got, want, "summary: {ctx}");
                nonzero += usize::from(want.total_hits > 0);
                let all: Vec<u8> = (0..8).collect();
                let want = sorted_cells(mem.cube(&set, q, &f, &spec, &all).await.unwrap());
                let got = sorted_cells(qw.cube(&set, q, &f, &spec, &all).await.expect(&ctx));
                assert_eq!(got, want, "cube: {ctx}");
            }
            let page = HitsQuery {
                limit: 10,
                ..Default::default()
            };
            let want = mem.hits(&set, q, f, &page).await.unwrap();
            let got = qw.hits(&set, q, f, &page).await.expect(qname);
            assert_eq!(got.total, want.total, "hits total: {qname} / {fname}");
            let key = |h: &usnm_search::Hit| {
                let ocr = (h.ocr_source.clone(), h.ocr_engine.clone());
                (h.doc_id.clone(), h.snippets.clone(), ocr)
            };
            assert_eq!(
                got.hits.iter().map(key).collect::<Vec<_>>(),
                want.hits.iter().map(key).collect::<Vec<_>>(),
                "hits: {qname} / {fname}"
            );
        }
    }
    assert!(
        nonzero > 20,
        "only {nonzero} Japanese cases matched anything"
    );
}
