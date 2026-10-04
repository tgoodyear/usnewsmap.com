//! Search backends for US News Map (05 §5.4).
//!
//! The API depends only on [`SearchBackend`]. Implementations:
//! - [`quickwit::QuickwitBackend`]: the production engine (ADR-0001).
//! - [`memory::MemoryBackend`]: a brute-force in-memory engine used for local
//!   development, tests, and as the correctness oracle for count checks.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

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
    /// Same-day order for hits: `title ordinal << 32 | edition << 16 | seq`.
    /// Numeric because Quickwit 0.9 can't sort on text fields (05 §5.5).
    pub sort_key: u64,
    /// The batch this copy of the page came from. The fixtures don't have it.
    #[serde(default)]
    pub batch: String,
    pub text: String,
}

/// The sealed indexes a published `index_version` names (08 §8.4.1), and
/// the copies of duplicated pages in them that searches must not see: pages
/// that ship in two batches, whose extra copy an earlier index already held
/// (04 §4.7).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct IndexSet {
    ids: Vec<String>,
    /// Batch → ids of its documents to hide.
    hidden: Arc<BTreeMap<String, BTreeSet<String>>>,
}

impl IndexSet {
    pub fn new(ids: Vec<String>) -> Self {
        Self {
            ids,
            hidden: Arc::default(),
        }
    }

    /// Hide each `(doc_id, batch)`: that batch's copy of the page.
    pub fn hiding(mut self, copies: impl IntoIterator<Item = (String, String)>) -> Self {
        let mut hidden: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
        for (doc_id, batch) in copies {
            hidden.entry(batch).or_default().insert(doc_id);
        }
        self.hidden = Arc::new(hidden);
        self
    }

    pub fn ids(&self) -> &[String] {
        &self.ids
    }

    /// The documents to hide, by batch.
    pub fn hidden(&self) -> &BTreeMap<String, BTreeSet<String>> {
        &self.hidden
    }

    /// Whether `batch`'s copy of `doc_id` is hidden.
    pub fn hides(&self, doc_id: &str, batch: &str) -> bool {
        self.hidden.get(batch).is_some_and(|d| d.contains(doc_id))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlaceSummary {
    pub place_id: String,
    pub hits: u64,
    pub first_day: u32,
    pub last_day: u32,
}

/// Result of the summary call: national series, per-place totals, and first
/// and last appearance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub total_hits: u64,
    /// Earliest and latest matching day; `None` when nothing matches.
    pub first_day: Option<u32>,
    pub last_day: Option<u32>,
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

/// Order of a hit list. Either way, pages on the same day keep the order of
/// `sort_key` (title, edition, page), reversed for newest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HitSort {
    #[default]
    Oldest,
    Newest,
}

impl HitSort {
    pub fn as_str(self) -> &'static str {
        match self {
            HitSort::Oldest => "oldest",
            HitSort::Newest => "newest",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "oldest" => Some(HitSort::Oldest),
            "newest" => Some(HitSort::Newest),
            _ => None,
        }
    }
}

/// Which hits to return. With neither `place_id` nor `lccn`, every match.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HitsQuery {
    pub place_id: Option<String>,
    pub lccn: Option<String>,
    pub sort: HitSort,
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
    /// The backend refused the request itself (a Quickwit 4xx): unlike
    /// `Backend`, trying again won't help.
    #[error("search backend rejected the request: {0}")]
    Rejected(String),
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

    /// Hits by date in `page.sort` order, then by `sort_key` (title, edition,
    /// page), with snippets.
    async fn hits(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        page: &HitsQuery,
    ) -> Result<HitsPage, SearchError>;

    async fn health(&self) -> Result<(), SearchError>;

    /// Make every index in the set searchable before a version goes live, or
    /// fail so the loader keeps serving the previous version.
    async fn prepare(&self, _indexes: &IndexSet) -> Result<(), SearchError> {
        Ok(())
    }
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
