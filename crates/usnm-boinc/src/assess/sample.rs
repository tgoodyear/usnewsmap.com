//! A stratified random sample over the prefetch manifest.
//!
//! Strata are language × decade × batch prefix × damage bin. Each stratum
//! gets pages in proportion to its size, at least `min_per_stratum` (or
//! all of it, if smaller). "Random" is an order by SHA-256 of the seed and
//! the doc id, so the same manifest and seed give the same sample. Each
//! page carries its weight (stratum pages / drawn pages) for corpus
//! estimates. The crash list is drawn the same way with its own seed,
//! from pages not in the OCR sample, and both files are written in hash
//! order so any prefix of a run is mixed.
use super::SampleRow;
use crate::atomic;
use anyhow::{ensure, Context};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use usnm_ocr_contract::{
    assess::Mode,
    boinc::sha256,
    prefetch::{decode_where, ManifestRow},
};
use usnm_ocr_worker::engine;

#[derive(Debug, Clone)]
pub struct Options {
    pub manifest: PathBuf,
    pub out_ocr: PathBuf,
    pub out_crash: PathBuf,
    pub ocr_pages: usize,
    pub crash_pages: usize,
    pub min_per_stratum: usize,
    pub seed: String,
    /// JSON object: LCCN to the title's MARC language codes, for pages
    /// whose language the damage pass could not detect.
    pub title_languages: Option<PathBuf>,
}

type Key = (String, i32, String, String);

pub fn damage_bin(rate: f64) -> &'static str {
    if rate < 0.1 {
        "low"
    } else if rate < 0.5 {
        "mid"
    } else {
        "high"
    }
}

pub fn prefix(batch: &str) -> &str {
    batch.split('_').next().unwrap_or(batch)
}

fn order(seed: &str, salt: &str, doc_id: &str) -> String {
    sha256(format!("{seed}\0{salt}\0{doc_id}").as_bytes())
}

/// How many pages each stratum gets out of `n`.
pub fn allocate(sizes: &BTreeMap<Key, usize>, n: usize, min: usize) -> BTreeMap<Key, usize> {
    let total: usize = sizes.values().sum();
    sizes
        .iter()
        .map(|(k, &size)| {
            let share = if total == 0 {
                0
            } else {
                ((n as f64) * (size as f64) / (total as f64)).round() as usize
            };
            (k.clone(), share.max(min).min(size))
        })
        .collect()
}

fn draw(
    strata: &BTreeMap<Key, Vec<(ManifestRow, String)>>,
    n: usize,
    min: usize,
    seed: &str,
    salt: &str,
    skip: &BTreeSet<String>,
    mode: Mode,
) -> Vec<(String, SampleRow)> {
    let pools: BTreeMap<Key, Vec<&(ManifestRow, String)>> = strata
        .iter()
        .map(|(k, rows)| {
            let mut v: Vec<_> = rows
                .iter()
                .filter(|(r, _)| !skip.contains(&r.doc_id))
                .collect();
            v.sort_by_cached_key(|(r, _)| order(seed, salt, &r.doc_id));
            (k.clone(), v)
        })
        .filter(|(_, v)| !v.is_empty())
        .collect();
    let sizes = pools.iter().map(|(k, v)| (k.clone(), v.len())).collect();
    let take = allocate(&sizes, n, min);
    let mut out = Vec::new();
    for (k, pool) in &pools {
        let t = take[k];
        let weight = pool.len() as f64 / t as f64;
        for (r, lang) in pool.iter().take(t) {
            out.push((
                order(seed, salt, &r.doc_id),
                SampleRow {
                    batch: r.batch.clone(),
                    batch_version: r.batch_version,
                    damage: k.3.clone(),
                    damage_rate: r.damage_rate,
                    decade: r.decade(),
                    doc_id: r.doc_id.clone(),
                    lang: lang.clone(),
                    mode,
                    prefix: k.2.clone(),
                    weight,
                    year: chrono::Datelike::year(&r.date),
                },
            ));
        }
    }
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

/// Counts written to stderr: what was drawn and what was left out.
#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    pub manifest_pages: usize,
    pub no_language: usize,
    pub no_model: usize,
    pub strata: usize,
    pub ocr: usize,
    pub crash: usize,
}

pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    ensure!(opts.ocr_pages > 0, "--ocr-pages must be positive");
    ensure!(!opts.seed.is_empty(), "--seed must not be empty");
    let titles: BTreeMap<String, Vec<String>> = match &opts.title_languages {
        Some(p) => serde_json::from_slice(&std::fs::read(p)?).context("reading title languages")?,
        None => BTreeMap::new(),
    };
    let installed: BTreeSet<String> = engine::MODELS_LOCK
        .lines()
        .filter(|s| !s.starts_with('#'))
        .filter_map(|s| s.split_whitespace().next())
        .filter_map(|s| s.strip_prefix("tessdata/")?.strip_suffix(".traineddata"))
        .map(str::to_owned)
        .collect();
    let rows = decode_where(bytes::Bytes::from(std::fs::read(&opts.manifest)?), |_| true)?;
    let mut summary = Summary {
        manifest_pages: rows.len(),
        ..Default::default()
    };
    let mut strata: BTreeMap<Key, Vec<(ManifestRow, String)>> = BTreeMap::new();
    for r in rows {
        let empty = vec![];
        let title = titles.get(&r.lccn).unwrap_or(&empty);
        let Some(lang) =
            usnm_ingest::ocr_local::enqueue::message_language(r.lang.as_deref(), title)
        else {
            summary.no_language += 1;
            continue;
        };
        if engine::route(&lang, &installed).is_none() {
            summary.no_model += 1;
            continue;
        }
        let key = (
            lang.clone(),
            r.decade(),
            prefix(&r.batch).to_owned(),
            damage_bin(r.damage_rate).to_owned(),
        );
        strata.entry(key).or_default().push((r, lang));
    }
    summary.strata = strata.len();
    let ocr = draw(
        &strata,
        opts.ocr_pages,
        opts.min_per_stratum,
        &opts.seed,
        "ocr",
        &BTreeSet::new(),
        Mode::Ocr,
    );
    let taken: BTreeSet<String> = ocr.iter().map(|(_, r)| r.doc_id.clone()).collect();
    let crash = if opts.crash_pages == 0 {
        vec![]
    } else {
        draw(
            &strata,
            opts.crash_pages,
            opts.min_per_stratum,
            &opts.seed,
            "crash",
            &taken,
            Mode::Crash,
        )
    };
    summary.ocr = ocr.len();
    summary.crash = crash.len();
    for (path, rows) in [(&opts.out_ocr, &ocr), (&opts.out_crash, &crash)] {
        let mut text = String::new();
        for (_, r) in rows {
            text.push_str(&serde_json::to_string(r)?);
            text.push('\n');
        }
        atomic(path, text.as_bytes())?;
    }
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocation_is_proportional_with_a_floor() {
        let k = |s: &str| (s.to_owned(), 1900, "x".to_owned(), "low".to_owned());
        let sizes = BTreeMap::from([(k("a"), 900), (k("b"), 90), (k("c"), 10), (k("d"), 1)]);
        let got = allocate(&sizes, 100, 3);
        assert_eq!(got[&k("a")], 90);
        assert_eq!(got[&k("b")], 9);
        assert_eq!(got[&k("c")], 3);
        assert_eq!(got[&k("d")], 1);
    }

    #[test]
    fn bins() {
        assert_eq!(damage_bin(0.0), "low");
        assert_eq!(damage_bin(0.3), "mid");
        assert_eq!(damage_bin(0.5), "high");
        assert_eq!(prefix("mdu_kale_ver01"), "mdu");
    }
}
