//! Releases into a real Quickwit writer node: a base and a delta, small splits
//! merged, and an index sealed. These run when `QUICKWIT_BIN` is set.

use std::path::Path;

use chrono::{NaiveDate, TimeZone, Utc};
use serde_json::Value;
use usnm_ingest::release::Release;
use usnm_ingest::source;

use crate::common::{
    curate, env, fixture_pages, fixtures, listed, loc_fixture, over_decades, quick_merges,
    read_jsonl, write_archive, Page,
};

fn writer_port() -> anyhow::Result<u16> {
    use std::io::ErrorKind;
    use std::net::{Ipv4Addr, TcpListener, UdpSocket};

    // Quickwit needs REST and adjacent gRPC ports, plus UDP gossip on REST's
    // port. The external process cannot inherit these sockets.
    for _ in 0..64 {
        let rest = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = rest.local_addr()?.port();
        let Some(grpc_port) = port.checked_add(1) else {
            continue;
        };
        let _grpc = match TcpListener::bind((Ipv4Addr::LOCALHOST, grpc_port)) {
            Ok(listener) => listener,
            Err(e) if e.kind() == ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e.into()),
        };
        let _gossip = match UdpSocket::bind((Ipv4Addr::LOCALHOST, port)) {
            Ok(socket) => socket,
            Err(e) if e.kind() == ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e.into()),
        };
        return Ok(port);
    }
    anyhow::bail!("no available ephemeral REST/gRPC/gossip port pair after 64 probes")
}

#[test]
fn independent_writer_test_processes_can_share_a_host() {
    if std::env::var_os("QUICKWIT_BIN").is_none_or(|b| b.is_empty()) {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    }
    let children: Vec<_> = (0..2)
        .map(|_| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "writer::a_writer_seals_an_index_that_received_no_documents",
                    "--nocapture",
                ])
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap()
        })
        .collect();
    for child in children {
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success()
                && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
            "concurrent writer failed:\n{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
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
    let node = QuickwitNode::start(Path::new(&bin), &qw, writer_port().unwrap(), &meta, &root)
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
            stage_index_storage: false,
            state: e.state.clone(),
            curated: e.curated.clone(),
            reference: e.reference.clone(),
            owner: "releaser".into(),
            full: false,
            synthetic: true,
            now: Utc.with_ymd_and_hms(2026, 10, day, 3, 0, 0).unwrap(),
            titles_left: None,
            american_stories: false,
            ja_latin: false,
            ocr_local: false,
            congress: false,
            congress_manual: false,
            partition_decade: false,
            single_text_field: false,
            page_language: false,
            split_docs: None,
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
        // Every split's footer, from the metastore (#125): part of the file.
        let footers = l["footer_bytes"].as_u64().unwrap();
        let largest = l["largest_footer_bytes"].as_u64().unwrap();
        assert!(largest > 0 && largest <= footers, "{l}");
        assert!(footers < l["bytes"].as_u64().unwrap(), "{l}");
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

/// A real Quickwit writer merges an index cut into many small splits down
/// to one before the release may publish it: the planner leaves splits
/// below its merge factor alone, and closing the index merges them.
/// Runs when `QUICKWIT_BIN` is set.
#[tokio::test]
async fn a_writer_merges_small_splits_before_the_index_is_sealed() {
    writer_merges_small_splits(None).await;
}

#[cfg(unix)]
#[test]
fn a_writer_uses_a_fresh_local_wal_and_seals_an_index() {
    if std::env::var_os("QUICKWIT_BIN").is_none_or(|b| b.is_empty()) {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    }
    if let Some(root) = std::env::var_os("USNM_TEST_LOCAL_WAL_ROOT") {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(writer_merges_small_splits(Some(Path::new(&root))));
        return;
    }
    // Isolate the environment override from all other concurrent writer tests.
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("local");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "writer::a_writer_uses_a_fresh_local_wal_and_seals_an_index",
            "--nocapture",
        ])
        .env("USNM_TEST_LOCAL_WAL_ROOT", &local)
        .env("USNM_WRITER_WAL_DIR", &local)
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "local-WAL writer failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn writer_merges_small_splits(local_wal_root: Option<&Path>) {
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
    let node = QuickwitNode::start(Path::new(&bin), &qw, writer_port().unwrap(), &meta, &root)
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
    if let Some(local) = local_wal_root {
        let wal = std::fs::read_link(qw.join("qwdata/wal")).unwrap();
        assert!(wal.starts_with(local.canonicalize().unwrap()));
        assert!(
            std::fs::read_dir(&wal).unwrap().next().is_some(),
            "Quickwit wrote no WAL files"
        );
        assert!(!wal.join("stale").exists());
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
        merges::template_split_target(),
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
    if let Some(local) = local_wal_root {
        assert_eq!(std::fs::read_dir(local).unwrap().count(), 0);
    }
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
    let node = QuickwitNode::start(Path::new(&bin), &qw, writer_port().unwrap(), &meta, &root)
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
    sink.create(id, usnm_ingest::sink::MainLayout::default())
        .await
        .unwrap();
    sink.finish(0).await.unwrap();
    let layout = sink.layout(&[id.to_owned()]).await.unwrap();
    assert_eq!((layout[0].splits, layout[0].docs), (0, 0));
    node.stop().await.unwrap();
}

/// Ingest v1 on a real Quickwit writer (#283): six forced commits are six
/// splits, below the merge factor; the release's seal restarts the writer
/// with a merge factor of 2, which merges them into one, and puts the
/// index's config back. Every page is in the index once. Then a full base
/// laid out by decade and a delta on it, released through v1 end to end:
/// one split a decade, each page once. Runs when `QUICKWIT_BIN` is set.
#[cfg(unix)]
#[test]
fn a_v1_writer_merges_by_restarting_and_holds_each_page_once() {
    if std::env::var_os("QUICKWIT_BIN").is_none_or(|b| b.is_empty()) {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    }
    if let Some(root) = std::env::var_os("USNM_TEST_LOCAL_WAL_ROOT") {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(v1_writer_merges_by_restarting(Path::new(&root)));
        return;
    }
    // The writer's queues on local disk, as in the job; the environment
    // override in a process of its own.
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("local");
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "writer::a_v1_writer_merges_by_restarting_and_holds_each_page_once",
            "--nocapture",
        ])
        .env("USNM_TEST_LOCAL_WAL_ROOT", &local)
        .env("USNM_WRITER_WAL_DIR", &local)
        .output()
        .unwrap();
    assert!(
        output.status.success() && String::from_utf8_lossy(&output.stdout).contains("1 passed;"),
        "v1 writer failed:\n{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

async fn v1_writer_merges_by_restarting(local: &Path) {
    let bin = std::env::var_os("QUICKWIT_BIN").unwrap();
    use usnm_ingest::merges::{self, Node};
    use usnm_ingest::sink::{IngestApi, QuickwitNode, QuickwitSink, INDEX_TEMPLATE};
    let e = env().await;
    let qw = e.root.join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(Path::new(&bin), &qw, writer_port().unwrap(), &meta, &root)
        .await
        .unwrap();
    let http = reqwest::Client::new();
    let n = Node {
        http: &http,
        base: &node.url,
    };
    // The index's pages, and how many of each `doc_id` it holds.
    let census = |id: String| {
        let (http, url) = (http.clone(), node.url.clone());
        async move {
            let v: Value = http
                .post(format!("{url}/api/v1/{id}/search"))
                .json(&serde_json::json!({
                    "query": "*", "max_hits": 0,
                    "aggs": {"twice": {"terms": {"field": "doc_id", "size": 100, "min_doc_count": 2}}},
                }))
                .send()
                .await
                .unwrap()
                .error_for_status()
                .unwrap()
                .json()
                .await
                .unwrap();
            let twice = v["aggregations"]["twice"]["buckets"]
                .as_array()
                .unwrap()
                .len();
            (v["num_hits"].as_u64().unwrap(), twice)
        }
    };

    let id = "pages-delta-20261020-1";
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
    let docs: Vec<Value> = ["pages-base-fixture", "pages-delta-fixture-1"]
        .iter()
        .flat_map(|f| read_jsonl(&fixtures().join(format!("indexes/{f}.jsonl"))))
        .collect();
    for chunk in docs.chunks(docs.len().div_ceil(6)) {
        let body: String = chunk.iter().map(|d| format!("{d}\n")).collect();
        // The v1 queue is created with the index's pipeline, just after it.
        let until = deadline(60);
        let v: Value = loop {
            let resp = http
                .post(format!(
                    "{}/api/v1/{id}/ingest?commit=force&use_legacy_ingest=true",
                    node.url
                ))
                .body(body.clone())
                .send()
                .await
                .unwrap();
            if resp.status() != reqwest::StatusCode::NOT_FOUND {
                break resp.error_for_status().unwrap().json().await.unwrap();
            }
            if tokio::time::Instant::now() >= until {
                let status = resp.status();
                let text = resp.text().await.unwrap_or_default();
                panic!(
                    "the v1 ingest endpoint of {id} did not appear within 60 s: {status} {text}"
                );
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        };
        assert_eq!(v["num_docs_for_processing"], chunk.len() as u64, "{v}");
    }
    let queues = std::fs::read_link(qw.join("qwdata/queues")).unwrap();
    assert!(queues.starts_with(local.canonicalize().unwrap()));
    assert!(
        std::fs::read_dir(&queues).unwrap().next().is_some(),
        "Quickwit wrote no queue files"
    );
    assert_eq!(n.splits(id).await.unwrap().len(), 6);
    let before = n.index_config(id).await.unwrap();
    let pid = node.pid();
    let layout = merges::seal_restarting(
        &n,
        &node,
        id,
        docs.len() as u64,
        merges::template_split_target(),
        &node.events(),
        &quick_merges(),
        None,
    )
    .await
    .unwrap();
    assert_ne!(node.pid(), pid, "the writer restarted");
    assert_eq!((layout.splits, layout.docs), (1, docs.len() as u64));
    assert_eq!(census(id.into()).await, (docs.len() as u64, 0));
    // The config is the index's own again.
    assert_eq!(n.index_config(id).await.unwrap(), before);
    assert_eq!(
        before["indexing_settings"]["merge_policy"]["merge_factor"],
        10
    );

    // A full base by decade and a delta on it, through the release.
    let pages = fixture_pages();
    let split = NaiveDate::from_ymd_opt(1897, 7, 1).unwrap();
    let (early, late): (Vec<&Page>, Vec<&Page>) = pages.iter().partition(|p| p.date < split);
    let (early, late) = (over_decades(&early), over_decades(&late));
    let archive = |name: &str, pages: &[Page]| {
        let path = e.root.join(format!("{name}.tar.gz"));
        write_archive(&path, &pages.iter().collect::<Vec<_>>(), false, true);
        listed(name, &path, None)
    };
    let mut published = Vec::new();
    for (day, full, batch) in [
        (21, true, archive("batch_fx_early_ver01", &early)),
        (28, false, archive("batch_fx_late_ver01", &late)),
    ] {
        curate(&e, vec![batch]).await;
        let r = Release {
            partition_decade: true,
            ..e.releaser(day, full)
        };
        let mut sink = QuickwitSink::new(&node.url, &root)
            .unwrap()
            .watching(&node)
            .merges(quick_merges())
            .ingest(IngestApi::V1);
        published.push(r.run(&mut sink).await.unwrap().unwrap());
    }
    let p = published.last().unwrap();
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
    assert_eq!(layout[1]["splits"], 1, "{}", layout[1]);
    for (l, id) in layout.iter().zip(&p.indexes) {
        let docs = l["docs"].as_u64().unwrap();
        assert!(docs > 0, "{l}");
        assert_eq!(census(id.clone()).await, (docs, 0), "{id}");
        let config = n.index_config(id).await.unwrap();
        assert_eq!(
            config["indexing_settings"]["merge_policy"]["merge_factor"], 10,
            "{id}"
        );
    }
    node.stop().await.unwrap();
    assert_eq!(std::fs::read_dir(local).unwrap().count(), 0);
}
