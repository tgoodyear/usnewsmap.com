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
        // American Stories' text (05 §5.5.4): words only in its headlines,
        // words each OCR misread where the other didn't, and a query whose
        // words are in different texts of one page.
        ("only in american stories", parse("bimetallism").unwrap()),
        (
            "phrase only in american stories",
            parse(r#""boy orator""#).unwrap(),
        ),
        (
            "near only in american stories",
            build("bryan chicago", Some(Mode::Near), 2, 0).unwrap(),
        ),
        ("not in american stories", parse("gold -bryan").unwrap()),
        ("misread by loc", parse("goid").unwrap()),
        ("misread by american stories", parse("golcl").unwrap()),
        ("prefix in american stories", parse("bimet*").unwrap()),
        ("across texts", parse("bryan speaker").unwrap()),
    ]
}

/// The main indexes searched in LoC's text alone, and in both texts
/// (05 §5.5.4).
fn sets() -> Vec<(&'static str, IndexSet)> {
    let loc = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    let both = loc.clone().with_american_stories(true);
    vec![("loc", loc), ("both texts", both)]
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
    let mut checked = 0;
    for ((sname, set), (qname, q)) in sets()
        .into_iter()
        .flat_map(|s| queries().into_iter().map(move |q| (s.clone(), q)))
    {
        let set = &set;
        for (fname, f) in filter_cases() {
            for spec in specs(&f) {
                let mut f = f.clone();
                // Day specs use their own window.
                f.from = spec.from;
                f.to = spec.to;
                let ctx = format!("{sname} / {qname} / {fname} / {:?}", spec.unit);
                let want = sorted(mem.summary(set, &q, &f, &spec).await.unwrap());
                let got = sorted(qw.summary(set, &q, &f, &spec).await.expect(&ctx));
                assert_eq!(got, want, "summary: {ctx}");

                let all: Vec<u8> = (0..8).collect();
                let want = sorted_cells(mem.cube(set, &q, &f, &spec, &all).await.unwrap());
                let got = sorted_cells(qw.cube(set, &q, &f, &spec, &all).await.expect(&ctx));
                assert_eq!(got, want, "cube: {ctx}");

                // Sharded cubes must partition the full cube exactly.
                let mut parts = Vec::new();
                for shards in [vec![0u8, 2, 4, 6], vec![1, 3, 5, 7]] {
                    parts.extend(qw.cube(set, &q, &f, &spec, &shards).await.expect(&ctx));
                }
                assert_eq!(sorted_cells(parts), want, "sharded cube: {ctx}");

                // A cube of some places (`/v1/days`) is the full cube's cells
                // for those places, one with no pages included.
                let places = vec![
                    "P00002".to_owned(),
                    "P00005".to_owned(),
                    "P99999".to_owned(),
                ];
                let want: Vec<CubeCell> = want
                    .iter()
                    .filter(|c| places.contains(&c.place_id))
                    .cloned()
                    .collect();
                let got = qw
                    .place_cube(set, &q, &f, &spec, &places)
                    .await
                    .expect(&ctx);
                assert_eq!(sorted_cells(got), want, "place cube: {ctx}");
                let mem_got = mem.place_cube(set, &q, &f, &spec, &places).await.unwrap();
                assert_eq!(sorted_cells(mem_got), want, "memory place cube: {ctx}");
                checked += 1;
            }
        }
    }
    assert!(checked > 100, "only {checked} cases ran");
}

/// The fixtures exercise American Stories' text: some pages match only
/// there, and only when the index set searches it.
#[tokio::test]
async fn american_stories_text_adds_pages_only_when_searched() {
    let mem = memory();
    let f = filters("1895-01-01", "1897-12-31");
    let spec = BucketSpec::new(BucketUnit::Year, f.from, f.to);
    let total = |set: IndexSet, q: &str| {
        let (mem, f, spec, q) = (&mem, &f, &spec, parse(q).unwrap());
        async move { mem.summary(&set, &q, f, spec).await.unwrap().total_hits }
    };
    let [(_, loc), (_, both)] = <[_; 2]>::try_from(sets()).ok().unwrap();
    // Only American Stories has these: its headlines, its misreading.
    for q in ["bimetallism", r#""boy orator""#, "bryan", "golcl"] {
        assert_eq!(total(loc.clone(), q).await, 0, "{q}");
        assert!(total(both.clone(), q).await > 0, "{q}");
    }
    // It reads some words LoC misread.
    for q in ["gold", "silver"] {
        assert!(
            total(both.clone(), q).await > total(loc.clone(), q).await,
            "{q}"
        );
    }
    // LoC's misreading: no more pages.
    assert!(total(loc.clone(), "goid").await > 0);
    assert_eq!(
        total(both.clone(), "goid").await,
        total(loc.clone(), "goid").await
    );
    assert!(total(both.clone(), "gold -bryan").await < total(both.clone(), "gold").await);
}

/// The aggregate's count of pages only American Stories' text matches
/// (05 §5.5.4) is the reference's.
#[tokio::test]
async fn american_stories_only_counts_match_the_reference_backend() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let [_, (_, both)] = <[_; 2]>::try_from(sets()).ok().unwrap();
    let mut found = 0;
    for (qname, q) in queries() {
        for (fname, f) in filter_cases() {
            let ctx = format!("{qname} / {fname}");
            let want = mem.american_stories_only(&both, &q, &f).await.unwrap();
            let got = qw.american_stories_only(&both, &q, &f).await.expect(&ctx);
            assert_eq!(got, want, "{ctx}");
            found += got;
        }
    }
    assert!(found > 0, "no page matched only in American Stories' text");
}

/// The count is the number of hits whose `matched_in` is American Stories'
/// text alone, for every query shape, `NOT` and words from both texts
/// included.
#[tokio::test]
async fn american_stories_only_counts_the_hits_matched_only_there() {
    let mem = memory();
    let [_, (_, both)] = <[_; 2]>::try_from(sets()).ok().unwrap();
    let all = HitsQuery {
        limit: 10_000,
        ..HitsQuery::default()
    };
    let mut found = 0;
    for (qname, q) in queries() {
        for (fname, f) in filter_cases() {
            let hits = mem.hits(&both, &q, &f, &all).await.unwrap();
            assert_eq!(hits.hits.len() as u64, hits.total);
            let only = hits
                .hits
                .iter()
                .filter(|h| h.matched_in.as_deref() == Some(&["american_stories"][..]))
                .count() as u64;
            let counted = mem.american_stories_only(&both, &q, &f).await.unwrap();
            assert_eq!(counted, only, "{qname} / {fname}");
            found += only;
        }
    }
    assert!(found > 0, "no page matched only in American Stories' text");
}

#[tokio::test]
async fn hits_pages_match_the_reference_backend() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let f = filters("1895-01-01", "1897-12-31");
    let mut from_american_stories = 0;
    for ((sname, set), (qname, q)) in sets()
        .into_iter()
        .flat_map(|s| queries().into_iter().map(move |q| (s.clone(), q)))
    {
        let set = &set;
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
                    days: offset == 0,
                    ..selector.clone()
                };
                let ctx = format!("{sname} / {qname} / {selector:?} / offset {offset}");
                let want = mem.hits(set, &q, &f, &page).await.unwrap();
                let got = qw.hits(set, &q, &f, &page).await.expect(&ctx);
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
                        // Both build snippets from the page text (#126),
                        // or American Stories' when only it matches.
                        h.snippets.clone(),
                        h.snippet_source,
                        // Which texts match, from the same stored texts.
                        h.matched_in.clone(),
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
                from_american_stories += got
                    .hits
                    .iter()
                    .filter(|h| h.snippet_source.is_some())
                    .count();
                assert!(
                    got.hits
                        .iter()
                        .all(|h| h.matched_in.is_some() == set.american_stories()),
                    "matched_in: {ctx}"
                );
                if qname == "phrase" && !got.hits.is_empty() {
                    assert!(got
                        .hits
                        .iter()
                        .any(|h| h.snippets.iter().any(|s| s.contains("<mark>"))));
                }
            }
        }
    }
    assert!(
        from_american_stories > 0,
        "no snippets from American Stories' text"
    );
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
    let f = filters("1895-01-01", "1897-12-31");
    for ((sname, set), (qname, q)) in sets()
        .into_iter()
        .flat_map(|s| queries().into_iter().map(move |q| (s.clone(), q)))
    {
        let set = &set;
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
                let got = backend.hits(set, &q, &f, &page).await.expect(qname);
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
        assert_eq!(pages[1], pages[0], "relevant: {sname} / {qname}");
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

/// Phrases holding common words search `text_cg` (05 §5.5.3) when the
/// indexes have it: the counts, cubes, edges and hits must be exactly the
/// exact phrase's, which the reference backend finds word by word.
#[tokio::test]
async fn phrases_through_common_word_pairs_match_the_reference_backend() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    let mem = memory();
    let ids: Vec<String> = INDEXES.iter().map(|s| (*s).to_owned()).collect();
    let mut matched = 0;
    // LoC's text alone, then both texts through both texts' pairs (05 §5.5.4).
    for american_stories in [false, true] {
        let set = IndexSet::new(ids.clone()).with_american_stories(american_stories);
        let grams = set.clone().with_common_grams(true);
        matched += common_word_phrases(&qw, &mem, &set, &grams).await;
    }
    assert!(matched >= 10, "only {matched} phrases matched the fixtures");
}

/// How many of the phrases matched: Quickwit through the pairs (`grams`)
/// against the memory backend on the same texts (`set`).
async fn common_word_phrases(
    qw: &QuickwitBackend,
    mem: &MemoryBackend,
    set: &IndexSet,
    grams: &IndexSet,
) -> usize {
    let phrases = [
        r#""cross of gold""#,
        r#""the friends of free""#,
        r#""of the railroad""#,
        r#""friends of free silver""#,
        r#""of the the cotton""#,
        r#""the cross of gold""#,
        r#""of the gold of""#,
        r#""cross of the gold""#,
        r#""cross of gold" -silver"#,
        r#""cross of gold" OR "the friends of free""#,
        // In American Stories' headlines only.
        r#""orator of the platte""#,
        r#""the boy orator""#,
        r#""quarantine at the port""#,
    ];
    let f = filters("1895-01-01", "1897-12-31");
    let spec = BucketSpec::new(BucketUnit::Month, f.from, f.to);
    let all: Vec<u8> = (0..8).collect();
    let mut matched = 0;
    for p in phrases {
        let q = parse(p).unwrap();
        let ctx = format!("{p} / american stories {}", set.american_stories());
        let want = sorted(mem.summary(set, &q, &f, &spec).await.unwrap());
        let got = sorted(qw.summary(grams, &q, &f, &spec).await.expect(&ctx));
        assert_eq!(got, want, "summary: {ctx}");
        if want.total_hits > 0 {
            matched += 1;
        }
        let want = sorted_cells(mem.cube(set, &q, &f, &spec, &all).await.unwrap());
        let got = sorted_cells(qw.cube(grams, &q, &f, &spec, &all).await.expect(&ctx));
        assert_eq!(got, want, "cube: {ctx}");
        // The pages only American Stories' text matches, through the pairs.
        if set.american_stories() {
            let want = mem.american_stories_only(set, &q, &f).await.unwrap();
            let got = qw.american_stories_only(grams, &q, &f).await.expect(&ctx);
            assert_eq!(got, want, "american stories only: {ctx}");
        }
        for sort in [HitSort::Oldest, HitSort::Newest] {
            let page = HitsQuery {
                sort,
                limit: 20,
                ..HitsQuery::default()
            };
            let want = mem.hits(set, &q, &f, &page).await.unwrap();
            let got = qw.hits(grams, &q, &f, &page).await.expect(&ctx);
            assert_eq!(got.total, want.total, "hits total: {ctx}");
            let ids = |h: &usnm_search::HitsPage| -> Vec<String> {
                h.hits.iter().map(|h| h.doc_id.clone()).collect()
            };
            assert_eq!(ids(&got), ids(&want), "hits: {ctx}");
            // The snippets still mark the phrase's words in `text`, or in
            // `text_as` when only it has the phrase.
            assert_eq!(
                got.hits.iter().map(|h| &h.snippets).collect::<Vec<_>>(),
                want.hits.iter().map(|h| &h.snippets).collect::<Vec<_>>(),
                "snippets: {ctx}"
            );
            assert_eq!(
                got.hits.iter().map(|h| &h.matched_in).collect::<Vec<_>>(),
                want.hits.iter().map(|h| &h.matched_in).collect::<Vec<_>>(),
                "matched_in: {ctx}"
            );
            if want.total > 0 {
                assert!(
                    got.hits
                        .iter()
                        .any(|h| h.snippets.iter().any(|s| s.contains("<mark>"))),
                    "no highlights: {ctx}"
                );
            }
        }
    }
    matched
}

/// The searcher's cache metrics have the names and labels the API reads
/// (#125): after a search over both indexes, each split's footer is cached.
#[tokio::test]
async fn cache_metrics_report_the_split_footers() {
    let Some(qw) = quickwit() else {
        eprintln!("QUICKWIT_URL not set; skipping");
        return;
    };
    use usnm_search::cache_metrics::Cache;
    let set = IndexSet::new(INDEXES.iter().map(|s| (*s).to_owned()).collect());
    let f = filters("1890-01-01", "1899-12-31");
    let spec = BucketSpec::new(BucketUnit::Year, f.from, f.to);
    qw.summary(&set, &parse("gold").unwrap(), &f, &spec)
        .await
        .unwrap();
    let report = qw.cache_metrics().await.unwrap();
    let footers = report[&Cache::SplitFooter];
    // At least one split per index, each footer cached once.
    assert!(footers.items >= 2, "{report:?}");
    assert!(
        footers.bytes > 0 && footers.misses >= footers.items,
        "{report:?}"
    );
    assert_eq!(footers.evictions, 0, "{report:?}");
    assert!(report.contains_key(&Cache::FastField), "{report:?}");
}
