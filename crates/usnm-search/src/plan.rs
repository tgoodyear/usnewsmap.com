//! Aggregate query planning (05 §5.7): one summary call, then a cube call that
//! is sharded by `place_shard` when it would exceed the engine's bucket limit.
//! The cube is only issued when its upper bound (places with hits × buckets)
//! fits the caller's cell budget, so an oversized request never reaches the
//! engine's aggregation limit. Alongside the cube, two one-hit queries find
//! the earliest and latest matching page.
//!
//! [`days`] plans `/v1/days`: matching pages per day for a few places.

use std::collections::HashMap;

use futures::stream::{self, StreamExt, TryStreamExt};
use usnm_core::cube::{shard_count, shards_for, PLACE_SHARDS, SHARD_TARGET_BUCKETS};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::{day_number, BucketSpec, BucketUnit};

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

/// One place's matching pages per day: `days` ascending (day numbers, as in
/// the index), `hits[i]` pages on `days[i]`, only days with a match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlaceDays {
    pub place_id: String,
    pub days: Vec<u32>,
    pub hits: Vec<u32>,
}

pub enum PlannedDays {
    /// One entry per requested place, in the order requested.
    Complete(Vec<PlaceDays>),
    /// More (place, day) cells than the budget could hold, so no day query
    /// was issued.
    TooManyCells { upper_bound: usize },
}

/// Matching pages per day for `places` (distinct, validated ids). A cube by
/// year comes first: each place-year can hold at most as many day cells as
/// it has pages, and as the year has days in the range, which bounds the
/// day cube. Over `max_cells` nothing more is asked. Otherwise the places
/// are grouped into day cubes of at most [`SHARD_TARGET_BUCKETS`] cells
/// each (a place over that alone gets its own), run two at a time.
pub async fn days(
    backend: &dyn SearchBackend,
    indexes: &IndexSet,
    query: &Node,
    filters: &Filters,
    places: &[String],
    max_cells: usize,
) -> Result<PlannedDays, SearchError> {
    let (from, to) = (filters.from, filters.to);
    let years = BucketSpec::new(BucketUnit::Year, from, to);
    let mut bound: HashMap<&str, usize> = HashMap::new();
    for c in backend
        .place_cube(indexes, query, filters, &years, places)
        .await?
    {
        let i = c.bucket as usize;
        let start = years.bucket_start(i);
        let end = if i + 1 < years.len() {
            years.bucket_start(i + 1).pred_opt().unwrap_or(to)
        } else {
            to
        };
        let days_in_range = (end - start).num_days().max(0) as usize + 1;
        if let Some(id) = places.iter().find(|p| **p == c.place_id) {
            *bound.entry(id.as_str()).or_default() += (c.hits as usize).min(days_in_range);
        }
    }
    let upper_bound: usize = bound.values().sum();
    if upper_bound > max_cells {
        return Ok(PlannedDays::TooManyCells { upper_bound });
    }
    // Places in the order asked, packed into calls of at most the target.
    let mut groups: Vec<(Vec<String>, usize)> = Vec::new();
    for p in places {
        let Some(&n) = bound.get(p.as_str()).filter(|n| **n > 0) else {
            continue;
        };
        match groups.last_mut() {
            Some((ids, cells)) if *cells + n <= SHARD_TARGET_BUCKETS => {
                ids.push(p.clone());
                *cells += n;
            }
            _ => groups.push((vec![p.clone()], n)),
        }
    }
    let spec = BucketSpec::new(BucketUnit::Day, from, to);
    let parts = stream::iter(groups)
        .map(|(ids, _)| {
            let spec = &spec;
            async move {
                backend
                    .place_cube(indexes, query, filters, spec, &ids)
                    .await
            }
        })
        .buffer_unordered(SHARD_CONCURRENCY)
        .try_collect::<Vec<Vec<CubeCell>>>()
        .await?;
    let origin = day_number(from);
    let mut by_place: HashMap<String, Vec<(u32, u32)>> = HashMap::new();
    for c in parts.into_iter().flatten() {
        by_place
            .entry(c.place_id)
            .or_default()
            .push((origin + c.bucket, c.hits));
    }
    Ok(PlannedDays::Complete(
        places
            .iter()
            .map(|p| {
                // Each place is in one call, so each day comes once.
                let mut cells = by_place.remove(p).unwrap_or_default();
                cells.sort_unstable();
                PlaceDays {
                    place_id: p.clone(),
                    days: cells.iter().map(|c| c.0).collect(),
                    hits: cells.iter().map(|c| c.1).collect(),
                }
            })
            .collect(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::MemoryBackend;
    use crate::PageDoc;
    use usnm_core::time::date_from_day;

    fn doc(i: u32, day: u32, place: u8) -> PageDoc {
        PageDoc {
            printed: None,
            ocr_source: None,
            ocr_engine: None,
            doc_id: format!("sn99{i:06}_x_ed-1_seq-1"),
            day,
            ym: 0,
            year: 0,
            place_id: format!("P{place:05}"),
            place_shard: place % 8,
            lccn: format!("sn99{place:06}"),
            state: "GA".into(),
            language: vec!["eng".into()],
            front_page: true,
            edition: 1,
            seq: 1,
            sort_key: 0,
            batch: "b".into(),
            text: if i.is_multiple_of(3) {
                "silver"
            } else {
                "gold"
            }
            .into(),
        }
    }

    fn filters(from: u32, to: u32) -> Filters {
        Filters {
            from: date_from_day(from),
            to: date_from_day(to),
            states: vec![],
            lccns: vec![],
            langs: vec![],
            front_only: false,
        }
    }

    /// Pages on days 71,000–71,799 in places 1–4, several a day.
    fn backend() -> MemoryBackend {
        let mut b = MemoryBackend::new();
        b.add_index(
            "i",
            (0..2_400).map(|i| doc(i, 71_000 + (i * 7) % 800, (i % 4) as u8 + 1)),
        );
        b
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    #[tokio::test]
    async fn days_match_brute_force_in_the_order_asked() {
        let b = backend();
        let idx = IndexSet::new(vec!["i".into()]);
        let q = usnm_core::query::parse("gold").unwrap();
        let f = filters(71_100, 71_650);
        let asked = ids(&["P00003", "P00009", "P00001"]);
        let PlannedDays::Complete(out) = days(&b, &idx, &q, &f, &asked, 700_000).await.unwrap()
        else {
            panic!("over budget")
        };
        assert_eq!(
            out.iter().map(|p| p.place_id.as_str()).collect::<Vec<_>>(),
            ["P00003", "P00009", "P00001"]
        );
        // A place with no matches has empty lists.
        assert!(out[1].days.is_empty() && out[1].hits.is_empty());
        for p in [&out[0], &out[2]] {
            let mut expected: std::collections::BTreeMap<u32, u32> = Default::default();
            for i in 0..2_400u32 {
                let d = doc(i, 71_000 + (i * 7) % 800, (i % 4) as u8 + 1);
                if d.place_id == p.place_id
                    && d.text == "gold"
                    && (71_100..=71_650).contains(&d.day)
                {
                    *expected.entry(d.day).or_default() += 1;
                }
            }
            assert!(!expected.is_empty());
            assert_eq!(p.days, expected.keys().copied().collect::<Vec<_>>());
            assert_eq!(p.hits, expected.values().copied().collect::<Vec<_>>());
        }
    }

    #[tokio::test]
    async fn too_many_day_cells_are_refused_before_the_day_query() {
        let b = backend();
        let idx = IndexSet::new(vec!["i".into()]);
        let q = usnm_core::query::parse("gold").unwrap();
        let f = filters(71_000, 71_799);
        let asked = ids(&["P00001", "P00002"]);
        // Each place has 400 matching pages on 200 days. No year in range
        // has fewer days than pages, so the bound is the pages: 800.
        match days(&b, &idx, &q, &f, &asked, 799).await.unwrap() {
            PlannedDays::TooManyCells { upper_bound } => assert_eq!(upper_bound, 800),
            PlannedDays::Complete(_) => panic!("within budget"),
        }
        let PlannedDays::Complete(out) = days(&b, &idx, &q, &f, &asked, 800).await.unwrap() else {
            panic!("over budget")
        };
        assert_eq!(out[0].days.len(), 200);
        assert_eq!(out[0].hits.iter().sum::<u32>(), 400);
        // Over one day, P00001's two pages there make one cell, not two.
        let one_day = filters(71_000, 71_000);
        match days(&b, &idx, &q, &one_day, &asked, 0).await.unwrap() {
            PlannedDays::TooManyCells { upper_bound } => assert_eq!(upper_bound, 1),
            PlannedDays::Complete(_) => panic!("within budget"),
        }
    }
}
