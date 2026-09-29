//! End to end on the synthetic corpus: turn the fixture pages back into two
//! LoC-style batch archives, run enqueue → curate → release twice (a base,
//! then a delta), and check the pipeline reproduces the checked-in fixtures
//! exactly: both indexes, the baselines, titles and places. Then load the
//! result the way the API does.

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
        };
        let mut sink = JsonlSink::new(self.root.join("reference/indexes"));
        r.run(&mut sink).await.unwrap()
    }

    fn index(&self, id: &str) -> BTreeMap<String, Value> {
        by_id(read_jsonl(
            &self.root.join(format!("reference/indexes/{id}.jsonl")),
        ))
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

    let want_base = by_id(read_jsonl(
        &fixtures().join("indexes/pages-base-fixture.jsonl"),
    ));
    assert_eq!(e.index("pages-base-20261001-1"), want_base);
    assert_eq!(p1.docs, want_base.len() as u64);

    // Nothing new: no release.
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
    let want_delta = by_id(read_jsonl(
        &fixtures().join("indexes/pages-delta-fixture-1.jsonl"),
    ));
    assert_eq!(e.index("pages-delta-20261008-1"), want_delta);

    // The reference snapshot matches the fixture's, file for file.
    let v = &p2.index_version;
    for f in ["baselines.json", "titles.json", "places.json"] {
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

    // The API loads it: checksums, manifest and version pairing all hold.
    let refdata = usnm_api::refdata::RefData::load(e.reference.as_ref())
        .await
        .unwrap();
    assert_eq!(refdata.version(), v);
    assert_eq!(refdata.places.len(), 6);

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
        };
        let mut sink = QuickwitSink::new(&node.url, &root).unwrap();
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
    assert_eq!(
        p.indexes,
        ["pages-base-20261001-1", "pages-delta-20261008-1"]
    );
    let current = e.reference_json("current.json").await;
    assert_eq!(current["backend"], "quickwit");

    // Every fixture document is searchable across base + delta, and a
    // phrase matches exactly as often as in the fixture corpus.
    let http = reqwest::Client::new();
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
        .flat_map(|f| read_jsonl(&fixtures().join(format!("indexes/{f}.jsonl"))))
        .collect();
    assert_eq!(count("*").await, fixture_docs.len() as u64);
    let phrase = fixture_docs
        .iter()
        .filter(|d| d["text"].as_str().unwrap().contains("cross of gold"))
        .count() as u64;
    assert_eq!(count("text:\"cross of gold\"").await, phrase);
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

    // Undo the bookkeeping, as if the process died right after current.json.
    let (mut run, etag) = e.state.run(&p1.index_version).await.unwrap().unwrap();
    run.status = RunStatus::Building;
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
    let want_delta = by_id(read_jsonl(
        &fixtures().join("indexes/pages-delta-fixture-1.jsonl"),
    ));
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
    };
    let mut sink = JsonlSink::new(e.root.join("idx"));
    let err = format!("{:#}", r.run(&mut sink).await.unwrap_err());
    assert!(err.contains("does not match its manifest entry"), "{err}");
}
