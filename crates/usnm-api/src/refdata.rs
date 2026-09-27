//! Published version pointer and its reference snapshot (04 §4.3).
//!
//! `current.json` names the sealed index set and the reference snapshot that
//! were published together; both are immutable, so the API can hot-swap to a
//! new version atomically. The same layout is read from a Blob container (via
//! the managed identity) or a local directory, and every reference file is
//! checked against the snapshot's `manifest.json` before it is used.

use std::collections::HashMap;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_core::time::{day_number, BucketSpec};
use usnm_search::IndexSet;
use usnm_store::{is_safe_segment, ObjectStore};

/// The reference files the API loads from each snapshot.
const FILES: [&str; 3] = ["places.json", "titles.json", "baselines.json"];

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

#[derive(Debug, Deserialize)]
struct Manifest {
    files: Vec<ManifestFile>,
}

#[derive(Debug, Deserialize)]
struct ManifestFile {
    path: String,
    sha256: String,
    bytes: u64,
}

impl RefData {
    /// Load whatever `current.json` names now.
    pub async fn load(store: &dyn ObjectStore) -> Result<Self, String> {
        let current = read_current(store).await?;
        Self::load_for(store, current).await
    }

    /// Load the reference snapshot `current` names, verifying each file
    /// against the snapshot manifest.
    pub async fn load_for(store: &dyn ObjectStore, current: Current) -> Result<Self, String> {
        let dir = &current.reference;
        let manifest: Manifest = parse(
            &format!("{dir}/manifest.json"),
            &fetch(store, &format!("{dir}/manifest.json")).await?,
        )?;
        let mut raw = Vec::with_capacity(FILES.len());
        for name in FILES {
            let path = format!("{dir}/{name}");
            let entry = manifest
                .files
                .iter()
                .find(|f| f.path == name)
                .ok_or_else(|| format!("{dir}/manifest.json does not list {name}"))?;
            let bytes = fetch(store, &path).await?;
            let digest = hex(&Sha256::digest(&bytes));
            if bytes.len() as u64 != entry.bytes || !digest.eq_ignore_ascii_case(&entry.sha256) {
                return Err(format!("{path} does not match its manifest entry"));
            }
            raw.push((path, bytes));
        }
        // Parsing large snapshots is CPU-bound; keep it off the async workers.
        tokio::task::spawn_blocking(move || Self::build(current, &raw))
            .await
            .map_err(|e| e.to_string())?
    }

    fn build(current: Current, raw: &[(String, Vec<u8>)]) -> Result<Self, String> {
        let [(pp, places), (tp, titles), (bp, baselines)] = raw else {
            return Err("reference snapshot is incomplete".into());
        };
        let places: Vec<Place> = parse(pp, places)?;
        let titles: Vec<Title> = parse(tp, titles)?;
        let mut baselines: HashMap<String, Vec<(u32, u32)>> = parse(bp, baselines)?;
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

/// Read only the version pointer. Its ids become object paths and cache
/// prefixes, so each must be a single safe path segment.
pub async fn read_current(store: &dyn ObjectStore) -> Result<Current, String> {
    let current: Current = parse("current.json", &fetch(store, "current.json").await?)?;
    for id in [&current.index_version, &current.reference]
        .into_iter()
        .chain(&current.indexes)
    {
        if !is_safe_segment(id) {
            return Err(format!("current.json: `{id}` is not a valid id"));
        }
    }
    if current.indexes.is_empty() {
        return Err("current.json lists no indexes".into());
    }
    Ok(current)
}

async fn fetch(store: &dyn ObjectStore, path: &str) -> Result<Vec<u8>, String> {
    store
        .get(path)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| format!("{path} not found in {store:?}"))
}

fn parse<T: for<'de> Deserialize<'de>>(path: &str, bytes: &[u8]) -> Result<T, String> {
    serde_json::from_slice(bytes).map_err(|e| format!("{path}: {e}"))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
