//! The main index laid out by decade (`--partition-decade`, 05 §5.5.5) and by
//! each page's languages (`--page-language`, 05 §5.5.8).

use std::collections::BTreeMap;
use std::path::Path;

use chrono::NaiveDate;
use serde_json::Value;
use usnm_ingest::release::{Published, Release};
use usnm_ingest::sink::JsonlSink;
use usnm_ingest::source;

use crate::common::{
    curate, env, fixture_pages, listed, loc_fixture, over_decades, put_overlay_part, quick_merges,
    read_jsonl, sha256_file, write_archive, Env, JaRow, Page,
};

impl Env {
    /// A release with or without `--partition-decade` (05 §5.5.5).
    async fn release_by_decade(&self, day: u32, full: bool, partition_decade: bool) -> Published {
        let r = Release {
            partition_decade,
            ..self.releaser(day, full)
        };
        let mut sink = JsonlSink::new(self.root.join("reference/indexes"));
        r.run(&mut sink).await.unwrap().unwrap()
    }
}

/// A full base built with `--partition-decade` (05 §5.5.5): every page has
/// its decade, the pages are written a decade at a time, and the version
/// says so. Deltas follow the published version whatever the setting says,
/// and a full base without it lays nothing out by decade.
#[tokio::test]
async fn a_base_laid_out_by_decade_and_the_releases_after_it() {
    let e = env().await;
    let pages = fixture_pages();
    let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    let group = |from: &str, to: &str| -> Vec<Page> {
        let g: Vec<&Page> = pages
            .iter()
            .filter(|p| p.date >= d(from) && p.date < d(to))
            .collect();
        over_decades(&g)
    };
    let batches = [
        ("batch_fx_a_ver01", group("1895-01-01", "1896-07-01")),
        ("batch_fx_b_ver01", group("1896-07-01", "1897-07-01")),
        ("batch_fx_c_ver01", group("1897-07-01", "1898-01-01")),
    ];
    let curate = |i: usize| {
        let (name, pages) = &batches[i];
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, &pages.iter().collect::<Vec<_>>(), false, true);
        let e = &e;
        let name = *name;
        async move {
            source::enqueue(&e.state, &[listed(name, &path, None)])
                .await
                .unwrap();
            e.worker("w").run(None).await.unwrap();
        }
    };
    let decade_of = |doc: &Value| usnm_core::decade::of_date(d(doc["date"].as_str().unwrap()));

    // 1. A partitioned base.
    curate(0).await;
    let p1 = e.release_by_decade(1, true, true).await;
    assert!(p1.full);
    let pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["decades"], usnm_core::decade::VERSION);
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p1.index_version))
        .await;
    assert_eq!(
        manifest["build"]["features"]["decades"],
        usnm_core::decade::VERSION
    );
    let yaml = manifest["build"]["templates"]["pages"]["yaml"]
        .as_str()
        .unwrap();
    assert!(yaml.contains("partition_key: decade"), "{yaml}");
    let base = &p1.indexes[0];
    let docs = read_jsonl(&e.root.join(format!("reference/indexes/{base}.jsonl")));
    assert_eq!(docs.len() as u64, p1.docs);
    for doc in &docs {
        assert_eq!(
            doc["decade"].as_u64(),
            Some(u64::from(decade_of(doc))),
            "{doc}"
        );
    }
    // Written a decade at a time: under the split target, each decade's
    // pages wait for the end and go out oldest decade first.
    let written: Vec<u16> = docs.iter().map(decade_of).collect();
    assert!(written.windows(2).all(|w| w[0] <= w[1]), "{written:?}");
    let mut seen = written.clone();
    seen.dedup();
    assert_eq!(seen, [1830, 1840, 1860, 1890]);
    // The order's files are gone.
    assert!(!e
        .root
        .join("reference/indexes")
        .join(usnm_ingest::decade_order::SPILL_DIR)
        .exists());

    // 2. A delta follows the version, the setting off: the field, and its
    // splits would be tagged rather than partitioned.
    curate(1).await;
    let p2 = e.release_by_decade(8, false, false).await;
    assert!(!p2.full);
    assert_eq!(
        e.reference_json("current.json").await["decades"],
        usnm_core::decade::VERSION
    );
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p2.index_version))
        .await;
    let yaml = manifest["build"]["templates"]["pages"]["yaml"]
        .as_str()
        .unwrap();
    assert!(
        yaml.contains("tag_fields: [decade]") && !yaml.contains("partition_key"),
        "{yaml}"
    );
    let delta = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", p2.indexes[1])),
    );
    assert!(!delta.is_empty());
    for doc in &delta {
        assert_eq!(
            doc["decade"].as_u64(),
            Some(u64::from(decade_of(doc))),
            "{doc}"
        );
    }

    // 3. A full base without the setting: no decades anywhere.
    let p3 = e.release_by_decade(15, true, false).await;
    assert!(p3.full);
    let pointer = e.reference_json("current.json").await;
    assert!(pointer.get("decades").is_none(), "{pointer}");
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p3.index_version))
        .await;
    assert!(manifest["build"]["features"].get("decades").is_none());
    let rebuilt = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", p3.indexes[0])),
    );
    assert!(rebuilt.iter().all(|doc| doc.get("decade").is_none()));
    // The partitioned base's pages are this base's, apart from the field.
    let rebuilt: BTreeMap<&str, &Value> = rebuilt
        .iter()
        .map(|doc| (doc["doc_id"].as_str().unwrap(), doc))
        .collect();
    for mut doc in docs {
        doc.as_object_mut().unwrap().remove("decade");
        assert_eq!(rebuilt[doc["doc_id"].as_str().unwrap()], &doc);
    }

    // 4. The setting on, but a delta: it follows the version, so no decades
    // until the next full base.
    curate(2).await;
    let p4 = e.release_by_decade(22, false, true).await;
    assert!(!p4.full);
    assert!(e
        .reference_json("current.json")
        .await
        .get("decades")
        .is_none());
    let delta = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", p4.indexes[1])),
    );
    assert!(!delta.is_empty() && delta.iter().all(|doc| doc.get("decade").is_none()));
}

/// Real German and English for some of the fixture's pages: the fixture's
/// own text mostly has too few different English words for any language to
/// win, so most of its pages keep their title's languages.
const ENGLISH_PAGE: &str = "The council of the city met on Tuesday evening at the court house, \
    and after a long debate it was voted that the new school should be built on the lot which \
    the board had bought from Mr. Smith last year. The mayor said that he would not sign the bill \
    unless the cost of the work was paid from the general fund, and the members agreed with him.";

const GERMAN_PAGE: &str = "Der Stadtrath hat am Dienstag beschlossen, dass die neue Schule auf \
    dem Grundstück gebaut werden soll, welches die Behörde im vorigen Jahre gekauft hat. Es ist \
    noch nicht bekannt, wann die Arbeit beginnen wird, aber alle sind der Meinung, dass es eine \
    gute Sache für die Stadt und für ihre Bürger ist, und dass man nicht länger warten kann.";

/// Pages per set of languages in a snapshot's language baselines.
fn pages_by_set(baselines: &Value) -> BTreeMap<String, u64> {
    let mut out = BTreeMap::new();
    for set in baselines["sets"].as_array().unwrap() {
        let key = usnm_core::page_language::set_key(
            &set["languages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|l| l.as_str().unwrap())
                .collect::<Vec<_>>(),
        );
        let n: u64 = set["baselines"]
            .as_object()
            .unwrap()
            .values()
            .flat_map(|days| days.as_array().unwrap())
            .map(|d| d[1].as_u64().unwrap())
            .sum();
        *out.entry(key).or_default() += n;
    }
    out
}

/// A full base built with `--page-language` (05 §5.5.8, #289): every page
/// has its own languages, the snapshot their baselines and the pages that
/// move to them, and the version says so. A delta follows the published
/// version whatever the setting says and carries the moves forward, and a
/// full base without it has none of it.
#[tokio::test]
async fn a_base_with_page_languages_and_the_releases_after_it() {
    use usnm_core::page_language::{FIELD, SET_FIELD, VERSION};
    let e = env().await;
    let d = |s: &str| NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap();
    let pages: Vec<Page> = fixture_pages()
        .into_iter()
        .map(|mut p| {
            // Front pages in German (the German title) or English (the
            // English and German title, and the Spanish one).
            if p.seq == 1 && !p.text.is_empty() {
                match p.lccn.as_str() {
                    "sn99000006" => p.text = GERMAN_PAGE.to_owned(),
                    "sn99000002" | "sn99000003" => p.text = ENGLISH_PAGE.to_owned(),
                    _ => {}
                }
            }
            p
        })
        .collect();
    let group = |from: &str, to: &str| -> Vec<&Page> {
        pages
            .iter()
            .filter(|p| p.date >= d(from) && p.date < d(to))
            .collect()
    };
    let batches = [
        ("batch_fx_a_ver01", group("1895-01-01", "1896-01-01")),
        ("batch_fx_b_ver01", group("1896-01-01", "1897-01-01")),
        ("batch_fx_c_ver01", group("1897-01-01", "1897-07-01")),
        ("batch_fx_d_ver01", group("1897-07-01", "1898-01-01")),
    ];
    let curate = |i: usize| {
        let (name, pages) = &batches[i];
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        let e = &e;
        let name = *name;
        async move {
            source::enqueue(&e.state, &[listed(name, &path, None)])
                .await
                .unwrap();
            e.worker("w").run(None).await.unwrap();
        }
    };
    let release = |day: u32, full: bool, page_language: bool| {
        let r = Release {
            page_language,
            ..e.releaser(day, full)
        };
        let root = e.root.join("reference/indexes");
        async move {
            let mut sink = JsonlSink::new(root);
            r.run(&mut sink).await.unwrap().unwrap()
        }
    };
    let docs_of =
        |index: &str| read_jsonl(&e.root.join(format!("reference/indexes/{index}.jsonl")));
    let sorted = |langs: &Value| -> Vec<String> {
        let mut l: Vec<String> = langs
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap().to_owned())
            .collect();
        l.sort();
        l
    };
    let title_langs = |doc: &Value| sorted(&doc["language"]);
    let langs = |doc: &Value| -> Vec<String> {
        doc[FIELD]
            .as_array()
            .unwrap_or_else(|| panic!("no {FIELD}: {doc}"))
            .iter()
            .map(|l| l.as_str().unwrap().to_owned())
            .collect()
    };
    let moved = |docs: &[Value]| docs.iter().filter(|d| langs(d) != title_langs(d)).count() as u64;

    // 1. A base with page languages.
    curate(0).await;
    curate(1).await;
    let p1 = release(1, true, true).await;
    assert!(p1.full);
    let pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["page_language"], VERSION);
    let snapshot = |v: &str, f: &str| {
        let path = format!("{v}/{f}");
        let e = &e;
        async move { e.reference_json(&path).await }
    };
    let manifest = snapshot(&p1.index_version, "manifest.json").await;
    assert_eq!(manifest["build"]["features"]["page_language"], VERSION);
    let yaml = manifest["build"]["templates"]["pages"]["yaml"]
        .as_str()
        .unwrap();
    assert!(yaml.contains("name: page_language_set,"), "{yaml}");
    let base = docs_of(&p1.indexes[0]);
    assert_eq!(base.len() as u64, p1.docs);
    for doc in &base {
        let l = langs(doc);
        assert_eq!(
            doc[SET_FIELD],
            usnm_core::page_language::set_key(&l),
            "{doc}"
        );
        let want: Vec<String> = match (doc["lccn"].as_str().unwrap(), doc["seq"].as_u64()) {
            ("sn99000006", Some(1)) => vec!["ger".into()],
            ("sn99000002" | "sn99000003", Some(1)) => vec!["eng".into()],
            // The fixture's own text: whatever the detector finds, mostly
            // too few words for any language, so the title's.
            _ => {
                let title: Vec<&str> = doc["language"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|l| l.as_str().unwrap())
                    .collect();
                let found = usnm_core::page_language::detect(doc["text"].as_str().unwrap(), &title);
                usnm_core::page_language::page_languages(Some(&found), &title)
            }
        };
        assert_eq!(l, want, "{doc}");
    }
    let moves = snapshot(&p1.index_version, "page_languages.json").await;
    assert_eq!(moves["version"], VERSION);
    assert_eq!(manifest["page_languages"]["pages"], moved(&base));
    assert!(moved(&base) > 0);
    // Every page counts once, in its own set: the documents' sets, and the
    // pages without a document (no text) in their title's.
    let by_title = pages_by_set(&snapshot(&p1.index_version, "language_baselines.json").await);
    let by_page = pages_by_set(&snapshot(&p1.index_version, "page_language_baselines.json").await);
    assert_eq!(
        by_title.values().sum::<u64>(),
        by_page.values().sum::<u64>()
    );
    let mut want: BTreeMap<String, u64> = BTreeMap::new();
    for doc in &base {
        *want
            .entry(doc[SET_FIELD].as_str().unwrap().to_owned())
            .or_default() += 1;
    }
    let title_pages = snapshot(&p1.index_version, "title_pages.json").await;
    let titles = snapshot(&p1.index_version, "titles.json").await;
    for t in titles.as_array().unwrap() {
        let lccn = t["lccn"].as_str().unwrap();
        let docs = base.iter().filter(|d| d["lccn"] == lccn).count() as u64;
        let without = title_pages[lccn].as_u64().unwrap() - docs;
        *want
            .entry(usnm_core::page_language::set_key(&sorted(&t["languages"])))
            .or_default() += without;
    }
    want.retain(|_, n| *n > 0);
    assert_eq!(by_page, want);

    // 2. A delta, the setting off: it follows the version, and carries the
    // base's moves forward with its own.
    curate(2).await;
    let p2 = release(8, false, false).await;
    assert!(!p2.full);
    assert_eq!(
        e.reference_json("current.json").await["page_language"],
        VERSION
    );
    let delta = docs_of(&p2.indexes[1]);
    assert!(!delta.is_empty() && delta.iter().all(|d| d.get(FIELD).is_some()));
    let manifest = snapshot(&p2.index_version, "manifest.json").await;
    assert_eq!(
        manifest["page_languages"]["pages"],
        moved(&base) + moved(&delta)
    );
    let by_title = pages_by_set(&snapshot(&p2.index_version, "language_baselines.json").await);
    let by_page = pages_by_set(&snapshot(&p2.index_version, "page_language_baselines.json").await);
    assert_eq!(
        by_title.values().sum::<u64>(),
        by_page.values().sum::<u64>()
    );
    // Every page of the English-only titles is English; the multilingual
    // title's moved to English (its text is English) but for its pages
    // without text.
    assert!(by_page["eng"] > by_title["eng"]);

    // 3. A full base without the setting: no page languages anywhere.
    let p3 = release(15, true, false).await;
    let pointer = e.reference_json("current.json").await;
    assert!(pointer.get("page_language").is_none(), "{pointer}");
    let rebuilt = docs_of(&p3.indexes[0]);
    assert!(rebuilt
        .iter()
        .all(|d| d.get(FIELD).is_none() && d.get(SET_FIELD).is_none()));
    let manifest = snapshot(&p3.index_version, "manifest.json").await;
    assert!(manifest.get("page_languages").is_none());
    assert!(manifest["build"]["features"].get("page_language").is_none());
    let files: Vec<&str> = manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect();
    assert!(
        !files.contains(&"page_language_baselines.json") && !files.contains(&"page_languages.json")
    );
    // Apart from the fields, the documents are the same.
    let rebuilt: BTreeMap<&str, &Value> = rebuilt
        .iter()
        .map(|doc| (doc["doc_id"].as_str().unwrap(), doc))
        .collect();
    for mut doc in base.into_iter().chain(delta) {
        let o = doc.as_object_mut().unwrap();
        o.remove(FIELD);
        o.remove(SET_FIELD);
        assert_eq!(rebuilt[doc["doc_id"].as_str().unwrap()], &doc);
    }

    // 4. The setting on, but a delta on a version without: none until the
    // next full base.
    curate(3).await;
    let p4 = release(22, false, true).await;
    assert!(!p4.full);
    assert!(e
        .reference_json("current.json")
        .await
        .get("page_language")
        .is_none());
}

/// On a version with page languages (05 §5.5.8), the Japanese index's
/// pages carry the languages the page-language baselines count them in: a
/// page whose main-index document moved takes its moved set, any other its
/// title's. So a Japanese search filters and counts as its baseline does.
#[tokio::test]
async fn the_japanese_index_carries_the_page_languages_it_is_counted_in() {
    use usnm_core::page_language::{FIELD, SET_FIELD};
    let e = env().await;
    // The English and German title's first pages in German: they move to
    // `ger`, and the Japanese OCR of one of them must say so.
    let pages: Vec<Page> = fixture_pages()
        .into_iter()
        .map(|mut p| {
            if p.lccn == "sn99000002" && p.seq == 1 && !p.text.is_empty() {
                p.text = GERMAN_PAGE.to_owned();
            }
            p
        })
        .collect();
    let path = e.root.join("batch_fx_ja_ver01.tar.gz");
    let all: Vec<&Page> = pages.iter().collect();
    write_archive(&path, &all, false, true);
    curate(
        &e,
        vec![listed("batch_fx_ja_ver01", &path, Some(sha256_file(&path)))],
    )
    .await;
    let p0 = pages
        .iter()
        .find(|p| p.lccn == "sn99000002" && p.seq == 1 && !p.text.is_empty())
        .unwrap();
    let row = |seq, loc_text, text| JaRow {
        lccn: &p0.lccn,
        date: p0.date,
        seq,
        batch: "batch_fx_ja",
        loc_text,
        text,
        ocred_at: 1,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(p0.seq, "garbled", "日本"),
            row(90, "missing", "米國と日本"),
        ],
    )
    .await;
    let r = Release {
        page_language: true,
        ..e.releaser(5, true)
    };
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    let published = r.run(&mut sink).await.unwrap().unwrap();
    let ja_id = format!(
        "pages-ja-{}",
        published.index_version.trim_start_matches("pages-v")
    );
    let ja = e.index(&ja_id);
    assert_eq!(ja.len(), 2);
    let main = e.index(&published.indexes[0]);
    let moved = format!("{}_{}_ed-1_seq-{}", p0.lccn, p0.date, p0.seq);
    assert_eq!(main[&moved][FIELD], serde_json::json!(["ger"]));
    // The page with a main-index document: its document's languages.
    assert_eq!(ja[&moved][FIELD], serde_json::json!(["ger"]));
    assert_eq!(ja[&moved][SET_FIELD], "ger");
    // The page curation never had: its title's, as the baselines count it.
    let missing = &ja[&format!("{}_{}_ed-1_seq-90", p0.lccn, p0.date)];
    assert_eq!(missing[FIELD], serde_json::json!(["eng", "ger"]));
    assert_eq!(missing[SET_FIELD], "eng_ger");
    // The Japanese template has the fields.
    let manifest = e
        .reference_json(&format!("{}/manifest.json", published.index_version))
        .await;
    let yaml = manifest["build"]["templates"]["pages-ja"]["yaml"]
        .as_str()
        .unwrap();
    assert!(yaml.contains("name: page_language_set,"), "{yaml}");
    // Without the setting, none of it.
    let r = Release {
        page_language: false,
        ..e.releaser(12, true)
    };
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    let published = r.run(&mut sink).await.unwrap().unwrap();
    let ja_id = format!(
        "pages-ja-{}",
        published.index_version.trim_start_matches("pages-v")
    );
    assert!(e
        .index(&ja_id)
        .values()
        .all(|d| d.get(FIELD).is_none() && d.get(SET_FIELD).is_none()));
}

/// A base built with `--page-language` on a real Quickwit writer (05
/// §5.5.8), and a delta on it: the index takes both fields, and the two
/// renderings of a page-language filter find the same pages, the ones the
/// detector gives that language. Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn releases_page_languages_into_a_quickwit_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_core::page_language::{detect, page_languages, set_key};
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, None)
    };
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7412, &meta, &root)
        .await
        .unwrap();
    let mut published = Vec::new();
    for (day, full, batch) in [
        (1, true, archive("batch_fx_early_ver01", &early)),
        (8, false, archive("batch_fx_late_ver01", &late)),
    ] {
        curate(&e, vec![batch]).await;
        let r = Release {
            page_language: full,
            ..e.releaser(day, full)
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    assert_eq!(
        e.reference_json("current.json").await["page_language"],
        usnm_core::page_language::VERSION
    );
    // What the detector gives each fixture page.
    let mut want: BTreeMap<String, u64> = BTreeMap::new();
    for doc in ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|f| loc_fixture(f).into_values())
    {
        let title: Vec<&str> = doc["language"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l.as_str().unwrap())
            .collect();
        let found = detect(doc["text"].as_str().unwrap(), &title);
        *want
            .entry(set_key(&page_languages(Some(&found), &title)))
            .or_default() += 1;
    }
    let http = reqwest::Client::new();
    let count = |q: String| {
        let url = format!("{}/api/v1/{}/search", node.url, p.indexes.join(","));
        let http = http.clone();
        async move {
            let v: Value = http
                .post(url)
                .json(&serde_json::json!({"query": q, "max_hits": 0}))
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            v["num_hits"].as_u64().unwrap_or_else(|| panic!("{q}: {v}"))
        }
    };
    for lang in ["eng", "ger", "spa"] {
        let has = |set: &str| usnm_core::page_language::set_languages(set).any(|l| l == lang);
        let pages: u64 = want.iter().filter(|(s, _)| has(s)).map(|(_, n)| n).sum();
        let others: Vec<&str> = want
            .keys()
            .map(String::as_str)
            .filter(|s| !has(s))
            .chain(["none"])
            .collect();
        assert_eq!(
            count(format!("page_language:IN [{lang}]")).await,
            pages,
            "{lang}"
        );
        assert_eq!(
            count(format!(
                "day:[0 TO 1000000] AND NOT page_language_set:IN [{}]",
                others.join(" ")
            ))
            .await,
            pages,
            "{lang}"
        );
    }
    node.stop().await.unwrap();
}

/// A base built with `--partition-decade` on a real Quickwit writer (05
/// §5.5.5): each split holds one decade, the release checks the merges per
/// decade and records the splits of each, and a delta on it tags its split
/// with its decades. Searches that name their decades get the same pages
/// as without, from fewer splits. Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn releases_a_base_laid_out_by_decade_into_a_quickwit_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_core::params::Filters;
    use usnm_core::query::parse;
    use usnm_core::time::{BucketSpec, BucketUnit};
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    use usnm_search::quickwit::QuickwitBackend;
    use usnm_search::{IndexSet, SearchBackend};

    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let (early, late) = (over_decades(&early), over_decades(&late));
    let archive = |name: &str, pages: &[Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, &pages.iter().collect::<Vec<_>>(), false, true);
        listed(name, &path, None)
    };
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7403, &meta, &root)
        .await
        .unwrap();
    let mut published = Vec::new();
    for (day, full, batch) in [
        (1, true, archive("batch_fx_early_ver01", &early)),
        (8, false, archive("batch_fx_late_ver01", &late)),
    ] {
        curate(&e, vec![batch]).await;
        let r = Release {
            partition_decade: true,
            single_text_field: false,
            page_language: false,
            split_docs: None,
            ..e.releaser(day, full)
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    assert_eq!(
        e.reference_json("current.json").await["decades"],
        usnm_core::decade::VERSION
    );
    // The base: one decade a split, counted per decade in the manifest.
    let manifest = e
        .reference_json(&format!("{}/manifest.json", p.index_version))
        .await;
    let layout = manifest["indexes"].as_array().unwrap();
    assert_eq!(
        layout[0]["splits_by_decade"],
        serde_json::json!({"1830": 1, "1840": 1, "1860": 1, "1890": 1}),
        "{}",
        layout[0]
    );
    // The delta: tagged, not partitioned, so one split with every decade.
    assert_eq!(layout[1]["splits"], 1, "{}", layout[1]);
    assert!(layout[1].get("splits_by_decade").is_none(), "{}", layout[1]);
    let http = reqwest::Client::new();
    let n = usnm_ingest::merges::Node {
        http: &http,
        base: &node.url,
    };
    let delta = n.splits(&p.indexes[1]).await.unwrap();
    assert_eq!(delta[0].decades, [1840, 1860, 1890]);
    assert_eq!(delta[0].partition_id, 0);

    // Searches name their decades as the API does, and find the same pages.
    let backend = QuickwitBackend::new(&node.url, std::time::Duration::from_secs(30)).unwrap();
    let plain = IndexSet::new(p.indexes.clone());
    let by_decade = plain.clone().with_decades(Some(1830..=1890));
    let q = parse("gold").unwrap();
    let mut found = 0;
    for (from, to) in [
        ("1868-01-01", "1868-12-31"),
        ("1839-01-01", "1841-12-31"),
        ("1860-01-01", "1899-12-31"),
        ("1830-01-01", "1897-12-31"),
    ] {
        let f = Filters {
            from: NaiveDate::parse_from_str(from, "%Y-%m-%d").unwrap(),
            to: NaiveDate::parse_from_str(to, "%Y-%m-%d").unwrap(),
            states: vec![],
            lccns: vec![],
            langs: vec![],
            front_only: false,
            series: Vec::new(),
        };
        let spec = BucketSpec::new(BucketUnit::Month, f.from, f.to);
        let a = backend.summary(&by_decade, &q, &f, &spec).await.unwrap();
        let b = backend.summary(&plain, &q, &f, &spec).await.unwrap();
        assert_eq!(a, b, "{from} to {to}");
        found += a.total_hits;
    }
    assert!(found > 0);
    // The splits a search for 1868 needs: the base's 1860s split and the
    // delta's, of the five.
    let mut targeted = 0;
    for id in &p.indexes {
        targeted += n
            .splits(id)
            .await
            .unwrap()
            .iter()
            .filter(|s| s.decades.contains(&1860))
            .count();
    }
    assert_eq!(targeted, 2);
    node.stop().await.unwrap();
}
