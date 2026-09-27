//! Search backends for US News Map (05 §5.4).
//!
//! The API depends only on [`SearchBackend`]. Implementations:
//! - [`quickwit::QuickwitBackend`]: the production engine (ADR-0001).
//! - [`memory::MemoryBackend`]: a brute-force in-memory engine used for local
//!   development, tests, and as the correctness oracle for count checks.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

pub mod memory;
pub mod plan;
pub mod quickwit;

/// One indexed page, as stored in the engine (05 §5.5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageDoc {
    pub doc_id: String,
    pub day: u32,
    pub ym: u32,
    pub year: u16,
    pub place_id: String,
    pub place_shard: u8,
    pub lccn: String,
    pub state: String,
    #[serde(default)]
    pub language: Vec<String>,
    pub front_page: bool,
    pub edition: u16,
    pub seq: u16,
    pub text: String,
}

/// The sealed indexes a published `index_version` names (08 §8.4.1).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct IndexSet(pub Vec<String>);

impl IndexSet {
    pub fn ids(&self) -> &[String] {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlaceSummary {
    pub place_id: String,
    pub hits: u64,
    pub first_day: u32,
}

/// Result of the summary call: national series, per-place totals and first appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub total_hits: u64,
    /// One entry per bucket of the request's [`BucketSpec`].
    pub series: Vec<u64>,
    pub places: Vec<PlaceSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CubeCell {
    pub place_id: String,
    pub bucket: u32,
    pub hits: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HitsQuery {
    pub place_id: Option<String>,
    pub lccn: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Hit {
    pub doc_id: String,
    pub day: u32,
    pub lccn: String,
    pub place_id: String,
    pub edition: u16,
    pub seq: u16,
    pub front_page: bool,
    /// HTML-escaped text with matches wrapped in `<mark>`.
    pub snippets: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HitsPage {
    pub total: u64,
    pub hits: Vec<Hit>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Capabilities {
    pub fuzzy: bool,
    pub max_slop: u8,
    pub nested_aggregations: bool,
}

#[derive(Debug, Error)]
pub enum SearchError {
    #[error("not supported by this search backend: {0}")]
    Unsupported(String),
    #[error("search backend timed out")]
    Timeout,
    #[error("search backend error: {0}")]
    Backend(String),
}

#[async_trait]
pub trait SearchBackend: Send + Sync {
    fn capabilities(&self) -> Capabilities;

    async fn summary(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
    ) -> Result<Summary, SearchError>;

    /// Cube cells for places whose `place_shard` is in `shards`.
    async fn cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError>;

    /// Hits sorted by date (then doc id), with snippets.
    async fn hits(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        page: &HitsQuery,
    ) -> Result<HitsPage, SearchError>;

    async fn health(&self) -> Result<(), SearchError>;
}

/// Escape text for HTML and wrap highlighted ranges in `<mark>`.
pub fn mark_html(segments: &[(bool, &str)]) -> String {
    let mut out = String::new();
    for (marked, text) in segments {
        if *marked {
            out.push_str("<mark>");
        }
        for c in text.chars() {
            match c {
                '&' => out.push_str("&amp;"),
                '<' => out.push_str("&lt;"),
                '>' => out.push_str("&gt;"),
                '"' => out.push_str("&quot;"),
                '\'' => out.push_str("&#39;"),
                c => out.push(c),
            }
        }
        if *marked {
            out.push_str("</mark>");
        }
    }
    out
}
