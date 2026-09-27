//! Aggregate query planning (05 §5.7): one summary call, then a cube call that
//! is sharded by `place_shard` when it would exceed the engine's bucket limit.

use futures::stream::{self, StreamExt, TryStreamExt};
use usnm_core::cube::{shard_count, shards_for};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use crate::{CubeCell, IndexSet, SearchBackend, SearchError, Summary};

/// Cube sub-queries run at most this many at a time.
pub const SHARD_CONCURRENCY: usize = 2;

pub struct Aggregate {
    pub summary: Summary,
    pub cells: Vec<CubeCell>,
    /// Number of cube calls made (1 = unsharded).
    pub cube_calls: u8,
}

pub async fn aggregate(
    backend: &dyn SearchBackend,
    indexes: &IndexSet,
    query: &Node,
    filters: &Filters,
    spec: &BucketSpec,
) -> Result<Aggregate, SearchError> {
    let summary = backend.summary(indexes, query, filters, spec).await?;
    if summary.places.is_empty() {
        return Ok(Aggregate {
            summary,
            cells: Vec::new(),
            cube_calls: 0,
        });
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
    Ok(Aggregate {
        summary,
        cells: parts.into_iter().flatten().collect(),
        cube_calls: n,
    })
}
