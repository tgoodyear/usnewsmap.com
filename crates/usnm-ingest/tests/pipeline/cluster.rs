//! The search cluster's sample (#239): the release's documents, a two-node
//! cluster that searches them, and a packaged set.

use std::collections::BTreeMap;
use std::sync::Arc;

use chrono::NaiveDate;
use serde_json::Value;
use usnm_ingest::source;
use usnm_store::{LocalStore, ObjectStore};

use crate::common::{
    curate, curate_early_and_late, env, fixture_pages, listed, put_american_stories, read_jsonl,
    sha256_file, write_archive, Page,
};

/// The search cluster's sample (#239, `usnm_ingest::cluster::sample`) at
/// 100% is exactly the documents a full release with American Stories'
/// text builds; a smaller share is a subset of them, the same every run.
#[tokio::test]
async fn a_full_sample_is_the_release_s_documents() {
    use usnm_ingest::cluster::sample;
    let e = env().await;
    let (early, late, _, _) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    curate(&e, late).await;
    let p = e.release_with(1, true, true).await.unwrap().unwrap();
    let released: BTreeMap<String, Value> = p
        .indexes
        .iter()
        .flat_map(|id| {
            read_jsonl(&e.root.join(format!("reference/indexes/{id}.jsonl")))
                .into_iter()
                .map(|d| (d["doc_id"].as_str().unwrap().to_owned(), d))
        })
        .collect();
    let out = LocalStore::new(e.root.join("bench"));
    let take = |cut: u32, name: &'static str| {
        let (curated, reference, out) = (e.curated.clone(), e.reference.clone(), &out);
        async move {
            let spec = sample::Spec {
                cut,
                american_stories: true,
                concurrency: 2,
                part_bytes: 4096,
                max_batches: None,
            };
            let published = sample::published(reference.as_ref()).await.unwrap();
            let m = sample::build(curated, published, out, &format!("sample/{name}"), &spec)
                .await
                .unwrap();
            let mut docs = BTreeMap::new();
            for part in &m.parts {
                let ndjson = sample::part(out, &part.path).await.unwrap();
                for line in ndjson.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                    let d: Value = serde_json::from_slice(line).unwrap();
                    docs.insert(d["doc_id"].as_str().unwrap().to_owned(), d);
                }
            }
            (m, docs)
        }
    };
    let (m, all) = take(10_000, "all").await;
    assert_eq!(all, released, "every page, every field");
    assert_eq!(m.docs, released.len() as u64);
    // Each batch is many times part_bytes: its lines are split across
    // parts, every part whole lines and at most part_bytes, but for a
    // document longer than that on its own.
    assert!(
        m.parts.len() > 3 * m.batches as usize,
        "{} parts",
        m.parts.len()
    );
    let longest = all
        .values()
        .map(|d| serde_json::to_vec(d).unwrap().len() as u64 + 1)
        .max()
        .unwrap();
    for p in &m.parts {
        assert!(p.bytes <= 4096 || p.docs == 1, "{p:?}");
        assert!(p.bytes <= 4096.max(longest), "{p:?}");
    }
    assert_eq!(m.parts.iter().map(|p| p.docs).sum::<u64>(), m.docs);
    assert!(m.docs_with_as > 0 && m.docs_only_as > 0, "{m:?}");
    assert_eq!(m.version, p.index_version);
    let (half, docs) = take(5_000, "half").await;
    assert!(docs.len() < all.len() && !docs.is_empty());
    assert!(docs.iter().all(|(id, d)| all[id] == *d));
    assert!(docs.keys().all(|id| sample::sampled(id, 5_000)));
    // The same pages again.
    let (_, again) = take(5_000, "half-again").await;
    assert_eq!(
        again.keys().collect::<Vec<_>>(),
        docs.keys().collect::<Vec<_>>()
    );
    let read_back = sample::manifest(&out, "sample/half").await.unwrap();
    assert_eq!(read_back.docs, half.docs);
    // A finished sample is never replaced.
    let spec = sample::Spec {
        cut: 100,
        american_stories: true,
        concurrency: 1,
        part_bytes: 4096,
        max_batches: None,
    };
    let published = sample::published(e.reference.as_ref()).await.unwrap();
    let err = sample::build(e.curated.clone(), published, &out, "sample/half", &spec)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("already exists"), "{err}");
    assert_eq!(
        sample::manifest(&out, "sample/half").await.unwrap(),
        read_back
    );
}

/// A two-node search cluster on localhost (#239): each node starts through
/// `usnm-qwcluster node` and finds the other in the seed registry, the
/// sample loads into an index with a shard on each indexer, the bench
/// searches it through node 0 as the API would, and a node that restarts
/// joins again. Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn a_two_node_cluster_indexes_and_searches_a_sample() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    use std::time::Duration;
    use usnm_ingest::cluster::{bench, load, members, sample};
    let e = env().await;
    let (early, late, _, _) = curate_early_and_late(&e).await;
    put_american_stories(&e).await;
    curate(&e, early).await;
    curate(&e, late).await;
    e.release_with(1, true, true).await.unwrap().unwrap();
    let store = LocalStore::new(e.root.join("bench"));
    let spec = sample::Spec {
        cut: 10_000,
        american_stories: true,
        concurrency: 2,
        part_bytes: sample::PART_BYTES,
        max_batches: None,
    };
    let m = sample::build(
        e.curated.clone(),
        sample::published(e.reference.as_ref()).await.unwrap(),
        &store,
        "sample/all",
        &spec,
    )
    .await
    .unwrap();

    let qw = e.root.join("qw");
    let registry = qw.join("registry");
    let meta = format!("file://{}", qw.join("meta").display());
    let indexes = format!("file://{}", qw.join("indexes").display());
    let start = |id: &str, services: &str, port: u16| {
        let log = std::fs::File::create(qw.join(format!("{id}.log"))).unwrap();
        tokio::process::Command::new(env!("CARGO_BIN_EXE_usnm-qwcluster"))
            .args(["node", "--node-id", id, "--services", services])
            .arg("--registry")
            .arg(&registry)
            .args(["--metastore", &meta, "--index-root", &indexes])
            .args(["--rest-port", &port.to_string(), "--advertise", "127.0.0.1"])
            .arg("--data-dir")
            .arg(qw.join(id))
            .arg("--quickwit-bin")
            .arg(&bin)
            .env("RUST_LOG", "warn")
            .stdout(log.try_clone().unwrap())
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .unwrap()
    };
    std::fs::create_dir_all(&qw).unwrap();
    let _n0 = start(
        "qw-0",
        "metastore,control_plane,janitor,indexer,searcher",
        7580,
    );
    let mut n1 = start("qw-1", "indexer,searcher", 7590);
    let root = "http://127.0.0.1:7580";
    let http = reqwest::Client::new();
    let logs = || {
        ["qw-0", "qw-1"]
            .map(|n| std::fs::read_to_string(qw.join(format!("{n}.log"))).unwrap_or_default())
            .join("\n----\n")
    };
    let (found, ready) = members::wait_ready(&http, root, 2, Duration::from_secs(120))
        .await
        .unwrap_or_else(|err| panic!("{err:#}\n{}", logs()));
    assert!(ready, "{found:?}\n{}", logs());
    assert_eq!(found.len(), 2);
    assert!(found[0].runs("metastore") && found[1].runs("indexer"));
    assert_eq!(found[1].rest_url.as_deref(), Some("http://127.0.0.1:7590"));

    let mut spec = load::Spec::new(root, "sample-2ix", &indexes);
    spec.split_docs = 20;
    spec.chunk_bytes = 16 * 1024;
    spec.poll = Duration::from_millis(500);
    spec.finalize_grace = Duration::from_secs(5);
    spec.merge_timeout = Duration::from_secs(300);
    let report = load::run(&store, "sample/all", &spec)
        .await
        .unwrap_or_else(|err| panic!("{err:#}\n{}", logs()));
    assert_eq!(report.docs, m.docs);
    assert_eq!(report.splits.docs, m.docs);
    assert_eq!(report.indexers, ["qw-0", "qw-1"]);
    assert_eq!(report.min_shards, 2);
    // Which indexer gets the documents is the router's choice: a load this
    // small can land on one shard before the router learns of the other.
    assert!(report.splits.splits >= 1, "{:?}", report.splits);
    assert!(report.commit_secs <= report.settle_secs && report.settle_secs <= report.seal_secs);
    let indexed: f64 = report.nodes.values().map(|n| n.docs_indexed).sum();
    assert_eq!(indexed as u64, m.docs, "{:?}", report.nodes);
    eprintln!(
        "splits by node {:?}, docs by node {:?}",
        report.splits.by_node,
        report
            .nodes
            .iter()
            .map(|(n, c)| (n, c.docs_indexed))
            .collect::<Vec<_>>()
    );

    let bounds = (
        NaiveDate::parse_from_str(&m.bounds.0, "%Y-%m-%d").unwrap(),
        NaiveDate::parse_from_str(&m.bounds.1, "%Y-%m-%d").unwrap(),
    );
    let bspec = bench::Spec {
        root: root.into(),
        indexes: vec!["sample-2ix".into()],
        american_stories: true,
        bounds,
        analyzer: usnm_core::common_grams::analyzer(m.common_grams).unwrap(),
        levels: vec![1, 4],
        offset: 0,
        pause: Duration::ZERO,
        levels_only: false,
        expect_searchers: 2,
        timeout: Duration::from_secs(60),
        wait_split_cache: None,
        // Quickwit's per-split resource stats over gRPC (#251).
        probe: true,
        profile: None,
        variant: None,
    };
    let b = bench::run("local", &bspec).await.unwrap();
    let probe = b.probe.as_ref().unwrap();
    assert!(
        probe.searches.iter().all(|p| p.error.is_none()),
        "{probe:?}"
    );
    assert!(probe.sum.localexec_num_splits > 0, "{probe:?}");
    assert!(probe.sum.splits.warmup_microsecs > 0, "{probe:?}");
    assert!(probe.sum.num_hits > 0, "{probe:?}");
    assert_eq!(b.searchers, 2);
    assert_eq!(
        b.passes.iter().map(|p| p.name.as_str()).collect::<Vec<_>>(),
        ["first", "warm", "c1", "c4"]
    );
    for p in &b.passes {
        assert_eq!(p.failed, 0, "{:?}", p.searches);
        assert_eq!(p.searches.len(), bench::SEARCHES.len());
        assert!(p.nodes.values().any(|n| n.leaf_splits > 0.0), "{p:?}");
    }
    // The fixtures (1895 to 1897) have "cross of gold" pages.
    let gold = &b.passes[0].searches[3];
    assert!(gold.pages.unwrap() > 0, "{gold:?}");
    assert!(gold.calls.iter().any(|c| c.kind == "summary"));

    // Node 1 restarts (a new generation, as at a new IP): it registers
    // again, joins through node 0, and serves leaf searches.
    let before = found[1].generation;
    n1.kill().await.unwrap();
    let _n1 = start("qw-1", "indexer,searcher", 7590);
    let mut rejoined = false;
    for _ in 0..120 {
        if let Ok(m) = members::members(&http, root).await {
            if m.iter()
                .any(|m| m.node_id == "qw-1" && m.ready && m.generation != before)
            {
                rejoined = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    assert!(rejoined, "{}", logs());
    let again = bench::run(
        "local-restart",
        &bench::Spec {
            levels: vec![2],
            levels_only: true,
            probe: false,
            ..bspec
        },
    )
    .await
    .unwrap();
    assert_eq!(again.passes[0].failed, 0);
}

/// A store that refuses `get` on sample parts and set documents, so tests
/// show their readers stream them (`get` has a size limit; parts don't).
#[derive(Debug)]
struct StreamOnly(LocalStore);

#[async_trait::async_trait]
impl ObjectStore for StreamOnly {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        if path.contains("/docs-") || path.ends_with("docs.ndjson.zst") {
            return Err(usnm_store::StoreError::TooLarge(path.into()));
        }
        self.0.get(path).await
    }
    async fn get_stream(
        &self,
        path: &str,
    ) -> Result<Option<usnm_store::ByteStream>, usnm_store::StoreError> {
        self.0.get_stream(path).await
    }
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, usnm_store::StoreError> {
        self.0.put_new(path, body, content_type).await
    }
    async fn put(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), usnm_store::StoreError> {
        self.0.put(path, body, content_type).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        self.0.list(prefix).await
    }
}

/// A packaged set (`usnm-qwcluster bundle`): the retained archives in one
/// tar, the sample's documents in one zstd stream that reads back whole and
/// checks out, and a manifest; a set is never replaced.
#[tokio::test]
async fn a_sample_set_packages_archives_and_documents() {
    use usnm_ingest::cluster::{sample, set};
    let e = env().await;
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let archive = e.root.join("batch_fx_set_ver01.tar.bz2");
    let other = e.root.join("batch_fx_two_ver01.tar.bz2");
    write_archive(&archive, &early, false, false);
    write_archive(&other, &late, false, false);
    let original = std::fs::read(&archive).unwrap();
    let raw: Arc<dyn ObjectStore> = Arc::new(LocalStore::new(e.root.join("raw")));
    let list = [
        listed("batch_fx_set_ver01", &archive, Some(sha256_file(&archive))),
        listed("batch_fx_two_ver01", &other, Some(sha256_file(&other))),
    ];
    source::enqueue(&e.state, &list).await.unwrap();
    let w = usnm_ingest::worker::Worker {
        raw: Some(raw.clone()),
        ..e.worker("w")
    };
    assert_eq!(w.run(None).await.unwrap(), 2);
    e.release(1, true).await.unwrap();
    let bench = StreamOnly(LocalStore::new(e.root.join("bench")));
    let spec = sample::Spec {
        cut: 10_000,
        american_stories: false,
        concurrency: 1,
        part_bytes: 8192,
        max_batches: None,
    };
    let published = sample::published(e.reference.as_ref()).await.unwrap();
    let sm = sample::build(e.curated.clone(), published, &bench, "sample/all", &spec)
        .await
        .unwrap();
    assert!(sm.parts.len() > 1, "several zstd frames, one stream");
    let sets: Arc<dyn ObjectStore> = Arc::new(StreamOnly(LocalStore::new(e.root.join("sets"))));
    let sources = || set::Sources {
        raw: raw.clone(),
        sample: &bench,
        sample_prefix: "sample/all",
        batches: vec!["batch_fx_set_ver01".into(), "batch_fx_two_ver01".into()],
    };
    let m = set::bundle(sets.clone(), "fx-all-v1", "every batch", sources())
        .await
        .unwrap();
    assert_eq!(
        (m.version, m.docs, m.pages),
        (Some(1), sm.docs, sm.pages_read)
    );
    assert_eq!(m.batches.len(), 2);
    assert_eq!(m.batches[0].bytes, original.len() as u64);
    assert!(!m.american_stories);
    assert_eq!(m.files.len(), 2);

    // raw.tar holds the archive byte for byte, and its manifest.
    let tar_bytes = std::fs::read(e.root.join("sets/fx-all-v1/raw.tar")).unwrap();
    assert_eq!(tar_bytes.len() as u64, m.files[0].bytes);
    let mut entries = BTreeMap::new();
    let mut t = tar::Archive::new(&tar_bytes[..]);
    for entry in t.entries().unwrap() {
        let mut entry = entry.unwrap();
        let path = entry.path().unwrap().display().to_string();
        let mut body = Vec::new();
        std::io::Read::read_to_end(&mut entry, &mut body).unwrap();
        entries.insert(path, body);
    }
    assert_eq!(
        entries["batch_fx_set_ver01/batch_fx_set_ver01.tar.bz2"],
        original
    );
    assert!(entries.contains_key("batch_fx_set_ver01/manifest.json"));

    // The documents read back as one stream, in order, checked.
    let (tx, mut rx) = tokio::sync::mpsc::channel(4);
    let feed = set::feed_docs(sets.as_ref(), &m, 4096, tx);
    let collect = async {
        let mut docs = 0u64;
        while let Some(c) = rx.recv().await {
            assert!(c.len() <= 4096);
            docs += c.iter().filter(|&&b| b == b'\n').count() as u64;
        }
        docs
    };
    let (fed, docs) = tokio::join!(feed, collect);
    fed.unwrap();
    assert_eq!(docs, m.docs);

    // Never replaced.
    let err = set::bundle(sets.clone(), "fx-all-v1", "again", sources())
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("never replaced"), "{err}");
}
