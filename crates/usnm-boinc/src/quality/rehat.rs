//! Both page hats, version 1 (the unit's, on whole-page `cer_sym`) and
//! version 2 (`usnm_ocr_quality::page_hat`, on the passage-matched
//! disagreement), recomputed from a staging root's texts with no results
//! and no scans (#962). This is how a campaign whose units are still staged
//! is compared across the two versions; `collect` writes version 2 into
//! `pages.parquet` itself.
//!
//! Reads `<root>/units/*/{quality.json, texts.json, loc.xml}` (single pages
//! and bundles) and writes one JSON line per page and reader to `--out`.
//! Pages with fewer than `--min-readers` readers, or whose texts are no
//! longer staged, are counted and skipped. Logs counts, never text.
use super::{units, ALTO_FILE, JOB_FILE, TEXTS_FILE};
use crate::read;
use anyhow::Context as _;
use serde::Serialize;
use std::{
    io::Write as _,
    path::{Path, PathBuf},
    sync::Mutex,
};
use usnm_ocr_contract::quality::{
    round6, Job, ReaderSpec, Texts, MAX_ALTO, MAX_BUNDLE_ALTO, MAX_BUNDLE_TEXTS, MAX_JOB_JSON,
    MAX_TEXTS,
};
use usnm_ocr_quality::{align, hat, page_hat, slot};

#[derive(Debug, Clone)]
pub struct Options {
    pub root: PathBuf,
    pub out: PathBuf,
    pub min_readers: usize,
    pub threads: usize,
}

#[derive(Debug, Default, Clone, PartialEq, Serialize)]
pub struct Summary {
    pub units: usize,
    pub pages: usize,
    pub few_readers: usize,
    pub no_texts: usize,
    pub rows: usize,
}

/// One page and reader, both versions.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Row {
    pub campaign: String,
    pub doc_id: String,
    pub reader: String,
    pub readers: String,
    pub chars: usize,
    pub est_cer_v1: Option<f64>,
    pub est_cer_raw_v1: Option<f64>,
    pub residual_v1: Option<f64>,
    pub est_cer_v2: Option<f64>,
    pub est_cer_raw_v2: Option<f64>,
    pub residual_v2: Option<f64>,
    pub matched: Option<f64>,
    pub pairs_v2: Vec<page_hat::PairD>,
}

/// One page's rows: the unit's hat as `report::analyse` solves it, and
/// version 2.
pub fn page(
    campaign: &str,
    doc_id: &str,
    readers: &[ReaderSpec],
    texts: &Texts,
    alto_text: impl FnOnce() -> String,
) -> Vec<Row> {
    let inputs = slot::reader_inputs(readers, texts, alto_text);
    let named: Vec<(&str, &str)> = inputs
        .iter()
        .map(|r| (r.name.as_str(), r.text.as_str()))
        .collect();
    let mut eqs = Vec::new();
    for i in 0..named.len() {
        for j in i + 1..named.len() {
            let p = align::pair(named[i].1, named[j].1);
            if p.chars_a + p.chars_b > 0 {
                eqs.push((i, j, p.cer_sym));
            }
        }
    }
    let one = hat::solve(named.len(), &eqs);
    let two = page_hat::solve(&named);
    let names = named.iter().map(|r| r.0).collect::<Vec<_>>().join(",");
    named
        .iter()
        .enumerate()
        .map(|(i, (name, text))| Row {
            campaign: campaign.to_owned(),
            doc_id: doc_id.to_owned(),
            reader: (*name).to_owned(),
            readers: names.clone(),
            chars: usnm_ocr_quality::text::normalize(text, false)
                .chars()
                .count(),
            est_cer_v1: one.determined.then(|| round6(one.estimates[i])),
            est_cer_raw_v1: one.determined.then(|| round6(one.raw[i])),
            residual_v1: one.determined.then(|| round6(one.residual)),
            est_cer_v2: two.readers[i].est_cer,
            est_cer_raw_v2: two.readers[i].est_cer_raw,
            residual_v2: two.residual,
            matched: two.readers[i].matched,
            pairs_v2: two.pairs.clone(),
        })
        .collect()
}

fn alto_text(bytes: Option<&[u8]>) -> String {
    bytes
        .filter(|b| !b.is_empty())
        .and_then(|b| super::parse_alto(b, 1000.0, 1000.0).ok())
        .map(|p| usnm_alto::text(&p))
        .unwrap_or_default()
}

fn unit_rows(
    campaign: &str,
    unit: &Path,
    min_readers: usize,
    s: &mut Summary,
) -> anyhow::Result<Vec<Row>> {
    let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)
        .with_context(|| format!("{}", unit.display()))?;
    let mut rows = Vec::new();
    let texts_path = unit.join(TEXTS_FILE);
    let alto_path = unit.join(ALTO_FILE);
    if job.pages.is_empty() {
        s.pages += 1;
        if job.readers.len() < min_readers {
            s.few_readers += 1;
            return Ok(rows);
        }
        let Some(texts) = read(&texts_path, MAX_TEXTS)
            .ok()
            .and_then(|b| serde_json::from_slice::<Texts>(&b).ok())
        else {
            s.no_texts += 1;
            return Ok(rows);
        };
        let alto = read(&alto_path, MAX_ALTO).ok();
        rows.extend(page(campaign, &job.doc_id, &job.readers, &texts, || {
            alto_text(alto.as_deref())
        }));
        return Ok(rows);
    }
    let bundle_texts = read(&texts_path, MAX_BUNDLE_TEXTS).unwrap_or_default();
    let bundle_alto = read(&alto_path, MAX_BUNDLE_ALTO).unwrap_or_default();
    for bp in &job.pages {
        s.pages += 1;
        if bp.readers.len() < min_readers {
            s.few_readers += 1;
            continue;
        }
        let Some(texts) = bp
            .inputs
            .texts
            .extract(&bundle_texts, "texts")
            .ok()
            .and_then(|b| serde_json::from_slice::<Texts>(b).ok())
        else {
            s.no_texts += 1;
            continue;
        };
        rows.extend(page(campaign, &bp.doc_id, &bp.readers, &texts, || {
            alto_text(bp.inputs.alto.extract(&bundle_alto, "alto").ok())
        }));
    }
    Ok(rows)
}

pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    let campaign = opts
        .root
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let dir = units(&opts.root);
    let mut unit_dirs: Vec<PathBuf> = std::fs::read_dir(&dir)
        .with_context(|| format!("{}", dir.display()))?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(JOB_FILE).exists())
        .collect();
    unit_dirs.sort();
    let next = Mutex::new(0usize);
    let results: Mutex<Vec<(usize, Vec<Row>)>> = Mutex::new(Vec::new());
    let summary = Mutex::new(Summary::default());
    let failed: Mutex<Option<anyhow::Error>> = Mutex::new(None);
    std::thread::scope(|scope| {
        for _ in 0..opts.threads.max(1) {
            scope.spawn(|| loop {
                let k = {
                    let mut n = next.lock().unwrap();
                    let k = *n;
                    *n += 1;
                    k
                };
                let Some(unit) = unit_dirs.get(k) else {
                    break;
                };
                let mut s = Summary::default();
                match unit_rows(&campaign, unit, opts.min_readers, &mut s) {
                    Ok(rows) => {
                        let mut t = summary.lock().unwrap();
                        t.units += 1;
                        t.pages += s.pages;
                        t.few_readers += s.few_readers;
                        t.no_texts += s.no_texts;
                        t.rows += rows.len();
                        results.lock().unwrap().push((k, rows));
                    }
                    Err(e) => {
                        failed.lock().unwrap().get_or_insert(e);
                        break;
                    }
                }
            });
        }
    });
    if let Some(e) = failed.into_inner().unwrap() {
        return Err(e);
    }
    let mut results = results.into_inner().unwrap();
    results.sort_by_key(|(k, _)| *k);
    let mut out = std::io::BufWriter::new(
        std::fs::File::create(&opts.out).with_context(|| format!("{}", opts.out.display()))?,
    );
    for (_, rows) in results {
        for r in rows {
            serde_json::to_writer(&mut out, &r)?;
            out.write_all(b"\n")?;
        }
    }
    out.flush()?;
    Ok(summary.into_inner().unwrap())
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_ocr_contract::quality::TextReader;

    fn spec(name: &str) -> ReaderSpec {
        ReaderSpec {
            name: name.into(),
            source: name.into(),
        }
    }

    fn texts(rs: &[(&str, &str)]) -> Texts {
        Texts {
            page: None,
            readers: rs
                .iter()
                .map(|(n, t)| TextReader {
                    alto: false,
                    name: (*n).into(),
                    regions: vec![],
                    source: (*n).into(),
                    text: (*t).into(),
                })
                .collect(),
        }
    }

    #[test]
    fn empty_loc_text_takes_the_alto_text() {
        let t = "the steamer arrived from havre with forty passengers aboard";
        let rows = page(
            "c",
            "d",
            &[spec("loc"), spec("as"), spec("ours")],
            &texts(&[("loc", "none"), ("as", t), ("ours", t)]),
            || t.to_owned(),
        );
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].chars, t.chars().count());
        assert_eq!(rows[0].est_cer_v2, Some(0.0));
        assert_eq!(rows[0].est_cer_v1, Some(0.0));
        assert_eq!(rows[0].readers, "loc,as,ours");
    }
}
