//! Aggregate query planning (05 §5.7): one summary call, then a cube call that
//! is sharded by `place_shard` when it would exceed the engine's bucket limit.
//! The cube is only issued when its upper bound (places with hits × buckets)
//! fits the caller's cell budget, so an oversized request never reaches the
//! engine's aggregation limit.

use futures::stream::{self, StreamExt, TryStreamExt};
use usnm_core::cube::{shard_count, shards_for, PLACE_SHARDS, SHARD_TARGET_BUCKETS};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use crate::{CubeCell, IndexSet, SearchBackend, SearchError, Summary};

/// Cube sub-queries run at most this many at a time.
pub const SHARD_CONCURRENCY: usize = 2;

/// Largest cube one request can compute: every shard at its bucket target.
pub const MAX_SHARDED_BUCKETS: usize = PLACE_SHARDS as usize * SHARD_TARGET_BUCKETS;

pub struct Aggregate {
    pub summary: Summary,
    pub cells: Vec<CubeCell>,
    /// Number of cube calls made (1 = unsharded).
    pub cube_calls: u8,
}

pub enum Planned {
    Complete(Aggregate),
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
        return Ok(Planned::Complete(Aggregate {
            summary,
            cells: Vec::new(),
            cube_calls: 0,
        }));
    }
    let upper_bound = summary.places.len().saturating_mul(spec.len());
    if upper_bound > max_cells.min(MAX_SHARDED_BUCKETS) {
        return Ok(Planned::TooManyCells { upper_bound });
    }
    let n = shard_count(summary.places.len(), spec.len());
    let parts: Vec<Vec<CubeCell>> = stream::iter(0..n)
        .map(|k| {
            let shards = shards_for(k, n);
            async move { backend.cube(indexes, query, filters, spec, &shards).await }
        })
        .buffer_unordered(SHARD_CONCURRENCY)
        .try_collect()
        .await?;
    Ok(Planned::Complete(Aggregate {
        summary,
        cells: parts.into_iter().flatten().collect(),
        cube_calls: n,
    }))
}
