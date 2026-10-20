//! Sending a partitioned base's pages a decade at a time (05 §5.5.5, #123).
//!
//! A base built with `--partition-decade` keeps each decade in its own
//! splits. Quickwit 0.9.1 cuts one split per decade from each commit, and a
//! commit takes whatever pages came in since the last one. Curated parts are
//! in archive order (title, then date), so a commit's pages span several
//! decades, and each commit would cut several small splits that all need
//! merging. Measured locally (05 §5.5.5), that is 5× the splits cut, 1.8×
//! the pages merged and 1.8× the writer's memory of a base without decades.
//!
//! So the release sends the pages of one decade together: each page goes to
//! its decade's file on the writer's scratch disk (zstd JSON lines), and a
//! decade's file is sent as soon as it holds `chunk` pages, the split target.
//! Each commit then holds one or two decades: locally, one merge pass over
//! the pages, as without decades, and 1.4× the writer's memory. At the end,
//! the files left are sent, oldest decade first. The disk this needs is
//! bounded: under `chunk` pages per decade, at most 15 × `chunk` pages (a
//! few GB compressed at 30,000), never the whole corpus. Each page is read from the
//! curated store once, as without partitions.
//!
//! With a sink that hands out a [`LineSender`] (the Quickwit writer), a
//! decade's full file is sent by a task of its own while the release reads
//! on into a new file for that decade (#283: the writer takes v1 ingest at
//! most `shard_throughput_limit`, 20 MB/s, and reading stopped for every
//! decade sent). Files go out one at a time, in the order they filled, so
//! the pages reach the writer in the same order as when they were sent in
//! line. At most [`QUEUED_FILES`] full files wait; past that, the release
//! waits for the sender, which bounds the disk.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::{BufRead, BufReader, BufWriter, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use serde_json::Value;

use crate::sink::{IndexSink, LineSender};

/// The directory under the writer's work directory that holds the files.
pub const SPILL_DIR: &str = "decade-spill";

/// zstd's level for the files: fast, and about a quarter of the JSON.
const LEVEL: i32 = 1;

/// Full files waiting for the background sender before the release waits.
pub const QUEUED_FILES: usize = 4;

/// One decade's pages not sent yet.
struct Spill {
    path: PathBuf,
    file: zstd::stream::write::Encoder<'static, BufWriter<File>>,
    docs: u64,
}

/// A closed file to send.
struct Full {
    decade: u16,
    path: PathBuf,
    docs: u64,
    disk_bytes: u64,
}

/// What [`DecadeOrder`] has done, for the release log and the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OrderStats {
    /// Runs of one decade's pages sent together.
    pub runs: u64,
    pub docs: u64,
    /// The most pages held on disk at once.
    pub peak_docs: u64,
    /// The most compressed bytes on disk at once (as of the last flush of
    /// each file, so a little under).
    pub peak_bytes: u64,
}

/// What the files and the sender share.
#[derive(Default)]
struct Shared {
    stats: OrderStats,
    /// Pages written to files and not sent yet.
    held: u64,
}

/// The background sender: its queue and its task.
struct Background {
    queue: tokio::sync::mpsc::Sender<Full>,
    task: Task,
}

/// The sender's task, aborted when dropped: a release that gives up (its
/// writer lock lost, `release::drain`) mid-wait leaves no sender posting.
/// Ends with the lines it hadn't sent yet.
struct Task(tokio::task::JoinHandle<anyhow::Result<Vec<u8>>>);

impl Task {
    async fn join(&mut self) -> anyhow::Result<Vec<u8>> {
        (&mut self.0).await.context("the decade sender's task")?
    }
}

impl Drop for Task {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Holds pages back by decade and sends them a decade at a time.
pub struct DecadeOrder {
    dir: PathBuf,
    chunk: u64,
    open: BTreeMap<u16, Spill>,
    /// Files made so far, for unique names.
    made: u64,
    shared: Arc<Mutex<Shared>>,
    background: Option<Background>,
}

impl DecadeOrder {
    /// Files in `work_dir/decade-spill`, emptied first: a previous release
    /// that stopped may have left some. Only the release holding the writer
    /// lock writes there. With `sender`, full files are sent in the
    /// background (module docs); without, in line.
    /// The background sender stops at its next line once `lease_lost` is
    /// set (the writer lock couldn't be renewed), as the release does at its
    /// next `WriterLease::check`.
    pub fn new(
        work_dir: &Path,
        chunk: u64,
        sender: Option<LineSender>,
        lease_lost: Option<Arc<std::sync::atomic::AtomicBool>>,
    ) -> anyhow::Result<Self> {
        let dir = work_dir.join(SPILL_DIR);
        if dir.exists() {
            tracing::info!(dir = %dir.display(), "removing a previous release's decade files");
            std::fs::remove_dir_all(&dir).with_context(|| format!("removing {}", dir.display()))?;
        }
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        let shared = Arc::new(Mutex::new(Shared::default()));
        let background = sender.map(|sender| {
            let (queue, rx) = tokio::sync::mpsc::channel(QUEUED_FILES);
            let task = Task(tokio::spawn(send_files(
                rx,
                sender,
                Arc::clone(&shared),
                lease_lost,
            )));
            Background { queue, task }
        });
        Ok(Self {
            dir,
            chunk: chunk.max(1),
            open: BTreeMap::new(),
            made: 0,
            shared,
            background,
        })
    }

    pub fn stats(&self) -> OrderStats {
        self.shared.lock().expect("stats").stats
    }

    /// Hold `doc` (with its `decade`) back, and send its decade's pages
    /// once there are `chunk` of them.
    pub async fn add(&mut self, sink: &mut dyn IndexSink, doc: &Value) -> anyhow::Result<()> {
        let decade = doc["decade"]
            .as_u64()
            .and_then(|d| u16::try_from(d).ok())
            .with_context(|| {
                format!(
                    "document `{}` has no decade",
                    doc["doc_id"].as_str().unwrap_or("?")
                )
            })?;
        let line = serde_json::to_vec(doc)?;
        let spill = match self.open.entry(decade) {
            std::collections::btree_map::Entry::Occupied(e) => e.into_mut(),
            std::collections::btree_map::Entry::Vacant(e) => {
                self.made += 1;
                let path = self.dir.join(format!("{decade}-{}.jsonl.zst", self.made));
                let file =
                    File::create(&path).with_context(|| format!("creating {}", path.display()))?;
                let file = zstd::stream::write::Encoder::new(BufWriter::new(file), LEVEL)?;
                e.insert(Spill {
                    path,
                    file,
                    docs: 0,
                })
            }
        };
        spill.file.write_all(&line)?;
        spill.file.write_all(b"\n")?;
        spill.docs += 1;
        let full = spill.docs >= self.chunk;
        {
            let mut sh = self.shared.lock().expect("held");
            sh.held += 1;
            sh.stats.peak_docs = sh.stats.peak_docs.max(sh.held);
        }
        if full {
            self.send(sink, decade).await?;
        }
        Ok(())
    }

    /// Send every decade's pages left, oldest first, and wait for the
    /// background sender to finish.
    pub async fn finish(&mut self, sink: &mut dyn IndexSink) -> anyhow::Result<OrderStats> {
        let decades: Vec<u16> = self.open.keys().copied().collect();
        for d in decades {
            self.send(sink, d).await?;
        }
        if let Some(Background { queue, mut task }) = self.background.take() {
            drop(queue);
            // `task` aborts the sender if this future is dropped mid-wait.
            let unsent = task.join().await?;
            // Back to the sink, so its `finish` sends them with the forced
            // commit, as in line.
            for line in unsent.split(|&b| b == b'\n').filter(|l| !l.is_empty()) {
                sink.add_line(line).await?;
            }
        }
        Ok(self.stats())
    }

    /// Close `decade`'s file and send it: queued for the background sender,
    /// or in line through `sink`.
    async fn send(&mut self, sink: &mut dyn IndexSink, decade: u16) -> anyhow::Result<()> {
        let Some(spill) = self.open.remove(&decade) else {
            return Ok(());
        };
        let mut file = spill.file.finish()?;
        file.flush()?;
        // Every file in the directory: open ones, queued ones and the one
        // being sent.
        let disk_bytes = spill_bytes(&self.dir);
        drop(file);
        let full = Full {
            decade,
            path: spill.path,
            docs: spill.docs,
            disk_bytes,
        };
        match &self.background {
            Some(bg) => {
                if bg.queue.send(full).await.is_err() {
                    // The sender stopped: its error says why.
                    let Background { mut task, .. } = self.background.take().expect("background");
                    task.join().await?;
                    anyhow::bail!("the decade sender stopped");
                }
                Ok(())
            }
            None => send_file(&full, &self.shared, &mut InLine(sink)).await,
        }
    }
}

/// The background sender: each queued file in turn. Ends with the lines it
/// hadn't sent yet.
async fn send_files(
    mut rx: tokio::sync::mpsc::Receiver<Full>,
    sender: LineSender,
    shared: Arc<Mutex<Shared>>,
    lease_lost: Option<Arc<std::sync::atomic::AtomicBool>>,
) -> anyhow::Result<Vec<u8>> {
    let mut sender = Leased {
        sender,
        lost: lease_lost,
    };
    while let Some(full) = rx.recv().await {
        send_file(&full, &shared, &mut sender).await?;
    }
    Ok(sender.sender.into_unsent())
}

/// The background sender, stopping once the writer lock is lost.
struct Leased {
    sender: LineSender,
    lost: Option<Arc<std::sync::atomic::AtomicBool>>,
}

#[async_trait::async_trait]
impl Lines for Leased {
    async fn line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        if self
            .lost
            .as_ref()
            .is_some_and(|l| l.load(std::sync::atomic::Ordering::SeqCst))
        {
            anyhow::bail!("the writer lock could not be renewed; the decade sender stopped");
        }
        self.sender.add_line(line).await
    }
}

/// Where a file's lines go: the background sender, or the sink in line.
#[async_trait::async_trait]
trait Lines: Send {
    async fn line(&mut self, line: &[u8]) -> anyhow::Result<()>;
}

#[async_trait::async_trait]
impl Lines for LineSender {
    async fn line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        self.add_line(line).await
    }
}

struct InLine<'a>(&'a mut dyn IndexSink);

#[async_trait::async_trait]
impl Lines for InLine<'_> {
    async fn line(&mut self, line: &[u8]) -> anyhow::Result<()> {
        self.0.add_line(line).await
    }
}

/// Send one file's lines to `to`, check the count, and delete it.
async fn send_file(full: &Full, shared: &Mutex<Shared>, to: &mut dyn Lines) -> anyhow::Result<()> {
    let mut lines = BufReader::new(zstd::stream::read::Decoder::new(
        File::open(&full.path).with_context(|| format!("opening {}", full.path.display()))?,
    )?);
    let mut line = Vec::new();
    let mut sent = 0u64;
    loop {
        line.clear();
        if lines.read_until(b'\n', &mut line)? == 0 {
            break;
        }
        if line.last() == Some(&b'\n') {
            line.pop();
        }
        to.line(&line).await?;
        sent += 1;
    }
    anyhow::ensure!(
        sent == full.docs,
        "{} held {} pages, but {sent} came back",
        full.path.display(),
        full.docs
    );
    std::fs::remove_file(&full.path)?;
    let held = {
        let mut sh = shared.lock().expect("held");
        sh.held -= sent;
        sh.stats.runs += 1;
        sh.stats.docs += sent;
        sh.stats.peak_bytes = sh.stats.peak_bytes.max(full.disk_bytes);
        sh.held
    };
    tracing::info!(
        decade = full.decade,
        pages = sent,
        held_pages = held,
        disk_mb = full.disk_bytes / (1024 * 1024),
        "sent a decade's pages"
    );
    Ok(())
}

/// The bytes of the files in the spill directory now.
fn spill_bytes(dir: &Path) -> u64 {
    std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .filter_map(|e| e.ok()?.metadata().ok())
                .filter(|m| m.is_file())
                .map(|m| m.len())
                .sum()
        })
        .unwrap_or(0)
}

impl Drop for DecadeOrder {
    fn drop(&mut self) {
        // Drops the task, which aborts the sender.
        self.background = None;
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use serde_json::json;

    use super::*;
    use crate::sink::MainLayout;

    #[derive(Default, Clone)]
    struct Recorder(Arc<Mutex<Vec<Value>>>);

    #[async_trait]
    impl IndexSink for Recorder {
        async fn create(&mut self, _: &str, _: MainLayout) -> anyhow::Result<()> {
            Ok(())
        }
        async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
            self.0.lock().unwrap().push(doc.clone());
            Ok(())
        }
        async fn finish(&mut self, _: u64) -> anyhow::Result<()> {
            Ok(())
        }
        fn backend(&self) -> &'static str {
            "test"
        }
    }

    fn doc(i: u32, decade: u16) -> Value {
        json!({"doc_id": format!("p{i}"), "decade": decade, "text": "gold ".repeat(50)})
    }

    #[tokio::test]
    async fn pages_go_out_a_decade_at_a_time() {
        let dir = tempfile::tempdir().unwrap();
        // A previous release's files are cleared.
        std::fs::create_dir_all(dir.path().join(SPILL_DIR)).unwrap();
        std::fs::write(dir.path().join(SPILL_DIR).join("1890.jsonl.zst"), b"x").unwrap();
        let mut sink = Recorder::default();
        let mut order = DecadeOrder::new(dir.path(), 3, None, None).unwrap();
        // Archive order: decades interleaved.
        let input = [1890, 1900, 1890, 1880, 1900, 1890, 1900, 1880, 1890];
        for (i, d) in input.iter().enumerate() {
            order.add(&mut sink, &doc(i as u32, *d)).await.unwrap();
        }
        // Two decades reached 3 pages and went out whole, in the order they did.
        let sent: Vec<String> = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|d| d["doc_id"].as_str().unwrap().to_owned())
            .collect();
        assert_eq!(sent, ["p0", "p2", "p5", "p1", "p4", "p6"]);
        let stats = order.finish(&mut sink).await.unwrap();
        let sent: Vec<(String, u64)> = sink
            .0
            .lock()
            .unwrap()
            .iter()
            .map(|d| {
                (
                    d["doc_id"].as_str().unwrap().to_owned(),
                    d["decade"].as_u64().unwrap(),
                )
            })
            .collect();
        // Then what is left, oldest decade first, and every page once.
        assert_eq!(
            sent[6..],
            [
                ("p3".to_owned(), 1880),
                ("p7".to_owned(), 1880),
                ("p8".to_owned(), 1890)
            ]
        );
        assert_eq!(sent.len(), input.len());
        assert_eq!((stats.runs, stats.docs), (4, 9));
        assert_eq!(stats.peak_docs, 6);
        assert!(stats.peak_bytes > 0);
        // The files are gone once sent, and the directory with the order.
        assert_eq!(
            std::fs::read_dir(dir.path().join(SPILL_DIR))
                .unwrap()
                .count(),
            0
        );
        drop(order);
        assert!(!dir.path().join(SPILL_DIR).exists());
        // Documents come back byte for byte.
        assert_eq!(sink.0.lock().unwrap()[0], doc(0, 1890));
    }

    #[tokio::test]
    async fn a_page_without_a_decade_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let mut order = DecadeOrder::new(dir.path(), 3, None, None).unwrap();
        let err = order
            .add(&mut Recorder::default(), &json!({"doc_id": "x"}))
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("`x` has no decade"), "{err}");
    }
}
