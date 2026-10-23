//! Quickwit backend (ADR-0001): translates the AST into Quickwit's query
//! language and aggregation requests, and queries an explicit set of sealed
//! indexes (08 §8.4.1).
//!
//! Behaviour confirmed against Quickwit 0.9.1 in Spike S-2 is marked `S-2`
//! (05 §5.5.1); `tests/quickwit_parity.rs` checks it against the reference
//! backend.

use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use crate::cache_metrics::{self, CacheReport};
use crate::thread_metrics::{self, ThreadReport};
use crate::{
    BrowseKey, Capabilities, CubeCell, HitsPage, HitsQuery, IndexSet, SearchBackend, SearchError,
    Summary,
};

mod error;
mod query;
mod request;
mod response;
#[cfg(test)]
mod test_support;

pub use error::cause;
pub use query::{
    american_stories_only_string, decade_clause, filter_clauses, hidden_clauses, query_for,
    query_string,
};
pub use request::{
    american_stories_only_request, browse_request, cube_request, hits_request, place_cube_request,
    summary_request,
};
pub use response::{
    check_complete, parse_browse, parse_cube, parse_hits, parse_summary, SearchResponse, StoredHit,
};

use error::{map_err, splits_named};

/// Upper bound on places in one terms aggregation.
const MAX_PLACES: u32 = 5_000;
/// Upper bound on titles: every title (4,693 on loc.gov in October 2026)
/// fits, so the newspaper count is exact (#121).
pub const MAX_PAPERS: u32 = 10_000;
/// Upper bound on title languages (about 50 in the catalog).
const MAX_LANGUAGES: u32 = 200;
/// Limit on reading the searcher's metrics, which it renders from memory.
const METRICS_TIMEOUT: Duration = Duration::from_secs(5);

/// What one read of the searcher's metrics found.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearcherMetrics {
    pub caches: CacheReport,
    pub threads: ThreadReport,
}

impl SearcherMetrics {
    /// Both reports from one Prometheus text.
    pub fn parse(text: &str) -> Self {
        Self {
            caches: cache_metrics::parse(text),
            threads: thread_metrics::parse(text),
        }
    }
}

pub struct QuickwitBackend {
    base_url: String,
    client: reqwest::Client,
    /// Limit on a search request, if different from the client's.
    search_timeout: Option<Duration>,
}

impl QuickwitBackend {
    /// `timeout` limits every request: searches, index lookups and health checks.
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, SearchError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| SearchError::Backend(e.to_string()))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client,
            search_timeout: None,
        })
    }

    /// Give searches their own limit, leaving index lookups and health
    /// checks on the client's.
    pub fn with_search_timeout(mut self, timeout: Duration) -> Self {
        self.search_timeout = Some(timeout);
        self
    }

    /// The searcher's cache, thread pool and runtime metrics, from its
    /// Prometheus endpoint (`GET /metrics`). Never waits longer than
    /// `METRICS_TIMEOUT`.
    pub async fn searcher_metrics(&self) -> Result<SearcherMetrics, SearchError> {
        let resp = self
            .client
            .get(format!("{}/metrics", self.base_url))
            .timeout(METRICS_TIMEOUT)
            .send()
            .await
            .map_err(map_err)?;
        let status = resp.status();
        if !status.is_success() {
            return Err(SearchError::Backend(format!(
                "quickwit /metrics returned {status}"
            )));
        }
        Ok(SearcherMetrics::parse(&resp.text().await.map_err(map_err)?))
    }

    async fn search(
        &self,
        indexes: &IndexSet,
        body: &Value,
    ) -> Result<SearchResponse, SearchError> {
        if indexes.ids().is_empty() {
            return Err(SearchError::Backend(
                "no indexes in the published version".into(),
            ));
        }
        // The query string sent, whose words a failure's cause leaves out.
        let sent = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
        let url = format!(
            "{}/api/v1/{}/search",
            self.base_url,
            indexes.ids().join(",")
        );
        let mut req = self.client.post(url).json(body);
        if let Some(t) = self.search_timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(map_err)?;
        let status = resp.status();
        if !status.is_success() {
            // Never include the response body: Quickwit may echo the query, and
            // query text must not reach logs (09 §9.4.2). Its cause, classified
            // from the body, is safe to log.
            // 408 is Quickwit cancelling a search at its own
            // `searcher.request_timeout_secs`: a timeout, not a refusal.
            if status == reqwest::StatusCode::REQUEST_TIMEOUT {
                return Err(SearchError::Timeout);
            }
            // A failure reading it is the real cause, with its own chain.
            let body = resp.text().await.map_err(map_err)?;
            // With failed splits not allowed (the default), Quickwit answers a
            // split's failure this way, naming the split in the body.
            let msg = format!(
                "quickwit returned {status} ({}){}",
                cause(&body, &sent),
                splits_named(&body, &sent)
            );
            return Err(if status.is_client_error() {
                SearchError::Rejected(msg)
            } else {
                SearchError::Backend(msg)
            });
        }
        let parsed: SearchResponse = resp.json().await.map_err(map_err)?;
        check_complete(&parsed, &sent)?;
        Ok(parsed)
    }
}

#[async_trait]
impl SearchBackend for QuickwitBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            fuzzy: false,
            max_slop: usnm_core::query::MAX_SLOP,
            nested_aggregations: true,
        }
    }

    async fn summary(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
    ) -> Result<Summary, SearchError> {
        let resp = self
            .search(indexes, &summary_request(query, filters, indexes, spec)?)
            .await?;
        parse_summary(&resp, spec)
    }

    async fn cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        let resp = self
            .search(
                indexes,
                &cube_request(query, filters, indexes, spec, shards)?,
            )
            .await?;
        parse_cube(&resp, spec)
    }

    async fn place_cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
        places: &[String],
    ) -> Result<Vec<CubeCell>, SearchError> {
        let resp = self
            .search(
                indexes,
                &place_cube_request(query, filters, indexes, spec, places)?,
            )
            .await?;
        parse_cube(&resp, spec)
    }

    async fn hits(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        page: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        let resp = self
            .search(indexes, &hits_request(query, filters, indexes, page)?)
            .await?;
        parse_hits(resp, query, indexes)
    }

    async fn browse(
        &self,
        indexes: &IndexSet,
        filters: &Filters,
        page: &HitsQuery,
        after: Option<BrowseKey>,
    ) -> Result<HitsPage, SearchError> {
        let resp = self
            .search(indexes, &browse_request(filters, indexes, page, after)?)
            .await?;
        parse_browse(resp, indexes)
    }

    async fn american_stories_only(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
    ) -> Result<u64, SearchError> {
        let resp = self
            .search(
                indexes,
                &american_stories_only_request(query, filters, indexes)?,
            )
            .await?;
        Ok(resp.num_hits)
    }

    /// A read-only searcher reads the metastore manifest once at start, so it
    /// can't find indexes created later by name in a search. Looking an index
    /// up by id loads it from storage (and then it's polled like the rest), so
    /// each index is looked up before its version is served (S-2, 08 §8.4.1).
    async fn prepare(&self, indexes: &IndexSet) -> Result<(), SearchError> {
        for id in indexes.ids() {
            let url = format!("{}/api/v1/indexes/{id}", self.base_url);
            let resp = self.client.get(url).send().await.map_err(map_err)?;
            if !resp.status().is_success() {
                return Err(SearchError::Backend(format!(
                    "index `{id}` is not available: quickwit returned {}",
                    resp.status()
                )));
            }
        }
        Ok(())
    }

    async fn health(&self) -> Result<(), SearchError> {
        let url = format!("{}/health/readyz", self.base_url);
        let resp = self.client.get(url).send().await.map_err(map_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(SearchError::Backend(format!(
                "quickwit readyz returned {}",
                resp.status()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Answers every request with `status` and an empty JSON object.
    async fn stub(status: u16) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\n\
                     content-length: 2\r\nconnection: close\r\n\r\n{{}}"
                );
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        url
    }

    #[tokio::test]
    async fn quickwit_cancelling_a_search_at_its_timeout_is_a_timeout() {
        let qw = QuickwitBackend::new(&stub(408).await, Duration::from_secs(5)).unwrap();
        let err = qw
            .search(&IndexSet::new(vec!["i".into()]), &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::Timeout), "{err}");
    }

    #[tokio::test]
    async fn a_refused_request_is_rejected_and_a_server_error_is_not() {
        for (status, rejected) in [(400, true), (422, true), (500, false), (503, false)] {
            let qw = QuickwitBackend::new(&stub(status).await, Duration::from_secs(5)).unwrap();
            let err = qw
                .search(&IndexSet::new(vec!["i".into()]), &serde_json::json!({}))
                .await
                .unwrap_err();
            assert_eq!(
                matches!(err, SearchError::Rejected(_)),
                rejected,
                "{status}: {err}"
            );
        }
    }
}
