//! The OCR nodes: each worker's `GET /status`, and its journal of finished
//! pages (`GET /journal?from=<offset>`), copied here so the totals outlive
//! a worker's restart and a page finished while the dashboard was down
//! still counts.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::io::Write as _;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::{bail, Context};
use chrono::{DateTime, Utc};
use usnm_ocr_contract::status::{Completion, WorkerStatus};

/// The largest status or journal answer read.
const MAX_BODY: usize = 4 * 1024 * 1024;

#[derive(Debug)]
pub struct Node {
    pub name: String,
    pub url: String,
    pub status: Option<WorkerStatus>,
    pub last_ok: Option<DateTime<Utc>>,
    pub error: Option<String>,
    /// How much of the worker's journal has been copied.
    pub offset: u64,
}

/// A node's name: `[A-Za-z0-9-]`, 1 to 32; it names a file here.
pub fn check_name(s: &str) -> anyhow::Result<()> {
    if !s.is_empty() && s.len() <= 32 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        Ok(())
    } else {
        bail!("a node name is 1 to 32 letters, digits or `-`")
    }
}

#[derive(Debug, Default)]
pub struct Nodes {
    pub nodes: Vec<Node>,
    state_dir: PathBuf,
    /// Every finished page copied, with its node's index.
    pub done: Vec<(usize, Completion)>,
    seen: HashSet<(usize, String, i64)>,
    /// Pages OCRed (outcome `done`) → their latest completion's index.
    pub ocred: HashMap<String, usize>,
}

/// What one poll of a node brought.
pub struct Polled {
    status: WorkerStatus,
    lines: Vec<Completion>,
    offset: u64,
}

impl Nodes {
    /// The nodes (`name=url`), and what was copied before, from `state_dir`.
    pub fn open(state_dir: PathBuf, nodes: &[(String, String)]) -> anyhow::Result<Self> {
        std::fs::create_dir_all(&state_dir)
            .with_context(|| format!("creating {}", state_dir.display()))?;
        let offsets: BTreeMap<String, u64> = match std::fs::read(state_dir.join("offsets.json")) {
            Ok(b) => serde_json::from_slice(&b).context("offsets.json")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => BTreeMap::new(),
            Err(e) => return Err(e.into()),
        };
        let mut me = Nodes {
            state_dir,
            ..Nodes::default()
        };
        for (name, url) in nodes {
            check_name(name)?;
            me.nodes.push(Node {
                name: name.clone(),
                url: url.trim_end_matches('/').to_owned(),
                status: None,
                last_ok: None,
                error: None,
                offset: offsets.get(name).copied().unwrap_or(0),
            });
        }
        for i in 0..me.nodes.len() {
            let path = me.journal_path(i);
            let text = match std::fs::read_to_string(&path) {
                Ok(t) => t,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(e.into()),
            };
            for l in text.lines() {
                if let Ok(c) = serde_json::from_str::<Completion>(l) {
                    if c.check().is_ok() {
                        me.add(i, c);
                    }
                }
            }
        }
        Ok(me)
    }

    fn journal_path(&self, i: usize) -> PathBuf {
        self.state_dir
            .join(format!("journal-{}.jsonl", self.nodes[i].name))
    }

    /// Add a completion; false if it was here already.
    fn add(&mut self, i: usize, c: Completion) -> bool {
        let key = (
            i,
            c.doc_id.clone(),
            c.finished_at.timestamp_nanos_opt().unwrap_or(0),
        );
        if !self.seen.insert(key) {
            return false;
        }
        if c.outcome == "done" {
            self.ocred.insert(c.doc_id.clone(), self.done.len());
        }
        self.done.push((i, c));
        true
    }

    /// Ask a node for its status and any journal past `offset`.
    pub async fn poll(client: &reqwest::Client, url: &str, offset: u64) -> anyhow::Result<Polled> {
        let status: WorkerStatus =
            serde_json::from_slice(&get(client, &format!("{url}/status")).await?)
                .context("the status isn't JSON")?;
        let mut offset = offset;
        if status.journal_bytes < offset {
            // A new journal: copy it from the start (repeats are dropped).
            offset = 0;
        }
        let mut lines = Vec::new();
        if status.journal_bytes > offset {
            let body = get(client, &format!("{url}/journal?from={offset}")).await?;
            if let Some(end) = body.iter().rposition(|b| *b == b'\n') {
                offset += end as u64 + 1;
                for l in String::from_utf8_lossy(&body[..end]).lines() {
                    match serde_json::from_str::<Completion>(l) {
                        Ok(c) if c.check().is_ok() => lines.push(c),
                        _ => tracing::warn!(url, "a journal line that isn't a completion; skipped"),
                    }
                }
            }
        }
        Ok(Polled {
            status,
            lines,
            offset,
        })
    }

    /// Apply a poll of node `i`: keep its status, copy its new lines.
    pub fn apply(&mut self, i: usize, r: anyhow::Result<Polled>, now: DateTime<Utc>) {
        match r {
            Ok(p) => {
                let mut new = Vec::new();
                for c in p.lines {
                    if self.add(i, c.clone()) {
                        new.push(c);
                    }
                }
                if let Err(e) = self.persist(i, &new, p.offset) {
                    tracing::warn!(error = format!("{e:#}"), "can't save the copied journal");
                }
                let n = &mut self.nodes[i];
                n.offset = p.offset;
                n.status = Some(p.status);
                n.last_ok = Some(now);
                n.error = None;
            }
            Err(e) => self.nodes[i].error = Some(format!("{e:#}")),
        }
    }

    fn persist(&self, i: usize, new: &[Completion], offset: u64) -> anyhow::Result<()> {
        if !new.is_empty() {
            let mut f = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.journal_path(i))?;
            let mut buf = Vec::new();
            for c in new {
                serde_json::to_writer(&mut buf, c)?;
                buf.push(b'\n');
            }
            f.write_all(&buf)?;
        }
        if self.nodes[i].offset != offset || !new.is_empty() {
            let mut offsets: BTreeMap<&str, u64> = self
                .nodes
                .iter()
                .map(|n| (n.name.as_str(), n.offset))
                .collect();
            offsets.insert(&self.nodes[i].name, offset);
            let tmp = self.state_dir.join("offsets.json.tmp");
            std::fs::write(&tmp, serde_json::to_vec(&offsets)?)?;
            std::fs::rename(&tmp, self.state_dir.join("offsets.json"))?;
        }
        Ok(())
    }
}

async fn get(client: &reqwest::Client, url: &str) -> anyhow::Result<Vec<u8>> {
    let mut resp = client
        .get(url)
        .timeout(Duration::from_secs(5))
        .send()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.without_url()))?;
    if !resp.status().is_success() {
        bail!("HTTP {}", resp.status().as_u16());
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp
        .chunk()
        .await
        .map_err(|e| anyhow::anyhow!("{}", e.without_url()))?
    {
        if body.len() + chunk.len() > MAX_BODY {
            bail!("the answer is over {MAX_BODY} bytes");
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum::routing::get as route;
    use axum::Router;
    use std::sync::{Arc, Mutex};

    fn line(doc: &str, outcome: &str) -> String {
        serde_json::to_string(&Completion {
            doc_id: doc.into(),
            language: "deu".into(),
            engine: "tesseract 5.5.0 frak2021".into(),
            queue: "ocr-bulk".into(),
            reason: "bulk".into(),
            outcome: outcome.into(),
            scan: "cache".into(),
            uploaded: outcome == "done",
            finished_at: "2026-10-11T23:00:00Z".parse().unwrap(),
            ..Completion::default()
        })
        .unwrap()
            + "\n"
    }

    /// A worker whose journal is whatever the test puts in it.
    async fn fake_worker(journal: Arc<Mutex<String>>) -> String {
        let j1 = journal.clone();
        let app = Router::new()
            .route(
                "/status",
                route(move || {
                    let j = j1.clone();
                    async move {
                        let len = j.lock().unwrap().len() as u64;
                        axum::Json(WorkerStatus {
                            node_id: "n1".into(),
                            journal_bytes: len,
                            ..WorkerStatus::default()
                        })
                    }
                }),
            )
            .route(
                "/journal",
                route(move |Query(q): Query<HashMap<String, usize>>| {
                    let j = journal.clone();
                    async move { j.lock().unwrap()[q["from"]..].to_owned() }
                }),
            );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn copies_the_journal_once_and_keeps_it_over_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let journal = Arc::new(Mutex::new(String::new()));
        let url = fake_worker(journal.clone()).await;
        let client = reqwest::Client::new();
        let nodes = [("node1".to_owned(), url.clone())];
        let mut n = Nodes::open(dir.path().to_owned(), &nodes).unwrap();
        journal
            .lock()
            .unwrap()
            .push_str(&line("sn1_1890-01-01_ed-1_seq-1", "done"));
        journal
            .lock()
            .unwrap()
            .push_str("{\"doc_id\":\"Seitentext hier\"}\n");
        journal
            .lock()
            .unwrap()
            .push_str(&line("sn1_1890-01-01_ed-1_seq-2", "failed"));
        let r = Nodes::poll(&client, &url, n.nodes[0].offset).await;
        n.apply(0, r, Utc::now());
        assert_eq!(n.done.len(), 2);
        assert_eq!(n.ocred.len(), 1);
        assert!(n.nodes[0].error.is_none());
        // Nothing new: nothing added.
        let r = Nodes::poll(&client, &url, n.nodes[0].offset).await;
        n.apply(0, r, Utc::now());
        assert_eq!(n.done.len(), 2);
        // The worker starts a new journal with the same page in it: the
        // dashboard reads it from the start and drops the repeat.
        *journal.lock().unwrap() = line("sn1_1890-01-01_ed-1_seq-1", "done");
        let r = Nodes::poll(&client, &url, n.nodes[0].offset).await;
        n.apply(0, r, Utc::now());
        assert_eq!(n.done.len(), 2);
        // Reopened: the copy and the offset are back.
        let again = Nodes::open(dir.path().to_owned(), &nodes).unwrap();
        assert_eq!(again.done.len(), 2);
        assert_eq!(again.nodes[0].offset, n.nodes[0].offset);
    }

    #[tokio::test]
    async fn a_node_that_is_away_shows_its_error() {
        let dir = tempfile::tempdir().unwrap();
        let nodes = [("node2".to_owned(), "http://127.0.0.1:9".to_owned())];
        let mut n = Nodes::open(dir.path().to_owned(), &nodes).unwrap();
        let r = Nodes::poll(&reqwest::Client::new(), "http://127.0.0.1:9", 0).await;
        n.apply(0, r, Utc::now());
        assert!(n.nodes[0].error.is_some());
        assert!(n.nodes[0].status.is_none());
        assert!(check_name("../x").is_err());
    }
}
