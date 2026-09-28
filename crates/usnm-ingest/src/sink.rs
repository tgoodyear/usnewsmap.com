//! Where index documents go: JSONL files (the API's memory backend, local
//! development and tests) or a Quickwit writer node (08 §8.4.1).

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use anyhow::{bail, Context};
use async_trait::async_trait;
use serde_json::Value;

/// The index config every base and delta shares (05 §5.5.1).
pub const INDEX_TEMPLATE: &str = include_str!("../../../infra/quickwit/pages-index.yaml");

/// Documents per ingest request, bounded well under Quickwit's 10 MiB body limit.
const CHUNK_BYTES: usize = 8 * 1024 * 1024;

/// Per-shard ingest rate on the writer node: Quickwit's maximum (its default
/// is 5 MB/s). A single-node writer can't spread load over more shards, so
/// past the ~50 MiB burst allowance ingest runs at this rate.
const SHARD_THROUGHPUT_LIMIT: &str = "20MB";

/// How long one ingest request keeps retrying while the node pushes back
/// (503 "no shards available" once the shard's rate limit is spent, or 429).
const INGEST_RETRY_FOR: Duration = Duration::from_secs(600);

#[async_trait]
pub trait IndexSink: Send {
    /// Start a new, empty index. Fails if it already exists.
    async fn create(&mut self, index_id: &str) -> anyhow::Result<()>;
    async fn add(&mut self, doc: &Value) -> anyhow::Result<()>;
    /// Flush and confirm the index holds exactly `expected` documents.
    async fn finish(&mut self, expected: u64) -> anyhow::Result<()>;
    /// `memory` or `quickwit`, recorded in `current.json`.
    fn backend(&self) -> &'static str;
}

/// `{dir}/{index_id}.jsonl`, the layout the memory backend loads.
pub struct JsonlSink {
    dir: PathBuf,
    current: Option<(PathBuf, std::io::BufWriter<std::fs::File>)>,
}

impl JsonlSink {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: dir.into(),
            current: None,
        }
    }
}

#[async_trait]
impl IndexSink for JsonlSink {
    async fn create(&mut self, index_id: &str) -> anyhow::Result<()> {
        std::fs::create_dir_all(&self.dir)?;
        let path = self.dir.join(format!("{index_id}.jsonl"));
        let file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .with_context(|| format!("{} (indexes are never rewritten)", path.display()))?;
        self.current = Some((path, std::io::BufWriter::new(file)));
        Ok(())
    }

    async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
        use std::io::Write;
        let (_, w) = self.current.as_mut().context("no index created")?;
        serde_json::to_writer(&mut *w, doc)?;
        w.write_all(b"\n")?;
        Ok(())
    }

    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        use std::io::{BufRead, Write};
        let (path, mut w) = self.current.take().context("no index created")?;
        w.flush()?;
        w.get_ref().sync_all()?;
        let lines = std::io::BufReader::new(std::fs::File::open(&path)?)
            .lines()
            .count() as u64;
        if lines != expected {
            bail!(
                "{} holds {lines} documents, expected {expected}",
                path.display()
            );
        }
        Ok(())
    }

    fn backend(&self) -> &'static str {
        "memory"
    }
}

/// Ingests into a Quickwit node that runs the indexer (the sole metastore writer).
pub struct QuickwitSink {
    base: String,
    index_root: String,
    http: reqwest::Client,
    index: Option<String>,
    buf: Vec<u8>,
    /// First pause after pushback; doubles up to 8 s.
    retry_initial: Duration,
    retry_for: Duration,
}

impl QuickwitSink {
    /// `index_root` is where new indexes live (`azure://qw-index` or `file:///…`).
    pub fn new(base_url: &str, index_root: &str) -> anyhow::Result<Self> {
        Ok(Self {
            base: base_url.trim_end_matches('/').to_owned(),
            index_root: index_root.trim_end_matches('/').to_owned(),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(300))
                .build()?,
            index: None,
            buf: Vec::new(),
            retry_initial: Duration::from_millis(500),
            retry_for: INGEST_RETRY_FOR,
        })
    }

    async fn send(&mut self, commit: &str) -> anyhow::Result<()> {
        if self.buf.is_empty() && commit != "force" {
            return Ok(());
        }
        let id = self.index.as_deref().context("no index created")?;
        let body = bytes::Bytes::from(std::mem::take(&mut self.buf));
        let url = format!("{}/api/v1/{id}/ingest?commit={commit}", self.base);
        let deadline = tokio::time::Instant::now() + self.retry_for;
        let mut pause = self.retry_initial;
        let (status, text) = loop {
            let resp = self
                .http
                .post(&url)
                .header("content-type", "application/json")
                .body(body.clone())
                .send()
                .await?;
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            // The whole request is resent; `finish` checks the final count, so
            // a duplicate would fail the release rather than go unnoticed.
            let pushback = status == reqwest::StatusCode::SERVICE_UNAVAILABLE
                || status == reqwest::StatusCode::TOO_MANY_REQUESTS;
            if !pushback || tokio::time::Instant::now() + pause > deadline {
                break (status, text);
            }
            tracing::debug!(index = id, %status, ?pause, "Quickwit pushed back; retrying");
            tokio::time::sleep(pause).await;
            pause = (pause * 2).min(Duration::from_secs(8));
        };
        if !status.is_success() {
            bail!(
                "ingest into `{id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        let v: Value = serde_json::from_str(&text).context("ingest response")?;
        if v["num_rejected_docs"].as_u64() != Some(0) {
            bail!(
                "ingest into `{id}` rejected documents: {}",
                text.chars().take(500).collect::<String>()
            );
        }
        Ok(())
    }

    async fn count(&self, id: &str) -> anyhow::Result<u64> {
        let v: Value = self
            .http
            .get(format!(
                "{}/api/v1/{id}/search?query=*&max_hits=0",
                self.base
            ))
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        v["num_hits"]
            .as_u64()
            .context("search response has no num_hits")
    }
}

#[async_trait]
impl IndexSink for QuickwitSink {
    async fn create(&mut self, index_id: &str) -> anyhow::Result<()> {
        let config = INDEX_TEMPLATE
            .replace("${INDEX_ID}", index_id)
            .replace("${INDEX_URI}", &format!("{}/{index_id}", self.index_root));
        let resp = self
            .http
            .post(format!("{}/api/v1/indexes", self.base))
            .header("content-type", "application/yaml")
            .body(config)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "creating index `{index_id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        self.index = Some(index_id.to_owned());
        Ok(())
    }

    async fn add(&mut self, doc: &Value) -> anyhow::Result<()> {
        let mut line = serde_json::to_vec(doc)?;
        line.push(b'\n');
        if line.len() > CHUNK_BYTES {
            bail!(
                "document `{}` is {} bytes, over the {CHUNK_BYTES}-byte ingest request limit",
                doc["doc_id"].as_str().unwrap_or("?"),
                line.len()
            );
        }
        // Send first if this document would push the request over the limit.
        if self.buf.len() + line.len() > CHUNK_BYTES {
            self.send("auto").await?;
        }
        self.buf.extend_from_slice(&line);
        Ok(())
    }

    async fn finish(&mut self, expected: u64) -> anyhow::Result<()> {
        // A forced commit publishes everything still buffered in the node.
        self.send("force").await?;
        let id = self.index.clone().context("no index created")?;
        let mut last = 0;
        for _ in 0..120 {
            last = self.count(&id).await?;
            if last == expected {
                return Ok(());
            }
            if last > expected {
                break;
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
        bail!("index `{id}` holds {last} documents, expected {expected}")
    }

    fn backend(&self) -> &'static str {
        "quickwit"
    }
}

/// A Quickwit indexer node run as a child process for the length of a
/// release: the one writer of the file-backed metastore.
pub struct QuickwitNode {
    child: tokio::process::Child,
    pub url: String,
}

impl QuickwitNode {
    /// `metastore` and `index_root` are `azure://qw-index` in Azure and
    /// `file:///…` locally. On Azure, Quickwit authenticates with the job's
    /// system-assigned identity (08 §8.2).
    pub async fn start(
        bin: &Path,
        work_dir: &Path,
        port: u16,
        metastore: &str,
        index_root: &str,
    ) -> anyhow::Result<Self> {
        let data = work_dir.join("qwdata");
        std::fs::create_dir_all(&data)?;
        let mut config = format!(
            "version: 0.8\ncluster_id: usnm-writer\nnode_id: writer\nlisten_address: 127.0.0.1\n\
             rest:\n  listen_port: {port}\ngrpc_listen_port: {}\ndata_dir: {}\n\
             metastore_uri: {metastore}\ndefault_index_root_uri: {index_root}\n\
             ingest_api:\n  shard_throughput_limit: {SHARD_THROUGHPUT_LIMIT}\n",
            port.checked_add(1)
                .context("--quickwit-port must be below 65535")?,
            data.display()
        );
        if let Ok(account) = std::env::var("QW_AZURE_STORAGE_ACCOUNT") {
            config.push_str(&format!("storage:\n  azure:\n    account: {account}\n"));
        }
        let config_path = work_dir.join("writer.yaml");
        std::fs::write(&config_path, config)?;
        let log = std::fs::File::create(work_dir.join("quickwit-writer.log"))?;
        let child = tokio::process::Command::new(bin)
            .args(["run", "--config"])
            .arg(&config_path)
            .env("QW_DISABLE_TELEMETRY", "1")
            // AZURE_CLIENT_ID selects the pipeline's user-assigned identity;
            // Quickwit's credential chain uses the system-assigned one (08 §8.2).
            .env_remove("AZURE_CLIENT_ID")
            .stdin(Stdio::null())
            .stdout(log.try_clone()?)
            .stderr(log)
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("starting {}", bin.display()))?;
        let node = Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
        };
        let http = reqwest::Client::new();
        for _ in 0..120 {
            if http
                .get(format!("{}/health/readyz", node.url))
                .send()
                .await
                .is_ok_and(|r| r.status().is_success())
            {
                return Ok(node);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        bail!("Quickwit writer did not become ready (see quickwit-writer.log)")
    }

    /// Stop the node and wait for it to exit.
    pub async fn stop(mut self) -> anyhow::Result<()> {
        if let Some(pid) = self.child.id() {
            // SIGTERM lets Quickwit shut down cleanly; `kill` is a shell builtin.
            let _ = tokio::process::Command::new("sh")
                .args(["-c", "kill -TERM \"$0\"", &pid.to_string()])
                .status()
                .await;
        }
        match tokio::time::timeout(Duration::from_secs(60), self.child.wait()).await {
            Ok(status) => {
                status?;
            }
            Err(_) => self.child.kill().await?,
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[tokio::test]
    async fn requests_stay_under_the_ingest_limit() {
        let mut s = QuickwitSink::new("http://127.0.0.1:9", "file:///tmp/x").unwrap();
        s.index = Some("i".into());
        // Documents that fit are buffered without sending.
        let doc = serde_json::json!({"doc_id": "a", "text": "x".repeat(1000)});
        s.add(&doc).await.unwrap();
        assert!(s.buf.len() < CHUNK_BYTES);
        // One document over the limit is refused, not sent.
        let big = serde_json::json!({"doc_id": "big", "text": "x".repeat(CHUNK_BYTES)});
        let err = s.add(&big).await.unwrap_err().to_string();
        assert!(err.contains("`big`"), "{err}");
    }

    /// A fake ingest endpoint that answers with `statuses` in turn (then 200)
    /// and records every body it receives.
    async fn fake_node(statuses: Vec<u16>) -> (QuickwitSink, Arc<Mutex<Vec<Vec<u8>>>>) {
        use axum::http::StatusCode;
        let seen: Arc<Mutex<Vec<Vec<u8>>>> = Arc::default();
        let queue = Arc::new(Mutex::new(std::collections::VecDeque::from(statuses)));
        let app = axum::Router::new().route(
            "/api/v1/{id}/ingest",
            axum::routing::post({
                let seen = seen.clone();
                move |body: axum::body::Bytes| async move {
                    seen.lock().unwrap().push(body.to_vec());
                    match queue.lock().unwrap().pop_front() {
                        Some(code) => (
                            StatusCode::from_u16(code).unwrap(),
                            r#"{"message":"ingest service is unavailable (no shards available)"}"#,
                        ),
                        None => (StatusCode::OK, r#"{"num_rejected_docs":0}"#),
                    }
                }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let mut s = QuickwitSink::new(&format!("http://{addr}"), "file:///tmp/x").unwrap();
        s.index = Some("i".into());
        s.retry_initial = Duration::from_millis(1);
        s.retry_for = Duration::from_secs(5);
        (s, seen)
    }

    #[tokio::test]
    async fn ingest_retries_while_the_node_pushes_back() {
        let (mut s, seen) = fake_node(vec![503, 429, 503]).await;
        s.buf = b"{\"doc_id\":\"a\"}\n".to_vec();
        s.send("auto").await.unwrap();
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 4);
        assert!(seen.iter().all(|b| b == b"{\"doc_id\":\"a\"}\n"));
    }

    #[tokio::test]
    async fn ingest_gives_up_after_the_retry_window() {
        let (mut s, _) = fake_node(vec![503; 1000]).await;
        s.retry_for = Duration::from_millis(50);
        s.buf = b"{}\n".to_vec();
        let err = s.send("auto").await.unwrap_err().to_string();
        assert!(err.contains("503"), "{err}");
    }

    #[tokio::test]
    async fn other_ingest_errors_are_not_retried() {
        let (mut s, seen) = fake_node(vec![400]).await;
        s.buf = b"{}\n".to_vec();
        let err = s.send("auto").await.unwrap_err().to_string();
        assert!(err.contains("400"), "{err}");
        assert_eq!(seen.lock().unwrap().len(), 1);
    }
}
