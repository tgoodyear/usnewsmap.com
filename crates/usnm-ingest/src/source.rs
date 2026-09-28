//! Where batches come from (04 §4.1, §4.4 `discover`).
//!
//! `enqueue` takes a batch list: JSON `[{name, url, sha256?, ocr_source?}]`,
//! where `name` is the LoC batch name with its version suffix
//! (`az_acacia_ver02`). It also reads LoC's own listing directly: the
//! Chronicling America collection JSON ([`LOC_DATASETS`]) carries a
//! `datasets` array with each batch's bulk OCR archive, sha256, size and
//! page count (spike S-1, 04 §4.1.1).

use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::Utc;
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};

use crate::state::{Batch, BatchStatus, State};

/// Sent with every request to LoC (04 §4.1 good-citizen policy).
pub const USER_AGENT: &str = concat!(
    "usnewsmap-ingest/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/tgoodyear/usnewsmap.com)"
);

/// The Chronicling America collection JSON, whose `datasets` array lists
/// every batch's bulk OCR archive (about 3,000 batches, 2.5 TB of `tar.bz2`).
pub const LOC_DATASETS: &str = "https://www.loc.gov/collections/chronicling-america/?fo=json&c=1";

#[derive(Debug, Clone, Deserialize)]
pub struct ListedBatch {
    /// LoC's listing calls this `batch`.
    #[serde(alias = "batch")]
    pub name: String,
    pub url: String,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub ocr_source: Option<String>,
    /// Titles with pages in the batch (LoC's listing has them).
    #[serde(default)]
    pub lccns: Vec<String>,
}

/// Parse a batch list: a JSON array of batches, or LoC's collection JSON
/// (an object with a `datasets` array). Other fields are ignored.
pub fn parse_list(bytes: &[u8]) -> anyhow::Result<Vec<ListedBatch>> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum List {
        Plain(Vec<ListedBatch>),
        Collection { datasets: Vec<ListedBatch> },
    }
    Ok(match serde_json::from_slice(bytes).context("batch list")? {
        List::Plain(v) | List::Collection { datasets: v } => v,
    })
}

/// Split `batch_az_acacia_ver02` into (`batch_az_acacia`, 2).
pub fn split_version(name: &str) -> anyhow::Result<(String, u16)> {
    let (base, ver) = name
        .rsplit_once("_ver")
        .with_context(|| format!("batch name `{name}` has no `_verNN` suffix"))?;
    let version: u16 = ver
        .parse()
        .ok()
        .filter(|v| *v > 0)
        .with_context(|| format!("batch name `{name}` has an invalid version"))?;
    if !usnm_store::is_safe_segment(base) {
        bail!("batch name `{name}` is not a safe identifier");
    }
    Ok((base.to_owned(), version))
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct EnqueueReport {
    pub new: usize,
    pub new_version: usize,
    pub requeued: usize,
    pub unchanged: usize,
}

/// Record every listed batch; queue new batches, new versions, and batches
/// that failed or were never curated.
pub async fn enqueue(state: &State, list: &[ListedBatch]) -> anyhow::Result<EnqueueReport> {
    let mut report = EnqueueReport::default();
    for l in list {
        // Several workers may enqueue the same list at once (the backfill
        // job's replicas do): a lost race just means re-reading the item.
        let mut outcome = Outcome::Conflict;
        for _ in 0..5 {
            outcome = enqueue_one(state, l).await?;
            if outcome != Outcome::Conflict {
                break;
            }
        }
        match outcome {
            Outcome::New => report.new += 1,
            Outcome::NewVersion => report.new_version += 1,
            Outcome::Requeued => report.requeued += 1,
            Outcome::Unchanged => report.unchanged += 1,
            Outcome::Conflict => bail!("{}: kept changing concurrently; run enqueue again", l.name),
        }
    }
    Ok(report)
}

#[derive(Debug, PartialEq, Eq)]
enum Outcome {
    New,
    NewVersion,
    Requeued,
    Unchanged,
    Conflict,
}

async fn enqueue_one(state: &State, l: &ListedBatch) -> anyhow::Result<Outcome> {
    let (batch, version) = split_version(&l.name)?;
    if let Some(sha) = &l.sha256 {
        anyhow::ensure!(
            sha.len() == 64 && sha.bytes().all(|b| b.is_ascii_hexdigit()),
            "{}: sha256 must be 64 hex digits",
            l.name
        );
    }
    let ocr_source = l
        .ocr_source
        .clone()
        .unwrap_or_else(|| "ndnp-original".into());
    let Some((mut b, etag)) = state.batch(&batch).await? else {
        let fresh = Batch {
            id: batch.clone(),
            batch: batch.clone(),
            version,
            source_url: l.url.clone(),
            source_sha256: l.sha256.clone(),
            ocr_source,
            status: BatchStatus::Queued,
            attempts: 0,
            lease: None,
            curated: None,
            last_error: None,
            updated_at: Utc::now(),
        };
        return Ok(if state.create_batch(&fresh).await? {
            Outcome::New
        } else {
            Outcome::Conflict
        });
    };
    let curated_version = b.curated.as_ref().map(|c| c.version);
    let outcome = if version > b.version {
        Outcome::NewVersion
    } else if version == b.version
        && curated_version != Some(version)
        && matches!(b.status, BatchStatus::Failed)
    {
        Outcome::Requeued
    } else {
        // Already queued, being curated, curated, or an older listing.
        return Ok(Outcome::Unchanged);
    };
    b.version = version;
    b.source_url = l.url.clone();
    b.source_sha256 = l.sha256.clone();
    b.ocr_source = ocr_source;
    b.status = BatchStatus::Queued;
    b.attempts = 0;
    b.last_error = None;
    b.updated_at = Utc::now();
    Ok(match state.replace_batch(&b, &etag).await? {
        Some(_) => outcome,
        None => Outcome::Conflict,
    })
}

pub(crate) fn local_path(url: &str) -> Option<&str> {
    url.strip_prefix("file://")
        .or_else(|| (!url.contains("://")).then_some(url))
}

/// `https`, or plain `http` to a loopback test server.
fn check_remote(url: &str) -> anyhow::Result<()> {
    let ok = url.starts_with("https://")
        || url.starts_with("http://127.0.0.1:")
        || url.starts_with("http://localhost:");
    if !ok {
        bail!("batch source `{url}` must be https, file:// or a local path");
    }
    Ok(())
}

/// GET `url`, retrying 5xx and connection errors with backoff (honoring
/// `Retry-After`) until the response starts. A 429 is a [`Throttled`] error
/// at once: LoC blocks the IP for about an hour, and every retry extends it.
async fn get(url: &str) -> anyhow::Result<reqwest::Response> {
    check_remote(url)?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .read_timeout(Duration::from_secs(120))
        .build()?;
    let mut delay = Duration::from_secs(5);
    for attempt in 1..=6 {
        let resp = client.get(url).send().await;
        let why = match &resp {
            Ok(r) => r.status().to_string(),
            Err(e) => e.to_string(),
        };
        let wait = match resp {
            Ok(r) if r.status().is_success() => return Ok(r),
            Ok(r) if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS => {
                return Err(Throttled(url.to_owned()).into())
            }
            Ok(r) if r.status().is_server_error() => r
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .map_or(delay, Duration::from_secs),
            Ok(r) => bail!("{url} returned {}", r.status()),
            Err(_) => delay,
        };
        if attempt == 6 {
            break;
        }
        tracing::warn!(
            url,
            attempt,
            wait_secs = wait.as_secs(),
            error = %why,
            "fetch failed; backing off"
        );
        tokio::time::sleep(wait.min(Duration::from_secs(600))).await;
        delay *= 2;
    }
    bail!("{url}: giving up after 6 attempts")
}

/// The server is rate limiting us (a 429, or an HTML challenge page where
/// JSON was expected). loc.gov blocks for an hour when its API limit is
/// exceeded, so callers stop rather than retry.
#[derive(Debug)]
pub struct Throttled(pub String);

impl std::fmt::Display for Throttled {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "rate limited by {}", self.0)
    }
}

impl std::error::Error for Throttled {}

/// GET a small JSON `url` (https, or loopback http in tests) into memory,
/// retrying server errors and interrupted bodies. `Ok(None)` is a 404; a
/// 429 or an HTML page is a [`Throttled`] error, never retried.
pub async fn get_json(url: &str) -> anyhow::Result<Option<Vec<u8>>> {
    check_remote(url)?;
    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .connect_timeout(Duration::from_secs(30))
        .timeout(Duration::from_secs(60))
        .build()?;
    let mut last = None;
    for attempt in 1..=3u64 {
        match client.get(url).send().await {
            Ok(r) if r.status() == reqwest::StatusCode::NOT_FOUND => return Ok(None),
            // A 429, or an HTML page (a CAPTCHA challenge, whatever its
            // status) where JSON was asked for.
            Ok(r)
                if r.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
                    || r.headers()
                        .get(reqwest::header::CONTENT_TYPE)
                        .and_then(|v| v.to_str().ok())
                        .is_some_and(|v| v.contains("html")) =>
            {
                return Err(Throttled(url.to_owned()).into())
            }
            Ok(r) if r.status().is_success() => match r.bytes().await {
                Ok(b) => return Ok(Some(b.to_vec())),
                Err(e) => last = Some(anyhow::Error::from(e).context(format!("{url}: body"))),
            },
            Ok(r) if r.status().is_server_error() => {
                last = Some(anyhow::anyhow!("{url} returned {}", r.status()))
            }
            Ok(r) => bail!("{url} returned {}", r.status()),
            Err(e) => last = Some(anyhow::Error::from(e).context(url.to_owned())),
        }
        tokio::time::sleep(Duration::from_secs(5 * attempt)).await;
    }
    Err(last.expect("an attempt failed"))
}

/// Fetch `url` (https, `file://` or a local path) into `dest`, returning its
/// sha256. Used for small files such as batch lists.
pub async fn fetch(url: &str, dest: &Path) -> anyhow::Result<String> {
    let mut d = open(url).await?;
    let dest = dest.to_owned();
    let reader = tokio::task::spawn_blocking(move || -> anyhow::Result<()> {
        let mut out = std::fs::File::create(&dest)?;
        std::io::copy(&mut d.reader, &mut out)?;
        out.sync_all()?;
        Ok(())
    });
    reader.await??;
    d.digest.await.context("download task stopped")?
}

/// An archive being streamed: read it from `reader` (a blocking reader, for
/// use on a blocking thread), then await `digest` for its sha256 once the
/// reader is exhausted. Nothing touches local disk, so a worker needs no
/// scratch space however large the archive (up to 3.8 GB, 04 §4.1.1).
pub struct Download {
    pub reader: ChannelReader,
    pub digest: tokio::sync::oneshot::Receiver<anyhow::Result<String>>,
}

/// Start streaming `url` (https, `file://` or a local path).
pub async fn open(url: &str) -> anyhow::Result<Download> {
    enum Source {
        File(tokio::fs::File),
        Http(reqwest::Response),
    }
    let source = match local_path(url) {
        Some(path) => Source::File(
            tokio::fs::File::open(path)
                .await
                .with_context(|| path.to_owned())?,
        ),
        None => Source::Http(get(url).await?),
    };
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<bytes::Bytes>>(16);
    let (done, digest) = tokio::sync::oneshot::channel();
    let url = url.to_owned();
    tokio::spawn(async move {
        let mut hash = Sha256::new();
        let result: anyhow::Result<()> = async {
            match source {
                Source::File(mut f) => {
                    use tokio::io::AsyncReadExt;
                    loop {
                        let mut buf = vec![0u8; 1 << 20];
                        let n = f.read(&mut buf).await?;
                        if n == 0 {
                            break;
                        }
                        buf.truncate(n);
                        hash.update(&buf);
                        if tx.send(Ok(buf.into())).await.is_err() {
                            bail!("reader stopped");
                        }
                    }
                }
                Source::Http(resp) => {
                    let mut stream = resp.bytes_stream();
                    while let Some(chunk) = stream.next().await {
                        let chunk =
                            chunk.with_context(|| format!("{url}: download interrupted"))?;
                        hash.update(&chunk);
                        if tx.send(Ok(chunk)).await.is_err() {
                            bail!("reader stopped");
                        }
                    }
                }
            }
            Ok(())
        }
        .await;
        match result {
            Ok(()) => {
                drop(tx);
                let _ = done.send(Ok(hex(&hash.finalize())));
            }
            Err(e) => {
                let _ = tx.send(Err(std::io::Error::other(format!("{e:#}")))).await;
                let _ = done.send(Err(e));
            }
        }
    });
    Ok(Download {
        reader: ChannelReader {
            rx,
            current: bytes::Bytes::new(),
        },
        digest,
    })
}

/// Blocking `Read` over chunks sent from an async task.
pub struct ChannelReader {
    rx: tokio::sync::mpsc::Receiver<std::io::Result<bytes::Bytes>>,
    current: bytes::Bytes,
}

impl std::io::Read for ChannelReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        while self.current.is_empty() {
            match self.rx.blocking_recv() {
                Some(Ok(b)) => self.current = b,
                Some(Err(e)) => return Err(e),
                None => return Ok(0),
            }
        }
        let n = buf.len().min(self.current.len());
        buf[..n].copy_from_slice(&self.current.split_to(n));
        Ok(n)
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::MemoryDocs;
    use std::sync::Arc;

    fn listed(name: &str) -> ListedBatch {
        ListedBatch {
            name: name.into(),
            url: format!("https://example.org/{name}.tar.bz2"),
            sha256: None,
            ocr_source: None,
            lccns: vec![],
        }
    }

    #[test]
    fn reads_loc_collection_listings() {
        // Two entries as LoC publishes them (trimmed).
        let loc = br#"{"title": "Chronicling America", "datasets": [
          {"archive_name": "dlc_zurich_ver04.tar.bz2", "batch": "dlc_zurich_ver04",
           "issue_count": 1, "lccns": ["sn85042252"], "page_count": 4,
           "sha256": "ad36f7bb2ef915867460ac2b3c70f9aaa33709a9a99f554a49a9dc4fc35f3ba9",
           "size": 58753, "url": "https://chroniclingamerica.loc.gov/data/ocr/dlc_zurich_ver04.tar.bz2"},
          {"batch": "vi_elgar_ver02", "sha256": null,
           "url": "https://chroniclingamerica.loc.gov/data/ocr/vi_elgar_ver02.tar.bz2"}
        ], "results": []}"#;
        let l = parse_list(loc).unwrap();
        assert_eq!(l.len(), 2);
        assert_eq!(split_version(&l[0].name).unwrap(), ("dlc_zurich".into(), 4));
        assert!(l[0].sha256.is_some() && l[1].sha256.is_none());
        let plain = br#"[{"name": "batch_a_ver01", "url": "/tmp/a.tar.gz"}]"#;
        assert_eq!(parse_list(plain).unwrap()[0].name, "batch_a_ver01");
        assert!(parse_list(br#"{"results": []}"#).is_err());
    }

    #[test]
    fn batch_names() {
        assert_eq!(
            split_version("batch_az_acacia_ver02").unwrap(),
            ("batch_az_acacia".into(), 2)
        );
        assert!(split_version("batch_az_acacia").is_err());
        assert!(split_version("batch_az_acacia_ver00").is_err());
        assert!(split_version("../x_ver01").is_err());
    }

    #[tokio::test]
    async fn enqueue_queues_new_batches_and_versions_once() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        let r = enqueue(&s, &[listed("batch_a_ver01"), listed("batch_b_ver01")])
            .await
            .unwrap();
        assert_eq!(r.new, 2);
        let r = enqueue(&s, &[listed("batch_a_ver01")]).await.unwrap();
        assert_eq!(r.unchanged, 1);
        let r = enqueue(&s, &[listed("batch_a_ver02")]).await.unwrap();
        assert_eq!(r.new_version, 1);
        let (b, _) = s.batch("batch_a").await.unwrap().unwrap();
        assert_eq!((b.version, b.status), (2, BatchStatus::Queued));
        // An older listing never downgrades.
        let r = enqueue(&s, &[listed("batch_a_ver01")]).await.unwrap();
        assert_eq!(r.unchanged, 1);
        assert_eq!(s.batch("batch_a").await.unwrap().unwrap().0.version, 2);
    }

    #[tokio::test]
    async fn concurrent_enqueues_of_the_same_list_agree() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        let list: Vec<ListedBatch> = (1..=20)
            .map(|i| listed(&format!("batch_{i}_ver01")))
            .collect();
        let (a, b, c) = tokio::join!(enqueue(&s, &list), enqueue(&s, &list), enqueue(&s, &list));
        let reports = [a.unwrap(), b.unwrap(), c.unwrap()];
        assert_eq!(reports.iter().map(|r| r.new).sum::<usize>(), 20);
        assert_eq!(s.batches(&[BatchStatus::Queued]).await.unwrap().len(), 20);
    }

    #[tokio::test]
    async fn fetches_local_files_with_their_hash() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.bin");
        std::fs::write(&src, b"abc").unwrap();
        let dest = dir.path().join("b.bin");
        let sha = fetch(src.to_str().unwrap(), &dest).await.unwrap();
        assert_eq!(
            sha,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert_eq!(std::fs::read(dest).unwrap(), b"abc");
        assert!(fetch("http://example.org/x", &dir.path().join("c"))
            .await
            .is_err());
    }

    /// Serve one response on a loopback port: `declared` in Content-Length,
    /// then `body`, then close (short when `body` is shorter).
    async fn serve_once(body: Vec<u8>, declared: usize) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut req = [0u8; 4096];
            let _ = sock.read(&mut req).await;
            let head = format!(
                "HTTP/1.1 200 OK\r\ncontent-length: {declared}\r\nconnection: close\r\n\r\n"
            );
            sock.write_all(head.as_bytes()).await.unwrap();
            for chunk in body.chunks(64 * 1024) {
                if sock.write_all(chunk).await.is_err() {
                    return;
                }
            }
            let _ = sock.shutdown().await;
        });
        format!("http://127.0.0.1:{}/batch.tar", addr.port())
    }

    fn read_all(d: Download) -> tokio::task::JoinHandle<(std::io::Result<Vec<u8>>, Download)> {
        tokio::task::spawn_blocking(move || {
            let mut d = d;
            let mut out = Vec::new();
            let r = std::io::Read::read_to_end(&mut d.reader, &mut out).map(|_| out);
            (r, d)
        })
    }

    #[tokio::test]
    async fn a_429_is_throttled_without_a_retry() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/batch.tar", listener.local_addr().unwrap());
        let hits = Arc::new(std::sync::atomic::AtomicU32::new(0));
        let seen = hits.clone();
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let mut req = [0u8; 4096];
                let _ = sock.read(&mut req).await;
                let reply = "HTTP/1.1 429 Too Many Requests\r\ncontent-length: 0\r\nconnection: close\r\n\r\n";
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        let err = open(&url).await.err().expect("a 429 fails");
        assert!(err.downcast_ref::<Throttled>().is_some(), "{err:#}");
        assert_eq!(hits.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn streams_http_bodies_larger_than_the_channel() {
        // 6 MiB in 64 KiB writes: far more chunks than the 16-slot channel holds.
        let body: Vec<u8> = (0..6 * 1024 * 1024u32).map(|i| (i % 251) as u8).collect();
        let url = serve_once(body.clone(), body.len()).await;
        let (read, d) = read_all(open(&url).await.unwrap()).await.unwrap();
        assert_eq!(read.unwrap(), body);
        let sha = d.digest.await.unwrap().unwrap();
        assert_eq!(sha, hex(&Sha256::digest(&body)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_interrupted_download_fails_the_reader_and_the_digest() {
        let body = vec![7u8; 1024 * 1024];
        let url = serve_once(body, 4 * 1024 * 1024).await;
        let (read, d) = read_all(open(&url).await.unwrap()).await.unwrap();
        let err = read.unwrap_err().to_string();
        assert!(err.contains("download interrupted"), "{err}");
        assert!(d.digest.await.unwrap().is_err());
    }
}
