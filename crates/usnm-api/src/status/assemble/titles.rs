//! Catalog completeness and readiness for release.

use super::Section;
use crate::refdata::RefData;
use schemars::JsonSchema;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use usnm_state::{
    state::MAX_DELTAS,
    summary::{BatchSummary, Summary},
};

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct Catalog {
    pub titles: usize,
    pub places: usize,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
pub struct TitlesPipeline {
    /// Titles with pages in curated batches.
    pub curated_titles: usize,
    /// Of those, titles the catalog doesn't have yet (they wait for titles-sync).
    pub awaiting_sync: Option<usize>,
    /// Curated batches with at least one such title: releases skip them.
    pub batches_waiting_for_titles: Option<usize>,
    /// Curated batches (at their curated version) not in the published version.
    pub unpublished_batches: Option<usize>,
    /// Batches the next release can take: unpublished ones whose titles are
    /// all catalogued, plus re-curated ones when it will be a full release.
    pub ready_for_release: Option<usize>,
    /// Published batches curated again at a newer version: only a full release takes them.
    pub recurated_awaiting_full: Option<usize>,
}

#[derive(Debug, Clone, Serialize, JsonSchema)]
#[schemars(rename = "StatusTitles")]
pub struct Titles {
    /// `catalog/titles.json` and `places.json` (maintained by titles-sync and geocode).
    pub catalog: Section<Catalog>,
    /// Titles and places in the published version.
    pub published_titles: usize,
    pub published_places: usize,
    pub pipeline: Section<TitlesPipeline>,
}

/// Titles in the catalog (for the sync counts).
pub struct CatalogTitles {
    pub lccns: HashSet<String>,
    pub places: usize,
}

pub fn titles(
    rd: &RefData,
    catalog: Option<&CatalogTitles>,
    catalog_reason: &str,
    pipeline: Option<&Summary>,
    pipeline_reason: &str,
) -> Titles {
    let pipeline = match pipeline {
        None => Section::unavailable(pipeline_reason),
        Some(s) => Section::of(titles_pipeline(rd, catalog, s)),
    };
    Titles {
        catalog: match catalog {
            Some(c) => Section::of(Catalog {
                titles: c.lccns.len(),
                places: c.places,
            }),
            None => Section::unavailable(catalog_reason),
        },
        published_titles: rd.titles.len(),
        published_places: rd.places.len(),
        pipeline,
    }
}

fn titles_pipeline(rd: &RefData, catalog: Option<&CatalogTitles>, s: &Summary) -> TitlesPipeline {
    let curated: Vec<&BatchSummary> = s
        .batches
        .iter()
        .filter(|b| b.curated_version.is_some())
        .collect();
    let curated_titles: BTreeSet<&str> = curated
        .iter()
        .flat_map(|b| b.curated_lccns.iter().map(String::as_str))
        .collect();
    let missing =
        |b: &BatchSummary, c: &CatalogTitles| b.curated_lccns.iter().any(|l| !c.lccns.contains(l));
    let awaiting_sync = catalog.map(|c| {
        curated_titles
            .iter()
            .filter(|l| !c.lccns.contains(**l))
            .count()
    });
    let waiting = catalog.map(|c| curated.iter().filter(|b| missing(b, c)).count());
    let published = rd.published_batches.as_ref();
    let next_full = rd.current.indexes.len() > MAX_DELTAS;
    let unpublished: Option<Vec<&&BatchSummary>> = published.map(|p| {
        curated
            .iter()
            .filter(|b| !p.contains_key(&b.batch))
            .collect()
    });
    TitlesPipeline {
        curated_titles: curated_titles.len(),
        awaiting_sync,
        batches_waiting_for_titles: waiting,
        // A full release (after MAX_DELTAS deltas) also takes re-curated batches.
        ready_for_release: match (published, catalog) {
            (Some(p), Some(c)) => Some(
                curated
                    .iter()
                    .filter(|b| match p.get(&b.batch) {
                        None => true,
                        Some(v) => next_full && Some(*v) != b.curated_version,
                    })
                    .filter(|b| !missing(b, c))
                    .count(),
            ),
            _ => None,
        },
        unpublished_batches: unpublished.as_ref().map(Vec::len),
        recurated_awaiting_full: published.map(|p| {
            curated
                .iter()
                .filter(|b| {
                    p.get(&b.batch)
                        .is_some_and(|v| Some(*v) != b.curated_version)
                })
                .count()
        }),
    }
}
