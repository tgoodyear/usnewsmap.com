//! The sparse place × bucket cube (06 §6.3.3).

use serde::Serialize;

/// Hard cap on non-zero cells; above it the API steps to a coarser bucket (ADR-0003).
pub const MAX_CELLS: usize = 700_000;

/// Target maximum buckets per engine aggregation call (05 §5.7).
pub const SHARD_TARGET_BUCKETS: usize = 150_000;

/// Number of `place_shard` values written at index time.
pub const PLACE_SHARDS: u8 = 8;

/// One non-zero cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Cell {
    pub place: u32,
    pub bucket: u32,
    pub hits: u32,
}

/// Columnar sparse cube: `p[i]` (place index), `b[i]` (bucket index), `h[i]` (hits).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct SparseCube {
    pub p: Vec<u32>,
    pub b: Vec<u32>,
    pub h: Vec<u32>,
}

impl SparseCube {
    /// Build from cells, merging duplicates (e.g. from overlapping shards) by summing.
    pub fn from_cells(mut cells: Vec<Cell>) -> Self {
        cells.sort_unstable_by_key(|c| (c.place, c.bucket));
        let mut cube = Self::default();
        for c in cells.into_iter().filter(|c| c.hits > 0) {
            if cube.p.last() == Some(&c.place) && cube.b.last() == Some(&c.bucket) {
                *cube.h.last_mut().expect("non-empty") += c.hits;
            } else {
                cube.p.push(c.place);
                cube.b.push(c.bucket);
                cube.h.push(c.hits);
            }
        }
        cube
    }

    pub fn len(&self) -> usize {
        self.h.len()
    }

    pub fn is_empty(&self) -> bool {
        self.h.is_empty()
    }
}

/// How many shard sub-queries a cube needs, given places with hits and buckets.
pub fn shard_count(places: usize, buckets: usize) -> u8 {
    let cells = places.saturating_mul(buckets);
    let n = cells.div_ceil(SHARD_TARGET_BUCKETS).max(1);
    u8::try_from(n).unwrap_or(PLACE_SHARDS).min(PLACE_SHARDS)
}

/// The `place_shard` values a sub-query `k` of `n` covers (shards are split round-robin).
pub fn shards_for(k: u8, n: u8) -> Vec<u8> {
    (0..PLACE_SHARDS).filter(|s| s % n == k).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merges_duplicate_cells() {
        let cube = SparseCube::from_cells(vec![
            Cell {
                place: 1,
                bucket: 2,
                hits: 3,
            },
            Cell {
                place: 0,
                bucket: 5,
                hits: 1,
            },
            Cell {
                place: 1,
                bucket: 2,
                hits: 4,
            },
            Cell {
                place: 2,
                bucket: 0,
                hits: 0,
            },
        ]);
        assert_eq!(cube.p, vec![0, 1]);
        assert_eq!(cube.b, vec![5, 2]);
        assert_eq!(cube.h, vec![1, 7]);
    }

    #[test]
    fn plans_shards_within_the_engine_limit() {
        assert_eq!(shard_count(100, 200), 1);
        assert_eq!(shard_count(3000, 211), 5);
        assert_eq!(shard_count(10_000, 10_000), 8);
        let all: Vec<u8> = (0..5).flat_map(|k| shards_for(k, 5)).collect();
        let mut sorted = all.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, (0..8).collect::<Vec<_>>());
        assert_eq!(shards_for(0, 1), (0..8).collect::<Vec<_>>());
    }
}
