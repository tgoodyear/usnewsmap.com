//! The fixtures, the stores and the release drivers the other modules share.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_core::text::Analyzer;
use usnm_core::time::date_from_day;
use usnm_ingest::docs::MemoryDocs;
use usnm_ingest::release::{Published, Release};
use usnm_ingest::sink::JsonlSink;
use usnm_ingest::source::{self, ListedBatch};
use usnm_ingest::state::State;
use usnm_ingest::worker::Worker;
use usnm_store::{LocalStore, ObjectStore};

pub(crate) fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

pub(crate) fn read_jsonl(path: &Path) -> Vec<Value> {
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

/// Documents keyed by id, without the provenance field the fixtures lack.
pub(crate) fn by_id(docs: Vec<Value>) -> BTreeMap<String, Value> {
    docs.into_iter()
        .map(|mut d| {
            d.as_object_mut().unwrap().remove("batch");
            (d["doc_id"].as_str().unwrap().to_owned(), d)
        })
        .collect()
}

/// A fixture index as the release builds it from LoC's text alone, without
/// `--american-stories`: without American Stories' text (`text_as`) and
/// the pages only it has text for (#218, 05 §5.5.4).
pub(crate) fn loc_fixture(index_id: &str) -> BTreeMap<String, Value> {
    let docs = read_jsonl(&fixtures().join(format!("indexes/{index_id}.jsonl")));
    by_id(
        docs.into_iter()
            .filter(|d| d["text"].as_str() != Some(""))
            .map(|mut d| {
                d.as_object_mut().unwrap().remove("text_as");
                d
            })
            .collect(),
    )
}

#[path = "../../../../fixtures/decades.rs"]
#[allow(dead_code)]
mod decades;

/// `pages` moved over several decades as `fixtures/decades.rs` moves the
/// fixture indexes' (05 §5.5.5): the 1830s, 1840s, 1860s and 1890s.
pub(crate) fn over_decades(pages: &[&Page]) -> Vec<Page> {
    use chrono::Datelike;
    pages
        .iter()
        .map(|p| Page {
            lccn: p.lccn.clone(),
            date: p
                .date
                .with_year(p.date.year() - decades::years_back(&p.lccn))
                .unwrap(),
            seq: p.seq,
            text: p.text.clone(),
        })
        .collect()
}

#[derive(Clone)]
pub(crate) struct Page {
    pub(crate) lccn: String,
    pub(crate) date: NaiveDate,
    pub(crate) seq: u16,
    pub(crate) text: String,
}

/// Every fixture page, including the empty ones (which are in the baselines
/// but not the indexes).
pub(crate) fn fixture_pages() -> Vec<Page> {
    let dir = fixtures();
    let titles: Vec<Value> =
        serde_json::from_slice(&std::fs::read(dir.join("fixture-v1/titles.json")).unwrap())
            .unwrap();
    let lccn_of: BTreeMap<String, String> = titles
        .iter()
        .map(|t| {
            (
                t["place_id"].as_str().unwrap().into(),
                t["lccn"].as_str().unwrap().into(),
            )
        })
        .collect();
    let mut text: BTreeMap<String, String> = BTreeMap::new();
    for f in ["pages-base-fixture", "pages-delta-fixture-1"] {
        for d in read_jsonl(&dir.join(format!("indexes/{f}.jsonl"))) {
            text.insert(
                d["doc_id"].as_str().unwrap().into(),
                d["text"].as_str().unwrap().into(),
            );
        }
    }
    let baselines: BTreeMap<String, Vec<(u32, u32)>> =
        serde_json::from_slice(&std::fs::read(dir.join("fixture-v1/baselines.json")).unwrap())
            .unwrap();
    let mut pages = Vec::new();
    for (place, series) in baselines {
        let lccn = &lccn_of[&place];
        for (day, n) in series {
            let date = date_from_day(day);
            for seq in 1..=u16::try_from(n).unwrap() {
                let id = format!("{lccn}_{date}_ed-1_seq-{seq}");
                pages.push(Page {
                    lccn: lccn.clone(),
                    date,
                    seq,
                    text: text.get(&id).cloned().unwrap_or_default(),
                });
            }
        }
    }
    pages
}

/// A batch archive in one of the two path layouts, compressed.
pub(crate) fn write_archive(path: &Path, pages: &[&Page], historical_layout: bool, gzip: bool) {
    let mut tar = tar::Builder::new(Vec::new());
    for p in pages {
        let name = if historical_layout {
            format!(
                "{}/{}/ed-1/seq-{}/ocr.txt",
                p.lccn,
                p.date.format("%Y/%m/%d"),
                p.seq
            )
        } else {
            format!("batch/{}/{}/ed-1/seq-{}/ocr.txt", p.lccn, p.date, p.seq)
        };
        let mut h = tar::Header::new_gnu();
        h.set_size(p.text.len() as u64);
        h.set_mode(0o644);
        h.set_cksum();
        tar.append_data(&mut h, &name, p.text.as_bytes()).unwrap();
        // Other files in the batch are skipped.
        let xml = name.replace("ocr.txt", "ocr.xml");
        let mut h = tar::Header::new_gnu();
        h.set_size(7);
        h.set_cksum();
        tar.append_data(&mut h, &xml, &b"<alto/>"[..]).unwrap();
    }
    let raw = tar.into_inner().unwrap();
    let data = if gzip {
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&raw).unwrap();
        gz.finish().unwrap()
    } else {
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        bz.write_all(&raw).unwrap();
        bz.finish().unwrap()
    };
    std::fs::write(path, data).unwrap();
}

pub(crate) struct Env {
    _dir: tempfile::TempDir,
    pub(crate) root: PathBuf,
    pub(crate) state: State,
    pub(crate) curated: Arc<dyn ObjectStore>,
    pub(crate) reference: Arc<LocalStore>,
}

pub(crate) async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().to_owned();
    let reference = Arc::new(LocalStore::new(root.join("reference")));
    // The catalog: the fixture titles and places.
    for (from, to) in [
        ("titles.json", "catalog/titles.json"),
        ("places.json", "catalog/places.json"),
    ] {
        let bytes = std::fs::read(fixtures().join("fixture-v1").join(from)).unwrap();
        reference.put(to, bytes, "application/json").await.unwrap();
    }
    Env {
        state: State::new(Arc::new(MemoryDocs::default())),
        curated: Arc::new(LocalStore::new(root.join("curated"))),
        reference,
        root,
        _dir: dir,
    }
}

impl Env {
    pub(crate) fn worker(&self, owner: &str) -> Worker {
        Worker {
            state: self.state.clone(),
            curated: self.curated.clone(),
            owner: owner.into(),
            lease: chrono::Duration::hours(2),
            fetch_interval: None,
            batch_limit: usnm_ingest::worker::BATCH_LIMIT,
            deadline: None,
            raw: None,
        }
    }

    pub(crate) async fn release(&self, day: u32, full: bool) -> Option<Published> {
        self.release_with(day, full, false).await.unwrap()
    }

    pub(crate) fn releaser(&self, day: u32, full: bool) -> Release {
        Release {
            stage_index_storage: false,
            state: self.state.clone(),
            curated: self.curated.clone(),
            reference: self.reference.clone(),
            owner: "releaser".into(),
            full,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
            american_stories: false,
            ja_latin: false,
            ocr_local: false,
            congress: false,
            partition_decade: false,
            single_text_field: false,
            page_language: false,
            split_docs: None,
        }
    }

    /// A release with or without American Stories' text (`--american-stories`).
    pub(crate) async fn release_with(
        &self,
        day: u32,
        full: bool,
        american_stories: bool,
    ) -> anyhow::Result<Option<Published>> {
        self.release_opts(day, full, american_stories, false).await
    }

    pub(crate) async fn release_opts(
        &self,
        day: u32,
        full: bool,
        american_stories: bool,
        ja_latin: bool,
    ) -> anyhow::Result<Option<Published>> {
        let r = Release {
            stage_index_storage: false,
            state: self.state.clone(),
            curated: self.curated.clone(),
            reference: self.reference.clone(),
            owner: "releaser".into(),
            full,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
            american_stories,
            ja_latin,
            ocr_local: false,
            congress: false,
            partition_decade: false,
            single_text_field: false,
            page_language: false,
            split_docs: None,
        };
        let mut sink = JsonlSink::new(self.root.join("reference/indexes"));
        r.run(&mut sink).await
    }

    /// An index's documents. A main index's are each checked for their
    /// common-word pairs (`text_cg`, 05 §5.5.3, and `text_as_cg` for
    /// American Stories' text, 05 §5.5.4) and returned without them, as the
    /// fixtures hold; the Japanese pages' index has none.
    pub(crate) fn index(&self, id: &str) -> BTreeMap<String, Value> {
        self.index_as(id, Analyzer::LATEST)
    }

    /// [`Env::index`] for an index whose pairs `analyzer` folded (#168).
    pub(crate) fn index_as(&self, id: &str, analyzer: Analyzer) -> BTreeMap<String, Value> {
        let mut docs = by_id(read_jsonl(
            &self.root.join(format!("reference/indexes/{id}.jsonl")),
        ));
        if id.starts_with("pages-ja-") {
            assert!(docs.values().all(|d| d.get("text_cg").is_none()), "{id}");
            return docs;
        }
        for (doc_id, doc) in &mut docs {
            let pairs = doc
                .as_object_mut()
                .unwrap()
                .remove("text_cg")
                .unwrap_or_else(|| panic!("{doc_id} has no text_cg"));
            let want = doc["text"]
                .as_str()
                .map(|t| usnm_core::common_grams::index_text(t, analyzer));
            assert_eq!(pairs.as_str().map(str::to_owned), want, "{doc_id}");
            let as_pairs = doc.as_object_mut().unwrap().remove("text_as_cg");
            let want = doc["text_as"]
                .as_str()
                .map(|t| usnm_core::common_grams::index_text(t, analyzer));
            assert_eq!(
                as_pairs.as_ref().and_then(Value::as_str).map(str::to_owned),
                want,
                "{doc_id}"
            );
        }
        docs
    }

    pub(crate) async fn reference_json(&self, path: &str) -> Value {
        serde_json::from_slice(&self.reference.get(path).await.unwrap().unwrap()).unwrap()
    }
}

/// An index run item as stored, untyped.
pub(crate) async fn raw_run(e: &Env, version: &str) -> Value {
    e.state
        .docs
        .get("index_runs", version, version)
        .await
        .unwrap()
        .unwrap()
        .doc
}

pub(crate) fn listed(name: &str, path: &Path, sha256: Option<String>) -> ListedBatch {
    ListedBatch {
        name: name.into(),
        url: path.to_str().unwrap().into(),
        sha256,
        ocr_source: None,
        lccns: vec![],
    }
}

pub(crate) fn sha256_file(path: &Path) -> String {
    use sha2::Digest;
    source::hex(&sha2::Sha256::digest(std::fs::read(path).unwrap()))
}

/// Merge waits for tests: poll often, settle quickly.
pub(crate) fn quick_merges() -> usnm_ingest::merges::MergeWait {
    usnm_ingest::merges::MergeWait {
        poll: std::time::Duration::from_millis(500),
        stable_polls: 3,
        timeout: std::time::Duration::from_secs(120),
        finalize_grace: std::time::Duration::from_secs(5),
        // Over Quickwit's 30 s actor heartbeat, so a pipeline still waiting
        // to be stopped doesn't count as stalled.
        finalize_stall: std::time::Duration::from_secs(60),
        report: Default::default(),
    }
}

/// Enqueue and curate `list`.
pub(crate) async fn curate(e: &Env, list: Vec<ListedBatch>) {
    let n = list.len();
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().new, n);
    assert_eq!(e.worker("w").run(None).await.unwrap(), n);
}

/// One row of a Japanese OCR overlay part, as `ocr/ja-ocr/jaocr.py` writes it.
pub(crate) struct JaRow<'a> {
    pub(crate) lccn: &'a str,
    pub(crate) date: NaiveDate,
    pub(crate) seq: u16,
    pub(crate) batch: &'a str,
    pub(crate) loc_text: &'a str,
    pub(crate) text: &'a str,
    pub(crate) ocred_at: i64,
}

pub(crate) async fn put_overlay_part(e: &Env, path: &str, rows: &[JaRow<'_>]) {
    put_overlay_part_with_engine(e, path, rows, "ndlocr-lite 636d1cf").await;
}

pub(crate) async fn put_overlay_part_with_engine(
    e: &Env,
    path: &str,
    rows: &[JaRow<'_>],
    engine: &str,
) {
    use arrow_array::{
        ArrayRef, Date32Array, FixedSizeBinaryArray, Int16Array, Int32Array, LargeStringArray,
        RecordBatch, StringArray, TimestampMicrosecondArray,
    };
    let s = |v: Vec<String>| Arc::new(StringArray::from(v)) as ArrayRef;
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
    let key = |r: &JaRow| format!("{}/{}/ed-1/seq-{}", r.lccn, r.date, r.seq);
    let cols: Vec<(&str, ArrayRef)> = vec![
        (
            "doc_id",
            s(rows.iter().map(|r| key(r).replace('/', "_")).collect()),
        ),
        ("page_key", s(rows.iter().map(key).collect())),
        ("lccn", s(rows.iter().map(|r| r.lccn.to_owned()).collect())),
        (
            "date",
            Arc::new(Date32Array::from(
                rows.iter()
                    .map(|r| (r.date - epoch).num_days() as i32)
                    .collect::<Vec<_>>(),
            )),
        ),
        (
            "edition",
            Arc::new(Int16Array::from(vec![1i16; rows.len()])),
        ),
        (
            "seq",
            Arc::new(Int16Array::from(
                rows.iter().map(|r| r.seq as i16).collect::<Vec<_>>(),
            )),
        ),
        (
            "batch",
            s(rows.iter().map(|r| r.batch.to_owned()).collect()),
        ),
        ("ocr_source", s(vec!["usnm-ndlocr-lite".into(); rows.len()])),
        ("ocr_engine", s(vec![engine.into(); rows.len()])),
        (
            "loc_text",
            s(rows.iter().map(|r| r.loc_text.to_owned()).collect()),
        ),
        ("text_status", s(vec!["ok".into(); rows.len()])),
        (
            "text",
            Arc::new(LargeStringArray::from(
                rows.iter().map(|r| r.text).collect::<Vec<_>>(),
            )),
        ),
        (
            "text_chars",
            Arc::new(Int32Array::from(
                rows.iter()
                    .map(|r| r.text.chars().count() as i32)
                    .collect::<Vec<_>>(),
            )),
        ),
        (
            "text_sha256",
            Arc::new(FixedSizeBinaryArray::try_from_iter(rows.iter().map(|_| [0u8; 32])).unwrap()),
        ),
        (
            "image_url",
            s(vec!["https://tile.loc.gov/x".into(); rows.len()]),
        ),
        (
            "ocred_at",
            Arc::new(
                TimestampMicrosecondArray::from(
                    rows.iter().map(|r| r.ocred_at).collect::<Vec<_>>(),
                )
                .with_timezone("UTC"),
            ),
        ),
    ];
    let batch = RecordBatch::try_from_iter(cols).unwrap();
    let mut buf = Vec::new();
    let mut w = parquet::arrow::ArrowWriter::try_new(&mut buf, batch.schema(), None).unwrap();
    w.write(&batch).unwrap();
    w.close().unwrap();
    e.curated
        .put(path, buf, "application/octet-stream")
        .await
        .unwrap();
}

/// American Stories' text for the fixture pages that have some (their
/// `text_as`), as `american_stories_write.py` writes it: a part per title
/// and year, then each year's marker. Plus a page no batch has, and a
/// blank text for a page with no text of LoC's either: neither is indexed.
/// The number of parts.
pub(crate) async fn put_american_stories(e: &Env) -> usize {
    use chrono::Datelike;
    type Rows = Vec<(String, String, NaiveDate, String)>;
    let mut parts: BTreeMap<(String, i32), Rows> = BTreeMap::new();
    let mut indexed = std::collections::BTreeSet::new();
    for f in ["pages-base-fixture", "pages-delta-fixture-1"] {
        for d in read_jsonl(&fixtures().join(format!("indexes/{f}.jsonl"))) {
            let id = d["doc_id"].as_str().unwrap().to_owned();
            indexed.insert(id.clone());
            let Some(text) = d["text_as"].as_str() else {
                continue;
            };
            let lccn = d["lccn"].as_str().unwrap().to_owned();
            let date = NaiveDate::parse_from_str(d["date"].as_str().unwrap(), "%Y-%m-%d").unwrap();
            parts.entry((lccn.clone(), date.year())).or_default().push((
                id,
                lccn,
                date,
                text.to_owned(),
            ));
        }
    }
    let blank = fixture_pages()
        .into_iter()
        .find(|p| !indexed.contains(&format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq)))
        .expect("a page with no text at all");
    let rows = parts
        .entry((blank.lccn.clone(), blank.date.year()))
        .or_default();
    rows.push((
        format!("{}_{}_ed-1_seq-{}", blank.lccn, blank.date, blank.seq),
        blank.lccn.clone(),
        blank.date,
        " \n ".into(),
    ));
    rows.push((
        format!("{}_{}_ed-1_seq-9", blank.lccn, blank.date),
        blank.lccn.clone(),
        blank.date,
        "A page no batch has".into(),
    ));
    let mut years = std::collections::BTreeSet::new();
    for ((lccn, year), rows) in &parts {
        let rows: Vec<(&str, &str, NaiveDate, &str)> = rows
            .iter()
            .map(|(id, l, d, t)| (id.as_str(), l.as_str(), *d, t.as_str()))
            .collect();
        e.curated
            .put(
                &format!("american-stories/pages/{lccn}/{year}-000.parquet"),
                usnm_ingest::american_stories::encode_part(&rows).unwrap(),
                "application/octet-stream",
            )
            .await
            .unwrap();
        years.insert(*year);
    }
    for year in years {
        e.curated
            .put(
                &format!("american-stories/years/{year}.json"),
                b"{}".to_vec(),
                "application/json",
            )
            .await
            .unwrap();
    }
    parts.len()
}

/// Curate the fixture pages as two batches: early (before July 1897) and late.
pub(crate) async fn curate_early_and_late(
    e: &Env,
) -> (Vec<ListedBatch>, Vec<ListedBatch>, usize, usize) {
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    (
        vec![listed("batch_fx_early_ver01", &a, None)],
        vec![listed("batch_fx_late_ver01", &b, None)],
        early.len(),
        late.len(),
    )
}

/// Our OCR's reading of a page in `ocr-local` (12 §12.5 Step 9), as
/// `ocr-sync` imports it: both texts' scores stored with the row.
pub(crate) fn local_row(
    p: &Page,
    batch: &str,
    ours: &str,
    loc: &str,
    node: &str,
    ocred_at: i64,
) -> usnm_ingest::ocr_local::pages::Imported {
    use usnm_ingest::ocr_local::pages::{Imported, Scores};
    let langs = ["eng"];
    Imported {
        row: usnm_overlay::OverlayRow {
            key: usnm_core::ids::PageKey::new(&p.lccn, p.date, 1, p.seq).unwrap(),
            batch: batch.into(),
            ocr_source: "usnm-tesseract".into(),
            ocr_engine: "tesseract/5.5.1 profile=eng".into(),
            loc_text: "bulk".into(),
            text: ours.into(),
            image_url: "https://tile.loc.gov/image-services/iiif/x".into(),
            ocred_at: Utc.timestamp_opt(ocred_at, 0).unwrap(),
        },
        batch_version: 1,
        source_part: "pages/x.parquet".into(),
        node: node.into(),
        reason: "bulk".into(),
        ours: Scores::of(ours, &langs),
        loc: Scores::of(loc, &langs),
    }
}

/// A page of English prose, `n` telling pages apart.
pub(crate) fn prose(n: usize) -> String {
    format!(
        "The city council met on Tuesday and voted for the new school on the hill. The \
         mayor said the railroad would bring more trade to the town and its farms. Cotton \
         prices rose in the market and the farmers were glad of it. The weather was fair \
         and the roads were dry for the first time this year. A new church will be built \
         on Main street next spring by the people of the {n} ward."
    )
}

/// The Latin text of our Japanese OCR on a page in `ocr-local` too.
pub(crate) const JA_LATIN: &str =
    "The relocation center held a meeting on Tuesday and the people of \
    the camp voted for a new school, a store and a church to be built in the spring by the \
    men and women who live there with their children and their old parents from the coast.";

/// OCR damage of the kind LoC's text has: every other word misread.
pub(crate) fn garble(text: &str) -> String {
    text.split_whitespace()
        .enumerate()
        .map(|(i, w)| {
            if i % 2 == 0 {
                w.chars()
                    .map(|c| match c {
                        'e' => 'c',
                        'h' => 'b',
                        'n' => 'u',
                        'i' => 'l',
                        'o' => 'a',
                        c => c,
                    })
                    .collect()
            } else {
                w.to_owned()
            }
        })
        .collect::<Vec<String>>()
        .join(" ")
}

/// Congress pages curated from the recorded UNT responses (#229), as
/// `congress-globe` writes them: metadc29274's pages in `seqs`, a new
/// attempt each call.
pub(crate) async fn put_globe(e: &Env, seqs: &[u32]) -> usnm_ingest::congress::curated::Counts {
    use usnm_ingest::congress::{curated, globe, parse};
    let dir = fixtures().join("../congress/unt");
    let read = |name: &str| std::fs::read(dir.join(name)).unwrap();
    let (items, _) = globe::parse_records(&read("oai-cglob-1.xml")).unwrap();
    let item = items.into_iter().find(|i| i.id == "metadc29274").unwrap();
    let canvases = globe::parse_manifest(&read("metadc29274-manifest.json")).unwrap();
    let mut dating = globe::Dating::new(&item).unwrap();
    let at = Utc.with_ymd_and_hms(2026, 10, 11, 0, 0, 0).unwrap();
    let mut rows = Vec::new();
    for c in canvases.iter().filter(|c| seqs.contains(&c.seq)) {
        let name = format!("metadc29274-m1-{}-ocr.json", c.seq);
        let lines = if dir.join(&name).exists() {
            parse::page_lines(&read(&name)).unwrap()
        } else {
            Vec::new()
        };
        let facts = dating.page(&lines);
        rows.push(globe::page_row(&item, c, &lines, &facts, at));
    }
    curated::write_attempt(e.curated.as_ref(), "globe", "unt", &item.id, &rows, at)
        .await
        .unwrap()
}

impl Env {
    /// A release with or without `--congress` (#229).
    pub(crate) async fn release_congress(
        &self,
        day: u32,
        full: bool,
        congress: bool,
    ) -> Option<Published> {
        let r = Release {
            congress,
            ..self.releaser(day, full)
        };
        let mut sink = JsonlSink::new(self.root.join("reference/indexes"));
        r.run(&mut sink).await.unwrap()
    }
}
