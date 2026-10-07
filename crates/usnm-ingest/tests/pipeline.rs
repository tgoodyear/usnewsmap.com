//! End to end on the synthetic corpus: turn the fixture pages back into two
//! LoC-style batch archives, run enqueue → curate → release twice (a base,
//! then a delta), and check the pipeline reproduces the checked-in fixtures
//! exactly: both indexes, the baselines, titles, places and pages per title.
//! Then load the result the way the API does.

use std::collections::BTreeMap;
use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_core::time::date_from_day;
use usnm_ingest::docs::MemoryDocs;
use usnm_ingest::release::{Published, Release};
use usnm_ingest::sink::JsonlSink;
use usnm_ingest::source::{self, ListedBatch};
use usnm_ingest::state::{BatchStatus, State};
use usnm_ingest::worker::Worker;
use usnm_store::{LocalStore, ObjectStore};

fn fixtures() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data")
}

fn read_jsonl(path: &Path) -> Vec<Value> {
    std::io::BufReader::new(std::fs::File::open(path).unwrap())
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}

/// Documents keyed by id, without the provenance field the fixtures lack.
fn by_id(docs: Vec<Value>) -> BTreeMap<String, Value> {
    docs.into_iter()
        .map(|mut d| {
            d.as_object_mut().unwrap().remove("batch");
            (d["doc_id"].as_str().unwrap().to_owned(), d)
        })
        .collect()
}

/// A fixture index as the release builds it from LoC's text alone: without
/// American Stories' text (`text_as`) and the pages only it has text for,
/// which the release doesn't load yet (#218, 05 §5.5.4).
fn loc_fixture(index_id: &str) -> BTreeMap<String, Value> {
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

struct Page {
    lccn: String,
    date: NaiveDate,
    seq: u16,
    text: String,
}

/// Every fixture page, including the empty ones (which are in the baselines
/// but not the indexes).
fn fixture_pages() -> Vec<Page> {
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
fn write_archive(path: &Path, pages: &[&Page], historical_layout: bool, gzip: bool) {
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

struct Env {
    _dir: tempfile::TempDir,
    root: PathBuf,
    state: State,
    curated: Arc<dyn ObjectStore>,
    reference: Arc<LocalStore>,
}

async fn env() -> Env {
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
    fn worker(&self, owner: &str) -> Worker {
        Worker {
            state: self.state.clone(),
            curated: self.curated.clone(),
            owner: owner.into(),
            lease: chrono::Duration::hours(2),
            fetch_interval: None,
            batch_limit: usnm_ingest::worker::BATCH_LIMIT,
            deadline: None,
        }
    }

    async fn release(&self, day: u32, full: bool) -> Option<Published> {
        let r = Release {
            state: self.state.clone(),
            curated: self.curated.clone(),
            reference: self.reference.clone(),
            owner: "releaser".into(),
            full,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
        };
        let mut sink = JsonlSink::new(self.root.join("reference/indexes"));
        r.run(&mut sink).await.unwrap()
    }

    /// An index's documents. A main index's are each checked for their
    /// common-word pairs (`text_cg`, 05 §5.5.3) and returned without them,
    /// as the fixtures hold; the Japanese pages' index has none.
    fn index(&self, id: &str) -> BTreeMap<String, Value> {
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
                .map(usnm_core::common_grams::index_text);
            assert_eq!(pairs.as_str().map(str::to_owned), want, "{doc_id}");
        }
        docs
    }

    async fn reference_json(&self, path: &str) -> Value {
        serde_json::from_slice(&self.reference.get(path).await.unwrap().unwrap()).unwrap()
    }
}

/// An index run item as stored, untyped.
async fn raw_run(e: &Env, version: &str) -> Value {
    e.state
        .docs
        .get("index_runs", version, version)
        .await
        .unwrap()
        .unwrap()
        .doc
}

fn listed(name: &str, path: &Path, sha256: Option<String>) -> ListedBatch {
    ListedBatch {
        name: name.into(),
        url: path.to_str().unwrap().into(),
        sha256,
        ocr_source: None,
        lccns: vec![],
    }
}

fn sha256_file(path: &Path) -> String {
    use sha2::Digest;
    source::hex(&sha2::Sha256::digest(std::fs::read(path).unwrap()))
}

#[tokio::test]
async fn a_version_without_these_common_word_pairs_is_rebuilt_in_full() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let base_archive = e.root.join("batch_fx_early_ver01.tar.gz");
    let delta_archive = e.root.join("batch_fx_late_ver01.tar.bz2");
    write_archive(&base_archive, &early, true, true);
    write_archive(&delta_archive, &late, false, false);
    let list = [listed("batch_fx_early_ver01", &base_archive, None)];
    source::enqueue(&e.state, &list).await.unwrap();
    e.worker("w1").run(None).await.unwrap();
    assert!(e.release(1, false).await.unwrap().full);
    // The published version names the pairs its indexes hold (05 §5.5.3).
    let mut pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], usnm_core::common_grams::VERSION);

    // A version published before the pairs (or with another list): the
    // next release rebuilds every index rather than add a delta to it.
    pointer.as_object_mut().unwrap().remove("common_grams");
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();
    let list = [listed("batch_fx_late_ver01", &delta_archive, None)];
    source::enqueue(&e.state, &list).await.unwrap();
    e.worker("w2").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(
        p2.full,
        "a delta would mix indexes with and without the pairs"
    );
    assert_eq!(p2.indexes.len(), 1);
    let pointer = e.reference_json("current.json").await;
    assert_eq!(pointer["common_grams"], usnm_core::common_grams::VERSION);
}

#[tokio::test]
async fn reproduces_the_fixture_corpus_as_a_base_and_a_delta() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let base_archive = e.root.join("batch_fx_early_ver01.tar.gz");
    let delta_archive = e.root.join("batch_fx_late_ver01.tar.bz2");
    write_archive(&base_archive, &early, true, true);
    write_archive(&delta_archive, &late, false, false);

    // Week 1: one batch → a base index.
    let list = [listed(
        "batch_fx_early_ver01",
        &base_archive,
        Some(sha256_file(&base_archive)),
    )];
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().new, 1);
    assert_eq!(e.worker("w1").run(None).await.unwrap(), 1);
    let p1 = e.release(1, false).await.unwrap();
    assert!(p1.full, "the first release is a base");
    assert_eq!(p1.indexes, ["pages-base-20261001-1"]);

    let want_base = loc_fixture("pages-base-fixture");
    assert_eq!(e.index("pages-base-20261001-1"), want_base);
    assert_eq!(p1.docs, want_base.len() as u64);

    // Nothing new (a weekly run when LoC published nothing): the listing
    // queues nothing, the worker finds nothing to claim, and nothing is released.
    let again = source::enqueue(&e.state, &list).await.unwrap();
    assert_eq!((again.new, again.unchanged), (0, 1));
    assert_eq!(e.worker("w1").run(None).await.unwrap(), 0);
    assert!(e.release(2, false).await.is_none());

    // Week 2: the second batch → a delta on top of the base.
    let list = [listed("batch_fx_late_ver01", &delta_archive, None)];
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().new, 1);
    assert_eq!(e.worker("w2").run(None).await.unwrap(), 1);
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(
        p2.indexes,
        ["pages-base-20261001-1", "pages-delta-20261008-1"]
    );
    let want_delta = loc_fixture("pages-delta-fixture-1");
    assert_eq!(e.index("pages-delta-20261008-1"), want_delta);

    // The reference snapshot matches the fixture's, file for file.
    let v = &p2.index_version;
    for f in [
        "baselines.json",
        "language_baselines.json",
        "titles.json",
        "places.json",
        "title_pages.json",
    ] {
        let want: Value =
            serde_json::from_slice(&std::fs::read(fixtures().join("fixture-v1").join(f)).unwrap())
                .unwrap();
        assert_eq!(e.reference_json(&format!("{v}/{f}")).await, want, "{f}");
    }
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v.as_str());
    assert_eq!(current["previous_version"], p1.index_version.as_str());
    assert_eq!(current["backend"], "memory");
    assert_eq!(current["bounds"]["from"], "1895-01-05");
    assert_eq!(current["synthetic"], true);
    // Stamped when the pointer was written, not when the run started (#119):
    // the same instant the run records as its publish.
    assert_ne!(current["published_at"], "2026-10-08T03:00:00Z");
    let run_published: chrono::DateTime<Utc> =
        serde_json::from_value(raw_run(&e, v).await["published_at"].clone()).unwrap();
    assert_eq!(
        current["published_at"],
        run_published.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
    );

    // The run item points at the batch list in the snapshot instead of
    // carrying it, and the manifest checksums the list like its other files.
    let item = raw_run(&e, v).await;
    assert!(item.get("batches").is_none(), "{item}");
    assert_eq!(item["batch_count"], 2);
    assert_eq!(item["batch_list"], format!("{v}/batches.json"));
    let listed = e.reference_json(&format!("{v}/batches.json")).await;
    assert_eq!(
        listed
            .as_array()
            .unwrap()
            .iter()
            .map(|b| b["batch"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["batch_fx_early", "batch_fx_late"]
    );
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert!(manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "batches.json"));
    // What built it (#161): the manifest has the templates in full, the run
    // item their checksums only.
    let build = &manifest["build"];
    assert_eq!(
        build["features"]["common_grams"],
        usnm_core::common_grams::VERSION
    );
    assert_eq!(
        build["templates"]["pages"]["yaml"],
        usnm_ingest::sink::INDEX_TEMPLATE
    );
    assert_eq!(
        item["build"]["templates"]["pages"]["sha256"],
        build["templates"]["pages"]["sha256"]
    );
    assert!(item["build"]["templates"]["pages"].get("yaml").is_none());
    assert_eq!(item["build"]["full"], build["full"]);
    // The memory sink has no engine to name.
    assert_eq!(build["engine"], serde_json::Value::Null);

    // The API loads it: checksums, manifest and version pairing all hold.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.version(), v);
    assert_eq!(refdata.places.len(), 6);
    // Pages per title count the same pages as the baselines.
    let title_pages = refdata.title_pages.as_ref().unwrap();
    assert_eq!(title_pages.len(), 6);
    assert_eq!(title_pages.values().sum::<u64>(), refdata.pages);

    // A full release (compaction) rebuilds one base with every page.
    let p3 = e.release(15, true).await.unwrap();
    assert_eq!(p3.indexes, ["pages-base-20261015-1"]);
    let mut both = want_base.clone();
    both.extend(want_delta.clone());
    assert_eq!(e.index("pages-base-20261015-1"), both);

    // Published indexes and snapshots are never rewritten.
    assert!(e
        .reference
        .get(&format!("{}/manifest.json", p1.index_version))
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn failed_and_leased_batches_are_retried_not_lost() {
    let e = env().await;
    let pages = fixture_pages();
    let few: Vec<&Page> = pages.iter().take(20).collect();
    let archive = e.root.join("batch_fx_small_ver01.tar.gz");
    write_archive(&archive, &few, true, true);

    // A wrong checksum fails the attempt and re-queues the batch.
    let bad = [listed(
        "batch_fx_small_ver01",
        &archive,
        Some("0".repeat(64)),
    )];
    source::enqueue(&e.state, &bad).await.unwrap();
    assert_eq!(e.worker("w1").run(None).await.unwrap(), 0);
    let (b, _) = e.state.batch("batch_fx_small").await.unwrap().unwrap();
    assert_eq!((b.status, b.attempts), (BatchStatus::Queued, 1));
    assert!(b.last_error.as_deref().unwrap().contains("sha256"));
    assert!(b.curated.is_none() && b.lease.is_none());

    // A claimed batch is invisible to other workers until its lease expires.
    let mut b2 = b.clone();
    b2.source_sha256 = None;
    let (_, etag) = e.state.batch("batch_fx_small").await.unwrap().unwrap();
    e.state.replace_batch(&b2, &etag).await.unwrap().unwrap();
    let w1 = e.worker("w1");
    let (claimed, _) = w1.claim().await.unwrap().unwrap();
    assert!(e.worker("w2").claim().await.unwrap().is_none());

    // A worker that lost its lease can't commit.
    let c = w1.curate(&claimed).await.unwrap();
    assert!(e
        .worker("w2")
        .commit("batch_fx_small", c.clone())
        .await
        .is_err());
    w1.commit("batch_fx_small", c).await.unwrap();
    let (b, _) = e.state.batch("batch_fx_small").await.unwrap().unwrap();
    assert_eq!(b.status, BatchStatus::Curated);
    assert_eq!(b.curated.unwrap().pages, 20);

    // A second release while the writer lock is held elsewhere is refused.
    e.state
        .lock(
            usnm_ingest::release::WRITER_LOCK,
            "someone-else",
            chrono::Duration::hours(1),
        )
        .await
        .unwrap();
    let r = Release {
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: false,
        synthetic: true,
        now: Utc::now(),
        titles_left: None,
    };
    let mut sink = JsonlSink::new(e.root.join("idx"));
    assert!(r.run(&mut sink).await.is_err());
}

/// The same two releases into a real Quickwit writer node run by the
/// pipeline (as the Azure job does). Runs when `QUICKWIT_BIN` is set; CI's
/// `quickwit` job sets it.
#[tokio::test]
async fn releases_into_a_quickwit_writer_node() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);

    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7390, &meta, &root)
        .await
        .unwrap();

    let mut published = Vec::new();
    for (day, name, path) in [
        (1, "batch_fx_early_ver01", &a),
        (8, "batch_fx_late_ver01", &b),
    ] {
        source::enqueue(&e.state, &[listed(name, path, None)])
            .await
            .unwrap();
        e.worker("w").run(None).await.unwrap();
        let r = Release {
            state: e.state.clone(),
            curated: e.curated.clone(),
            reference: e.reference.clone(),
            owner: "releaser".into(),
            full: false,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    assert_eq!(
        p.indexes,
        ["pages-base-20261001-1", "pages-delta-20261008-1"]
    );
    let current = e.reference_json("current.json").await;
    assert_eq!(current["backend"], "quickwit");

    // The manifest reports the splits of both indexes, merged and closed.
    let manifest = e.reference_json("pages-v20261008-1/manifest.json").await;
    let layout = manifest["indexes"].as_array().unwrap();
    assert_eq!(layout.len(), 2);
    for (l, id) in layout.iter().zip(&p.indexes) {
        assert_eq!(l["index_id"], id.as_str());
        let splits = l["splits"].as_u64().unwrap();
        assert!((1..=7).contains(&splits), "{l}");
        assert!(l["bytes"].as_u64().unwrap() > 0, "{l}");
    }
    let docs: u64 = layout.iter().map(|l| l["docs"].as_u64().unwrap()).sum();
    let http = reqwest::Client::new();
    let base: Value = http
        .get(format!("{}/api/v1/indexes/{}", node.url, p.indexes[0]))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let source = base["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source_id"] == "_ingest-source")
        .unwrap();
    assert_eq!(
        source["enabled"], false,
        "a sealed index takes no more writes"
    );

    // Every fixture document is searchable across base + delta, and a
    // phrase matches exactly as often as in the fixture corpus.
    let count = |q: &'static str| {
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
            v["num_hits"].as_u64().unwrap()
        }
    };
    let fixture_docs: Vec<Value> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|f| loc_fixture(f).into_values())
        .collect();
    assert_eq!(count("*").await, fixture_docs.len() as u64);
    assert_eq!(docs, fixture_docs.len() as u64);
    let phrase = fixture_docs
        .iter()
        .filter(|d| d["text"].as_str().unwrap().contains("cross of gold"))
        .count() as u64;
    assert_eq!(count("text:\"cross of gold\"").await, phrase);
    node.stop().await.unwrap();
}

/// Merge waits for tests: poll often, settle quickly.
fn quick_merges() -> usnm_ingest::merges::MergeWait {
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

/// A real Quickwit writer merges an index cut into many small splits down
/// to one before the release may publish it: the planner leaves splits
/// below its merge factor alone, and closing the index merges them.
/// Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn a_writer_merges_small_splits_before_the_index_is_sealed() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::merges::{self, Node};
    use usnm_ingest::sink::{QuickwitNode, INDEX_TEMPLATE};
    let dir = tempfile::tempdir().unwrap();
    let qw = dir.path().join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    // A previous run's data is cleared when the node starts.
    std::fs::create_dir_all(qw.join("qwdata/wal")).unwrap();
    std::fs::write(qw.join("qwdata/wal/stale"), b"x").unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7395, &meta, &root)
        .await
        .unwrap();
    assert!(!qw.join("qwdata/wal/stale").exists());

    let http = reqwest::Client::new();
    let id = "pages-delta-20261015-1";
    let config = INDEX_TEMPLATE
        .replace("${INDEX_ID}", id)
        .replace("${INDEX_URI}", &format!("{root}/{id}"));
    http.post(format!("{}/api/v1/indexes", node.url))
        .header("content-type", "application/yaml")
        .body(config)
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    // Six forced commits: six splits, under the merge factor of 10.
    let docs: Vec<Value> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|f| read_jsonl(&fixtures().join(format!("indexes/{f}.jsonl"))))
        .collect();
    for chunk in docs.chunks(docs.len().div_ceil(6)) {
        let body: String = chunk.iter().map(|d| format!("{d}\n")).collect();
        let v: Value = http
            .post(format!("{}/api/v1/{id}/ingest?commit=force", node.url))
            .body(body)
            .send()
            .await
            .unwrap()
            .error_for_status()
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(v["num_rejected_docs"], 0);
    }
    let n = Node {
        http: &http,
        base: &node.url,
    };
    assert_eq!(n.splits(id).await.unwrap().len(), 6);
    let layout = merges::seal(
        &n,
        id,
        docs.len() as u64,
        Some(&node.events()),
        &quick_merges(),
        None,
    )
    .await
    .unwrap();
    assert_eq!((layout.splits, layout.docs), (1, docs.len() as u64));

    // When the final merges stop (issue #84), the release reopens the index
    // to run them again: the writer rejects the reopening document, starts a
    // new merge pipeline, and runs its final merges once the index is closed
    // again. Nothing changes in a merged index, and it ends closed.
    let events = node.events();
    let spawns = events.spawns(id);
    merges::rerun_final_merges(&n, id, &events, &quick_merges(), deadline(120))
        .await
        .unwrap();
    assert_eq!(events.spawns(id), spawns + 1);
    let until = deadline(120);
    while events.final_merges(id) != merges::FinalMerges::Done {
        assert!(
            tokio::time::Instant::now() < until,
            "{:?}",
            events.final_merges(id)
        );
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    }
    let splits = n.splits(id).await.unwrap();
    assert_eq!(splits.len(), 1);
    assert_eq!(splits[0].docs, docs.len() as u64);
    let index: Value = http
        .get(format!("{}/api/v1/indexes/{id}", node.url))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let source = index["sources"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["source_id"] == "_ingest-source")
        .unwrap();
    assert_eq!(source["enabled"], false);
    node.stop().await.unwrap();
}

fn deadline(secs: u64) -> tokio::time::Instant {
    tokio::time::Instant::now() + std::time::Duration::from_secs(secs)
}

/// A batch can hold pages but no text (every page's OCR empty or failed):
/// the release then commits nothing. A real writer starts no ingest-source
/// pipeline for an index that was never written, so there is no "merge
/// pipeline completed" line to wait for; the empty index seals anyway.
/// Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn a_writer_seals_an_index_that_received_no_documents() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_ingest::sink::{IndexSink, QuickwitNode, QuickwitSink};
    let dir = tempfile::tempdir().unwrap();
    let qw = dir.path().join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7397, &meta, &root)
        .await
        .unwrap();
    let id = "pages-delta-20261022-1";
    let mut sink = QuickwitSink::new(&node.url, &root)
        .unwrap()
        .watching(&node)
        .merges(usnm_ingest::merges::MergeWait {
            timeout: std::time::Duration::from_secs(45),
            ..quick_merges()
        });
    sink.create(id).await.unwrap();
    sink.finish(0).await.unwrap();
    let layout = sink.layout(&[id.to_owned()]).await.unwrap();
    assert_eq!((layout[0].splits, layout[0].docs), (0, 0));
    node.stop().await.unwrap();
}

/// A real LoC bulk OCR archive (see `tests/data/README.md`): the layout,
/// compression and checksum as LoC publishes them.
#[tokio::test]
async fn curates_a_real_loc_archive() {
    let e = env().await;
    let archive =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/dlc_zurich_ver04.tar.bz2");
    let list = source::parse_list(
        format!(
            r#"{{"datasets": [{{"batch": "dlc_zurich_ver04", "url": "{}", "page_count": 4,
            "sha256": "ad36f7bb2ef915867460ac2b3c70f9aaa33709a9a99f554a49a9dc4fc35f3ba9"}}]}}"#,
            archive.display()
        )
        .as_bytes(),
    )
    .unwrap();
    source::enqueue(&e.state, &list).await.unwrap();
    assert_eq!(e.worker("w").run(None).await.unwrap(), 1);
    let (b, _) = e.state.batch("dlc_zurich").await.unwrap().unwrap();
    let c = b.curated.unwrap();
    assert_eq!(
        (c.version, c.pages, c.lccns.as_slice()),
        (4, 4, &["sn85042252".to_owned()][..])
    );
    assert_eq!(
        (c.first.as_str(), c.last.as_str()),
        ("1865-08-10", "1865-08-10")
    );
    let mut rows = Vec::new();
    let part = e.curated.get(&c.parts[0]).await.unwrap().unwrap();
    usnm_ingest::curated::read_part(part.into(), true, |r| {
        rows.push(r);
        Ok(())
    })
    .unwrap();
    let mut seqs: Vec<u16> = rows.iter().map(|r| r.key.seq).collect();
    seqs.sort_unstable();
    assert_eq!(seqs, [1, 2, 3, 4]);
    assert!(rows
        .iter()
        .all(|r| r.text.as_ref().is_some_and(|t| t.len() > 500)));
}

/// A release that crashed after writing `current.json` but before recording
/// the publish in Cosmos: the next release treats `current.json` as the
/// truth, repairs the state and builds on the version that is live.
#[tokio::test]
async fn recovers_from_a_crash_between_publish_and_bookkeeping() {
    use usnm_ingest::state::RunStatus;
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    source::enqueue(&e.state, &[listed("batch_fx_early_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p1 = e.release(1, false).await.unwrap();
    // A publish well before the restart, so the restart's time can't match it.
    let mut pointer = e.reference_json("current.json").await;
    pointer["published_at"] = "2026-10-01T04:00:00Z".into();
    e.reference
        .put(
            "current.json",
            serde_json::to_vec(&pointer).unwrap(),
            "application/json",
        )
        .await
        .unwrap();

    // Undo the bookkeeping, as if the process died right after current.json.
    let (mut run, etag) = e.state.run(&p1.index_version).await.unwrap().unwrap();
    run.status = RunStatus::Building;
    run.published_at = None;
    e.state.update_run(&run, &etag).await.unwrap();
    e.state
        .set_current_version("pages-v20260901-1")
        .await
        .unwrap();

    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert_eq!(
        p2.indexes,
        ["pages-base-20261001-1", "pages-delta-20261008-1"]
    );
    assert_eq!(
        e.state
            .run(&p1.index_version)
            .await
            .unwrap()
            .unwrap()
            .0
            .status,
        RunStatus::Published
    );
    // The repaired run takes current.json's time, not the restart's.
    let repaired = e.state.run(&p1.index_version).await.unwrap().unwrap().0;
    assert_eq!(
        repaired
            .published_at
            .map(|t| t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)),
        Some("2026-10-01T04:00:00Z".to_owned())
    );
    assert_eq!(
        e.state.current_version().await.unwrap().as_deref(),
        Some(p2.index_version.as_str())
    );
}

/// Workers that die mid-batch never record a failure; the attempt cap still
/// stops the batch being retried forever.
#[tokio::test]
async fn batches_abandoned_by_crashed_workers_fail_at_the_attempt_cap() {
    let e = env().await;
    let a = e.root.join("batch_fx_x_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(3).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_x_ver01", &a, None)])
        .await
        .unwrap();
    for _ in 0..usnm_ingest::worker::MAX_ATTEMPTS {
        let w = e.worker("crashy");
        let (mut b, _) = w.claim().await.unwrap().unwrap();
        // Crash: the lease simply runs out.
        let (_, etag) = e.state.batch(&b.batch).await.unwrap().unwrap();
        b.lease.as_mut().unwrap().until = Utc::now() - chrono::Duration::seconds(1);
        e.state.replace_batch(&b, &etag).await.unwrap().unwrap();
    }
    assert!(e.worker("w").claim().await.unwrap().is_none());
    let (b, _) = e.state.batch("batch_fx_x").await.unwrap().unwrap();
    assert_eq!(b.status, BatchStatus::Failed);
    assert!(b.last_error.unwrap().contains("abandoned"));
}

/// Records documents and claims to be Quickwit: stands in for a backend switch.
struct OtherBackend(u64);

#[async_trait::async_trait]
impl usnm_ingest::sink::IndexSink for OtherBackend {
    async fn create(&mut self, _index_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn add(&mut self, _doc: &Value) -> anyhow::Result<()> {
        self.0 += 1;
        Ok(())
    }
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        anyhow::ensure!(self.0 == expected);
        Ok(())
    }
    fn backend(&self) -> &'static str {
        "quickwit"
    }
}

/// Deltas only stack on indexes in the same engine: switching backend
/// rebuilds everything into a new base there.
#[tokio::test]
async fn a_backend_switch_forces_a_full_release() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    source::enqueue(&e.state, &[listed("batch_fx_early_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    e.release(1, false).await.unwrap(); // memory
    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let r = Release {
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: false,
        synthetic: true,
        now: Utc.with_ymd_and_hms(2026, 10, 8, 3, 0, 0).unwrap(),
        titles_left: None,
    };
    let mut sink = OtherBackend(0);
    let p = r.run(&mut sink).await.unwrap().unwrap();
    assert!(p.full);
    assert_eq!(p.indexes, ["pages-base-20261008-1"]);
    assert_eq!(sink.0, p.docs);
    assert_eq!(
        e.reference_json("current.json").await["backend"],
        "quickwit"
    );
}

/// A release that no longer holds the writer lock stops without publishing.
#[tokio::test]
async fn a_release_that_loses_the_writer_lock_does_not_publish() {
    let e = env().await;
    let a = e.root.join("batch_fx_x_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(40).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_x_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let r = Release {
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: false,
        synthetic: true,
        now: Utc::now(),
        titles_left: None,
    };
    let lease = r.lock().await.unwrap();
    // Another writer takes the lock over (e.g. after this one stalled).
    let taken = serde_json::json!({
        "id": "quickwit-writer", "kind": "quickwit-writer", "owner": "someone-else",
        "until": Utc::now() + chrono::Duration::hours(1),
    });
    e.state
        .docs
        .upsert("ops", "quickwit-writer", &taken)
        .await
        .unwrap();
    let mut sink = JsonlSink::new(e.root.join("idx"));
    assert!(r.run_held(&mut sink, &lease).await.is_err());
    assert!(e.reference.get("current.json").await.unwrap().is_none());
}

/// A batch whose title isn't in the catalog yet waits for a later release;
/// the batches that are ready publish without it.
#[tokio::test]
async fn batches_with_uncatalogued_titles_wait_for_a_later_release() {
    let e = env().await;
    let pages = fixture_pages();
    let ready: Vec<&Page> = pages
        .iter()
        .filter(|p| !p.text.is_empty())
        .take(20)
        .collect();
    let unknown = Page {
        lccn: "sn99999999".into(),
        date: ready[0].date,
        seq: 1,
        text: "armistice".into(),
    };
    let a = e.root.join("batch_fx_ready_ver01.tar.gz");
    let b = e.root.join("batch_fx_new_ver01.tar.gz");
    write_archive(&a, &ready, true, true);
    write_archive(&b, &[&unknown], true, true);
    source::enqueue(
        &e.state,
        &[
            listed("batch_fx_ready_ver01", &a, None),
            listed("batch_fx_new_ver01", &b, None),
        ],
    )
    .await
    .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p = e.release(1, false).await.unwrap();
    assert_eq!(p.pages, 20);
    // Nothing is ready to add until the title is catalogued.
    assert!(e.release(2, false).await.is_none());
}

/// With titles-sync unfinished, a full base, asked for or forced (there is
/// no published version yet), is refused; a delta goes ahead.
#[tokio::test]
async fn an_unfinished_titles_sync_holds_back_a_full_release_only() {
    let e = env().await;
    let pages = fixture_pages();
    let some: Vec<&Page> = pages
        .iter()
        .filter(|p| !p.text.is_empty())
        .take(20)
        .collect();
    let a = e.root.join("batch_fx_some_ver01.tar.gz");
    write_archive(&a, &some, true, true);
    source::enqueue(&e.state, &[listed("batch_fx_some_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let release = |full: bool, titles_left: Option<String>| Release {
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full,
        synthetic: true,
        now: Utc.with_ymd_and_hms(2026, 10, 1, 3, 0, 0).unwrap(),
        titles_left,
    };
    let why = Some("LoC rate limited titles-sync with 5 of 9 titles left".to_owned());
    let mut sink = JsonlSink::new(e.root.join("reference/indexes"));
    // Not asked for, but forced: nothing is published yet.
    let err = release(false, why.clone())
        .run(&mut sink)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("nothing was released"), "{err}");
    assert!(release(false, None).run(&mut sink).await.unwrap().is_some());
    let err = release(true, why.clone())
        .run(&mut sink)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("5 of 9 titles left"), "{err}");
    // A delta goes ahead (here with nothing new to add).
    assert!(release(false, why).run(&mut sink).await.unwrap().is_none());
}

/// A published title the catalog puts in another place (a merge) stays
/// where it was published until a full release, and a corrected point
/// rides a delta with the published place's id (04 §4.6).
#[tokio::test]
async fn a_title_moved_to_another_place_waits_for_a_full_release() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let (late_a, late_b) = late.split_at(late.len() / 2);
    let archives: Vec<(String, PathBuf)> = [("early", &early[..]), ("a", late_a), ("b", late_b)]
        .iter()
        .map(|(name, pages)| {
            let name = format!("batch_fx_{name}_ver01");
            let path = e.root.join(format!("{name}.tar.gz"));
            write_archive(&path, pages, true, true);
            (name, path)
        })
        .collect();
    let add = |i: usize| {
        let (name, path) = archives[i].clone();
        let e = &e;
        async move {
            source::enqueue(&e.state, &[listed(&name, &path, None)])
                .await
                .unwrap();
            e.worker("w").run(None).await.unwrap();
        }
    };
    let edit = |f: Box<dyn Fn(&mut Value)>, path: &'static str| {
        let e = &e;
        async move {
            let mut v = e.reference_json(path).await;
            f(&mut v);
            e.reference
                .put(path, serde_json::to_vec(&v).unwrap(), "application/json")
                .await
                .unwrap();
        }
    };
    add(0).await;
    let p1 = e.release(1, false).await.unwrap();

    // A corrected point for a published place: a delta, whose snapshot has
    // the new point under the same id.
    edit(
        Box::new(|v| v[0]["lat"] = serde_json::json!(41.5)),
        "catalog/places.json",
    )
    .await;
    add(1).await;
    let p2 = e.release(2, false).await.unwrap();
    assert!(!p2.full);
    assert_ne!(p2.index_version, p1.index_version);
    let places = e
        .reference_json(&format!("{}/places.json", p2.index_version))
        .await;
    let first = places
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "P00001")
        .unwrap();
    assert_eq!(first["lat"], 41.5);

    // A published title moved to another place: the delta keeps it where
    // it was published, so its new pages join its published ones.
    let moved = late_b
        .iter()
        .find(|p| !p.text.is_empty())
        .unwrap()
        .lccn
        .clone();
    let catalog = e.reference_json("catalog/titles.json").await;
    let at = catalog
        .as_array()
        .unwrap()
        .iter()
        .position(|t| t["lccn"] == moved.as_str())
        .unwrap();
    let was = catalog[at]["place_id"].as_str().unwrap().to_owned();
    let to = if was == "P00001" { "P00002" } else { "P00001" };
    edit(
        Box::new(move |v| v[at]["place_id"] = serde_json::json!(to)),
        "catalog/titles.json",
    )
    .await;
    add(2).await;
    let p3 = e.release(3, false).await.unwrap();
    assert!(!p3.full, "no full base on its own (#172)");
    let place_in = |titles: &Value| {
        titles
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["lccn"] == moved.as_str())
            .unwrap()["place_id"]
            .as_str()
            .unwrap()
            .to_owned()
    };
    let titles = e
        .reference_json(&format!("{}/titles.json", p3.index_version))
        .await;
    assert_eq!(place_in(&titles), was, "kept where it was published");
    let delta = e.index(p3.indexes.last().unwrap());
    let of_title: Vec<&Value> = delta
        .values()
        .filter(|d| d["lccn"] == moved.as_str())
        .collect();
    assert!(!of_title.is_empty());
    assert!(of_title.iter().all(|d| d["place_id"] == was.as_str()));

    // A full release moves it everywhere.
    let p4 = e.release(4, true).await.unwrap();
    assert!(p4.full);
    let titles = e
        .reference_json(&format!("{}/titles.json", p4.index_version))
        .await;
    assert_eq!(place_in(&titles), to);
    let docs = e.index(&p4.indexes[0]);
    let of_title: Vec<&Value> = docs
        .values()
        .filter(|d| d["lccn"] == moved.as_str())
        .collect();
    assert!(!of_title.is_empty());
    assert!(of_title.iter().all(|d| d["place_id"] == to));
}

/// The pipeline state with writes of the release's progress item failing,
/// as when Cosmos throttles or drops one request.
struct NoProgress(MemoryDocs, Arc<std::sync::atomic::AtomicUsize>);

#[async_trait::async_trait]
impl usnm_ingest::docs::DocStore for NoProgress {
    async fn get(
        &self,
        c: &str,
        pk: &str,
        id: &str,
    ) -> anyhow::Result<Option<usnm_ingest::docs::Versioned>> {
        self.0.get(c, pk, id).await
    }
    async fn create(&self, c: &str, pk: &str, doc: &Value) -> anyhow::Result<Option<String>> {
        self.0.create(c, pk, doc).await
    }
    async fn replace(
        &self,
        c: &str,
        pk: &str,
        doc: &Value,
        etag: &str,
    ) -> anyhow::Result<Option<String>> {
        self.0.replace(c, pk, doc, etag).await
    }
    async fn upsert(&self, c: &str, pk: &str, doc: &Value) -> anyhow::Result<()> {
        if pk == "release-progress" {
            self.1.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            anyhow::bail!("Cosmos upsert in `ops` returned 503 Service Unavailable");
        }
        self.0.upsert(c, pk, doc).await
    }
    async fn list(
        &self,
        c: &str,
        field: &str,
        values: &[&str],
    ) -> anyhow::Result<Vec<usnm_ingest::docs::Versioned>> {
        self.0.list(c, field, values).await
    }
}

/// Recording progress for the status page never fails a release.
#[tokio::test]
async fn a_release_publishes_when_its_progress_cannot_be_recorded() {
    let mut e = env().await;
    let attempts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    e.state = State::new(Arc::new(NoProgress(
        MemoryDocs::default(),
        attempts.clone(),
    )));
    let a = e.root.join("batch_fx_p_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(40).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_p_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    // The commit records when the batch was curated.
    let (b, _) = e.state.batch("batch_fx_p").await.unwrap().unwrap();
    assert!(b.curated_at.is_some_and(|t| t == b.updated_at));
    let p = e.release(1, true).await.unwrap();
    assert!(p.docs > 0);
    assert!(e.reference.get("current.json").await.unwrap().is_some());
    // The release did try (the first report is immediate).
    assert!(attempts.load(std::sync::atomic::Ordering::SeqCst) > 0);
}

/// Curate one small real batch, then add `n` curated batches that copy its
/// parts and counts to paths as long as production's (a job replica's
/// owner id in every attempt path) and list every fixture title.
async fn synthetic_batches(e: &Env, n: usize) -> Vec<String> {
    let pages = fixture_pages();
    let mut one_per_title: BTreeMap<&str, &Page> = BTreeMap::new();
    for p in pages.iter().filter(|p| !p.text.is_empty()) {
        one_per_title.entry(p.lccn.as_str()).or_insert(p);
    }
    let seed: Vec<&Page> = one_per_title.into_values().collect();
    let a = e.root.join("batch_fx_seed_ver01.tar.gz");
    write_archive(&a, &seed, true, true);
    source::enqueue(&e.state, &[listed("batch_fx_seed_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let (template, _) = e.state.batch("batch_fx_seed").await.unwrap().unwrap();
    let c = template.curated.clone().unwrap();
    let part = e.curated.get(&c.parts[0]).await.unwrap().unwrap();
    let counts = e.curated.get(&c.counts).await.unwrap().unwrap();
    let mut names = Vec::new();
    for i in 0..n {
        let name = format!("xx_synthetic{i:05}");
        let prefix = format!(
            "pages/{name}/v01/20260929T101112123456Z-caj-usnm-backfill-4f7x2-kq9zd-1-0badf00d-a1"
        );
        let mut curated = c.clone();
        curated.parts = Vec::new();
        for k in 0..2 {
            let path = format!("{prefix}/part-{k:04}.parquet");
            e.curated
                .put(&path, part.clone(), "application/octet-stream")
                .await
                .unwrap();
            curated.parts.push(path);
        }
        curated.counts = format!("{prefix}/counts.json");
        e.curated
            .put(&curated.counts, counts.clone(), "application/json")
            .await
            .unwrap();
        let mut b = template.clone();
        b.id = name.clone();
        b.batch = name.clone();
        b.curated = Some(curated);
        assert!(e.state.create_batch(&b).await.unwrap());
        names.push(name);
    }
    names
}

/// A version's batch list grows with the corpus; its Cosmos run item must
/// not (Cosmos DB refuses items over 2 MB). The list lives in the version's
/// reference snapshot, and the next incremental release reads it from there.
#[tokio::test]
async fn run_items_stay_small_however_many_batches_a_version_has() {
    const N: usize = 3_000;
    let e = env().await;
    synthetic_batches(&e, N).await;
    let p1 = e.release(1, true).await.unwrap();
    let v1 = &p1.index_version;
    let item = serde_json::to_vec(&raw_run(&e, v1).await).unwrap().len();
    let list = e
        .reference
        .get(&format!("{v1}/batches.json"))
        .await
        .unwrap()
        .unwrap()
        .len();
    eprintln!(
        "{} batches: run item {item} bytes, batch list {list} bytes ({} per batch)",
        N + 1,
        list / (N + 1)
    );
    assert!(item < 64 * 1024, "run item is {item} bytes");
    // Inline, the list alone would be most of the way to the limit.
    assert!(list > 1_000_000, "batch list is {list} bytes");
    let (run, _) = e.state.run(v1).await.unwrap().unwrap();
    assert_eq!(run.batch_count, Some(N as u64 + 1));
    assert!(run.batches.is_none());

    // One more batch: the next release is a delta with only that batch.
    let pages = fixture_pages();
    let late: Vec<&Page> = pages
        .iter()
        .filter(|p| !p.text.is_empty())
        .rev()
        .take(5)
        .collect();
    let a = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &late, true, true);
    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(p2.indexes.len(), 2);
    assert_eq!(p2.docs, 5);
    let v2 = &p2.index_version;
    let listed2 = e.reference_json(&format!("{v2}/batches.json")).await;
    assert_eq!(listed2.as_array().unwrap().len(), N + 2);
    assert_eq!(raw_run(&e, v2).await["batch_count"], N + 2);
    assert!(serde_json::to_vec(&raw_run(&e, v2).await).unwrap().len() < 64 * 1024);
    // Nothing new since: no release.
    assert!(e.release(9, false).await.is_none());
}

/// Runs published before the batch list moved to the snapshot carry it
/// inline (and their snapshots have no `batches.json`). The next release
/// reads the inline list and writes the new format.
#[tokio::test]
async fn an_incremental_release_builds_on_a_run_with_an_inline_batch_list() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    source::enqueue(&e.state, &[listed("batch_fx_early_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p1 = e.release(1, false).await.unwrap();
    let v1 = &p1.index_version;

    // Rewrite the published run the way older releases stored it.
    let list = e.reference_json(&format!("{v1}/batches.json")).await;
    let mut old = raw_run(&e, v1).await;
    let doc = old.as_object_mut().unwrap();
    doc.remove("batch_count");
    doc.remove("batch_list");
    doc.insert("batches".into(), list);
    e.state.docs.upsert("index_runs", v1, &old).await.unwrap();
    std::fs::remove_file(e.root.join(format!("reference/{v1}/batches.json"))).unwrap();
    let (run, _) = e.state.run(v1).await.unwrap().unwrap();
    assert_eq!(run.batches.as_ref().map(Vec::len), Some(1));

    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    let want_delta = loc_fixture("pages-delta-fixture-1");
    assert_eq!(e.index(&p2.indexes[1]), want_delta);
    let item = raw_run(&e, &p2.index_version).await;
    assert!(item.get("batches").is_none());
    assert_eq!(item["batch_count"], 2);
    let list = e
        .reference_json(&format!("{}/batches.json", p2.index_version))
        .await;
    assert_eq!(list.as_array().unwrap().len(), 2);
    // The older item keeps its inline list.
    assert!(raw_run(&e, v1).await["batches"].is_array());
}

/// A batch list that doesn't match its manifest entry stops the release.
#[tokio::test]
async fn a_batch_list_that_does_not_match_its_manifest_stops_the_release() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let a = e.root.join("batch_fx_early_ver01.tar.gz");
    let b = e.root.join("batch_fx_late_ver01.tar.gz");
    write_archive(&a, &early, true, true);
    write_archive(&b, &late, false, true);
    source::enqueue(&e.state, &[listed("batch_fx_early_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let p1 = e.release(1, false).await.unwrap();
    // Emptied, the published list would make every batch look new.
    e.reference
        .put(
            &format!("{}/batches.json", p1.index_version),
            b"[]".to_vec(),
            "application/json",
        )
        .await
        .unwrap();
    source::enqueue(&e.state, &[listed("batch_fx_late_ver01", &b, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let r = Release {
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: false,
        synthetic: true,
        now: Utc.with_ymd_and_hms(2026, 10, 8, 3, 0, 0).unwrap(),
        titles_left: None,
    };
    let mut sink = JsonlSink::new(e.root.join("idx"));
    let err = format!("{:#}", r.run(&mut sink).await.unwrap_err());
    assert!(err.contains("does not match its manifest entry"), "{err}");
}

/// A sink whose final check fails, as a writer that died mid-merge does.
struct FailingSink;

#[async_trait::async_trait]
impl usnm_ingest::sink::IndexSink for FailingSink {
    async fn create(&mut self, _index_id: &str) -> anyhow::Result<()> {
        Ok(())
    }
    async fn add(&mut self, _doc: &Value) -> anyhow::Result<()> {
        Ok(())
    }
    async fn finish(&mut self, _expected: u64) -> anyhow::Result<()> {
        anyhow::bail!("the writer exited")
    }
    fn backend(&self) -> &'static str {
        "memory"
    }
}

/// A release that fails records the run as failed, with when it failed, for the status page.
#[tokio::test]
async fn a_failed_release_records_when_it_failed() {
    let e = env().await;
    let a = e.root.join("batch_fx_fail_ver01.tar.gz");
    write_archive(
        &a,
        &fixture_pages().iter().take(20).collect::<Vec<_>>(),
        true,
        true,
    );
    source::enqueue(&e.state, &[listed("batch_fx_fail_ver01", &a, None)])
        .await
        .unwrap();
    e.worker("w").run(None).await.unwrap();
    let now = Utc.with_ymd_and_hms(2026, 10, 9, 3, 0, 0).unwrap();
    let r = Release {
        state: e.state.clone(),
        curated: e.curated.clone(),
        reference: e.reference.clone(),
        owner: "releaser".into(),
        full: true,
        synthetic: true,
        now,
        titles_left: None,
    };
    let before = Utc::now();
    let err = r.run(&mut FailingSink).await.unwrap_err();
    assert!(format!("{err:#}").contains("the writer exited"));
    let (run, _) = e
        .state
        .run("pages-v20261009-1")
        .await
        .unwrap()
        .expect("the run is recorded");
    assert_eq!(run.status, usnm_state::state::RunStatus::Failed);
    let failed_at = run.failed_at.expect("failed_at is recorded");
    assert!(
        failed_at >= before,
        "failed_at is when it failed, not when it started"
    );
    assert!(run.last_error.unwrap().contains("the writer exited"));
}

/// Enqueue and curate `list`.
async fn curate(e: &Env, list: Vec<ListedBatch>) {
    let n = list.len();
    assert_eq!(source::enqueue(&e.state, &list).await.unwrap().new, n);
    assert_eq!(e.worker("w").run(None).await.unwrap(), n);
}

/// Pages that ship in two batches are indexed and counted once (04 §4.7).
/// Of a page's copies the one kept has text, then comes from the batch whose
/// name sorts first. A delta can't drop a copy a published index holds, so
/// the snapshot lists it for searches to hide.
#[tokio::test]
async fn pages_in_two_batches_are_kept_once() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, Some(sha256_file(&path)))
    };

    // Week 1, a base: the early pages, and 25 of them again in a batch whose
    // name sorts later, so the early batch's copies are kept.
    curate(
        &e,
        vec![
            archive("batch_fx_early_ver01", &early),
            archive("batch_zz_ver01", &early[..25]),
        ],
    )
    .await;
    // `usnm-ingest duplicates` measures them before any release.
    let report = usnm_ingest::dedup::report(&e.state, e.curated.as_ref())
        .await
        .unwrap();
    assert_eq!(report["duplicate_pages"], 25);
    assert_eq!(report["distinct_pages"], early.len());
    assert_eq!(
        report["pairs"],
        serde_json::json!([{"kept": "batch_fx_early", "other": "batch_zz", "pages": 25}])
    );
    let p1 = e.release(1, false).await.unwrap();
    assert!(p1.full);
    let want_base = loc_fixture("pages-base-fixture");
    let base = e.index(&p1.indexes[0]);
    assert_eq!(base, want_base, "each page once");
    assert_eq!(p1.docs, want_base.len() as u64);
    assert_eq!(p1.pages, early.len() as u64);
    let v1 = &p1.index_version;
    assert_eq!(raw_run(&e, v1).await["duplicate_pages"], 25);
    let manifest = e.reference_json(&format!("{v1}/manifest.json")).await;
    assert_eq!(manifest["duplicate_pages"], 25);
    // A base indexes one copy of everything, so nothing needs hiding.
    assert_eq!(
        e.reference_json(&format!("{v1}/duplicates.json")).await,
        serde_json::json!([])
    );
    let title_pages: BTreeMap<String, u64> =
        serde_json::from_value(e.reference_json(&format!("{v1}/title_pages.json")).await).unwrap();
    assert_eq!(title_pages.values().sum::<u64>(), early.len() as u64);

    // Week 2, a delta: the late pages, and a batch whose name sorts first
    // with 10 more early pages (already published) and 10 of the late ones.
    let again_early: Vec<&Page> = early[30..40].to_vec();
    let mut aa = again_early.clone();
    aa.extend(&late[..10]);
    curate(
        &e,
        vec![
            archive("batch_aa_ver01", &aa),
            archive("batch_fx_late_ver01", &late),
        ],
    )
    .await;
    let p2 = e.release(8, false).await.unwrap();
    assert!(!p2.full);
    assert_eq!(p2.pages, pages.len() as u64);
    let v2 = &p2.index_version;
    assert_eq!(raw_run(&e, v2).await["duplicate_pages"], 25 + 10 + 10);
    // The delta has every late page once, plus batch_aa's copies of the 10
    // early pages: they win, and the base's copies are hidden.
    let want_delta = loc_fixture("pages-delta-fixture-1");
    let delta = e.index(p2.indexes.last().unwrap());
    let early_ids: Vec<String> = again_early
        .iter()
        .map(|p| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq))
        .filter(|id| want_base.contains_key(id))
        .collect();
    assert!(!early_ids.is_empty());
    let mut want = want_delta.clone();
    want.extend(
        early_ids
            .iter()
            .map(|id| (id.clone(), want_base[id].clone())),
    );
    assert_eq!(delta, want);
    let hidden = e.reference_json(&format!("{v2}/duplicates.json")).await;
    let want_hidden: Vec<Value> = early_ids
        .iter()
        .map(|id| serde_json::json!({"doc_id": id, "batch": "batch_fx_early"}))
        .collect();
    let mut got: Vec<Value> = hidden.as_array().unwrap().clone();
    got.sort_by_key(|h| h["doc_id"].as_str().unwrap().to_owned());
    let mut want_hidden = want_hidden;
    want_hidden.sort_by_key(|h| h["doc_id"].as_str().unwrap().to_owned());
    assert_eq!(got, want_hidden);
    // Every page counts once: the snapshot matches the fixture's.
    for f in [
        "baselines.json",
        "title_pages.json",
        "language_baselines.json",
    ] {
        let want: Value =
            serde_json::from_slice(&std::fs::read(fixtures().join("fixture-v1").join(f)).unwrap())
                .unwrap();
        assert_eq!(e.reference_json(&format!("{v2}/{f}")).await, want, "{f}");
    }

    // The API loads the copies to hide with the snapshot.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.duplicate_pages, Some(45));
    assert_eq!(refdata.hidden.len(), early_ids.len());
    assert_eq!(refdata.pages, pages.len() as u64);
    assert!(refdata.index_set().hides(&early_ids[0], "batch_fx_early"));

    // A full release keeps the same copies and needs to hide none.
    let p3 = e.release(15, true).await.unwrap();
    let mut both = want_base.clone();
    both.extend(want_delta);
    assert_eq!(e.index(&p3.indexes[0]), both);
    let v3 = &p3.index_version;
    assert_eq!(
        e.reference_json(&format!("{v3}/duplicates.json")).await,
        serde_json::json!([])
    );
    let kept: BTreeMap<String, String> = read_jsonl(
        &e.root
            .join(format!("reference/indexes/{}.jsonl", p3.indexes[0])),
    )
    .into_iter()
    .map(|d| {
        (
            d["doc_id"].as_str().unwrap().to_owned(),
            d["batch"].as_str().unwrap().to_owned(),
        )
    })
    .collect();
    assert!(early_ids.iter().all(|id| kept[id] == "batch_aa"));
}

/// A version released before duplicates were handled indexed every copy, so
/// the first delta on top of it hides every copy it doesn't keep.
#[tokio::test]
async fn a_delta_on_an_older_version_hides_every_extra_copy() {
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, Some(sha256_file(&path)))
    };
    curate(
        &e,
        vec![
            archive("batch_fx_early_ver01", &early),
            archive("batch_zz_ver01", &early[..25]),
        ],
    )
    .await;
    let p1 = e.release(1, false).await.unwrap();
    // As an older release wrote it: no list of hidden copies.
    std::fs::remove_file(
        e.root
            .join(format!("reference/{}/duplicates.json", p1.index_version)),
    )
    .unwrap();

    curate(&e, vec![archive("batch_fx_late_ver01", &late)]).await;
    let p2 = e.release(8, false).await.unwrap();
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", p2.index_version))
        .await;
    let base = e.index(&p1.indexes[0]);
    let want: Vec<Value> = early[..25]
        .iter()
        .map(|p| format!("{}_{}_ed-1_seq-{}", p.lccn, p.date, p.seq))
        .filter(|id| base.contains_key(id))
        .map(|id| serde_json::json!({"doc_id": id, "batch": "batch_zz"}))
        .collect();
    assert!(!want.is_empty());
    assert_eq!(hidden, Value::Array(want));
}

/// Searches on Quickwit hide the copies a delta couldn't drop: the base
/// holds one copy of a page and the delta the copy that wins, and every
/// count and hit list sees the page once. Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn quickwit_searches_hide_the_copies_a_delta_could_not_drop() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use usnm_core::params::Filters;
    use usnm_core::query::parse;
    use usnm_core::time::{BucketSpec, BucketUnit};
    use usnm_ingest::sink::{QuickwitNode, QuickwitSink};
    use usnm_search::quickwit::QuickwitBackend;
    use usnm_search::{HitsQuery, IndexSet, SearchBackend};

    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let early: Vec<&Page> = pages.iter().filter(|p| p.date < split).collect();
    let again: Vec<&Page> = early[30..40].to_vec();
    let archive = |name: &str, pages: &[&Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, pages, false, true);
        listed(name, &path, None)
    };
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, 7393, &meta, &root)
        .await
        .unwrap();
    let mut published = Vec::new();
    for (day, batch) in [
        (1, archive("batch_fx_early_ver01", &early)),
        (8, archive("batch_aa_ver01", &again)),
    ] {
        curate(&e, vec![batch]).await;
        let r = Release {
            state: e.state.clone(),
            curated: e.curated.clone(),
            reference: e.reference.clone(),
            owner: "releaser".into(),
            full: false,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges());
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    let hidden = e
        .reference_json(&format!("{}/duplicates.json", p.index_version))
        .await;
    let hidden: Vec<(String, String)> = hidden
        .as_array()
        .unwrap()
        .iter()
        .map(|h| {
            (
                h["doc_id"].as_str().unwrap().to_owned(),
                h["batch"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    assert!(!hidden.is_empty());

    let backend = QuickwitBackend::new(&node.url, std::time::Duration::from_secs(30)).unwrap();
    let both = IndexSet::new(p.indexes.clone());
    let deduped = both.clone().hiding(hidden.clone());
    let from = NaiveDate::from_ymd_opt(1895, 1, 1).unwrap();
    let to = NaiveDate::from_ymd_opt(1897, 6, 30).unwrap();
    let f = Filters {
        from,
        to,
        states: vec![],
        lccns: vec![],
        langs: vec![],
        front_only: false,
    };
    let spec = BucketSpec::new(BucketUnit::Year, from, to);
    let every = parse("the OR a OR of OR and").unwrap();
    let with = backend.summary(&both, &every, &f, &spec).await.unwrap();
    let without = backend.summary(&deduped, &every, &f, &spec).await.unwrap();
    assert_eq!(with.total_hits - without.total_hits, hidden.len() as u64);
    let indexed: u64 = ["pages-base-fixture"]
        .iter()
        .map(|f| read_jsonl(&fixtures().join(format!("indexes/{f}.jsonl"))).len() as u64)
        .sum();
    assert_eq!(
        p.docs,
        hidden.len() as u64,
        "the delta holds the winning copies"
    );
    assert!(without.total_hits <= indexed);

    // A hidden page's hit list shows it once, from the batch that won.
    let (doc_id, _) = &hidden[0];
    let key = usnm_core::ids::PageKey::from_doc_id(doc_id).unwrap();
    let day = Filters {
        from: key.date,
        to: key.date,
        lccns: vec![key.lccn.clone()],
        ..f.clone()
    };
    let page = HitsQuery {
        lccn: Some(key.lccn.clone()),
        limit: 50,
        ..HitsQuery::default()
    };
    let ids = |set: &IndexSet| {
        let (backend, day, page, every) = (&backend, &day, &page, &every);
        let set = set.clone();
        async move {
            backend
                .hits(&set, every, day, page)
                .await
                .unwrap()
                .hits
                .into_iter()
                .filter(|h| h.doc_id == *doc_id)
                .count()
        }
    };
    assert_eq!(ids(&both).await, 2);
    assert_eq!(ids(&deduped).await, 1);
    node.stop().await.unwrap();
}

/// One row of a Japanese OCR overlay part, as `ja-ocr/jaocr.py` writes it.
struct JaRow<'a> {
    lccn: &'a str,
    date: NaiveDate,
    seq: u16,
    batch: &'a str,
    loc_text: &'a str,
    text: &'a str,
    ocred_at: i64,
}

async fn put_overlay_part(e: &Env, path: &str, rows: &[JaRow<'_>]) {
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
        ("ocr_engine", s(vec!["ndlocr-lite test".into(); rows.len()])),
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

/// Our Japanese OCR (#139): a release indexes the overlay's pages in their own
/// index, newest copy of each page, only for batches in the version, and adds
/// the pages curation never had to the baselines (not the ones it had).
#[tokio::test]
async fn a_release_indexes_the_japanese_ocr_and_counts_missing_pages() {
    let e = env().await;
    let pages = fixture_pages();
    let path = e.root.join("batch_fx_ja_ver01.tar.gz");
    let all: Vec<&Page> = pages.iter().collect();
    write_archive(&path, &all, false, true);
    curate(
        &e,
        vec![listed("batch_fx_ja_ver01", &path, Some(sha256_file(&path)))],
    )
    .await;

    let p0 = pages.iter().find(|p| !p.text.is_empty()).unwrap();
    // Another page of the same title and day that LoC gave text, in the same batch.
    let p1 = pages
        .iter()
        .find(|p| p.lccn == p0.lccn && p.date == p0.date && p.seq != p0.seq && !p.text.is_empty())
        .expect("two pages with text on one day");
    let same_day = pages
        .iter()
        .filter(|p| p.lccn == p0.lccn && p.date == p0.date)
        .count();
    let row = |seq, batch, loc_text, text, at| JaRow {
        lccn: &p0.lccn,
        date: p0.date,
        seq,
        batch,
        loc_text,
        text,
        ocred_at: at,
    };
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.parquet",
        &[
            row(90, "batch_fx_ja", "missing", "古い読み", 1),
            row(p0.seq, "batch_fx_ja", "garbled", "日本", 1),
            row(91, "batch_elsewhere", "missing", "東京", 1),
            // In the version's batch; a newer copy below is from a batch outside it.
            row(92, "batch_fx_ja", "missing", "大阪", 1),
            // Marked missing (from some archive), but curation has it with text: counted already.
            row(p1.seq, "batch_fx_ja", "missing", "平和", 1),
        ],
    )
    .await;
    put_overlay_part(
        &e,
        "ocr-ja/pages/a.2.parquet",
        &[
            row(90, "batch_fx_ja", "missing", "米國と日本の戰爭", 2),
            row(92, "batch_elsewhere", "missing", "外", 5),
        ],
    )
    .await;

    let published = e.release(5, true).await.expect("published");
    let current = e.reference_json("current.json").await;
    let ja_id = format!(
        "pages-ja-{}",
        published.index_version.trim_start_matches("pages-v")
    );
    assert_eq!(current["ja"]["indexes"], serde_json::json!([ja_id]));
    assert_eq!(current["ja"]["pages"], 4);
    assert_eq!(current["ja"]["fold"], usnm_core::ja::FOLD_VERSION);

    let ja = e.index(&ja_id);
    assert_eq!(ja.len(), 4, "{:?}", ja.keys());
    // The out-of-version copy didn't hide the in-version one.
    assert_eq!(
        ja[&format!("{}_{}_ed-1_seq-92", p0.lccn, p0.date)]["printed"],
        "大阪"
    );
    let missing = &ja[&format!("{}_{}_ed-1_seq-90", p0.lccn, p0.date)];
    // The newest copy wins, and its text is the folded tokens of the printed text.
    assert_eq!(missing["printed"], "米國と日本の戰爭");
    assert_eq!(
        missing["text"],
        usnm_core::ja::index_text("米國と日本の戰爭")
    );
    assert_eq!(missing["ocr_engine"], "ndlocr-lite test");

    // The missing pages join the baselines once; the garbled one, and the one
    // curation had in spite of its label, were counted already.
    let v = &published.index_version;
    let titles: Vec<Value> =
        serde_json::from_value(e.reference_json(&format!("{v}/titles.json")).await).unwrap();
    let place = titles
        .iter()
        .find(|t| t["lccn"] == p0.lccn.as_str())
        .unwrap()["place_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let baselines = e.reference_json(&format!("{v}/baselines.json")).await;
    let day = usnm_core::time::day_number(p0.date);
    let at_day = baselines[&place]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d[0] == day)
        .unwrap()[1]
        .as_u64()
        .unwrap();
    assert_eq!(at_day, same_day as u64 + 2);
    let title_pages = e.reference_json(&format!("{v}/title_pages.json")).await;
    let fixture_title_pages: Value = serde_json::from_slice(
        &std::fs::read(fixtures().join("fixture-v1/title_pages.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        title_pages[&p0.lccn],
        fixture_title_pages[&p0.lccn].as_u64().unwrap() + 2
    );
    // The release's page total is the snapshot's.
    let snapshot_total: u64 = title_pages
        .as_object()
        .unwrap()
        .values()
        .map(|v| v.as_u64().unwrap())
        .sum();
    assert_eq!(published.pages, snapshot_total);

    let record = e.reference_json(&format!("{v}/ocr_ja.json")).await;
    assert_eq!(record["added_to_baselines"], 2);
    // The four pages kept have short test texts: all near-blank (no other kind is listed).
    assert_eq!(record["kinds"]["all"]["near_blank"], 4);
    assert!(record["kinds"]["all"].get("japanese").is_none());
    assert_eq!(record["kinds"]["by_loc_text"]["garbled"]["near_blank"], 1);
    assert_eq!(record["skipped"], 1);
    assert_eq!(record["parts"].as_array().unwrap().len(), 2);
    let manifest = e.reference_json(&format!("{v}/manifest.json")).await;
    assert!(manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .any(|f| f["path"] == "ocr_ja.json"));
    assert_eq!(
        manifest["built_from"]["ocr_ja"].as_array().unwrap().len(),
        2
    );
}

/// New Japanese OCR with no new batches (#139): an incremental release
/// publishes it on the same main indexes, with a new Japanese index and
/// snapshot; with neither new batches nor new OCR, nothing is released.
#[tokio::test]
async fn new_japanese_ocr_alone_is_released_on_the_same_indexes() {
    let e = env().await;
    let pages = fixture_pages();
    let path = e.root.join("batch_fx_ja_ver01.tar.gz");
    let all: Vec<&Page> = pages.iter().collect();
    write_archive(&path, &all, false, true);
    curate(
        &e,
        vec![listed("batch_fx_ja_ver01", &path, Some(sha256_file(&path)))],
    )
    .await;
    let p0 = pages.iter().find(|p| !p.text.is_empty()).unwrap();
    let row = |seq, text| JaRow {
        lccn: &p0.lccn,
        date: p0.date,
        seq,
        batch: "batch_fx_ja",
        loc_text: "missing",
        text,
        ocred_at: 1,
    };

    // A base with the first OCR part.
    put_overlay_part(&e, "ocr-ja/pages/a.parquet", &[row(90, "米國と日本の戰爭")]).await;
    let v1 = e.release(5, true).await.expect("base");
    // No new batches, no new OCR: nothing to release.
    assert!(e.release(6, false).await.is_none());

    // New OCR arrives: released on the same main indexes.
    put_overlay_part(&e, "ocr-ja/pages/b.parquet", &[row(91, "東京の新聞")]).await;
    let v2 = e.release(6, false).await.expect("overlay-only release");
    assert!(!v2.full);
    assert_eq!(v2.indexes, v1.indexes, "no new main index");
    assert_eq!(v2.docs, 0);
    assert_eq!(v2.pages, v1.pages + 1);
    let current = e.reference_json("current.json").await;
    assert_eq!(current["index_version"], v2.index_version.as_str());
    assert_eq!(current["indexes"], serde_json::json!(v1.indexes));
    let ja_id = format!(
        "pages-ja-{}",
        v2.index_version.trim_start_matches("pages-v")
    );
    assert_eq!(current["ja"]["indexes"], serde_json::json!([ja_id]));
    assert_eq!(e.index(&ja_id).len(), 2);
    let run = raw_run(&e, &v2.index_version).await;
    assert_eq!(run["new_index"], ja_id.as_str());
    // Its build record lists only the template it applied (#161).
    assert!(run["build"]["templates"].get("pages").is_none(), "{run}");
    assert!(run["build"]["templates"]["pages-ja"]["sha256"].is_string());
    let manifest = e
        .reference_json(&format!("{}/manifest.json", v2.index_version))
        .await;
    assert_eq!(
        manifest["build"]["templates"]["pages-ja"]["yaml"],
        usnm_ingest::ocr_ja::JA_TEMPLATE
    );
    assert!(manifest["build"]["templates"].get("pages").is_none());
    let record = e
        .reference_json(&format!("{}/ocr_ja.json", v2.index_version))
        .await;
    assert_eq!(record["added_to_baselines"], 2);

    // And again nothing, until something changes.
    assert!(e.release(7, false).await.is_none());
}

/// OCR that changed without a page to index doesn't make an overlay-only
/// release: the run would name a Japanese index it never wrote.
#[tokio::test]
async fn japanese_ocr_with_nothing_to_index_waits() {
    let e = env().await;
    let pages = fixture_pages();
    let path = e.root.join("batch_fx_ja_ver01.tar.gz");
    let all: Vec<&Page> = pages.iter().collect();
    write_archive(&path, &all, false, true);
    curate(
        &e,
        vec![listed("batch_fx_ja_ver01", &path, Some(sha256_file(&path)))],
    )
    .await;
    let p0 = pages.iter().find(|p| !p.text.is_empty()).unwrap();
    e.release(5, true).await.expect("base");
    // A part whose only page is from a batch outside the version: changed, nothing to index.
    put_overlay_part(
        &e,
        "ocr-ja/pages/x.parquet",
        &[JaRow {
            lccn: &p0.lccn,
            date: p0.date,
            seq: 90,
            batch: "batch_elsewhere",
            loc_text: "missing",
            text: "東京",
            ocred_at: 1,
        }],
    )
    .await;
    assert!(e.release(6, false).await.is_none());
}
