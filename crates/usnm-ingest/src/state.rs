//! Typed pipeline state on top of [`DocStore`] (05 §5.9.1): batches (the
//! work queue), issues, the single-writer lock and index runs.

use std::sync::Arc;

use anyhow::{bail, Context};
use chrono::{DateTime, Duration, Utc};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::docs::{DocStore, Versioned};

pub const BATCHES: &str = "batches";
pub const ISSUES: &str = "issues";
pub const INDEX_RUNS: &str = "index_runs";
pub const OPS: &str = "ops";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BatchStatus {
    Queued,
    Downloading,
    Curated,
    Failed,
}

impl BatchStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Downloading => "downloading",
            Self::Curated => "curated",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Lease {
    pub owner: String,
    pub until: DateTime<Utc>,
}

/// The committed output of one curated batch version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Curated {
    pub version: u16,
    /// Object paths in the curated store.
    pub parts: Vec<String>,
    /// Pages per (lccn, day), for baselines without re-reading the parts.
    pub counts: String,
    pub pages: u64,
    /// Pages with `text_status = ok` (the ones that are indexed).
    pub ok_pages: u64,
    pub lccns: Vec<String>,
    pub source_sha256: String,
    pub first: String,
    pub last: String,
}

/// One LoC batch (partition key `/batch`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Batch {
    pub id: String,
    pub batch: String,
    /// The newest version seen; the one to curate.
    pub version: u16,
    pub source_url: String,
    #[serde(default)]
    pub source_sha256: Option<String>,
    pub ocr_source: String,
    pub status: BatchStatus,
    #[serde(default)]
    pub attempts: u32,
    #[serde(default)]
    pub lease: Option<Lease>,
    /// The last committed curation (may be an older version while a newer one is queued).
    #[serde(default)]
    pub curated: Option<Curated>,
    #[serde(default)]
    pub last_error: Option<String>,
    pub updated_at: DateTime<Utc>,
}

/// One issue (lccn + date + edition), partition key `/lccn`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub id: String,
    pub lccn: String,
    pub date: String,
    pub edition: u16,
    pub pages: u32,
    pub empty_pages: u32,
    pub batch: String,
    pub batch_version: u16,
    pub ocr_source: String,
}

/// The batches a published version was built from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunBatch {
    pub batch: String,
    pub curated: Curated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RunStatus {
    Building,
    Published,
    Failed,
}

/// One index build (partition key `/index_version`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexRun {
    pub id: String,
    pub index_version: String,
    pub full: bool,
    /// Every index the version serves (base + deltas).
    pub indexes: Vec<String>,
    /// The index this run wrote.
    pub new_index: String,
    pub batches: Vec<RunBatch>,
    pub status: RunStatus,
    pub docs: u64,
    pub pages: u64,
    pub started_at: DateTime<Utc>,
    #[serde(default)]
    pub published_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub previous_version: Option<String>,
    #[serde(default)]
    pub last_error: Option<String>,
}

/// Typed access to the pipeline's state.
#[derive(Clone)]
pub struct State {
    pub docs: Arc<dyn DocStore>,
}

fn to_value<T: Serialize>(item: &T) -> anyhow::Result<Value> {
    Ok(serde_json::to_value(item)?)
}

fn from<T: DeserializeOwned>(v: Versioned) -> anyhow::Result<(T, String)> {
    let id = v.doc["id"].as_str().unwrap_or("?").to_owned();
    let item = serde_json::from_value(v.doc).with_context(|| format!("state item `{id}`"))?;
    Ok((item, v.etag))
}

impl State {
    pub fn new(docs: Arc<dyn DocStore>) -> Self {
        Self { docs }
    }

    // ---------------------------------------------------------------- batches

    pub async fn batch(&self, batch: &str) -> anyhow::Result<Option<(Batch, String)>> {
        self.docs
            .get(BATCHES, batch, batch)
            .await?
            .map(from)
            .transpose()
    }

    pub async fn batches(&self, statuses: &[BatchStatus]) -> anyhow::Result<Vec<(Batch, String)>> {
        let values: Vec<&str> = statuses.iter().map(|s| s.as_str()).collect();
        self.docs
            .list(BATCHES, "status", &values)
            .await?
            .into_iter()
            .map(from)
            .collect()
    }

    pub async fn create_batch(&self, b: &Batch) -> anyhow::Result<bool> {
        Ok(self
            .docs
            .create(BATCHES, &b.batch, &to_value(b)?)
            .await?
            .is_some())
    }

    /// Replace if unchanged since read; returns the new ETag, or `None` if another writer won.
    pub async fn replace_batch(&self, b: &Batch, etag: &str) -> anyhow::Result<Option<String>> {
        self.docs
            .replace(BATCHES, &b.batch, &to_value(b)?, etag)
            .await
    }

    pub async fn upsert_issue(&self, i: &Issue) -> anyhow::Result<()> {
        self.docs.upsert(ISSUES, &i.lccn, &to_value(i)?).await
    }

    // ------------------------------------------------------------ index runs

    pub async fn run(&self, version: &str) -> anyhow::Result<Option<(IndexRun, String)>> {
        self.docs
            .get(INDEX_RUNS, version, version)
            .await?
            .map(from)
            .transpose()
    }

    pub async fn create_run(&self, r: &IndexRun) -> anyhow::Result<bool> {
        Ok(self
            .docs
            .create(INDEX_RUNS, &r.index_version, &to_value(r)?)
            .await?
            .is_some())
    }

    pub async fn update_run(&self, r: &IndexRun, etag: &str) -> anyhow::Result<String> {
        self.docs
            .replace(INDEX_RUNS, &r.index_version, &to_value(r)?, etag)
            .await?
            .with_context(|| format!("index run `{}` changed concurrently", r.id))
    }

    // ------------------------------------------------------------------- ops

    /// The published version, mirrored from `current.json` (05 §5.9.1).
    pub async fn current_version(&self) -> anyhow::Result<Option<String>> {
        Ok(self
            .docs
            .get(OPS, "current", "current")
            .await?
            .and_then(|v| v.doc["index_version"].as_str().map(str::to_owned)))
    }

    pub async fn set_current_version(&self, version: &str) -> anyhow::Result<()> {
        let doc = serde_json::json!({
            "id": "current", "kind": "current", "index_version": version,
            "updated_at": Utc::now(),
        });
        self.docs.upsert(OPS, "current", &doc).await
    }

    /// Take the single-writer lock (08 §8.4.1). Fails if another live holder has it.
    pub async fn lock(&self, name: &str, owner: &str, ttl: Duration) -> anyhow::Result<()> {
        let doc = serde_json::json!({
            "id": name, "kind": name, "owner": owner, "until": Utc::now() + ttl,
        });
        if self.docs.create(OPS, name, &doc).await?.is_some() {
            return Ok(());
        }
        let Some(held) = self.docs.get(OPS, name, name).await? else {
            bail!("lock `{name}` vanished while being taken; retry");
        };
        let until: Option<DateTime<Utc>> = serde_json::from_value(held.doc["until"].clone()).ok();
        let holder = held.doc["owner"].as_str().unwrap_or("?");
        if holder != owner && until.is_some_and(|u| u > Utc::now()) {
            bail!(
                "lock `{name}` is held by `{holder}` until {}",
                until.expect("checked")
            );
        }
        match self.docs.replace(OPS, name, &doc, &held.etag).await? {
            Some(_) => Ok(()),
            None => bail!("lock `{name}` was taken concurrently"),
        }
    }

    pub async fn unlock(&self, name: &str, owner: &str) -> anyhow::Result<()> {
        if let Some(held) = self.docs.get(OPS, name, name).await? {
            if held.doc["owner"].as_str() == Some(owner) {
                let doc = serde_json::json!({
                    "id": name, "kind": name, "owner": "", "until": Utc::now(),
                });
                self.docs.replace(OPS, name, &doc, &held.etag).await?;
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::docs::MemoryDocs;

    #[tokio::test]
    async fn writer_lock_is_exclusive_until_it_expires_or_is_released() {
        let s = State::new(Arc::new(MemoryDocs::default()));
        s.lock("quickwit-writer", "a", Duration::hours(2))
            .await
            .unwrap();
        // Re-entrant for the same owner, exclusive for others.
        s.lock("quickwit-writer", "a", Duration::hours(2))
            .await
            .unwrap();
        assert!(s
            .lock("quickwit-writer", "b", Duration::hours(2))
            .await
            .is_err());
        s.unlock("quickwit-writer", "b").await.unwrap();
        assert!(s
            .lock("quickwit-writer", "b", Duration::hours(2))
            .await
            .is_err());
        s.unlock("quickwit-writer", "a").await.unwrap();
        s.lock("quickwit-writer", "b", Duration::seconds(-1))
            .await
            .unwrap();
        // An expired lease can be taken over.
        s.lock("quickwit-writer", "c", Duration::hours(1))
            .await
            .unwrap();
    }
}
