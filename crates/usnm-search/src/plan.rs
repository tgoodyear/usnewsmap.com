//! Aggregate query planning (05 §5.7): one summary call, then a cube call that
//! is sharded by `place_shard` when it would exceed the engine's bucket limit.
//! The cube is only issued when its upper bound (places with hits × buckets)
//! fits the caller's cell budget, so an oversized request never reaches the
//! engine's aggregation limit. Alongside the cube, two one-hit queries find
//! the earliest and latest matching page.

use futures::stream::{self, StreamExt, TryStreamExt};
use usnm_core::cube::{shard_count, shards_for, PLACE_SHARDS, SHARD_TARGET_BUCKETS};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use crate::{CubeCell, Hit, HitSort, HitsQuery, IndexSet, SearchBackend, SearchError, Summary};

/// Cube sub-queries run at most this many at a time.
pub const SHARD_CONCURRENCY: usize = 2;

/// Largest cube one request can compute: every shard at its bucket target.
pub const MAX_SHARDED_BUCKETS: usize = PLACE_SHARDS as usize * SHARD_TARGET_BUCKETS;

pub struct Aggregate {
    pub summary: Summary,
    pub cells: Vec<CubeCell>,
    /// Number of cube calls made (1 = unsharded).
    pub cube_calls: u8,
    /// The earliest and latest matching page; `None` when nothing matches.
    pub first: Option<Hit>,
    pub last: Option<Hit>,
}

pub enum Planned {
    Complete(Box<Aggregate>),
    /// The cube's upper bound exceeds the budget, so no cube query was issued;
    /// the caller should retry with coarser buckets or reject the request.
    TooManyCells {
        upper_bound: usize,
    },
}

pub async fn aggregate(
    backend: &dyn SearchBackend,
    indexes: &IndexSet,
    query: &Node,
    filters: &Filters,
    spec: &BucketSpec,
    max_cells: usize,
) -> Result<Planned, SearchError> {
    let summary = backend.summary(indexes, query, filters, spec).await?;
    if summary.places.is_empty() {
        return Ok(Planned::Complete(Box::new(Aggregate {
            summary,
            cells: Vec::new(),
            cube_calls: 0,
            first: None,
            last: None,
        })));
    }
    let upper_bound = summary.places.len().saturating_mul(spec.len());
    if upper_bound > max_cells.min(MAX_SHARDED_BUCKETS) {
        return Ok(Planned::TooManyCells { upper_bound });
    }
    let n = shard_count(summary.places.len(), spec.len());
    let cube = stream::iter(0..n)
        .map(|k| {
            let shards = shards_for(k, n);
            async move { backend.cube(indexes, query, filters, spec, &shards).await }
        })
        .buffer_unordered(SHARD_CONCURRENCY)
        .try_collect::<Vec<Vec<CubeCell>>>();
    let edges = futures::future::try_join(
        edge(backend, indexes, query, filters, HitSort::Oldest),
        edge(backend, indexes, query, filters, HitSort::Newest),
    );
    let (parts, (first, last)) = futures::future::try_join(cube, edges).await?;
    Ok(Planned::Complete(Box::new(Aggregate {
        summary,
        cells: parts.into_iter().flatten().collect(),
        cube_calls: n,
        first,
        last,
    })))
}

/// The first hit in `sort` order: the earliest or latest matching page.
async fn edge(
    backend: &dyn SearchBackend,
    indexes: &IndexSet,
    query: &Node,
    filters: &Filters,
    sort: HitSort,
) -> Result<Option<Hit>, SearchError> {
    let page = HitsQuery {
        sort,
        limit: 1,
        ..HitsQuery::default()
    };
    let hits = backend.hits(indexes, query, filters, &page).await?;
    Ok(hits.hits.into_iter().next())
}
