//! The writer node's REST API: its splits, sources, config and merge gauges.

use anyhow::{bail, Context};
use serde_json::Value;

use super::layout::{parse_splits, Split};
use super::INGEST_SOURCE;

/// Splits listed per request.
const PAGE: usize = 1000;

/// `(running, queued)` merge operations, from the node's Prometheus metrics.
/// Quickwit registers the gauges with the first merge, so a missing one is 0.
pub fn merge_gauges(metrics: &str) -> (u64, u64) {
    let gauge = |name: &str| {
        metrics
            .lines()
            .find_map(|line| {
                let value = line.strip_prefix(name)?.strip_prefix(' ')?;
                let v: f64 = value.trim().parse().ok()?;
                (v >= 0.0).then_some(v as u64)
            })
            .unwrap_or(0)
    };
    (
        gauge("quickwit_indexing_ongoing_merge_operations"),
        gauge("quickwit_indexing_pending_merge_operations"),
    )
}

/// The bytes the writer's merges have written, a counter.
const MERGER_WRITE_SERIES: &str = "quickwit_write_bytes{component=\"merger\"}";

/// One series' value from the node's Prometheus metrics, by its name with
/// its labels as Quickwit writes them; `None` until Quickwit registers it.
fn series(metrics: &str, name: &str) -> Option<u64> {
    metrics.lines().find_map(|line| {
        let v: f64 = line
            .strip_prefix(name)?
            .strip_prefix(' ')?
            .trim()
            .parse()
            .ok()?;
        (v >= 0.0).then_some(v as u64)
    })
}

/// What the node's metrics say about its merges.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Gauges {
    pub(super) running: u64,
    pub(super) queued: u64,
    /// The input bytes of the merges queued.
    pub(super) pending_bytes: Option<u64>,
    /// The bytes merges have written since the writer started (a counter).
    pub(super) merged_bytes: Option<u64>,
}

/// The writer node's REST API, for merges and splits.
pub struct Node<'a> {
    pub http: &'a reqwest::Client,
    pub base: &'a str,
}

impl Node<'_> {
    /// Every published split of `index_id`, sorted by id.
    pub async fn splits(&self, index_id: &str) -> anyhow::Result<Vec<Split>> {
        let mut all = Vec::new();
        loop {
            let v: Value = self
                .http
                .get(format!("{}/api/v1/indexes/{index_id}/splits", self.base))
                .query(&[
                    ("split_states", "Published"),
                    ("offset", &all.len().to_string()),
                    ("limit", &PAGE.to_string()),
                ])
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("listing the splits of `{index_id}`"))?
                .json()
                .await?;
            let page = parse_splits(&v)?;
            let n = page.len();
            all.extend(page);
            if n < PAGE {
                break;
            }
        }
        all.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(all)
    }

    pub(super) async fn gauges(&self) -> anyhow::Result<Gauges> {
        let text = self
            .http
            .get(format!("{}/metrics", self.base))
            .send()
            .await?
            .error_for_status()?
            .text()
            .await?;
        let (running, queued) = merge_gauges(&text);
        Ok(Gauges {
            running,
            queued,
            pending_bytes: series(&text, "quickwit_indexing_pending_merge_bytes"),
            merged_bytes: series(&text, MERGER_WRITE_SERIES),
        })
    }

    /// Stop ingesting into `index_id`, which runs its final merges.
    pub async fn close(&self, index_id: &str) -> anyhow::Result<()> {
        self.toggle(index_id, false).await
    }

    /// Enable `index_id`'s ingest source again and have Quickwit start its
    /// pipelines, without adding a document: a new merge pipeline reads the
    /// index's unmerged splits from the metastore.
    ///
    /// Quickwit starts an ingest source's pipelines only while it has shards,
    /// and deletes them once they have been idle for 15 minutes and indexed.
    /// A document the index's strict mapping rejects opens a shard (an empty
    /// one) and is not written.
    pub(super) async fn reopen(&self, index_id: &str) -> anyhow::Result<()> {
        self.toggle(index_id, true).await?;
        let v: Value = self
            .http
            .post(format!("{}/api/v1/{index_id}/ingest", self.base))
            .body(format!("{{\"{REOPEN_FIELD}\": true}}\n"))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("reopening `{index_id}`"))?
            .json()
            .await?;
        let ingested = v["num_ingested_docs"].as_u64();
        if ingested != Some(0) || v["num_rejected_docs"].as_u64() != Some(1) {
            bail!("reopening `{index_id}`: the writer did not reject the empty document: {v}");
        }
        Ok(())
    }

    /// `index_id`'s config, as Quickwit has it.
    pub async fn index_config(&self, index_id: &str) -> anyhow::Result<Value> {
        let v: Value = self
            .http
            .get(format!("{}/api/v1/indexes/{index_id}", self.base))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("reading the config of `{index_id}`"))?
            .json()
            .await?;
        match &v["index_config"] {
            Value::Object(_) => Ok(v["index_config"].clone()),
            _ => bail!("the metadata of `{index_id}` has no index_config"),
        }
    }

    /// Replace `index_id`'s config. Quickwit restarts its indexing pipelines
    /// with it, but not its merge pipelines: those take it when the writer
    /// starts again.
    pub async fn update_index(&self, index_id: &str, config: &Value) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(format!("{}/api/v1/indexes/{index_id}", self.base))
            .json(config)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "updating the config of `{index_id}` returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        Ok(())
    }

    /// Whether the writer's metastore has `index_id`.
    pub async fn has_index(&self, index_id: &str) -> anyhow::Result<bool> {
        let resp = self
            .http
            .get(format!("{}/api/v1/indexes/{index_id}", self.base))
            .send()
            .await?;
        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(false);
        }
        resp.error_for_status()
            .with_context(|| format!("reading the metadata of `{index_id}`"))?;
        Ok(true)
    }

    /// The ids of `index_id`'s sources (`_ingest-api-source`, `kafka`, …).
    pub async fn sources(&self, index_id: &str) -> anyhow::Result<Vec<String>> {
        let v: Value = self
            .http
            .get(format!("{}/api/v1/indexes/{index_id}", self.base))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("reading the metadata of `{index_id}`"))?
            .json()
            .await?;
        Ok(v["sources"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|s| s["source_id"].as_str().map(str::to_owned))
            .collect())
    }

    /// `index_id`'s staged splits created before `before` (seconds since
    /// the epoch), by id: what a writer that stopped in the middle of a
    /// merge or a commit left behind. A merge stages its output before it
    /// replaces its inputs, so such a split holds no document the published
    /// splits don't.
    pub async fn staged_splits_before(
        &self,
        index_id: &str,
        before: i64,
    ) -> anyhow::Result<Vec<String>> {
        let mut out = Vec::new();
        let mut offset = 0usize;
        loop {
            let v: Value = self
                .http
                .get(format!("{}/api/v1/indexes/{index_id}/splits", self.base))
                .query(&[
                    ("split_states", "Staged"),
                    ("offset", &offset.to_string()),
                    ("limit", &PAGE.to_string()),
                ])
                .send()
                .await?
                .error_for_status()
                .with_context(|| format!("listing the staged splits of `{index_id}`"))?
                .json()
                .await?;
            let page = v["splits"]
                .as_array()
                .context("the split list has no `splits`")?;
            for s in page {
                let id = s["split_id"].as_str().context("a split has no id")?;
                let created = s["create_timestamp"]
                    .as_i64()
                    .with_context(|| format!("split `{id}` has no create_timestamp"))?;
                if s["split_state"] == "Staged" && created < before {
                    out.push(id.to_owned());
                }
            }
            offset += page.len();
            if page.len() < PAGE {
                break;
            }
        }
        out.sort();
        Ok(out)
    }

    /// Mark `split_ids` of `index_id` for deletion: they leave the split
    /// lists, and the writer's janitor removes their files.
    pub async fn mark_for_deletion(
        &self,
        index_id: &str,
        split_ids: &[String],
    ) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(format!(
                "{}/api/v1/indexes/{index_id}/splits/mark-for-deletion",
                self.base
            ))
            .json(&serde_json::json!({ "split_ids": split_ids }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "marking splits of `{index_id}` for deletion returned {status}: {}",
                text.chars().take(300).collect::<String>()
            );
        }
        Ok(())
    }

    async fn toggle(&self, index_id: &str, enable: bool) -> anyhow::Result<()> {
        let resp = self
            .http
            .put(format!(
                "{}/api/v1/indexes/{index_id}/sources/{INGEST_SOURCE}/toggle",
                self.base
            ))
            .json(&serde_json::json!({ "enable": enable }))
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!(
                "{} ingest into `{index_id}` returned {status}: {}",
                if enable { "enabling" } else { "disabling" },
                text.chars().take(300).collect::<String>()
            );
        }
        Ok(())
    }
}

/// A field no index mapping has: a document with only this field is
/// rejected (`mode: strict`), which `Node::reopen` relies on.
const REOPEN_FIELD: &str = "usnm_reopen";

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[test]
    fn reads_the_merge_gauges() {
        let metrics = "# HELP quickwit_indexing_ongoing_merge_operations Number of ongoing merge operations\n\
                       # TYPE quickwit_indexing_ongoing_merge_operations gauge\n\
                       quickwit_indexing_ongoing_merge_operations 1\n\
                       quickwit_indexing_pending_merge_operations 3\n\
                       quickwit_indexing_pending_merge_bytes 12345\n";
        assert_eq!(merge_gauges(metrics), (1, 3));
        assert_eq!(
            merge_gauges(
                "quickwit_indexing_pending_merge_operations 2\nquickwit_indexing_ongoing_merge_operations 0.0\n"
            ),
            (0, 2)
        );
        // A labelled series isn't the total; before the first merge there
        // are no gauges at all.
        assert_eq!(
            merge_gauges("quickwit_indexing_ongoing_merge_operations{x=\"y\"} 2\n"),
            (0, 0)
        );
        assert_eq!(merge_gauges(""), (0, 0));
        assert_eq!(
            series(metrics, "quickwit_indexing_pending_merge_bytes"),
            Some(12345)
        );
        let written = "quickwit_write_bytes{component=\"split_downloader_merge\"} 7\n\
                       quickwit_write_bytes{component=\"merger\"} 1048576\n";
        assert_eq!(series(written, MERGER_WRITE_SERIES), Some(1 << 20));
        assert_eq!(series("", MERGER_WRITE_SERIES), None);
    }

    /// The splits a stopped writer left staged are the ones created before
    /// this writer started; one it staged since is its own merge's.
    #[tokio::test]
    async fn only_splits_staged_before_the_writer_started_are_left_over() {
        let marked: Arc<Mutex<Vec<Value>>> = Arc::default();
        let app = axum::Router::new()
            .route(
                "/api/v1/indexes/{id}/splits",
                axum::routing::get(|q: axum::extract::RawQuery| async move {
                    let q = q.0.unwrap_or_default();
                    assert!(q.contains("split_states=Staged"), "{q}");
                    axum::Json(serde_json::json!({"splits": [
                        {"split_id": "old", "split_state": "Staged", "create_timestamp": 99},
                        {"split_id": "new", "split_state": "Staged", "create_timestamp": 100},
                    ]}))
                }),
            )
            .route(
                "/api/v1/indexes/{id}/splits/mark-for-deletion",
                axum::routing::put({
                    let marked = marked.clone();
                    move |axum::Json(v): axum::Json<Value>| async move {
                        marked.lock().unwrap().push(v);
                        axum::Json(Value::Null)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();
        let base = format!("http://{addr}");
        let node = Node {
            http: &http,
            base: &base,
        };
        let left = node.staged_splits_before("i", 100).await.unwrap();
        assert_eq!(left, ["old"]);
        node.mark_for_deletion("i", &left).await.unwrap();
        assert_eq!(
            *marked.lock().unwrap(),
            [serde_json::json!({"split_ids": ["old"]})]
        );
    }
}
