//! Published version pointer and its reference snapshot (04 §4.3).
//!
//! `current.json` names the sealed index set and the reference snapshot that
//! were published together; both are immutable, so the API can hot-swap to a
//! new version atomically. This loader reads a local directory; reading the
//! same layout from Blob Storage via managed identity is the next increment.

use std::collections::HashMap;
use std::path::Path;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use usnm_core::time::{day_number, BucketSpec};
use usnm_search::IndexSet;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Bounds {
    pub from: NaiveDate,
    pub to: NaiveDate,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Current {
    pub index_version: String,
    pub indexes: Vec<String>,
    /// Reference snapshot directory name.
    pub reference: String,
    pub bounds: Bounds,
    pub published_at: String,
    #[serde(default)]
    pub synthetic: bool,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Place {
    pub id: String,
    pub ordinal: u32,
    pub name: String,
    pub state: String,
    pub lat: f64,
    pub lon: f64,
    pub precision: String,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Title {
    pub lccn: String,
    pub name: String,
    pub place_id: String,
    pub state: String,
    #[serde(default)]
    pub languages: Vec<String>,
}

/// Everything the API needs in memory for one published version.
#[derive(Debug)]
pub struct RefData {
    pub current: Current,
    pub places: Vec<Place>,
    pub place_index: HashMap<String, usize>,
    pub titles: HashMap<String, Title>,
    /// Pages published per place, as sorted `(day, pages)`.
    pub baselines: HashMap<String, Vec<(u32, u32)>>,
}

impl RefData {
    pub fn load(dir: &Path) -> Result<Self, String> {
        let current: Current = read_json(&dir.join("current.json"))?;
        let snap = dir.join(&current.reference);
        let places: Vec<Place> = read_json(&snap.join("places.json"))?;
        let titles: Vec<Title> = read_json(&snap.join("titles.json"))?;
        let mut baselines: HashMap<String, Vec<(u32, u32)>> =
            read_json(&snap.join("baselines.json"))?;
        for series in baselines.values_mut() {
            series.sort_unstable();
        }
        let place_index = places
            .iter()
            .enumerate()
            .map(|(i, p)| (p.id.clone(), i))
            .collect();
        Ok(Self {
            current,
            places,
            place_index,
            titles: titles.into_iter().map(|t| (t.lccn.clone(), t)).collect(),
            baselines,
        })
    }

    pub fn version(&self) -> &str {
        &self.current.index_version
    }

    pub fn index_set(&self) -> IndexSet {
        IndexSet(self.current.indexes.clone())
    }

    pub fn bounds(&self) -> (NaiveDate, NaiveDate) {
        (self.current.bounds.from, self.current.bounds.to)
    }

    pub fn place(&self, id: &str) -> Option<&Place> {
        self.place_index.get(id).map(|&i| &self.places[i])
    }

    /// Pages published per bucket for one place (only places matching `states`, if any).
    pub fn place_baseline(&self, place_id: &str, spec: &BucketSpec) -> Vec<u64> {
        let mut out = vec![0u64; spec.len()];
        let (from, to) = (day_number(spec.from), day_number(spec.to));
        if let Some(series) = self.baselines.get(place_id) {
            let start = series.partition_point(|(d, _)| *d < from);
            for &(day, pages) in series[start..].iter().take_while(|(d, _)| *d <= to) {
                out[spec.index_of_day(day)] += u64::from(pages);
            }
        }
        out
    }

    /// National pages published per bucket, restricted to `states` when non-empty.
    pub fn national_baseline(&self, spec: &BucketSpec, states: &[String]) -> Vec<u64> {
        let mut out = vec![0u64; spec.len()];
        for p in self
            .places
            .iter()
            .filter(|p| states.is_empty() || states.contains(&p.state))
        {
            for (o, v) in out.iter_mut().zip(self.place_baseline(&p.id, spec)) {
                *o += v;
            }
        }
        out
    }
}

fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
    serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}
