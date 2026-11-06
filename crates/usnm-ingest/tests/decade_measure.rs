//! A measurement, not a check (#123, 05 §5.5.5): what laying a base out by
//! decade costs the Quickwit writer. It builds a synthetic corpus in archive
//! order (batches of titles, each title's pages by date, decades weighted
//! like production's) and indexes it three times into a fresh writer node:
//! without decades, partitioned by decade in archive order, and partitioned
//! with the pages sent a decade at a time (`decade_order`, as a release
//! does). For each it reports the writer's peak memory, the splits the
//! indexer cut (before merging), the merges, the splits left, and the time.
//!
//! ```sh
//! QUICKWIT_BIN=/path/to/quickwit cargo test --release -p usnm-ingest \
//!   --test decade_measure -- --ignored --nocapture
//! ```
//!
//! `USNM_MEASURE_DOCS` sets the corpus size (default 150,000 pages),
//! `USNM_MEASURE_WORDS` the mean words a page (default 1,000),
//! `USNM_MEASURE_RUNS` the runs (default `off,archive,decade`);
//! `USNM_WRITER_HEAP` and `USNM_WRITER_COMMIT_SECS` tune the writer as in a
//! release.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;
use usnm_ingest::decade_order::DecadeOrder;
use usnm_ingest::merges::MergeWait;
use usnm_ingest::sink::{Decades, IndexSink, QuickwitNode, QuickwitSink};

mod synthetic;

#[derive(Debug, Default)]
struct Seen {
    peak_rss: u64,
    /// Every split seen, by id: (docs, merge ops, decades).
    splits: BTreeMap<String, (u64, u64, Vec<String>)>,
}

/// Every second, the splits the metastore lists (merged-away ones too);
/// every quarter second, the writer's memory.
fn watch(
    url: String,
    index: String,
    pid: u32,
    seen: Arc<Mutex<Seen>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let http = reqwest::Client::new();
        let mut tick = 0u64;
        loop {
            if let Ok(out) = tokio::process::Command::new("ps")
                .args(["-o", "rss=", "-p", &pid.to_string()])
                .output()
                .await
            {
                if let Ok(kb) = String::from_utf8_lossy(&out.stdout).trim().parse::<u64>() {
                    let mut s = seen.lock().unwrap();
                    s.peak_rss = s.peak_rss.max(kb * 1024);
                }
            }
            if tick.is_multiple_of(4) {
                for state in ["Published", "MarkedForDeletion"] {
                    let Ok(resp) = http
                        .get(format!("{url}/api/v1/indexes/{index}/splits"))
                        .query(&[("split_states", state), ("limit", "10000")])
                        .send()
                        .await
                    else {
                        continue;
                    };
                    let Ok(v) = resp.json::<Value>().await else {
                        continue;
                    };
                    let mut s = seen.lock().unwrap();
                    for split in v["splits"].as_array().into_iter().flatten() {
                        let tags: Vec<String> = split["tags"]
                            .as_array()
                            .into_iter()
                            .flatten()
                            .filter_map(|t| t.as_str()?.strip_prefix("decade:").map(str::to_owned))
                            .collect();
                        s.splits.insert(
                            split["split_id"].as_str().unwrap_or_default().to_owned(),
                            (
                                split["num_docs"].as_u64().unwrap_or(0),
                                split["num_merge_ops"].as_u64().unwrap_or(0),
                                tags,
                            ),
                        );
                    }
                }
            }
            tick += 1;
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    })
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Run {
    Off,
    PartitionedArchiveOrder,
    PartitionedByDecade,
}

async fn measure(bin: &Path, run: Run, docs: u64, port: u16) {
    let dir = tempfile::tempdir().unwrap();
    let qw = dir.path().join("qw");
    std::fs::create_dir_all(&qw).unwrap();
    let meta = format!("file://{}/meta", qw.display());
    let root = format!("file://{}/indexes", qw.display());
    let node = QuickwitNode::start(bin, &qw, port, &meta, &root)
        .await
        .unwrap();
    let id = "pages-base-measure";
    let decades = match run {
        Run::Off => Decades::Off,
        _ => Decades::Partitioned,
    };
    let mut sink = QuickwitSink::new(&node.url, &root)
        .unwrap()
        .watching(&node)
        .merges(MergeWait {
            poll: Duration::from_secs(1),
            stable_polls: 5,
            timeout: Duration::from_secs(3600),
            finalize_grace: Duration::from_secs(10),
            finalize_stall: Duration::from_secs(120),
            report: Default::default(),
        });
    sink.create(id, decades.into()).await.unwrap();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let watcher = watch(
        node.url.clone(),
        id.to_owned(),
        node.pid().unwrap(),
        seen.clone(),
    );
    let started = Instant::now();
    let mut order = (run == Run::PartitionedByDecade).then(|| {
        let chunk = usnm_ingest::merges::template_setting("split_num_docs_target").unwrap();
        DecadeOrder::new(dir.path(), chunk, None, None).unwrap()
    });
    // Generated on a thread of its own, a few thousand pages ahead.
    let (tx, mut rx) = tokio::sync::mpsc::channel::<Value>(2_000);
    let corpus = synthetic::Corpus {
        docs,
        // Words a page, about 6 bytes each: 700 to 1,300 by default.
        words_per_page: std::env::var("USNM_MEASURE_WORDS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(1_000),
        with_decade: decades.on(),
        // About 2,000 to 8,000 pages a title in the batch.
        title_pages: (2_000, 8_000),
        planted: Vec::new(),
    };
    let generator = std::thread::spawn(move || {
        synthetic::corpus(&corpus, |d| {
            let _ = tx.blocking_send(d);
        })
    });
    let mut sent = 0u64;
    while let Some(d) = rx.recv().await {
        match order.as_mut() {
            Some(o) => o.add(&mut sink, &d).await.unwrap(),
            None => sink.add(&d).await.unwrap(),
        }
        sent += 1;
    }
    generator.join().unwrap();
    let order_stats = match order.as_mut() {
        Some(o) => Some(o.finish(&mut sink).await.unwrap()),
        None => None,
    };
    let send_secs = started.elapsed().as_secs_f64();
    let sealing = Instant::now();
    sink.finish(sent).await.unwrap();
    let seal_secs = sealing.elapsed().as_secs_f64();
    // One more look at the splits.
    tokio::time::sleep(Duration::from_secs(2)).await;
    watcher.abort();
    let layout = sink.layout(&[id.to_owned()]).await.unwrap().remove(0);
    let s = std::mem::take(&mut *seen.lock().unwrap());
    let first: Vec<u64> = s
        .splits
        .values()
        .filter(|(_, ops, _)| *ops == 0)
        .map(|(d, _, _)| *d)
        .collect();
    let merged: Vec<u64> = s
        .splits
        .values()
        .filter(|(_, ops, _)| *ops > 0)
        .map(|(d, _, _)| *d)
        .collect();
    let mut sorted = first.clone();
    sorted.sort_unstable();
    let median = sorted.get(sorted.len() / 2).copied().unwrap_or(0);
    let mixed = s
        .splits
        .values()
        .filter(|(_, ops, t)| *ops == 0 && t.len() > 1)
        .count();
    eprintln!(
        "{run:?}: {sent} pages; writer peak RSS {:.0} MiB; first-round splits {} (median {median} pages, smallest {}, largest {}, {mixed} with several decades); \
         merges {} ({} pages merged); final splits {} (largest {} pages, by decade {:?}); send {send_secs:.0} s, merge wait {seal_secs:.0} s; order {order_stats:?}",
        s.peak_rss as f64 / 1048576.0,
        first.len(),
        sorted.first().unwrap_or(&0),
        sorted.last().unwrap_or(&0),
        merged.len(),
        merged.iter().sum::<u64>(),
        layout.splits,
        layout.largest_split_docs,
        layout.splits_by_decade,
    );
    node.stop().await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "a measurement: run with --ignored and QUICKWIT_BIN"]
async fn measure_decade_layouts() {
    let Some(bin) = std::env::var_os("QUICKWIT_BIN").filter(|b| !b.is_empty()) else {
        eprintln!("QUICKWIT_BIN not set; skipping");
        return;
    };
    let docs = std::env::var("USNM_MEASURE_DOCS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(150_000);
    let bin = Path::new(&bin);
    let runs = std::env::var("USNM_MEASURE_RUNS").unwrap_or_else(|_| "off,archive,decade".into());
    for (i, run) in runs.split(',').enumerate() {
        let run = match run {
            "off" => Run::Off,
            "archive" => Run::PartitionedArchiveOrder,
            "decade" => Run::PartitionedByDecade,
            other => panic!("unknown run {other}"),
        };
        measure(bin, run, docs, 7420 + 2 * i as u16).await;
    }
}
