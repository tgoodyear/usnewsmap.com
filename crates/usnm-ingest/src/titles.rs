//! `titles-sync` and `geocode` (04 §4.6): the working catalog
//! (`catalog/titles.json`, `catalog/places.json`) from LoC's title records.
//!
//! - **titles-sync** fetches `https://www.loc.gov/item/{lccn}/?fo=json` for
//!   every title that has pages (LoC's batch listing names them) and isn't
//!   cached yet, keeping the fields we use in `raw/titles.json`. Then it
//!   runs `geocode`. Requests are paced under loc.gov's JSON API limit (20 a
//!   minute; exceeding it blocks for an hour, and any request during the
//!   block starts the hour again). LoC also limits below that when it is
//!   busy: in prod, a 429 or CAPTCHA page came after about 340 requests at
//!   about 13 a minute. With a deadline, the sync then sends nothing for 65
//!   minutes and resumes 1.5 times slower; without one, or when the pause
//!   would pass it, the run stops early. The cache keeps what was fetched,
//!   so the next run continues. A title takes about 4.5 s (LoC's response
//!   time, over the 3.5 s pace), so ~3,100 titles take about 4 hours, plus
//!   an hour for each block. A few titles have no record under the LCCN
//!   their pages ship with, but one under another LCCN:
//!   `catalog/overrides/titles.json` names it, and the sync fetches that
//!   record for the title instead.
//! - **geocode** builds the catalog from that cache alone, so it is
//!   deterministic and needs no network. A title's place is the city in its
//!   LoC title, e.g. "(North Platte, Neb.)", in the state LoC lists at the
//!   city's position (or `catalog/overrides/title-places.json`); spellings of one town share a
//!   place ([`crate::places`]: generic variants, `catalog/overrides/place-aliases.json`,
//!   and "X City" next to "X"). Coordinates are, in order: a manual override
//!   (`catalog/overrides/places.json` in git); the median of LoC's
//!   `latlong` on records that list only this city (unless it is more than
//!   100 km from the state's only gazetteer town of the name); the US gazetteer; the
//!   median of LoC's `latlong` on records that list several cities (one
//!   point for all of them, so a last resort); else the state's centroid
//!   (precision `state`).
//!
//! Ordinals and place ids are stable: records already in the catalog keep
//! them, new ones get the next free number, and nothing is ever removed
//! (published snapshots keep their own copies, 04 §4.7). Merged places keep
//! the id most of their titles had; the others stay, named by no title.

use std::collections::BTreeMap;

use anyhow::Context;
use serde::{Deserialize, Serialize};
use usnm_store::ObjectStore;

use crate::catalog::{Place, Title, PLACES, TITLES};

mod geocode;
mod sync;

pub use geocode::{
    build, geocode, place_overrides, GeocodeReport, PlaceOverride, CITY_SUFFIX_KM, DISAGREE_KM,
    FAR_KM,
};
pub use sync::{
    parse_item, record_lccns, sync, title_records, Pacing, SyncReport, TitleOverride,
    LOC_BLOCK_PAUSE, LOC_INTERVAL,
};

/// LoC's item endpoint; a title record is `{base}/{lccn}/?fo=json`.
pub const LOC_ITEMS: &str = "https://www.loc.gov/item";
/// The fields of every fetched title record, by LCCN.
pub const RAW: &str = "raw/titles.json";
/// Manual coordinates, `[{added, city, lat, lon, name?, precision?, reason, ref, source, state}]`
/// (with the audit fields of [`crate::audit`]), kept in git
/// (`catalog/overrides/places.json`) and compiled in, so every environment
/// applies the same corrections (08 §8.9).
pub const PLACE_OVERRIDES: &str = include_str!("../../../catalog/overrides/places.json");
/// Titles whose LoC record is under another LCCN,
/// `{lccn: {added, reason, record, ref, source}}` (with the audit fields of
/// [`crate::audit`]),
/// kept in git (`catalog/overrides/titles.json`) and compiled in like the
/// place overrides.
pub const TITLE_OVERRIDES: &str = include_str!("../../../catalog/overrides/titles.json");

/// What `geocode` uses from a LoC title record.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RawTitle {
    pub lccn: String,
    /// e.g. "The North Platte Tribune (North Platte, Neb.) 1890-1894".
    pub title: String,
    #[serde(default)]
    pub cities: Vec<String>,
    #[serde(default)]
    pub states: Vec<String>,
    #[serde(default)]
    pub latlong: Option<[f64; 2]>,
    #[serde(default)]
    pub dates: Option<String>,
    #[serde(default)]
    pub languages: Vec<String>,
    /// The LCCN of the record this came from, when it isn't the title's own
    /// (`catalog/overrides/titles.json`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub record: Option<String>,
}

fn valid_lccn(lccn: &str) -> bool {
    usnm_core::ids::PageKey::new(lccn, chrono::NaiveDate::MIN, 1, 1).is_ok()
}

async fn load_raw(store: &dyn ObjectStore) -> anyhow::Result<BTreeMap<String, RawTitle>> {
    Ok(match store.get(RAW).await? {
        Some(b) => serde_json::from_slice(&b).context(RAW)?,
        None => BTreeMap::new(),
    })
}

async fn save_raw(store: &dyn ObjectStore, raw: &BTreeMap<String, RawTitle>) -> anyhow::Result<()> {
    store
        .put(RAW, serde_json::to_vec(raw)?, "application/json")
        .await?;
    Ok(())
}

/// The working catalog as stored, or empty before the first sync.
async fn load_catalog(store: &dyn ObjectStore) -> anyhow::Result<(Vec<Title>, Vec<Place>)> {
    let titles = match store.get(TITLES).await? {
        Some(b) => serde_json::from_slice(&b).context(TITLES)?,
        None => vec![],
    };
    let places = match store.get(PLACES).await? {
        Some(b) => serde_json::from_slice(&b).context(PLACES)?,
        None => vec![],
    };
    Ok((titles, places))
}
