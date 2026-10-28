//! Project manifest storage and fetch time from sampled scan sizes and pacing.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::bail;
use serde::{Deserialize, Serialize};
use usnm_ocr_contract::prefetch::ManifestRow;

use super::Sidecar;

/// What a sample says about the whole manifest.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Estimate {
    pub resolution: String,
    pub manifest_pages: u64,
    /// Issues in the manifest: one loc.gov API request each.
    pub manifest_issues: u64,
    pub sampled_pages: u64,
    pub sample_bytes_mean: f64,
    pub sample_bytes_median: f64,
    pub sample_bytes_p90: f64,
    pub sample_download_ms_median: f64,
    /// Σ over (language, decade) strata of the stratum's pages times its
    /// sampled mean; strata with no sampled page use the sample's mean.
    pub projected_bytes: f64,
    /// Strata with no sampled page, and their pages.
    pub unsampled_strata: u64,
    pub unsampled_pages: u64,
    /// The API requests alone at their gap.
    pub api_hours: f64,
    /// The image requests at max(gap, mean download time / concurrency),
    /// for concurrency 1 and 2.
    pub image_hours_c1: f64,
    pub image_hours_c2: f64,
    /// By stratum: (pages in the manifest, sampled, sampled mean bytes).
    pub strata: BTreeMap<String, (u64, u64, f64)>,
}

fn quantile(sorted: &[f64], q: f64) -> f64 {
    if sorted.is_empty() {
        return 0.0;
    }
    let i = ((sorted.len() - 1) as f64 * q).round() as usize;
    sorted[i]
}

/// Project the whole manifest's size and fetch time from a sample's index
/// lines. `api_gap` and `tile_gap` are the pacing the full run would use.
pub fn estimate(
    manifest: &[ManifestRow],
    index: &[Sidecar],
    api_gap: Duration,
    tile_gap: Duration,
) -> anyhow::Result<Estimate> {
    let by_id: BTreeMap<&str, &ManifestRow> =
        manifest.iter().map(|r| (r.doc_id.as_str(), r)).collect();
    let stratum = |r: &ManifestRow| format!("{}/{}s", r.lang_or_und(), r.decade());
    let mut sampled: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    let mut sizes = Vec::new();
    let mut downloads = Vec::new();
    let mut resolution = String::new();
    // The latest line per page, if a page was fetched twice.
    let mut latest: BTreeMap<&str, &Sidecar> = BTreeMap::new();
    for s in index {
        latest.insert(&s.doc_id, s);
    }
    for s in latest.values() {
        let Some(r) = by_id.get(s.doc_id.as_str()) else {
            continue;
        };
        if resolution.is_empty() {
            resolution = s.resolution.clone();
        } else if resolution != s.resolution {
            bail!("the index mixes {resolution} and {}", s.resolution);
        }
        sampled.entry(stratum(r)).or_default().push(s.bytes as f64);
        sizes.push(s.bytes as f64);
        downloads.push(s.download_ms as f64);
    }
    if sizes.is_empty() {
        bail!("no index line names a page of the manifest");
    }
    sizes.sort_by(f64::total_cmp);
    downloads.sort_by(f64::total_cmp);
    let mean = sizes.iter().sum::<f64>() / sizes.len() as f64;
    let mut counts: BTreeMap<String, u64> = BTreeMap::new();
    let mut issues = std::collections::BTreeSet::new();
    for r in manifest {
        *counts.entry(stratum(r)).or_default() += 1;
        issues.insert((r.lccn.as_str(), r.date, r.edition));
    }
    let mut e = Estimate {
        resolution,
        manifest_pages: manifest.len() as u64,
        manifest_issues: issues.len() as u64,
        sampled_pages: sizes.len() as u64,
        sample_bytes_mean: mean,
        sample_bytes_median: quantile(&sizes, 0.5),
        sample_bytes_p90: quantile(&sizes, 0.9),
        sample_download_ms_median: quantile(&downloads, 0.5),
        ..Estimate::default()
    };
    for (k, n) in &counts {
        let (m, got) = match sampled.get(k) {
            Some(v) => (v.iter().sum::<f64>() / v.len() as f64, v.len() as u64),
            None => {
                e.unsampled_strata += 1;
                e.unsampled_pages += n;
                (mean, 0)
            }
        };
        e.projected_bytes += *n as f64 * m;
        e.strata.insert(k.clone(), (*n, got, m));
    }
    let dl = downloads.iter().sum::<f64>() / downloads.len() as f64 / 1000.0;
    let per = |c: f64| tile_gap.as_secs_f64().max(dl / c);
    e.api_hours = e.manifest_issues as f64 * api_gap.as_secs_f64() / 3600.0;
    e.image_hours_c1 = e.manifest_pages as f64 * per(1.0) / 3600.0;
    e.image_hours_c2 = e.manifest_pages as f64 * per(2.0) / 3600.0;
    Ok(e)
}
