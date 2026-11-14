//! Join both apps' canonical results per page into the campaign's tables.
//!
//! Reads the unit directories under the two staging roots and the spools
//! stock `batch_collect_assimilator` writes (`<workunit>__file_<open
//! name>`), and writes under `audit/quality/<campaign>/` in the curated
//! store (or `--out-dir`):
//!
//! - `pages.parquet`: one row per page per reader: the reader's page-level
//!   record (estimate, completeness, precision, findability, the pairs as
//!   JSON) with the page's features and the job's group and language.
//! - `crops.parquet`: one row per crop per reader, for pages whose
//!   `usnm_qwen` unit has returned: the reader's words inside the crop box
//!   (LoC's from `loc.xml`, a reader marked `alto` from `ours.xml`, an
//!   American Stories region's legibility where one overlaps the box), the
//!   vision-language reading, the CER against it and the Dice, and the
//!   crop-level hat over every reader with text in the box plus the
//!   vision-language reader.
//! - `summary.md`: the campaign in one table.
//!
//! A `usnm_qwen` reading joins only where the pairs that agreed on its
//! canonical result vouch for it. With `--qwen-crop-states`, the directory
//! `qwen compare --crop-states` writes, a result an agreeing pair judged
//! under `crop-consensus-v2` keeps only the crops that pair marked `agree`:
//! the others (looped, no text, or not judged) keep their rows without the
//! reading, and `vlm_crop_state` says why. A result no agreeing pair judged
//! crop by crop was accepted by the strict rule, which vouches for every
//! crop, as before.
//!
//! With `--qwen-pages`, the directory `qwen compare --pages` writes, a
//! bundle unit with no canonical result yet (its pair disagreed on some
//! page, or BOINC gave up on it) joins each page two hosts agreed on, from
//! that pair's record, as `vlm_crop_state` `page-strict-v1` (#1435). A
//! canonical result, when there is one, is joined instead.
//!
//! [`crop_rows`] is the crop join on its own, so `estimate` runs it
//! offline on the same inputs.
use super::{parse_alto, units, validate, ALTO_FILE, JOB_FILE, PAGE_FILE, TEXTS_FILE};
use crate::{atomic, read};
use anyhow::Context;
use arrow_array::{
    builder::{BooleanBuilder, Float64Builder, Int64Builder, StringBuilder},
    ArrayRef, RecordBatch,
};
use arrow_schema::{DataType, Field, Schema};
use image::GenericImageView as _;
use parquet::{
    arrow::ArrowWriter,
    basic::{Compression, ZstdLevel},
    file::properties::WriterProperties,
};
use std::{
    collections::BTreeMap,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};
use usnm_alto::Page;
use usnm_ocr_contract::{
    quality::{
        BundlePage, Job, PageResult, ResultManifest, Texts, MAX_ALTO, MAX_BUNDLE_ALTO,
        MAX_BUNDLE_SCAN, MAX_BUNDLE_TEXTS, MAX_JOB_JSON, MAX_SCAN, MAX_TEXTS,
    },
    qwen::{self, Crop, Readings},
};
use usnm_ocr_quality::{align, crops, hat, page_hat, report::round6};

/// Our ALTO of the page, when a reader of `texts.json` is marked `alto`.
pub const OURS_FILE: &str = "ours.xml";
/// The vision-language reader's name in the crop tables.
pub const VLM: &str = "vlm";
/// Surya's name in the crop tables, read from its crop units (#968).
pub const SURYA: &str = crate::surya::collect::CROP_READER;

#[derive(Debug, Clone)]
pub struct Options {
    pub campaign: String,
    /// The `usnm_quality` staging root and spool.
    pub root: PathBuf,
    pub spool: PathBuf,
    /// The `usnm_qwen` staging root and spool.
    pub qwen_root: PathBuf,
    pub qwen_spool: PathBuf,
    /// Where batch-108 (`XIAOMI_BATCH`) results are: Xiaomi-OCR-0 units' canonical
    /// results. A Xiaomi-pinned job reads only from here and every other job only from
    /// `qwen_spool` (batch 8), so a page's reading never comes from the other reader (#1776).
    /// Without it a Xiaomi job has no result yet.
    pub qwen_xiaomi_spool: Option<PathBuf>,
    /// Where `qwen compare --crop-states` keeps each judged pair's crop
    /// states. Without it every canonical crop joins, as the strict rule has it.
    pub qwen_crop_states: Option<PathBuf>,
    /// Qwen units and crops kept out of the tables until re-read
    /// (`quality.excluded_qwen_readings`). A unit's crops still join the page; only its
    /// readings are left out, so no `vlm` row carries them.
    pub qwen_exclusions: super::exclusions::Exclusions,
    /// Where `qwen compare --pages` keeps each bundle page's pair records.
    /// With it, a bundle unit with no canonical result joins the readings of
    /// each page two hosts agreed on (#1435); without it only canonical
    /// results join.
    pub qwen_pages: Option<PathBuf>,
    /// The `usnm_surya` staging root and spool (#968): its crop units'
    /// texts join `crops.parquet` as the reader `surya`.
    pub surya_root: Option<PathBuf>,
    pub surya_spool: Option<PathBuf>,
    /// A local directory instead of the curated store.
    pub out_dir: Option<PathBuf>,
    pub curated_url: Option<String>,
    /// Content-addressed row parts; unchanged units avoid repeating page-hat computation.
    pub cache: Option<PathBuf>,
    /// Maximum dirty canonical units computed before refusing an incomplete publication.
    pub max_dirty_units: usize,
    /// Start no further dirty unit once the run has taken this long. A unit already started
    /// finishes. The deferral is reported like `max_dirty_units`, so a pass running under the
    /// server's step lock can bound how long it holds it and leave the rest to the next one.
    pub max_compute: Option<std::time::Duration>,
    /// Compute and store row parts only, publishing nothing. Cached units are not read back and
    /// the end-of-run checks that the step lock keeps still are skipped, so this is safe
    /// without the lock: a part is keyed on the hashes of its inputs, which are checked again
    /// after the rows are computed, and a part keyed on inputs that were rewritten meanwhile is
    /// never used by a later publication. Needs `cache`.
    pub warm_only: bool,
    /// The quality measure's resource files (`<lang>.qe`, pinned by
    /// `ocr/qe/resources.lock.json`): every crop text of a page of a language
    /// with one is measured into `crops.parquet`'s `qe_*` columns. Pages carry
    /// the unit's own measure whatever this is.
    pub qe_dir: Option<PathBuf>,
}

/// The exclusions in force in `quality.excluded_qwen_readings`, read with the DSN file given or
/// the one `USNM_QUALITY_DSN_FILE` names. With neither, none (the collector then runs where
/// the database is out of reach). A DSN that cannot be read is an error, never "no exclusions".
pub async fn load_exclusions(
    dsn_file: Option<&Path>,
) -> anyhow::Result<super::exclusions::Exclusions> {
    use usnm_ocr_local::quality_db::QualityDb;
    let db = match dsn_file {
        Some(path) => Some(QualityDb::from_file(&path.to_string_lossy())?),
        None => QualityDb::from_env()?,
    };
    let Some(db) = db else {
        return Ok(Default::default());
    };
    let exclusions = super::exclusions::Exclusions::from_rows(db.excluded_qwen_readings().await?);
    let (whole, partial) = exclusions.counts();
    eprintln!("qwen exclusions: {whole} units excluded whole, {partial} with crops excluded");
    Ok(exclusions)
}

/// What a collection did besides its rows, for the operator and the driver.
#[derive(Debug, Default, Clone, PartialEq, serde::Serialize)]
pub struct Progress {
    /// Units whose rows were computed (and cached) by this run.
    pub dirty: usize,
    /// Whether a unit was left for a later run by `max_dirty_units` or `max_compute`.
    pub deferred: bool,
    /// Seconds reading every Qwen and Surya dependency into the cache's inventory.
    pub inventory_secs: f64,
    /// Seconds keying, loading and computing units.
    pub units_secs: f64,
    /// Seconds re-keying every unit before publication.
    pub verify_secs: f64,
}

#[derive(Debug, Default, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Summary {
    /// Pages whose `usnm_quality` result is in.
    pub pages: usize,
    /// Of them, pages whose `usnm_qwen` readings are in too.
    pub with_readings: usize,
    /// Of them, pages with at least one Surya crop text in.
    pub with_surya: usize,
    /// Submitted units with no canonical result yet.
    pub pending: usize,
    /// Units BOINC gave up on.
    pub failed: usize,
    pub page_rows: usize,
    pub crop_rows: usize,
    /// Pages whose rows keep the unit's version 1 hat: the texts were not
    /// readable, or LoC's text needs the ALTO's and no ALTO is staged.
    pub hat_v1_kept: usize,
}

/// One row of `crops.parquet`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CropRow {
    pub doc_id: String,
    pub job_id: Option<String>,
    pub source: Option<String>,
    pub crop_id: String,
    pub stratum: String,
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
    pub reader: String,
    /// The reader's words in the box; none when the reader has no boxes.
    pub text: Option<String>,
    pub vlm_text: Option<String>,
    pub vlm_model: Option<String>,
    pub vlm_device: Option<String>,
    /// None in older exports, whose null text may mean missing or completed empty.
    /// False too for a reading no agreeing pair vouched for.
    pub vlm_reading_present: Option<bool>,
    /// Why the reading joins or not: `strict-v1` (the strict rule accepted
    /// the whole result), `page-strict-v1` (two hosts agreed on this page
    /// of a bundle with no canonical result, `--qwen-pages`), `agree` (an
    /// agreeing pair vouched for this crop under `crop-consensus-v2`), or the
    /// crop's best state when none did (`looped`, `no_text`, `unjudged`).
    /// None without a reading, in older exports, and offline.
    pub vlm_crop_state: Option<String>,
    pub vlm_reference_chars: Option<usize>,
    pub vlm_reference_words: Option<usize>,
    /// The reader as the hypothesis against the vision-language reading.
    pub cer_vs_vlm: Option<f64>,
    pub dice_vs_vlm: Option<f64>,
    /// The crop-level hat's estimate for this reader.
    pub hat_est_cer: Option<f64>,
    pub hat_readers: usize,
    /// A region of the reader's text overlaps the box.
    pub region_overlap: bool,
    pub region_kind: Option<String>,
    pub region_legibility: Option<String>,
    /// The quality measure of `text`, with `collect --qe-dir`.
    #[serde(flatten)]
    pub qe: QeColumns,
}

/// The reference-free quality measure of a reader's text (`usnm_core::qe`,
/// `ocr/qe/README.md`) as table columns: in `pages.parquet` the unit's own
/// (`ReaderReport::qe`), in `crops.parquet` the server's with
/// `collect --qe-dir`. All null without one. `qe_version` is the measure's
/// version and `qe_resources` the sha256 of the resource file.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct QeColumns {
    #[serde(default)]
    pub qe_version: Option<String>,
    #[serde(default)]
    pub qe_resources: Option<String>,
    #[serde(default)]
    pub qe_words: Option<u64>,
    #[serde(default)]
    pub qe_known3: Option<u64>,
    #[serde(default)]
    pub qe_chars: Option<u64>,
    #[serde(default)]
    pub qe_no_words: Option<bool>,
    #[serde(default)]
    pub qe_oov: Option<f64>,
    #[serde(default)]
    pub qe_charlm_bpc: Option<f64>,
    #[serde(default)]
    pub qe_lm_oov: Option<f64>,
}

impl QeColumns {
    pub fn of(r: Option<&usnm_ocr_contract::quality::QeReport>) -> Self {
        let Some(r) = r else {
            return Self::default();
        };
        Self {
            qe_version: Some(r.version.clone()),
            qe_resources: Some(r.resources.clone()),
            qe_words: Some(r.words),
            qe_known3: Some(r.known3),
            qe_chars: Some(r.chars),
            qe_no_words: Some(r.no_words),
            qe_oov: r.oov,
            qe_charlm_bpc: r.charlm_bpc,
            qe_lm_oov: r.lm_oov,
        }
    }
}

/// What the crop join needs for one page.
pub struct CropInputs<'a> {
    pub doc_id: &'a str,
    pub job_id: &'a str,
    /// LoC's ALTO in page pixels.
    pub loc: &'a Page,
    /// Our ALTO in page pixels, when a reader is marked `alto`.
    pub ours: Option<&'a Page>,
    pub texts: &'a Texts,
    /// The reader order of the job.
    pub readers: &'a [String],
    pub crops: &'a [Crop],
    pub readings: Option<&'a Readings>,
    /// Which crops of `readings` the agreeing pairs vouched for; None joins
    /// every crop and says nothing about it.
    pub vouch: Option<&'a crate::qwen::consensus::Vouch>,
    /// Surya's read of each crop, by crop id, from the crop units.
    pub surya: Option<&'a BTreeMap<String, crate::surya::collect::CropText>>,
    /// The quality measure of the page's language, to measure every crop
    /// text with (`collect --qe-dir`).
    pub qe: Option<&'a usnm_ocr_quality::qe::Measure>,
}

/// One reader's text in a crop box and the region of its text that
/// overlaps the box: `(name, text, overlap, kind, legibility)`.
type ReaderInBox = (String, Option<String>, bool, Option<String>, Option<String>);

fn overlaps(a: [u32; 4], b: [u32; 4]) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

/// The rows of `crops.parquet` for one page: every reader against the
/// vision-language reading, and the crop-level hat.
pub fn crop_rows(inputs: &CropInputs) -> Vec<CropRow> {
    let readings: BTreeMap<&str, &qwen::Reading> = inputs
        .readings
        .map(|r| r.readings.iter().map(|x| (x.id.as_str(), x)).collect())
        .unwrap_or_default();
    let mut rows = Vec::new();
    for crop in inputs.crops {
        let vouched = inputs.vouch.is_none_or(|v| v.vouches(&crop.id));
        let reading = readings.get(crop.id.as_str()).filter(|_| vouched);
        let vlm_crop_state = inputs
            .readings
            .and(inputs.vouch)
            .map(|v| v.label(&crop.id).to_owned());
        let vlm = reading.filter(|r| !r.text.trim().is_empty());
        let reference_counts = reading.map(|v| {
            let normalized = usnm_ocr_quality::text::normalize(&v.text, false);
            (
                normalized.chars().count(),
                align::bag(&normalized).values().sum::<usize>(),
            )
        });
        // Each reader's text in the box, where it has boxes.
        let mut texts: Vec<ReaderInBox> = Vec::new();
        for name in inputs.readers {
            let spec = inputs.texts.readers.iter().find(|r| &r.name == name);
            let text = if name == "loc" {
                Some(crops::text_in_box(inputs.loc, crop.rect))
            } else if spec.is_some_and(|s| s.alto) {
                inputs.ours.map(|p| crops::text_in_box(p, crop.rect))
            } else {
                None
            };
            let region = spec.and_then(|s| s.regions.iter().find(|g| overlaps(g.rect, crop.rect)));
            texts.push((
                name.clone(),
                text,
                region.is_some(),
                region.and_then(|g| g.kind.clone()),
                region.and_then(|g| g.legibility.clone()),
            ));
        }
        // Surya's read of the crop, from its crop unit, when it read it.
        // Crop ids are ordinal (`c01`, ...), so the unit's box and stratum
        // must be this crop's: a Surya campaign cut with another seed or
        // ALTO is not joined.
        let surya = inputs
            .surya
            .and_then(|m| m.get(crop.id.as_str()))
            .filter(|c| c.rect == crop.rect && c.stratum == crop.stratum)
            .filter(|c| !c.text.trim().is_empty());
        // The crop-level hat over the readers with text in the box, plus
        // the vision-language reading and Surya's.
        let mut hat_members: Vec<(&str, &str)> = texts
            .iter()
            .filter_map(|(n, t, _, _, _)| t.as_deref().map(|t| (n.as_str(), t)))
            .collect();
        if let Some(v) = vlm {
            hat_members.push((VLM, &v.text));
        }
        if let Some(c) = surya {
            hat_members.push((SURYA, &c.text));
        }
        let mut pairs = Vec::new();
        for i in 0..hat_members.len() {
            for j in i + 1..hat_members.len() {
                pairs.push((
                    i,
                    j,
                    align::pair(hat_members[i].1, hat_members[j].1).cer_sym,
                ));
            }
        }
        let h = hat::solve(hat_members.len(), &pairs);
        let est = |name: &str| -> Option<f64> {
            h.determined
                .then(|| {
                    hat_members
                        .iter()
                        .position(|(n, _)| *n == name)
                        .map(|i| round6(h.estimates[i]))
                })
                .flatten()
        };
        let base = |reader: String| CropRow {
            doc_id: inputs.doc_id.to_owned(),
            job_id: Some(inputs.job_id.to_owned()),
            source: match reader.as_str() {
                VLM => inputs.readings.map(|r| format!("qwen:{}", r.job_id)),
                SURYA => surya.map(|c| format!("surya:whole:{}", c.job_id)),
                _ => inputs
                    .texts
                    .readers
                    .iter()
                    .find(|r| r.name == reader)
                    .map(|r| r.source.clone()),
            },
            crop_id: crop.id.clone(),
            stratum: crop.stratum.clone(),
            x0: crop.rect[0],
            y0: crop.rect[1],
            x1: crop.rect[2],
            y1: crop.rect[3],
            reader,
            text: None,
            vlm_text: reading.map(|v| v.text.clone()),
            vlm_model: inputs.readings.and_then(|r| r.model.clone()),
            vlm_device: inputs.readings.map(|r| r.device.clone()),
            vlm_reading_present: Some(reading.is_some()),
            vlm_crop_state: vlm_crop_state.clone(),
            vlm_reference_chars: reference_counts.map(|(chars, _)| chars),
            vlm_reference_words: reference_counts.map(|(_, words)| words),
            cer_vs_vlm: None,
            dice_vs_vlm: None,
            hat_est_cer: None,
            hat_readers: hat_members.len(),
            region_overlap: false,
            region_kind: None,
            region_legibility: None,
            qe: QeColumns::default(),
        };
        let measured = |text: Option<&str>| {
            QeColumns::of(inputs.qe.zip(text).map(|(m, t)| m.report(t)).as_ref())
        };
        for (name, text, overlap, kind, legibility) in &texts {
            let mut row = base(name.clone());
            row.text = text.clone();
            row.region_overlap = *overlap;
            row.region_kind = kind.clone();
            row.region_legibility = legibility.clone();
            row.qe = measured(text.as_deref());
            if let (Some(t), Some(v)) = (text, vlm) {
                let p = align::pair(t, &v.text);
                row.cer_vs_vlm = Some(round6(p.cer_a));
                row.dice_vs_vlm = Some(round6(p.dice));
            }
            row.hat_est_cer = est(name);
            rows.push(row);
        }
        if let Some(v) = vlm {
            let mut row = base(VLM.into());
            row.text = Some(v.text.clone());
            row.qe = measured(Some(&v.text));
            row.cer_vs_vlm = Some(0.0);
            row.dice_vs_vlm = Some(1.0);
            row.hat_est_cer = est(VLM);
            rows.push(row);
        }
        if let Some(c) = surya {
            let mut row = base(SURYA.into());
            row.text = Some(c.text.clone());
            row.qe = measured(Some(&c.text));
            if let Some(v) = vlm {
                let p = align::pair(&c.text, &v.text);
                row.cer_vs_vlm = Some(round6(p.cer_a));
                row.dice_vs_vlm = Some(round6(p.dice));
            }
            row.hat_est_cer = est(SURYA);
            rows.push(row);
        }
    }
    rows
}

/// One row of `pages.parquet`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct PageRow {
    pub doc_id: String,
    pub group: String,
    pub language: String,
    pub job_id: String,
    pub outcome: String,
    pub reader: String,
    pub source: String,
    pub chars: usize,
    pub words: usize,
    pub est_cer: Option<f64>,
    pub est_cer_raw: Option<f64>,
    pub completeness: Option<f64>,
    pub precision: Option<f64>,
    pub findable_content: Option<f64>,
    pub findable_places: Option<f64>,
    pub content_found: usize,
    pub content_total: usize,
    pub places_found: usize,
    pub places_total: usize,
    pub cer_vs_loc: Option<f64>,
    pub wer_vs_loc: Option<f64>,
    pub dice_vs_loc: Option<f64>,
    pub word_ratio_vs_loc: Option<f64>,
    pub damage_rate: Option<f64>,
    pub function_share: Option<f64>,
    pub garbage_share: Option<f64>,
    pub numeric_share: Option<f64>,
    pub short_line_share: Option<f64>,
    pub fraktur_score: Option<f64>,
    pub ink_word_coverage: Option<f64>,
    pub head_words: Option<u64>,
    pub near_words: Option<u64>,
    pub known_words: Option<u64>,
    pub detected_lang: Option<String>,
    pub lang_decision: Option<String>,
    pub pairs_json: String,
    pub hat_determined: Option<bool>,
    pub hat_residual: Option<f64>,
    pub legibility_ceiling: Option<f64>,
    pub consensus_words: Option<usize>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub skew_deg: Option<f64>,
    pub blur: Option<f64>,
    pub contrast: Option<f64>,
    pub noise: Option<f64>,
    pub bleed_through: Option<f64>,
    pub ink_density: Option<f64>,
    pub bg_tile_std: Option<f64>,
    pub otsu_local_diff: Option<f64>,
    pub img_columns: Option<usize>,
    pub gutter_clarity: Option<f64>,
    pub est_line_height_px: Option<f64>,
    pub vertical_stroke_ratio: Option<f64>,
    pub blocks: Option<usize>,
    pub lines: Option<usize>,
    pub line_words: Option<usize>,
    pub line_height_px: Option<f64>,
    pub columns: Option<usize>,
    pub mean_word_conf: Option<f64>,
    pub total_ms: u64,
    pub plan_class: Option<String>,
    pub with_readings: bool,
    /// Which hat `est_cer`, `est_cer_raw`, `hat_determined`, `hat_residual`
    /// and `legibility_ceiling` are: 2 (`usnm_ocr_quality::page_hat`,
    /// passage-matched) where the page's texts were read, else 1 (the
    /// unit's whole-page hat). A table without this column is 1.
    pub hat_version: u32,
    /// The unit's own (version 1) values, whatever `hat_version` is.
    pub est_cer_v1: Option<f64>,
    pub est_cer_raw_v1: Option<f64>,
    pub hat_determined_v1: Option<bool>,
    pub hat_residual_v1: Option<f64>,
    pub legibility_ceiling_v1: Option<f64>,
    /// Version 2: the mean share of this reader's characters placed in the
    /// other readers' texts (coverage, not error).
    pub matched: Option<f64>,
    /// Version 2: the page's pairs (`page_hat::PairD`) as JSON; empty for 1.
    pub hat_pairs_json: String,
    /// The unit's quality measure of the reader's text, when its job
    /// carried the language's resource file.
    #[serde(flatten)]
    pub qe: QeColumns,
}

fn compute_ink_word_coverage(
    words: usize,
    img: Option<&usnm_ocr_contract::quality::ImageFeatures>,
    lay: Option<&usnm_ocr_contract::quality::LayoutFeatures>,
) -> Option<f64> {
    let i = img?;
    if i.width == 0 || i.height == 0 || i.ink_density <= 0.0 {
        return None;
    }
    let lh = i
        .est_line_height_px
        .or_else(|| lay.and_then(|l| l.line_height_px))?
        .max(6.0);
    let ink_area = f64::from(i.width) * f64::from(i.height) * i.ink_density;
    let expected_words = (ink_area / (lh * lh)).max(1.0);
    Some(round6(words as f64 / expected_words))
}

fn loc_pair_metrics(
    x: &usnm_ocr_contract::quality::ReaderReport,
) -> (Option<f64>, Option<f64>, Option<f64>, Option<f64>) {
    if x.name == "loc" || x.name == "loc-hist" {
        return (
            Some(0.0),
            Some(0.0),
            Some(1.0),
            (x.words > 0).then_some(1.0),
        );
    }
    if let Some(p) = x.pairs.get("loc").or_else(|| x.pairs.get("loc-hist")) {
        let ratio = (p.words_b > 0).then(|| round6(p.words_a as f64 / p.words_b as f64));
        (Some(p.cer_a), Some(p.wer_a), Some(p.dice), ratio)
    } else {
        (None, None, None, None)
    }
}

/// Enriches `PageRow` entries with intrinsic single-reader lexical and garbage
/// damage metrics (`usnm_core::damage`) computed from each reader's text in `texts`.
pub fn enrich_rows_with_damage(rows: &mut [PageRow], texts: &Texts, loc_fallback: Option<&str>) {
    for row in rows {
        let mut text_opt = texts
            .readers
            .iter()
            .find(|r| r.name == row.reader)
            .map(|r| r.text.as_str());
        if (row.reader == "loc" || row.reader == "loc-hist")
            && text_opt.is_none_or(|t| t.trim().is_empty() || t.trim().eq_ignore_ascii_case("none"))
        {
            if let Some(fb) = loc_fallback {
                text_opt = Some(fb);
            }
        }
        let Some(text) = text_opt else {
            continue;
        };
        let marc = usnm_core::lang::iso3_to_marc(&row.language).unwrap_or(row.language.as_str());
        let iso3 = usnm_core::lang::marc_to_iso3(&row.language).unwrap_or(row.language.as_str());
        let scored = usnm_core::damage::score(text, &[marc]);
        let forced = usnm_core::damage::score_forced(text, iso3);
        row.damage_rate = forced
            .and_then(|f| f.damage_rate)
            .or(scored.damage_rate)
            .map(round6);
        row.function_share = forced
            .and_then(|f| f.function_share)
            .or(scored.function_share)
            .map(round6);
        row.garbage_share = scored.garbage_share.map(round6);
        row.numeric_share = scored.numeric_share.map(round6);
        row.short_line_share = scored.short_line_share.map(round6);
        row.fraktur_score = forced
            .and_then(|f| f.fraktur_score)
            .or(scored.fraktur_score)
            .map(round6);
        row.head_words = forced
            .map(|f| f.damage.head)
            .or_else(|| scored.damage.map(|d| d.head));
        row.near_words = forced
            .map(|f| f.damage.near)
            .or_else(|| scored.damage.map(|d| d.near));
        row.known_words = forced.map(|f| f.known_words);
        row.detected_lang = Some(scored.lang);
        row.lang_decision = Some(scored.decision.as_str().to_owned());
    }
}

/// Replaces the rows' hat with version 2 (`page_hat`), solved on the
/// passage-matched disagreement over the page's texts as the unit read
/// them (`slot::reader_inputs`, with LoC's ALTO text from `alto_text` for an
/// empty LoC text). The unit's values stay in the `*_v1` columns. When a
/// LoC reader needs the ALTO text and `alto_text` has none, the unit's
/// texts can't be rebuilt: the rows keep version 1 and this returns false.
pub fn apply_hat_v2(
    rows: &mut [PageRow],
    readers: &[usnm_ocr_contract::quality::ReaderSpec],
    texts: &Texts,
    alto_text: impl FnOnce() -> Option<String>,
) -> bool {
    let Some(inputs) = usnm_ocr_quality::slot::reader_inputs(readers, texts, alto_text) else {
        return false;
    };
    let named: Vec<(&str, &str)> = inputs
        .iter()
        .map(|r| (r.name.as_str(), r.text.as_str()))
        .collect();
    let h = page_hat::solve(&named);
    let pairs_json = serde_json::to_string(&h.pairs).unwrap_or_default();
    for row in rows {
        let Some(x) = h.readers.iter().find(|x| x.name == row.reader) else {
            continue;
        };
        row.hat_version = page_hat::VERSION;
        row.est_cer = x.est_cer;
        row.est_cer_raw = x.est_cer_raw;
        row.matched = x.matched;
        row.hat_determined = Some(h.determined);
        row.hat_residual = h.residual;
        row.legibility_ceiling = h.ceiling_est;
        row.hat_pairs_json = pairs_json.clone();
    }
    true
}

/// LoC's ALTO text, for the unit's empty-text fallback; none when the ALTO
/// is missing, empty (truncated after submission) or does not parse.
pub fn alto_text_of(bytes: Option<&[u8]>) -> Option<String> {
    bytes
        .filter(|b| !b.is_empty())
        .and_then(|b| parse_alto(b, 1000.0, 1000.0).ok())
        .map(|p| usnm_alto::text(&p))
}

/// The rows of `pages.parquet` for one result.
pub fn page_rows(job: &Job, r: &ResultManifest, with_readings: bool) -> Vec<PageRow> {
    let img = r.page.as_ref().and_then(|p| p.image.as_ref());
    let lay = r.page.as_ref().and_then(|p| p.layout.as_ref());
    r.readers
        .iter()
        .map(|x| {
            let (cer_vs_loc, wer_vs_loc, dice_vs_loc, word_ratio_vs_loc) = loc_pair_metrics(x);
            let ink_word_coverage = compute_ink_word_coverage(x.words, img, lay);
            PageRow {
                doc_id: job.doc_id.clone(),
                group: job.group.clone(),
                language: job.language.clone(),
                job_id: job.id.clone(),
                outcome: format!("{:?}", r.outcome).to_lowercase(),
                reader: x.name.clone(),
                source: x.source.clone(),
                chars: x.chars,
                words: x.words,
                est_cer: x.est_cer,
                est_cer_raw: x.est_cer_raw,
                completeness: x.completeness,
                precision: x.precision,
                findable_content: x.findable_content,
                findable_places: x.findable_places,
                content_found: x.content_found,
                content_total: x.content_total,
                places_found: x.places_found,
                places_total: x.places_total,
                cer_vs_loc,
                wer_vs_loc,
                dice_vs_loc,
                word_ratio_vs_loc,
                damage_rate: None,
                function_share: None,
                garbage_share: None,
                numeric_share: None,
                short_line_share: None,
                fraktur_score: None,
                ink_word_coverage,
                head_words: None,
                near_words: None,
                known_words: None,
                detected_lang: None,
                lang_decision: None,
                pairs_json: serde_json::to_string(&x.pairs).unwrap_or_default(),
                hat_determined: r.hat.as_ref().map(|h| h.determined),
                hat_residual: r.hat.as_ref().and_then(|h| h.residual),
                legibility_ceiling: r.legibility.as_ref().and_then(|l| l.ceiling_est),
                consensus_words: r.legibility.as_ref().map(|l| l.consensus_words),
                width: img.map(|i| i.width),
                height: img.map(|i| i.height),
                skew_deg: img.map(|i| i.skew_deg),
                blur: img.map(|i| i.blur),
                contrast: img.map(|i| i.contrast),
                noise: img.map(|i| i.noise),
                bleed_through: img.map(|i| i.bleed_through),
                ink_density: img.map(|i| i.ink_density),
                bg_tile_std: img.and_then(|i| i.bg_tile_std),
                otsu_local_diff: img.and_then(|i| i.otsu_local_diff),
                img_columns: img.and_then(|i| i.img_columns),
                gutter_clarity: img.and_then(|i| i.gutter_clarity),
                est_line_height_px: img.and_then(|i| i.est_line_height_px),
                vertical_stroke_ratio: img.and_then(|i| i.vertical_stroke_ratio),
                blocks: lay.map(|l| l.blocks),
                lines: lay.map(|l| l.lines),
                line_words: lay.map(|l| l.words),
                line_height_px: lay.and_then(|l| l.line_height_px),
                columns: lay.map(|l| l.columns),
                mean_word_conf: lay.and_then(|l| l.mean_word_conf),
                total_ms: r.timings.total_ms,
                plan_class: r.host.plan_class.clone(),
                with_readings,
                hat_version: 1,
                est_cer_v1: x.est_cer,
                est_cer_raw_v1: x.est_cer_raw,
                hat_determined_v1: r.hat.as_ref().map(|h| h.determined),
                hat_residual_v1: r.hat.as_ref().and_then(|h| h.residual),
                legibility_ceiling_v1: r.legibility.as_ref().and_then(|l| l.ceiling_est),
                matched: None,
                hat_pairs_json: String::new(),
                qe: QeColumns::of(x.qe.as_ref()),
            }
        })
        .collect()
}

/// The rows of `pages.parquet` for one page of a multi-page bundle result.
pub fn bundle_page_rows(
    job: &Job,
    bp: &BundlePage,
    pr: &PageResult,
    plan_class: Option<String>,
    with_readings: bool,
) -> Vec<PageRow> {
    let img = pr.page.as_ref().and_then(|p| p.image.as_ref());
    let lay = pr.page.as_ref().and_then(|p| p.layout.as_ref());
    pr.readers
        .iter()
        .map(|x| {
            let (cer_vs_loc, wer_vs_loc, dice_vs_loc, word_ratio_vs_loc) = loc_pair_metrics(x);
            let ink_word_coverage = compute_ink_word_coverage(x.words, img, lay);
            PageRow {
                doc_id: bp.doc_id.clone(),
                group: bp.group.clone(),
                language: bp.language.clone(),
                job_id: job.id.clone(),
                outcome: format!("{:?}", pr.outcome).to_lowercase(),
                reader: x.name.clone(),
                source: x.source.clone(),
                chars: x.chars,
                words: x.words,
                est_cer: x.est_cer,
                est_cer_raw: x.est_cer_raw,
                completeness: x.completeness,
                precision: x.precision,
                findable_content: x.findable_content,
                findable_places: x.findable_places,
                content_found: x.content_found,
                content_total: x.content_total,
                places_found: x.places_found,
                places_total: x.places_total,
                cer_vs_loc,
                wer_vs_loc,
                dice_vs_loc,
                word_ratio_vs_loc,
                damage_rate: None,
                function_share: None,
                garbage_share: None,
                numeric_share: None,
                short_line_share: None,
                fraktur_score: None,
                ink_word_coverage,
                head_words: None,
                near_words: None,
                known_words: None,
                detected_lang: None,
                lang_decision: None,
                pairs_json: serde_json::to_string(&x.pairs).unwrap_or_default(),
                hat_determined: pr.hat.as_ref().map(|h| h.determined),
                hat_residual: pr.hat.as_ref().and_then(|h| h.residual),
                legibility_ceiling: pr.legibility.as_ref().and_then(|l| l.ceiling_est),
                consensus_words: pr.legibility.as_ref().map(|l| l.consensus_words),
                width: img.map(|i| i.width),
                height: img.map(|i| i.height),
                skew_deg: img.map(|i| i.skew_deg),
                blur: img.map(|i| i.blur),
                contrast: img.map(|i| i.contrast),
                noise: img.map(|i| i.noise),
                bleed_through: img.map(|i| i.bleed_through),
                ink_density: img.map(|i| i.ink_density),
                bg_tile_std: img.and_then(|i| i.bg_tile_std),
                otsu_local_diff: img.and_then(|i| i.otsu_local_diff),
                img_columns: img.and_then(|i| i.img_columns),
                gutter_clarity: img.and_then(|i| i.gutter_clarity),
                est_line_height_px: img.and_then(|i| i.est_line_height_px),
                vertical_stroke_ratio: img.and_then(|i| i.vertical_stroke_ratio),
                blocks: lay.map(|l| l.blocks),
                lines: lay.map(|l| l.lines),
                line_words: lay.map(|l| l.words),
                line_height_px: lay.and_then(|l| l.line_height_px),
                columns: lay.map(|l| l.columns),
                mean_word_conf: lay.and_then(|l| l.mean_word_conf),
                total_ms: pr.timings.total_ms,
                plan_class: plan_class.clone(),
                with_readings,
                hat_version: 1,
                est_cer_v1: x.est_cer,
                est_cer_raw_v1: x.est_cer_raw,
                hat_determined_v1: pr.hat.as_ref().map(|h| h.determined),
                hat_residual_v1: pr.hat.as_ref().and_then(|h| h.residual),
                legibility_ceiling_v1: pr.legibility.as_ref().and_then(|l| l.ceiling_est),
                matched: None,
                hat_pairs_json: String::new(),
                qe: QeColumns::of(x.qe.as_ref()),
            }
        })
        .collect()
}
/// A column of a table being built: every column is nullable.
enum Column {
    Str(Vec<Option<String>>),
    F64(Vec<Option<f64>>),
    I64(Vec<Option<i64>>),
    Bool(Vec<Option<bool>>),
}

/// Rows of serialisable records to Parquet: each field of the record is a
/// column typed by its JSON value (string, number, bool), numbers that are
/// all integers as `Int64`, others as `Float64`. Field order is the
/// struct's.
pub fn to_parquet<T: serde::Serialize>(rows: &[T]) -> anyhow::Result<Vec<u8>> {
    to_parquet_typed(rows, QE_TYPES)
}

/// The quality-measure columns keep their types when every value is null
/// (no `--qe-dir`, units staged without the file, readings with no words).
const QE_TYPES: &[(&str, DataType)] = &[
    ("qe_version", DataType::Utf8),
    ("qe_resources", DataType::Utf8),
    ("qe_words", DataType::Int64),
    ("qe_known3", DataType::Int64),
    ("qe_chars", DataType::Int64),
    ("qe_no_words", DataType::Boolean),
    ("qe_oov", DataType::Float64),
    ("qe_charlm_bpc", DataType::Float64),
    ("qe_lm_oov", DataType::Float64),
];

fn crop_parquet(rows: &[CropRow]) -> anyhow::Result<Vec<u8>> {
    let types: Vec<(&str, DataType)> = QE_TYPES
        .iter()
        .cloned()
        .chain([
            ("vlm_reference_chars", DataType::Int64),
            ("vlm_reference_words", DataType::Int64),
            ("vlm_reading_present", DataType::Boolean),
        ])
        .collect();
    to_parquet_typed(rows, &types)
}

fn to_parquet_typed<T: serde::Serialize>(
    rows: &[T],
    types: &[(&str, DataType)],
) -> anyhow::Result<Vec<u8>> {
    let values: Vec<serde_json::Map<String, serde_json::Value>> = rows
        .iter()
        .map(|r| match serde_json::to_value(r)? {
            serde_json::Value::Object(o) => Ok(o),
            _ => anyhow::bail!("a row is not an object"),
        })
        .collect::<anyhow::Result<_>>()?;
    let names: Vec<String> = values
        .first()
        .map(|o| o.keys().cloned().collect())
        .unwrap_or_default();
    let mut columns: Vec<(String, Column)> = Vec::new();
    for name in &names {
        let cells: Vec<&serde_json::Value> = values.iter().map(|o| &o[name]).collect();
        let hint = types.iter().find(|(n, _)| *n == name).map(|(_, t)| t);
        let col = if hint == Some(&DataType::Int64) {
            Column::I64(
                cells
                    .iter()
                    .map(|v| {
                        if v.is_null() {
                            Ok(None)
                        } else {
                            v.as_i64()
                                .map(Some)
                                .with_context(|| format!("column {name} must fit Int64"))
                        }
                    })
                    .collect::<anyhow::Result<_>>()?,
            )
        } else if hint == Some(&DataType::Float64) {
            Column::F64(
                cells
                    .iter()
                    .map(|v| {
                        if v.is_null() {
                            Ok(None)
                        } else {
                            v.as_f64()
                                .map(Some)
                                .with_context(|| format!("column {name} must be Float64"))
                        }
                    })
                    .collect::<anyhow::Result<_>>()?,
            )
        } else if hint == Some(&DataType::Utf8) {
            Column::Str(
                cells
                    .iter()
                    .map(|v| {
                        if v.is_null() {
                            Ok(None)
                        } else {
                            v.as_str()
                                .map(|x| Some(x.to_owned()))
                                .with_context(|| format!("column {name} must be Utf8"))
                        }
                    })
                    .collect::<anyhow::Result<_>>()?,
            )
        } else if hint == Some(&DataType::Boolean) {
            Column::Bool(
                cells
                    .iter()
                    .map(|v| {
                        if v.is_null() {
                            Ok(None)
                        } else {
                            v.as_bool()
                                .map(Some)
                                .with_context(|| format!("column {name} must be Boolean"))
                        }
                    })
                    .collect::<anyhow::Result<_>>()?,
            )
        } else if cells.iter().all(|v| v.is_null() || v.is_string()) {
            Column::Str(
                cells
                    .iter()
                    .map(|v| v.as_str().map(str::to_owned))
                    .collect(),
            )
        } else if cells.iter().all(|v| v.is_null() || v.is_boolean()) {
            Column::Bool(cells.iter().map(|v| v.as_bool()).collect())
        } else if cells
            .iter()
            .all(|v| v.is_null() || v.is_i64() || v.is_u64())
        {
            Column::I64(
                cells
                    .iter()
                    .map(|v| {
                        v.as_i64()
                            .or_else(|| v.as_u64().and_then(|u| i64::try_from(u).ok()))
                    })
                    .collect(),
            )
        } else if cells.iter().all(|v| v.is_null() || v.is_number()) {
            Column::F64(cells.iter().map(|v| v.as_f64()).collect())
        } else {
            anyhow::bail!("column {name} mixes types");
        };
        columns.push((name.clone(), col));
    }
    let fields: Vec<Field> = columns
        .iter()
        .map(|(n, c)| {
            Field::new(
                n,
                match c {
                    Column::Str(_) => DataType::Utf8,
                    Column::F64(_) => DataType::Float64,
                    Column::I64(_) => DataType::Int64,
                    Column::Bool(_) => DataType::Boolean,
                },
                true,
            )
        })
        .collect();
    let arrays: Vec<ArrayRef> = columns
        .into_iter()
        .map(|(_, c)| -> ArrayRef {
            match c {
                Column::Str(v) => {
                    let mut b = StringBuilder::new();
                    for x in v {
                        b.append_option(x);
                    }
                    Arc::new(b.finish())
                }
                Column::F64(v) => {
                    let mut b = Float64Builder::new();
                    for x in v {
                        b.append_option(x);
                    }
                    Arc::new(b.finish())
                }
                Column::I64(v) => {
                    let mut b = Int64Builder::new();
                    for x in v {
                        b.append_option(x);
                    }
                    Arc::new(b.finish())
                }
                Column::Bool(v) => {
                    let mut b = BooleanBuilder::new();
                    for x in v {
                        b.append_option(x);
                    }
                    Arc::new(b.finish())
                }
            }
        })
        .collect();
    let schema = Arc::new(Schema::new(fields));
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(6).expect("valid level"),
        ))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema.clone(), Some(props))?;
    if !arrays.is_empty() {
        w.write(&RecordBatch::try_new(schema, arrays)?)?;
    }
    Ok(w.into_inner()?)
}

fn mean(xs: impl Iterator<Item = f64>) -> Option<f64> {
    let v: Vec<f64> = xs.collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn opt(x: Option<f64>) -> String {
    x.map_or("-".into(), |v| format!("{v:.4}"))
}

/// The campaign in one Markdown table per reader, over the rows.
pub fn summary_md(campaign: &str, s: &Summary, pages: &[PageRow], crops: &[CropRow]) -> String {
    let mut out = format!("# Quality campaign {campaign}\n\nPages with a result: {}; with readings: {}; with Surya crops: {}; pending: {}; failed: {}.\n\n", s.pages, s.with_readings, s.with_surya, s.pending, s.failed);
    out.push_str("| Reader | Pages | Mean est. CER | Mean completeness | Mean precision | Mean findable content | Mean findable places |\n|---|---|---|---|---|---|---|\n");
    let mut readers: Vec<&str> = pages.iter().map(|r| r.reader.as_str()).collect();
    readers.sort_unstable();
    readers.dedup();
    for name in &readers {
        let rs: Vec<&PageRow> = pages.iter().filter(|r| r.reader == *name).collect();
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} | {} | {} | {} |",
            rs.len(),
            opt(mean(rs.iter().filter_map(|r| r.est_cer))),
            opt(mean(rs.iter().filter_map(|r| r.completeness))),
            opt(mean(rs.iter().filter_map(|r| r.precision))),
            opt(mean(rs.iter().filter_map(|r| r.findable_content))),
            opt(mean(rs.iter().filter_map(|r| r.findable_places))),
        );
    }
    out.push_str("\n| Reader | Crops | Mean CER vs VLM | Mean Dice vs VLM | Mean crop hat est. CER |\n|---|---|---|---|---|\n");
    let mut readers: Vec<&str> = crops.iter().map(|r| r.reader.as_str()).collect();
    readers.sort_unstable();
    readers.dedup();
    for name in &readers {
        let rs: Vec<&CropRow> = crops.iter().filter(|r| r.reader == *name).collect();
        let _ = writeln!(
            out,
            "| {name} | {} | {} | {} | {} |",
            rs.len(),
            opt(mean(rs.iter().filter_map(|r| r.cer_vs_vlm))),
            opt(mean(rs.iter().filter_map(|r| r.dice_vs_vlm))),
            opt(mean(rs.iter().filter_map(|r| r.hat_est_cer))),
        );
    }
    out
}

pub(crate) fn spool_files(spool: &Path, workunit: &str, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|n| spool.join(format!("{workunit}__file_{n}")))
        .collect()
}

pub(crate) fn gave_up(spool: &Path, workunit: &str) -> bool {
    spool.join(format!("{workunit}_error")).exists()
        || spool.join(format!("{workunit}_no_output_files")).exists()
}

/// The spool `job`'s canonical result lands in: a Xiaomi-OCR-0 job's is the
/// batch-108 one (None when none is configured), any other job's the batch-8 one.
pub(crate) fn qwen_spool_for<'a>(
    job: &qwen::Job,
    spool: &'a Path,
    xiaomi_spool: Option<&'a Path>,
) -> Option<&'a Path> {
    if job
        .model
        .as_deref()
        .is_some_and(usnm_ocr_contract::qwen::is_xiaomi_model)
    {
        xiaomi_spool
    } else {
        Some(spool)
    }
}

/// A page's `usnm_qwen` crops and canonical readings, when both exist.
struct QwenPage {
    crops: Vec<Crop>,
    readings: Option<Readings>,
    /// What the agreeing pairs vouch for, with crop ids as in `crops`;
    /// None without crop states.
    vouch: Option<crate::qwen::consensus::Vouch>,
}

impl QwenPage {
    /// The canonical readings, when at least one crop of them is vouched for.
    fn vouched_readings(&self) -> Option<&Readings> {
        self.readings.as_ref().filter(|r| {
            self.vouch
                .as_ref()
                .is_none_or(|v| r.readings.iter().any(|x| v.vouches(&x.id)))
        })
    }
}

/// `readings` without the crops `hide` names. When that leaves no reading at all, the page has
/// none: not an empty result that would still count as a joined reading.
fn without_excluded(readings: Option<Readings>, hide: impl Fn(&str) -> bool) -> Option<Readings> {
    let mut r = readings?;
    let before = r.readings.len();
    r.readings.retain(|x| !hide(&x.id));
    (before == 0 || !r.readings.is_empty()).then_some(r)
}

/// Every submitted `usnm_qwen` unit by doc id (supporting both single-page
/// and multi-page bundle units), with its readings where the result is in.
/// With `page_records`, the directory `qwen compare --pages` writes, a
/// bundle with no canonical result yet, or one a `page-v1` pair settled,
/// gives each page two hosts agreed on its readings from the agreeing pair's
/// record (#1435); a bundle settled by its pages joins no other page, its
/// canonical result included. A page packed into a later bundle keeps an
/// earlier bundle's readings until the later one accepts it.
fn qwen_pages(
    root: &Path,
    spool: &Path,
    xiaomi_spool: Option<&Path>,
    crop_states: Option<&Path>,
    page_records: Option<&Path>,
    exclusions: &super::exclusions::Exclusions,
) -> anyhow::Result<BTreeMap<String, QwenPage>> {
    let mut out = BTreeMap::new();
    let dir = units(root);
    if !dir.exists() {
        return Ok(out);
    }
    let mut unit_dirs: Vec<PathBuf> = std::fs::read_dir(&dir)?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.join(crate::qwen::JOB_FILE).exists())
        .collect();
    unit_dirs.sort();
    for unit in unit_dirs {
        let job: qwen::Job = serde_json::from_slice(&read(
            &unit.join(crate::qwen::JOB_FILE),
            qwen::MAX_JOB_JSON,
        )?)
        .with_context(|| format!("{}", unit.display()))?;
        let wu = job.workunit();
        let files = qwen_spool_for(&job, spool, xiaomi_spool)
            .map(|dir| spool_files(dir, &wu, &crate::qwen::outputs()))
            .unwrap_or_default();
        let (mut readings, mut vouch) = if !files.is_empty() && files.iter().all(|f| f.exists()) {
            let got = crate::qwen::validate::load(&files)
                .with_context(|| format!("canonical result {wu}"))?;
            anyhow::ensure!(
                got.result.job_id == job.id,
                "{wu}: result does not belong to its job"
            );
            let vouch = crop_states
                .map(|dir| {
                    crate::qwen::consensus::vouched(dir, &job.id, &got.result.readings_sha256)
                })
                .transpose()
                .with_context(|| format!("{wu}: crop states"))?;
            (Some(got.readings), vouch)
        } else {
            (None, None)
        };
        // An excluded unit reads as if no result had come in. An excluded crop has no reading,
        // but is dropped only once a page has been accepted or not: whether a retry takes the
        // page over from an earlier bundle depends on the result as it came in.
        let excluded = exclusions.job(&job.id);
        if excluded.is_some_and(|e| e.whole) {
            (readings, vouch) = (None, None);
        }
        let whole = excluded.is_some_and(|e| e.whole);
        if job.pages.is_empty() {
            if let Some(e) = excluded {
                readings = without_excluded(readings, |id| e.hides(None, id));
            }
            out.insert(
                job.doc_id.clone(),
                QwenPage {
                    crops: job.crops,
                    readings,
                    vouch,
                },
            );
        } else {
            // A bundle a `page-v1` pair settled is canonical with pages the
            // pair disagreed on: only its accepted pages are joined.
            let by_pages = page_records.is_some_and(|dir| {
                dir.join(&job.id)
                    .join(crate::qwen::validate::UNIT_VERDICT_FILE)
                    .is_file()
            });
            let mut agreed = match page_records {
                Some(dir) if !whole && (readings.is_none() || by_pages) => {
                    crate::qwen::ledger::accepted_readings(root, &unit, &job, dir)
                        .with_context(|| format!("{wu}: page records"))?
                }
                _ => BTreeMap::new(),
            };
            for bp in &job.pages {
                if let Some(page_readings) = agreed.remove(&bp.doc_id) {
                    let page_readings = match excluded {
                        Some(e) => without_excluded(Some(page_readings), |id| {
                            e.hides(Some(&bp.doc_id), id)
                        }),
                        None => Some(page_readings),
                    };
                    out.insert(
                        bp.doc_id.clone(),
                        QwenPage {
                            crops: bp.crops.clone(),
                            readings: page_readings,
                            vouch: Some(crate::qwen::consensus::Vouch::Page),
                        },
                    );
                    continue;
                }
                let prefix = format!("{}:", bp.doc_id);
                let page_readings = readings.as_ref().filter(|_| !by_pages).map(|r| {
                    let mut pr = r.clone();
                    pr.readings = r
                        .readings
                        .iter()
                        .filter_map(|rd| {
                            rd.id.strip_prefix(&prefix).map(|cid| qwen::Reading {
                                id: cid.to_owned(),
                                ms: rd.ms,
                                text: rd.text.clone(),
                                tokens: rd.tokens,
                            })
                        })
                        .collect();
                    pr
                });
                // A bundle's crop states name its crops `<doc_id>:<crop>`.
                let page_vouch = vouch.as_ref().map(|v| match v {
                    crate::qwen::consensus::Vouch::Strict => crate::qwen::consensus::Vouch::Strict,
                    crate::qwen::consensus::Vouch::Page => crate::qwen::consensus::Vouch::Page,
                    crate::qwen::consensus::Vouch::Crops(states) => {
                        crate::qwen::consensus::Vouch::Crops(
                            states
                                .iter()
                                .filter_map(|(id, s)| {
                                    id.strip_prefix(&prefix).map(|cid| (cid.to_owned(), *s))
                                })
                                .collect(),
                        )
                    }
                });
                // A page packed again (#1435) keeps what an earlier bundle
                // gave it until this one accepts it: by its page records, or
                // by a canonical result vouching for every crop of the page.
                // A result that did not run (two agreeing `NoGpu` results, say)
                // reads no crop, so it accepts nothing.
                let accepts = page_readings.as_ref().is_some_and(|r| {
                    r.outcome == qwen::Outcome::Ok
                        && bp
                            .crops
                            .iter()
                            .all(|c| r.readings.iter().any(|x| x.id == c.id))
                }) && page_vouch
                    .as_ref()
                    .is_none_or(|v| bp.crops.iter().all(|c| v.vouches(&c.id)));
                if !accepts && out.get(&bp.doc_id).is_some_and(|p| p.readings.is_some()) {
                    continue;
                }
                let page_readings = match excluded {
                    Some(e) => without_excluded(page_readings, |id| e.hides(Some(&bp.doc_id), id)),
                    None => page_readings,
                };
                out.insert(
                    bp.doc_id.clone(),
                    QwenPage {
                        crops: bp.crops.clone(),
                        readings: page_readings,
                        vouch: page_vouch,
                    },
                );
            }
        }
    }
    Ok(out)
}

/// The crops to join for a page and the page's size in pixels: the
/// `usnm_qwen` unit's crops when there is one, else, when Surya crop units
/// exist, the recipe's cut from the ALTO at the page's size with the
/// quality job's seed and count (the same cut `stage-crops` made), so
/// Surya's rows are not lost to a campaign without Qwen. `dims` gives the
/// size, from the image or a bundle's `dims.json`; it is asked only when
/// there are crops.
/// A page's crops and its `(width, height)` in pixels.
type PageCrops = (Vec<Crop>, (u32, u32));

fn crops_for_page(
    q: Option<&QwenPage>,
    surya: Option<&BTreeMap<String, crate::surya::collect::CropText>>,
    dims: impl FnOnce() -> anyhow::Result<(u32, u32)>,
    alto: &[u8],
    crop_seed: u64,
    k_crops: u32,
) -> anyhow::Result<Option<PageCrops>> {
    Ok(match (q, surya) {
        (Some(q), _) => Some((q.crops.clone(), dims()?)),
        (None, Some(s)) if !s.is_empty() => {
            let d = dims()?;
            Some((
                crate::qwen::stage::cut_dims(d, alto, crop_seed, k_crops)?,
                d,
            ))
        }
        (None, _) => None,
    })
}

/// The three tables of a campaign, built from the roots and spools.
pub struct Collected {
    pub summary: Summary,
    pub pages: Vec<PageRow>,
    pub crops: Vec<CropRow>,
}

impl Summary {
    pub(super) fn add(&mut self, other: &Self) {
        self.pages += other.pages;
        self.with_readings += other.with_readings;
        self.with_surya += other.with_surya;
        self.pending += other.pending;
        self.failed += other.failed;
        self.page_rows += other.page_rows;
        self.crop_rows += other.crop_rows;
        self.hat_v1_kept += other.hat_v1_kept;
    }

    pub(super) fn since(&self, before: &Self) -> Self {
        Self {
            pages: self.pages - before.pages,
            with_readings: self.with_readings - before.with_readings,
            with_surya: self.with_surya - before.with_surya,
            pending: self.pending - before.pending,
            failed: self.failed - before.failed,
            page_rows: 0,
            crop_rows: 0,
            hat_v1_kept: self.hat_v1_kept - before.hat_v1_kept,
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct InventoryUnit {
    /// Scheduling hint only. The normal collector must still validate every canonical output.
    pub fingerprint: Option<String>,
    pub status: &'static str,
}

pub fn inventory(
    opts: &Options,
    selected: Option<&str>,
) -> anyhow::Result<BTreeMap<String, InventoryUnit>> {
    let cache = super::collect_cache::Cache::new(
        opts.cache.as_ref().context("inventory requires --cache")?,
        opts,
    )?;
    // Fail on unavailable roots and directory entry errors, never report unknown as empty.
    std::fs::read_dir(&opts.spool)?;
    let mut result = BTreeMap::new();
    for (index, entry) in std::fs::read_dir(units(&opts.root))?.enumerate() {
        anyhow::ensure!(index < 10_000, "inventory exceeds 10000 unit entries");
        let unit = entry?.path();
        if !unit.join("submitted").try_exists()? {
            continue;
        }
        let name = unit
            .file_name()
            .and_then(|n| n.to_str())
            .context("non-UTF8 unit name")?
            .to_owned();
        if selected.is_some_and(|selected| selected != name) {
            continue;
        }
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)?;
        let files = spool_files(&opts.spool, &job.workunit(), &super::outputs());
        let failed = ["error", "no_output_files"]
            .iter()
            .try_fold(false, |failed, suffix| {
                Ok::<_, std::io::Error>(
                    failed
                        || opts
                            .spool
                            .join(format!("{}_{suffix}", job.workunit()))
                            .try_exists()?,
                )
            })?;
        let present = files.iter().try_fold(true, |present, path| {
            Ok::<_, std::io::Error>(path.try_exists()? && present)
        })?;
        let fingerprint = if present && !failed {
            Some(cache.inventory_key(&unit, &job, &files)?)
        } else {
            None
        };
        result.insert(
            name,
            InventoryUnit {
                fingerprint,
                status: if failed {
                    "failed"
                } else if present {
                    "canonical-present"
                } else {
                    "pending"
                },
            },
        );
    }
    cache.verify()?;
    Ok(result)
}

fn is_input_change(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<super::collect_cache::InputChanged>()
        .is_some()
}

/// `collect_units`, refusing an incomplete publication: a deferred unit is an error.
pub fn collect(opts: &Options) -> anyhow::Result<Collected> {
    let (collected, progress) = collect_units(opts)?;
    anyhow::ensure!(
        !progress.deferred,
        "collection cache warmed {} dirty units; at least 1 deferred; no incomplete snapshot published",
        progress.dirty
    );
    Ok(collected)
}

pub fn collect_units(opts: &Options) -> anyhow::Result<(Collected, Progress)> {
    anyhow::ensure!(
        opts.max_dirty_units > 0,
        "--max-dirty-units must be positive"
    );
    anyhow::ensure!(
        !(opts.warm_only || opts.max_compute.is_some()) || opts.cache.is_some(),
        "--warm-only and --max-compute-secs need --cache"
    );
    let started = std::time::Instant::now();
    let cache = opts
        .cache
        .as_ref()
        .map(|dir| super::collect_cache::Cache::new(dir, opts))
        .transpose()?;
    let inventory_secs = started.elapsed().as_secs_f64();
    let qe = opts
        .qe_dir
        .as_deref()
        .map(super::qe::Dir::open)
        .transpose()?;
    let mut dirty = 0;
    let mut deferred = false;
    let mut summary = Summary::default();
    let mut pages = Vec::new();
    let mut crop_table = Vec::new();
    let qwen = qwen_pages(
        &opts.qwen_root,
        &opts.qwen_spool,
        opts.qwen_xiaomi_spool.as_deref(),
        opts.qwen_crop_states.as_deref(),
        opts.qwen_pages.as_deref(),
        &opts.qwen_exclusions,
    )?;
    let surya = match (&opts.surya_root, &opts.surya_spool) {
        (Some(root), Some(spool)) => crate::surya::collect::results(root, spool)?.crops,
        (None, None) => BTreeMap::new(),
        _ => anyhow::bail!("--surya-root and --surya-spool go together"),
    };
    let dir = units(&opts.root);
    let mut unit_dirs: Vec<PathBuf> = if dir.exists() {
        std::fs::read_dir(&dir)?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join("submitted").exists())
            .collect()
    } else {
        vec![]
    };
    unit_dirs.sort();
    for unit in unit_dirs {
        if cache.is_some() && dirty >= opts.max_dirty_units {
            deferred = true;
            break;
        }
        let job: Job = serde_json::from_slice(&read(&unit.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("{}", unit.display()))?;
        let wu = job.workunit();
        if gave_up(&opts.spool, &wu) {
            summary.failed += 1;
            continue;
        }
        let files = spool_files(&opts.spool, &wu, &super::outputs());
        if !files.iter().all(|f| f.exists()) {
            summary.pending += 1;
            continue;
        }
        let key = match cache
            .as_ref()
            .map(|c| c.key(&unit, &job, &files))
            .transpose()
        {
            Ok(key) => key,
            // Rewritten while keyed, and a warming run has no step lock to keep it still: leave
            // it for the next round, which the driver is told to run.
            Err(e) if opts.warm_only && is_input_change(&e) => {
                deferred = true;
                continue;
            }
            Err(e) => return Err(e),
        };
        if let (Some(cache), Some(key)) = (&cache, &key) {
            cache.track(&unit, &files, key);
            if opts.warm_only {
                if cache.has(key) {
                    continue;
                }
            } else if let Some(part) = cache.load(key)? {
                summary.add(&part.summary);
                pages.extend(part.pages);
                crop_table.extend(part.crops);
                continue;
            }
            if opts.max_compute.is_some_and(|max| started.elapsed() >= max) {
                deferred = true;
                break;
            }
            dirty += 1;
        }
        let before = summary.clone();
        let pages_start = pages.len();
        let crops_start = crop_table.len();
        let got = validate::load(&files).with_context(|| format!("canonical result {wu}"))?;
        anyhow::ensure!(
            got.result.job_id == job.id && got.result.engine == job.engine,
            "{wu}: result does not belong to its job"
        );
        if job.pages.is_empty() {
            summary.pages += 1;
            let q = qwen.get(&job.doc_id);
            let readings = q.and_then(|q| q.readings.as_ref());
            let vouch = q.and_then(|q| q.vouch.as_ref());
            let vouched = q.and_then(QwenPage::vouched_readings).is_some();
            if vouched {
                summary.with_readings += 1;
            }
            let surya_crops = surya.get(&job.doc_id);
            if surya_crops.is_some_and(|c| c.values().any(|t| !t.text.trim().is_empty())) {
                summary.with_surya += 1;
            }
            let texts_opt: Option<Texts> = read(&unit.join(TEXTS_FILE), MAX_TEXTS)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let mut unit_rows = page_rows(&job, &got.result, vouched);
            let alto_bytes = read(&unit.join(ALTO_FILE), MAX_ALTO)?;
            let v2 = texts_opt.as_ref().is_some_and(|texts| {
                enrich_rows_with_damage(&mut unit_rows, texts, None);
                apply_hat_v2(&mut unit_rows, &job.readers, texts, || {
                    alto_text_of(Some(&alto_bytes))
                })
            });
            if !v2 {
                summary.hat_v1_kept += 1;
            }
            pages.extend(unit_rows);
            let page = read(&unit.join(PAGE_FILE), MAX_SCAN)?;
            let page_dims = || -> anyhow::Result<(u32, u32)> {
                Ok(image::load_from_memory(&page)
                    .with_context(|| format!("{wu}: page.jpg"))?
                    .dimensions())
            };
            if let Some((crops, (w, h))) = crops_for_page(
                q,
                surya_crops,
                page_dims,
                &alto_bytes,
                job.crop_seed,
                job.k_crops,
            )? {
                let loc = parse_alto(&alto_bytes, f64::from(w), f64::from(h))?;
                let ours = if unit.join(OURS_FILE).exists() {
                    Some(parse_alto(
                        &read(&unit.join(OURS_FILE), MAX_ALTO)?,
                        f64::from(w),
                        f64::from(h),
                    )?)
                } else {
                    None
                };
                let texts = match texts_opt {
                    Some(t) => t,
                    None => serde_json::from_slice(&read(&unit.join(TEXTS_FILE), MAX_TEXTS)?)?,
                };
                let readers: Vec<String> = job.readers.iter().map(|r| r.name.clone()).collect();
                let measure = match &qe {
                    Some(d) => d.measure(&job.language)?,
                    None => None,
                };
                crop_table.extend(crop_rows(&CropInputs {
                    doc_id: &job.doc_id,
                    job_id: &job.id,
                    loc: &loc,
                    ours: ours.as_ref(),
                    texts: &texts,
                    readers: &readers,
                    crops: &crops,
                    readings,
                    vouch,
                    surya: surya_crops,
                    qe: measure.as_deref(),
                }));
            }
        } else {
            anyhow::ensure!(
                got.result.pages.len() == job.pages.len(),
                "{wu}: bundle result has {} pages, expected {}",
                got.result.pages.len(),
                job.pages.len()
            );
            // The pages' sizes from dims.json when the unit has one; only
            // units packed before it was written read page.jpg (#1743).
            let dims =
                super::page_dims::load(&unit, &job).with_context(|| format!("{wu}: dims.json"))?;
            let bundle_page = match &dims {
                Some(_) => None,
                None => Some(
                    read(&unit.join(PAGE_FILE), MAX_BUNDLE_SCAN)
                        .with_context(|| format!("{wu}: no dims.json, and page.jpg"))?,
                ),
            };
            let bundle_alto = read(&unit.join(ALTO_FILE), MAX_BUNDLE_ALTO)?;
            let bundle_texts = read(&unit.join(TEXTS_FILE), MAX_BUNDLE_TEXTS)?;
            for (i, (bp, pr)) in job.pages.iter().zip(&got.result.pages).enumerate() {
                anyhow::ensure!(
                    bp.doc_id == pr.doc_id,
                    "{wu}: bundle page doc_id mismatch ({} vs {})",
                    bp.doc_id,
                    pr.doc_id
                );
                summary.pages += 1;
                let q = qwen.get(&bp.doc_id);
                let readings = q.and_then(|q| q.readings.as_ref());
                let vouch = q.and_then(|q| q.vouch.as_ref());
                let vouched = q.and_then(QwenPage::vouched_readings).is_some();
                if vouched {
                    summary.with_readings += 1;
                }
                let surya_crops = surya.get(&bp.doc_id);
                if surya_crops.is_some_and(|c| c.values().any(|t| !t.text.trim().is_empty())) {
                    summary.with_surya += 1;
                }
                let mut unit_rows =
                    bundle_page_rows(&job, bp, pr, got.result.host.plan_class.clone(), vouched);
                let mut v2 = false;
                if let Ok(t_bytes) = bp.inputs.texts.extract(&bundle_texts, "texts") {
                    if let Ok(page_texts) = serde_json::from_slice::<Texts>(t_bytes) {
                        enrich_rows_with_damage(&mut unit_rows, &page_texts, None);
                        v2 = apply_hat_v2(&mut unit_rows, &bp.readers, &page_texts, || {
                            alto_text_of(bp.inputs.alto.extract(&bundle_alto, "alto").ok())
                        });
                    }
                }
                if !v2 {
                    summary.hat_v1_kept += 1;
                }
                pages.extend(unit_rows);
                let p_bytes = match &bundle_page {
                    Some(b) => Some(bp.inputs.page.extract(b, "page")?),
                    None => None,
                };
                let a_bytes = bp.inputs.alto.extract(&bundle_alto, "alto")?;
                let page_dims = || -> anyhow::Result<(u32, u32)> {
                    match (&dims, p_bytes) {
                        (Some(d), _) => d
                            .get(i)
                            .with_context(|| format!("{wu} ({}): dims.json", bp.doc_id)),
                        (None, Some(p)) => Ok(image::load_from_memory(p)
                            .with_context(|| format!("{wu} ({}): page.jpg", bp.doc_id))?
                            .dimensions()),
                        (None, None) => anyhow::bail!("{wu} ({}): no page size", bp.doc_id),
                    }
                };
                if let Some((crops, (w, h))) =
                    crops_for_page(q, surya_crops, page_dims, a_bytes, bp.crop_seed, bp.k_crops)?
                {
                    let t_bytes = bp.inputs.texts.extract(&bundle_texts, "texts")?;
                    let loc = parse_alto(a_bytes, f64::from(w), f64::from(h))?;
                    let ours_path = unit.join(format!("{}.ours.xml", bp.doc_id));
                    let ours = if ours_path.exists() {
                        Some(parse_alto(
                            &read(&ours_path, MAX_ALTO)?,
                            f64::from(w),
                            f64::from(h),
                        )?)
                    } else {
                        None
                    };
                    let texts: Texts = serde_json::from_slice(t_bytes)?;
                    let readers: Vec<String> = bp.readers.iter().map(|r| r.name.clone()).collect();
                    let measure = match &qe {
                        Some(d) => d.measure(&bp.language)?,
                        None => None,
                    };
                    crop_table.extend(crop_rows(&CropInputs {
                        doc_id: &bp.doc_id,
                        job_id: &job.id,
                        loc: &loc,
                        ours: ours.as_ref(),
                        texts: &texts,
                        readers: &readers,
                        crops: &crops,
                        readings,
                        vouch,
                        surya: surya_crops,
                        qe: measure.as_deref(),
                    }));
                }
            }
        }
        if let (Some(cache), Some(key)) = (&cache, &key) {
            let mut unchanged = cache.readings_unchanged(&job.doc_id)?;
            for bp in &job.pages {
                unchanged = unchanged && cache.readings_unchanged(&bp.doc_id)?;
            }
            if !unchanged {
                // Published from what was read, collected again next run. A warming run has
                // nothing to publish, so the unit is for its next round.
                deferred |= opts.warm_only;
                continue;
            }
            match cache.key(&unit, &job, &files) {
                Ok(again) if &again == key => {}
                // Rewritten while it was read. A warming run has no step lock to keep inputs
                // still and publishes nothing, so the next round keys the unit afresh.
                Ok(_) if opts.warm_only => {
                    deferred = true;
                    continue;
                }
                Err(e) if opts.warm_only && is_input_change(&e) => {
                    deferred = true;
                    continue;
                }
                Ok(_) => anyhow::bail!("{wu}: collection inputs changed; no part published"),
                Err(e) => return Err(e),
            }
            cache.store(
                key,
                &super::collect_cache::Part {
                    summary: summary.since(&before),
                    pages: pages[pages_start..].to_vec(),
                    crops: crop_table[crops_start..].to_vec(),
                },
            )?;
        }
    }
    let units_secs = started.elapsed().as_secs_f64() - inventory_secs;
    let mut verify_secs = 0.0;
    if let (Some(cache), false, false) = (&cache, opts.warm_only, deferred) {
        let verifying = std::time::Instant::now();
        cache.verify()?;
        verify_secs = verifying.elapsed().as_secs_f64();
    }
    summary.page_rows = pages.len();
    summary.crop_rows = crop_table.len();
    Ok((
        Collected {
            summary,
            pages,
            crops: crop_table,
        },
        Progress {
            dirty,
            deferred,
            inventory_secs: round6(inventory_secs),
            units_secs: round6(units_secs),
            verify_secs: round6(verify_secs),
        },
    ))
}

/// The three files of a campaign, by name.
pub fn files(
    campaign: &str,
    c: &Collected,
) -> anyhow::Result<Vec<(String, Vec<u8>, &'static str)>> {
    Ok(vec![
        (
            "pages.parquet".into(),
            to_parquet(&c.pages)?,
            "application/vnd.apache.parquet",
        ),
        (
            "crops.parquet".into(),
            crop_parquet(&c.crops)?,
            "application/vnd.apache.parquet",
        ),
        (
            "summary.md".into(),
            summary_md(campaign, &c.summary, &c.pages, &c.crops).into_bytes(),
            "text/markdown",
        ),
    ])
}

pub async fn run(opts: &Options) -> anyhow::Result<Summary> {
    anyhow::ensure!(
        opts.warm_only || opts.out_dir.is_some() != opts.curated_url.is_some(),
        "one of --out-dir and --curated-url"
    );
    anyhow::ensure!(
        !opts.warm_only || (opts.out_dir.is_none() && opts.curated_url.is_none()),
        "--warm-only publishes nothing"
    );
    anyhow::ensure!(
        usnm_store::is_safe_segment(&opts.campaign),
        "invalid campaign name {:?}",
        opts.campaign
    );
    let (c, progress) = collect_units(opts)?;
    // One JSON line the driver and the operator read: where the time went, and whether a unit
    // was left for the next run. Stdout stays free for the caller's own output.
    eprintln!(
        "{}",
        serde_json::json!({
            "phase": if opts.warm_only { "warm" } else { "collect" },
            "progress": progress,
        })
    );
    if opts.warm_only {
        return Ok(c.summary);
    }
    anyhow::ensure!(
        !progress.deferred,
        "collection cache warmed {} dirty units; at least 1 deferred; no incomplete snapshot published",
        progress.dirty
    );
    let out = files(&opts.campaign, &c)?;
    if let Some(dir) = &opts.out_dir {
        for (name, bytes, _) in &out {
            atomic(&dir.join(name), bytes)?;
        }
    }
    if let Some(url) = &opts.curated_url {
        let store = usnm_store::open(url)?;
        for (name, bytes, mime) in out {
            store
                .put(
                    &format!("audit/quality/{}/{name}", opts.campaign),
                    bytes,
                    mime,
                )
                .await?;
        }
    }
    Ok(c.summary)
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;

    #[test]
    fn parquet_from_rows_types_columns() {
        #[derive(serde::Serialize)]
        struct R {
            a: String,
            b: Option<f64>,
            c: u32,
            d: bool,
        }
        let bytes = to_parquet(&[
            R {
                a: "x".into(),
                b: None,
                c: 1,
                d: true,
            },
            R {
                a: "y".into(),
                b: Some(0.5),
                c: 2,
                d: false,
            },
        ])
        .unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::from(bytes),
        )
        .unwrap();
        let schema = reader.schema().clone();
        assert_eq!(schema.field(0).data_type(), &DataType::Utf8);
        assert_eq!(schema.field(1).data_type(), &DataType::Float64);
        assert_eq!(schema.field(2).data_type(), &DataType::Int64);
        assert_eq!(schema.field(3).data_type(), &DataType::Boolean);
        let batches: Vec<RecordBatch> = reader.build().unwrap().map(|b| b.unwrap()).collect();
        assert_eq!(batches.iter().map(|b| b.num_rows()).sum::<usize>(), 2);
        assert!(to_parquet::<R>(&[]).is_ok());
    }

    #[test]
    fn page_rows_carry_the_units_quality_measure() {
        let tmp = tempfile::tempdir().unwrap();
        let job = crate::quality::work::tests::unit(tmp.path());
        let l = crate::quality::work::prepare(tmp.path()).unwrap();
        let mut c = crate::quality::work::compute(&crate::quality::work::Inputs {
            job: &l.job,
            page: &l.page,
            alto: &l.alto,
            texts: &l.texts,
            places: &l.places,
            qe: None,
        })
        .unwrap();
        c.result.readers[0].qe = Some(usnm_ocr_contract::quality::QeReport {
            version: "qe-v1".into(),
            resources: "c".repeat(64),
            words: 10,
            known3: 7,
            chars: 60,
            no_words: false,
            oov: Some(0.1),
            charlm_bpc: Some(2.0),
            lm_oov: Some(2.542466),
        });
        let rows = page_rows(&job, &c.result, false);
        assert_eq!(rows[0].qe.qe_lm_oov, Some(2.542466));
        assert_eq!(rows[0].qe.qe_known3, Some(7));
        assert_eq!(rows[1].qe, QeColumns::default());
        // Measured and all-null tables have the same column types.
        let schema = |rows: &[PageRow]| {
            let bytes = bytes::Bytes::from(to_parquet(rows).unwrap());
            let r = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(bytes)
                .unwrap();
            r.schema()
                .fields()
                .iter()
                .filter(|f| f.name().starts_with("qe_"))
                .map(|f| (f.name().clone(), f.data_type().clone()))
                .collect::<Vec<_>>()
        };
        let measured = schema(&rows);
        assert_eq!(measured.len(), 9);
        assert_eq!(measured, schema(&rows[1..]));
    }

    #[test]
    fn crop_rows_compare_every_reader_with_the_reading() {
        let tmp = tempfile::tempdir().unwrap();
        let job = crate::quality::work::tests::unit(tmp.path());
        let l = crate::quality::work::prepare(tmp.path()).unwrap();
        let (w, h) = image::load_from_memory(&l.page).unwrap().dimensions();
        let loc = parse_alto(&l.alto, f64::from(w), f64::from(h)).unwrap();
        let crops = crate::qwen::stage::cut(&l.page, &l.alto, job.crop_seed, job.k_crops).unwrap();
        assert!(!crops.is_empty());
        let readings = Readings {
            v: qwen::VERSION,
            job_id: job.id.clone(),
            device: "cpu".into(),
            diag: Default::default(),
            model: Some("qwen3-vl-2b".into()),
            outcome: qwen::Outcome::Ok,
            prompt_version: qwen::PROMPT_VERSION,
            readings: crops
                .iter()
                .map(|c| qwen::Reading {
                    id: c.id.clone(),
                    ms: 1,
                    text: c.loc_text.replace("council", "counci1"),
                    tokens: 5,
                })
                .collect(),
        };
        let readers: Vec<String> = job.readers.iter().map(|r| r.name.clone()).collect();
        let rows = crop_rows(&CropInputs {
            doc_id: "p1",
            job_id: &job.id,
            loc: &loc,
            ours: None,
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: Some(&readings),
            vouch: None,
            surya: None,
            qe: None,
        });
        // Three readers plus the vision-language row per crop.
        assert_eq!(rows.len(), crops.len() * 4);
        let loc_row = rows.iter().find(|r| r.reader == "loc").unwrap();
        assert_eq!(loc_row.job_id.as_deref(), Some(job.id.as_str()));
        assert_eq!(
            loc_row.source.as_deref(),
            l.texts
                .readers
                .iter()
                .find(|r| r.name == "loc")
                .map(|r| r.source.as_str())
        );
        assert!(loc_row.text.as_deref().is_some_and(|t| !t.is_empty()));
        assert!(loc_row.cer_vs_vlm.is_some());
        assert!(loc_row.vlm_reference_chars.is_some_and(|n| n > 0));
        assert!(loc_row.vlm_reference_words.is_some_and(|n| n > 0));
        // Only LoC and the reading have text in the box: the hat has two
        // members and no estimate.
        assert_eq!(loc_row.hat_readers, 2);
        assert_eq!(loc_row.hat_est_cer, None);
        let as_row = rows.iter().find(|r| r.reader == "as").unwrap();
        assert_eq!(as_row.text, None);
        assert_eq!(as_row.cer_vs_vlm, None);
        let vlm_row = rows.iter().find(|r| r.reader == VLM).unwrap();
        assert_eq!(vlm_row.dice_vs_vlm, Some(1.0));
        assert_eq!(vlm_row.qe, QeColumns::default());
        // With the language's measure every crop text is measured, the
        // reading too, and a reader with no text in the box is not.
        let text: String = crops.iter().map(|c| format!("{} ", c.loc_text)).collect();
        let (entries, vocab) = usnm_core::qe::counts(&text.to_lowercase(), 5);
        let bytes = usnm_core::qe::pack(
            text.to_lowercase().split_whitespace().map(str::to_owned),
            entries,
            &usnm_core::qe::PackOptions {
                language: "eng",
                order: 5,
                vocab,
                prune_min_count: 1,
                prune_from_order: 5,
                hunspell: false,
                licence: "CC-BY-SA-4.0",
                notice: "test\n",
            },
        )
        .unwrap();
        let measure = usnm_ocr_quality::qe::Measure::load(&bytes).unwrap();
        let measured = crop_rows(&CropInputs {
            doc_id: "p1",
            job_id: &job.id,
            loc: &loc,
            ours: None,
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: Some(&readings),
            vouch: None,
            surya: None,
            qe: Some(&measure),
        });
        for r in &measured {
            assert_eq!(r.qe.qe_version.is_some(), r.text.is_some(), "{}", r.reader);
        }
        let m = |name: &str| measured.iter().find(|r| r.reader == name).unwrap();
        assert_eq!(m("loc").qe.qe_resources.as_deref(), Some(measure.sha256()));
        assert!(m(VLM).qe.qe_lm_oov.is_some());
        let parquet = crop_parquet(&measured).unwrap();
        assert!(!parquet.is_empty());
        // Without readings there are reader rows and nothing to compare.
        let rows = crop_rows(&CropInputs {
            doc_id: "p1",
            job_id: &job.id,
            loc: &loc,
            ours: Some(&loc),
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: None,
            vouch: None,
            surya: None,
            qe: None,
        });
        assert_eq!(rows.len(), crops.len() * 3);
        assert!(rows.iter().all(|r| r.vlm_text.is_none()));
        assert!(rows.iter().all(|r| r.vlm_reading_present == Some(false)));
        let parquet = crop_parquet(&rows).unwrap();
        let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
            bytes::Bytes::from(parquet),
        )
        .unwrap();
        for name in ["vlm_reference_chars", "vlm_reference_words"] {
            assert_eq!(
                reader.schema().field_with_name(name).unwrap().data_type(),
                &DataType::Int64
            );
        }
        let batches: Vec<_> = reader.build().unwrap().map(Result::unwrap).collect();
        for batch in batches {
            for name in ["vlm_reference_chars", "vlm_reference_words"] {
                let column = batch.column_by_name(name).unwrap();
                assert_eq!(column.null_count(), batch.num_rows());
            }
        }
        assert!(rows
            .iter()
            .all(|r| r.vlm_reference_chars.is_none() && r.vlm_reference_words.is_none()));
        // Surya's crop texts join as a reader of their own, in the hat with
        // LoC and the reading; a crop Surya did not read gets no row.
        let mut surya = BTreeMap::new();
        surya.insert(
            crops[0].id.clone(),
            crate::surya::collect::CropText {
                job_id: "j".into(),
                crop_id: crops[0].id.clone(),
                rect: crops[0].rect,
                stratum: crops[0].stratum.clone(),
                outcome: usnm_ocr_contract::surya::Outcome::Ok,
                device: "cpu".into(),
                text: format!("{} extra", crops[0].loc_text),
                plan_class: None,
            },
        );
        let rows = crop_rows(&CropInputs {
            doc_id: "p1",
            job_id: &job.id,
            loc: &loc,
            ours: None,
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: Some(&readings),
            vouch: None,
            surya: Some(&surya),
            qe: None,
        });
        assert_eq!(rows.len(), crops.len() * 4 + 1);
        // The same id with another box (a campaign cut with another seed)
        // is not this crop and gets no row.
        let mut other = surya.clone();
        other.get_mut(&crops[0].id).unwrap().rect[0] += 1;
        let misaligned = crop_rows(&CropInputs {
            doc_id: "p1",
            job_id: &job.id,
            loc: &loc,
            ours: None,
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: Some(&readings),
            vouch: None,
            surya: Some(&other),
            qe: None,
        });
        assert_eq!(misaligned.len(), crops.len() * 4);
        assert!(misaligned.iter().all(|r| r.reader != SURYA));
        let s_row = rows.iter().find(|r| r.reader == SURYA).unwrap();
        assert_eq!(s_row.job_id.as_deref(), Some(job.id.as_str()));
        assert_eq!(s_row.source.as_deref(), Some("surya:whole:j"));
        assert_eq!(s_row.crop_id, crops[0].id);
        assert!(s_row.text.as_deref().is_some_and(|t| t.ends_with(" extra")));
        assert!(s_row.cer_vs_vlm.is_some_and(|c| c > 0.0));
        // LoC, the reading and Surya: three members, so the hat solves.
        assert_eq!(s_row.hat_readers, 3);
        assert!(s_row.hat_est_cer.is_some());
        let loc_row = rows
            .iter()
            .find(|r| r.reader == "loc" && r.crop_id == crops[0].id)
            .unwrap();
        assert_eq!(loc_row.hat_readers, 3);
        assert!(overlaps([0, 0, 10, 10], [5, 5, 20, 20]));
        assert!(!overlaps([0, 0, 10, 10], [10, 0, 20, 20]));
    }

    #[test]
    fn crop_reference_counts_follow_canonical_normalization_and_dice_bags() {
        let tmp = tempfile::tempdir().unwrap();
        let job = crate::quality::work::tests::unit(tmp.path());
        let l = crate::quality::work::prepare(tmp.path()).unwrap();
        let (w, h) = image::load_from_memory(&l.page).unwrap().dimensions();
        let loc = parse_alto(&l.alto, f64::from(w), f64::from(h)).unwrap();
        let crops = crate::qwen::stage::cut(&l.page, &l.alto, job.crop_seed, job.k_crops).unwrap();
        let readers: Vec<String> = job.readers.iter().map(|r| r.name.clone()).collect();
        for (text, chars, words) in [
            ("", 0, 0),
            (" \n\t ", 0, 0),
            ("-\n", 0, 0),
            ("...", 3, 0),
            ("The council", 11, 2),
        ] {
            let readings = Readings {
                v: qwen::VERSION,
                job_id: job.id.clone(),
                device: "cpu".into(),
                diag: Default::default(),
                model: Some("qwen3-vl-2b".into()),
                outcome: qwen::Outcome::Ok,
                prompt_version: qwen::PROMPT_VERSION,
                readings: crops
                    .iter()
                    .map(|c| qwen::Reading {
                        id: c.id.clone(),
                        ms: 1,
                        text: text.into(),
                        tokens: 5,
                    })
                    .collect(),
            };
            let rows = crop_rows(&CropInputs {
                doc_id: "p1",
                job_id: &job.id,
                loc: &loc,
                ours: None,
                texts: &l.texts,
                readers: &readers,
                crops: &crops,
                readings: Some(&readings),
                vouch: None,
                surya: None,
                qe: None,
            });
            assert!(!rows.is_empty());
            assert!(rows.iter().all(|r| {
                r.vlm_reference_chars == Some(chars) && r.vlm_reference_words == Some(words)
            }));
            assert!(rows.iter().all(|r| {
                r.vlm_text.as_deref() == Some(text) && r.vlm_reading_present == Some(true)
            }));
            if text.trim().is_empty() {
                assert!(rows
                    .iter()
                    .all(|r| r.cer_vs_vlm.is_none() && r.dice_vs_vlm.is_none()));
                assert!(rows.iter().all(|r| r.reader != VLM && r.hat_readers == 1));
            }
            let mut legacy = serde_json::to_value(&rows[0]).unwrap();
            for field in [
                "job_id",
                "source",
                "vlm_reading_present",
                "vlm_reference_chars",
                "vlm_reference_words",
            ] {
                legacy.as_object_mut().unwrap().remove(field);
            }
            let legacy: CropRow = serde_json::from_value(legacy).unwrap();
            assert!(legacy.vlm_reference_chars.is_none() && legacy.vlm_reference_words.is_none());
            assert!(legacy.job_id.is_none() && legacy.source.is_none());
            assert!(legacy.vlm_reading_present.is_none());
            let parquet = crop_parquet(&rows).unwrap();
            let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(
                bytes::Bytes::from(parquet),
            )
            .unwrap();
            for name in ["vlm_reference_chars", "vlm_reference_words"] {
                assert_eq!(
                    reader.schema().field_with_name(name).unwrap().data_type(),
                    &DataType::Int64
                );
            }
        }
    }

    /// A campaign with Surya crop units and no Qwen unit still gets its
    /// crop rows: the crops are cut with the quality job's recipe, Surya's
    /// text is a reader among them, and nothing is compared with a reading.
    #[tokio::test]
    async fn surya_crop_rows_join_without_a_qwen_unit() {
        let tmp = tempfile::tempdir().unwrap();
        let qroot = tmp.path().join("quality");
        let unit = units(&qroot).join("sn83045462_1890-05-03_ed-1_seq-1");
        let job = crate::quality::work::tests::unit(&unit);
        std::fs::write(unit.join("submitted"), b"").unwrap();
        crate::quality::work::run(&unit).await.unwrap();
        let spool = tmp.path().join("spool");
        std::fs::create_dir_all(&spool).unwrap();
        for name in crate::quality::outputs() {
            std::fs::copy(
                unit.join(name),
                spool.join(format!("{}__file_{name}", job.workunit())),
            )
            .unwrap();
        }
        let page = std::fs::read(unit.join(PAGE_FILE)).unwrap();
        let alto = std::fs::read(unit.join(ALTO_FILE)).unwrap();
        let crops = crate::qwen::stage::cut(&page, &alto, job.crop_seed, job.k_crops).unwrap();
        assert!(!crops.is_empty());
        let (sroot, sspool) = (tmp.path().join("surya"), tmp.path().join("surya-spool"));
        let mut base = crate::surya::work::tests::job();
        base.doc_id = job.doc_id.clone();
        let cj = crate::surya::collect::tests::crop_job(
            &base,
            &crops[0].id,
            crops[0].rect,
            &crops[0].stratum,
        );
        crate::surya::collect::tests::unit_with_result(
            &sroot,
            &sspool,
            &cj,
            &[(
                usnm_ocr_contract::surya::InputMode::Whole,
                usnm_ocr_contract::surya::Outcome::Ok,
                "Surya read this line\n",
            )],
        );
        let c = collect(&Options {
            campaign: "t".into(),
            root: qroot,
            spool,
            qwen_root: tmp.path().join("no-qwen"),
            qwen_spool: tmp.path().join("no-qwen"),
            qwen_xiaomi_spool: None,
            qwen_crop_states: None,
            qwen_exclusions: Default::default(),
            qwen_pages: None,
            surya_root: Some(sroot),
            surya_spool: Some(sspool),
            out_dir: Some(tmp.path().join("out")),
            curated_url: None,
            cache: None,
            max_dirty_units: 8,
            max_compute: None,
            warm_only: false,
            qe_dir: None,
        })
        .unwrap();
        assert_eq!(
            (
                c.summary.pages,
                c.summary.with_readings,
                c.summary.with_surya
            ),
            (1, 0, 1)
        );
        // Every reader of the job per crop, plus Surya's row on its crop.
        assert_eq!(c.crops.len(), crops.len() * job.readers.len() + 1);
        let s_row = c.crops.iter().find(|r| r.reader == SURYA).unwrap();
        assert_eq!(s_row.crop_id, crops[0].id);
        assert_eq!((s_row.x0, s_row.y1), (crops[0].rect[0], crops[0].rect[3]));
        assert_eq!(s_row.text.as_deref(), Some("Surya read this line\n"));
        assert!(s_row.vlm_text.is_none() && s_row.cer_vs_vlm.is_none());
        // LoC and Surya have text in the box: two members, no estimate.
        assert_eq!(s_row.hat_readers, 2);
        assert!(c.crops.iter().all(|r| r.vlm_text.is_none()));
    }

    #[test]
    fn old_results_preserve_unmeasured_metrics_in_both_collector_paths() {
        use usnm_ocr_contract::quality::{BundlePageInputs, BundleSlice};
        let tmp = tempfile::tempdir().unwrap();
        let job = crate::quality::work::tests::unit(tmp.path());
        let l = crate::quality::work::prepare(tmp.path()).unwrap();
        let c = crate::quality::work::compute(&crate::quality::work::Inputs {
            job: &job,
            page: &l.page,
            alto: &l.alto,
            texts: &l.texts,
            places: &l.places,
            qe: l.qe.as_deref(),
        })
        .unwrap();
        let mut legacy = serde_json::to_value(&c.result).unwrap();
        let image = legacy["page"]["image"].as_object_mut().unwrap();
        for key in [
            "bg_tile_std",
            "otsu_local_diff",
            "img_columns",
            "gutter_clarity",
            "est_line_height_px",
            "vertical_stroke_ratio",
        ] {
            image.remove(key);
        }
        let result: ResultManifest = serde_json::from_value(legacy).unwrap();
        let bp = BundlePage {
            crop_seed: job.crop_seed,
            doc_id: job.doc_id.clone(),
            function_words: vec![],
            group: job.group.clone(),
            k_crops: job.k_crops,
            language: job.language.clone(),
            readers: job.readers.clone(),
            inputs: BundlePageInputs {
                page: BundleSlice::of(0, b"", "fixture"),
                alto: BundleSlice::of(0, b"", "fixture"),
                texts: BundleSlice::of(0, b"", "fixture"),
            },
        };
        let pr = PageResult {
            doc_id: job.doc_id.clone(),
            hat: result.hat.clone(),
            legibility: result.legibility.clone(),
            outcome: result.outcome,
            page: result.page.clone(),
            readers: result.readers.clone(),
            timings: result.timings.clone(),
            unit_error: None,
        };
        for rows in [
            page_rows(&job, &result, false),
            bundle_page_rows(&job, &bp, &pr, None, false),
        ] {
            assert!(!rows.is_empty());
            for row in rows {
                assert!(row.bg_tile_std.is_none());
                assert!(row.otsu_local_diff.is_none());
                assert!(row.img_columns.is_none());
                assert!(row.gutter_clarity.is_none());
                assert!(row.est_line_height_px.is_none());
                assert!(row.vertical_stroke_ratio.is_none());
            }
        }
        let mut measured = result.clone();
        measured
            .page
            .as_mut()
            .unwrap()
            .image
            .as_mut()
            .unwrap()
            .bg_tile_std = Some(0.0);
        assert_eq!(page_rows(&job, &measured, false)[0].bg_tile_std, Some(0.0));
    }
    #[test]
    fn page_rows_enrich_lexical_damage_and_loc_pairwise_metrics() {
        let tmp = tempfile::tempdir().unwrap();
        let mut job = crate::quality::work::tests::unit(tmp.path());
        let l = crate::quality::work::prepare(tmp.path()).unwrap();
        let c = crate::quality::work::compute(&crate::quality::work::Inputs {
            job: &job,
            page: &l.page,
            alto: &l.alto,
            texts: &l.texts,
            places: &l.places,
            qe: l.qe.as_deref(),
        })
        .unwrap();
        job.language = "eng".into();
        let mut rows = page_rows(&job, &c.result, false);
        enrich_rows_with_damage(&mut rows, &l.texts, None);
        let loc_row = rows.iter().find(|r| r.reader == "loc").unwrap();
        assert_eq!(loc_row.cer_vs_loc, Some(0.0));
        assert_eq!(loc_row.dice_vs_loc, Some(1.0));
        assert_eq!(loc_row.word_ratio_vs_loc, Some(1.0));
        assert!(loc_row.function_share.is_some());
        assert!(loc_row.garbage_share.is_some());
        let ours_row = rows.iter().find(|r| r.reader == "ours-tesseract").unwrap();
        assert!(ours_row.cer_vs_loc.is_some());
        assert!(ours_row.wer_vs_loc.is_some());
        assert!(ours_row.dice_vs_loc.is_some());
        assert!(ours_row.word_ratio_vs_loc.is_some());
        assert!(ours_row.function_share.is_some());
        assert!(ours_row.known_words.is_some());
        assert!(ours_row.numeric_share.is_some());
        assert!(ours_row.short_line_share.is_some());
        assert!(ours_row.fraktur_score.is_some());
        assert!(ours_row.ink_word_coverage.is_some());
        assert!(ours_row.bg_tile_std.is_some());
        assert!(ours_row.otsu_local_diff.is_some());
        assert!(ours_row.img_columns.is_some());
        assert!(ours_row.gutter_clarity.is_some());
        assert!(ours_row.est_line_height_px.is_some());
        assert!(ours_row.vertical_stroke_ratio.is_some());

        // Analytic cross-reader coverage ratio identity:
        // ink_word_coverage(A) / ink_word_coverage(loc) == word_ratio_vs_loc(A).
        let cov_ratio = ours_row.ink_word_coverage.unwrap() / loc_row.ink_word_coverage.unwrap();
        assert!(
            (cov_ratio - ours_row.word_ratio_vs_loc.unwrap()).abs() <= 1e-4,
            "cov_ratio {cov_ratio} != word_ratio_vs_loc {:?}",
            ours_row.word_ratio_vs_loc
        );
    }

    #[test]
    fn hat_v2_replaces_the_estimate_and_keeps_the_units() {
        let tmp = tempfile::tempdir().unwrap();
        let job = crate::quality::work::tests::unit(tmp.path());
        let l = crate::quality::work::prepare(tmp.path()).unwrap();
        let c = crate::quality::work::compute(&crate::quality::work::Inputs {
            job: &job,
            page: &l.page,
            alto: &l.alto,
            texts: &l.texts,
            places: &l.places,
            qe: l.qe.as_deref(),
        })
        .unwrap();
        let mut rows = page_rows(&job, &c.result, false);
        assert!(rows.iter().all(|r| r.hat_version == 1
            && r.est_cer == r.est_cer_v1
            && r.hat_pairs_json.is_empty()));
        let v1: Vec<Option<f64>> = rows.iter().map(|r| r.est_cer).collect();
        // A LoC text that needs the ALTO, with none staged: version 1 stays.
        let mut needs = l.texts.clone();
        for r in needs.readers.iter_mut().filter(|r| r.name == "loc") {
            r.text = "none".into();
        }
        let before = rows.clone();
        assert!(!apply_hat_v2(&mut rows, &job.readers, &needs, || {
            alto_text_of(Some(b""))
        }));
        assert_eq!(rows, before);
        assert!(apply_hat_v2(&mut rows, &job.readers, &l.texts, || {
            alto_text_of(Some(&l.alto))
        }));
        for (r, old) in rows.iter().zip(&v1) {
            assert_eq!(r.hat_version, page_hat::VERSION);
            assert_eq!(&r.est_cer_v1, old);
            assert!(r.est_cer.is_some(), "{}", r.reader);
            assert!(r.matched.is_some(), "{}", r.reader);
            let pairs: Vec<page_hat::PairD> = serde_json::from_str(&r.hat_pairs_json).unwrap();
            assert_eq!(pairs.len(), 3);
        }
        // The fixture's readers are in one order, so both hats rank them
        // the same.
        let order = |f: &dyn Fn(&PageRow) -> f64| {
            let mut v: Vec<(String, f64)> = rows.iter().map(|r| (r.reader.clone(), f(r))).collect();
            v.sort_by(|a, b| a.1.total_cmp(&b.1));
            v.into_iter().map(|x| x.0).collect::<Vec<_>>()
        };
        assert_eq!(
            order(&|r| r.est_cer.unwrap()),
            order(&|r| r.est_cer_v1.unwrap())
        );
        // The columns are typed in Parquet.
        assert!(to_parquet(&rows).is_ok());
    }
    pub(in crate::quality) async fn cached_fixture(root: &Path) -> (Options, PathBuf, Job) {
        let unit = units(&root.join("quality")).join("page");
        let job = crate::quality::work::tests::unit(&unit);
        std::fs::write(unit.join("submitted"), b"").unwrap();
        crate::quality::work::run(&unit).await.unwrap();
        let opts = Options {
            campaign: "test".into(),
            root: root.join("quality"),
            spool: root.join("spool"),
            qwen_root: root.join("qwen"),
            qwen_spool: root.join("qwen-spool"),
            qwen_xiaomi_spool: None,
            qwen_crop_states: None,
            qwen_exclusions: Default::default(),
            qwen_pages: None,
            surya_root: None,
            surya_spool: None,
            out_dir: Some(root.join("out")),
            curated_url: None,
            cache: Some(root.join("cache")),
            max_dirty_units: 1,
            max_compute: None,
            warm_only: false,
            qe_dir: None,
        };
        canonical(&unit, &job, &opts.spool);
        (opts, unit, job)
    }

    fn canonical(unit: &Path, job: &Job, spool: &Path) {
        std::fs::create_dir_all(spool).unwrap();
        for name in super::super::outputs() {
            std::fs::copy(
                unit.join(name),
                spool.join(format!("{}__file_{name}", job.workunit())),
            )
            .unwrap();
        }
    }

    #[tokio::test]
    async fn inventory_tracks_pending_completion_and_changed_inputs_without_publishing() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        let first = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        assert!(first.is_some());
        assert!(!opts.out_dir.as_ref().unwrap().exists());
        let canonical_path = opts.spool.join(format!(
            "{}__file_{}",
            job.workunit(),
            crate::quality::RESULT_FILE
        ));
        let bytes = std::fs::read(&canonical_path).unwrap();
        std::fs::remove_file(&canonical_path).unwrap();
        let pending = inventory(&opts, None).unwrap();
        assert_eq!(pending["page"].status, "pending");
        assert!(pending["page"].fingerprint.is_none());
        std::fs::write(&canonical_path, bytes).unwrap();
        assert_eq!(
            inventory(&opts, Some("page")).unwrap()["page"].fingerprint,
            first
        );
        std::fs::write(unit.join(OURS_FILE), b"late independent sidecar").unwrap();
        assert_ne!(inventory(&opts, None).unwrap()["page"].fingerprint, first);
        let failed = opts.spool.join(format!("{}_error", job.workunit()));
        std::fs::write(&failed, b"failed").unwrap();
        assert_eq!(inventory(&opts, None).unwrap()["page"].status, "failed");
        std::fs::remove_file(failed).unwrap();
        let unavailable = dir.path().join("unavailable-spool");
        std::fs::rename(&opts.spool, &unavailable).unwrap();
        assert!(inventory(&opts, None).is_err());
        std::fs::rename(unavailable, &opts.spool).unwrap();
        std::fs::write(unit.join(JOB_FILE), b"corrupt").unwrap();
        assert!(inventory(&opts, None).is_err());
    }

    fn parts(opts: &Options) -> Vec<PathBuf> {
        let mut files = std::fs::read_dir(opts.cache.as_ref().unwrap())
            .unwrap()
            .map(|p| p.unwrap().path())
            .filter(|p| p.is_file())
            .collect::<Vec<_>>();
        files.sort();
        files
    }

    #[tokio::test]
    async fn warm_only_stores_parts_without_publishing_and_a_later_run_publishes_them_clean() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, _, _) = cached_fixture(dir.path()).await;
        let warm = Options {
            warm_only: true,
            qe_dir: None,
            out_dir: None,
            ..opts.clone()
        };
        let (_, progress) = collect_units(&warm).unwrap();
        assert_eq!((progress.dirty, progress.deferred), (1, false));
        assert_eq!(parts(&opts).len(), 1);
        assert!(!opts.out_dir.as_ref().unwrap().exists());
        // Everything is cached now: a second warm computes nothing and a publication,
        // which would fail on any dirty unit with a zero compute budget, succeeds.
        let (_, again) = collect_units(&warm).unwrap();
        assert_eq!((again.dirty, again.deferred), (0, false));
        let bounded = Options {
            max_compute: Some(std::time::Duration::ZERO),
            ..opts.clone()
        };
        let summary = run(&bounded).await.unwrap();
        assert_eq!(summary.pages, 1);
        assert!(opts
            .out_dir
            .as_ref()
            .unwrap()
            .join("pages.parquet")
            .exists());
        // A warming run publishes nowhere, and needs a cache to warm.
        assert!(run(&Options {
            out_dir: Some(dir.path().join("elsewhere")),
            ..warm.clone()
        })
        .await
        .is_err());
        assert!(collect_units(&Options {
            cache: None,
            ..warm
        })
        .is_err());
    }

    #[tokio::test]
    async fn compute_budget_defers_dirty_units_but_not_cached_ones() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, _, _) = cached_fixture(dir.path()).await;
        let bounded = Options {
            max_compute: Some(std::time::Duration::ZERO),
            max_dirty_units: 8,
            ..opts.clone()
        };
        let error = run(&bounded).await.unwrap_err().to_string();
        assert!(error.contains("deferred"), "{error}");
        assert!(parts(&opts).is_empty());
        assert!(!opts
            .out_dir
            .as_ref()
            .unwrap()
            .join("pages.parquet")
            .exists());
        // Without a cache there are no parts to compute ahead of time, so a budget is refused
        // rather than ignored.
        assert!(collect_units(&Options {
            cache: None,
            ..bounded.clone()
        })
        .is_err());
        // Only a key that failed because an input was rewritten is one a warming run skips.
        let rewritten = anyhow::Error::new(super::super::collect_cache::InputChanged(
            "input changed while hashing",
        ));
        assert!(is_input_change(&rewritten));
        assert!(!is_input_change(&anyhow::anyhow!(
            "input changed while hashing"
        )));
        // Warmed without a budget, the same budget publishes: nothing is left to compute.
        run(&Options {
            max_compute: None,
            ..bounded.clone()
        })
        .await
        .unwrap();
        run(&bounded).await.unwrap();
    }

    #[tokio::test]
    async fn warming_budget_stops_before_reading_any_further_unit() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, _) = cached_fixture(dir.path()).await;
        for name in ["zz-deferred", "zzz-not-visited"] {
            let other = units(&opts.root).join(name);
            let mut job = crate::quality::work::tests::unit(&other);
            job.doc_id = name.into();
            job.id = job.identity().unwrap();
            std::fs::write(other.join(JOB_FILE), serde_json::to_vec(&job).unwrap()).unwrap();
            std::fs::write(other.join("submitted"), b"").unwrap();
            for output in crate::quality::outputs() {
                std::fs::copy(
                    unit.join(output),
                    opts.spool
                        .join(format!("{}__file_{output}", job.workunit())),
                )
                .unwrap();
            }
            if name == "zz-deferred" {
                std::fs::remove_file(other.join(PAGE_FILE)).unwrap();
                std::fs::create_dir(other.join(PAGE_FILE)).unwrap();
            }
        }
        let error = run(&opts).await.unwrap_err().to_string();
        assert!(error.contains("deferred"), "{error}");
        assert_eq!(parts(&opts).len(), 1);
        assert!(!opts
            .out_dir
            .as_ref()
            .unwrap()
            .join("pages.parquet")
            .exists());
    }

    #[tokio::test]
    async fn incremental_warming_resumes_without_partial_publication_and_reuses_exact_rows() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, job) = cached_fixture(dir.path()).await;
        run(&Options {
            cache: None,
            ..opts.clone()
        })
        .await
        .unwrap();
        let output = opts.out_dir.as_ref().unwrap().join("pages.parquet");
        let published = std::fs::read(&output).unwrap();
        let other = units(&opts.root).join("second");
        let mut second = crate::quality::work::tests::unit(&other);
        second.doc_id = "second".into();
        second.id = second.identity().unwrap();
        std::fs::write(other.join(JOB_FILE), serde_json::to_vec(&second).unwrap()).unwrap();
        std::fs::write(other.join("submitted"), b"").unwrap();
        crate::quality::work::run(&other).await.unwrap();
        canonical(&other, &second, &opts.spool);
        let error = run(&opts).await.unwrap_err().to_string();
        assert!(error.contains("1 deferred"), "{error}");
        assert_eq!(parts(&opts).len(), 1);
        assert_eq!(std::fs::read(&output).unwrap(), published);
        run(&opts).await.unwrap();
        let first = collect(&opts).unwrap();
        assert_eq!(first.summary.pages, 2);
        let original = parts(&opts)
            .iter()
            .map(|p| {
                (
                    p.clone(),
                    std::fs::read(p).unwrap(),
                    std::fs::metadata(p).unwrap().modified().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let again = collect(&opts).unwrap();
        assert_eq!(again.summary, first.summary);
        assert_eq!(again.pages, first.pages);
        assert_eq!(again.crops, first.crops);
        for (path, bytes, modified) in original {
            assert_eq!(std::fs::read(&path).unwrap(), bytes);
            assert_eq!(
                std::fs::metadata(path).unwrap().modified().unwrap(),
                modified
            );
        }
        let mut uncached = Options {
            cache: None,
            ..opts
        };
        let fresh = collect(&uncached).unwrap();
        assert_eq!(fresh.pages, first.pages);
        assert_eq!(fresh.crops, first.crops);
        // A newly canonical QUALITY result is collected even though the earlier unit was cached.
        std::fs::remove_file(uncached.spool.join(format!(
            "{}__file_{}",
            job.workunit(),
            crate::quality::RESULT_FILE
        )))
        .unwrap();
        uncached.cache = Some(dir.path().join("another-cache"));
        assert_eq!(collect(&uncached).unwrap().summary.pending, 1);
        canonical(&unit, &job, &uncached.spool);
        let error = collect(&uncached).err().unwrap().to_string();
        assert!(error.contains("1 deferred"), "{error}");
        assert_eq!(collect(&uncached).unwrap().summary.pages, 2);
    }

    #[tokio::test]
    async fn incremental_late_canonical_qwen_refreshes_join_and_retains_prior_parts() {
        use usnm_ocr_contract::boinc::sha256;
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, quality) = cached_fixture(dir.path()).await;
        let qunit = units(&opts.qwen_root).join("page");
        let first = dir.path().join("staged-qwen-first");
        let mut job = crate::qwen::stage::local_unit(
            &unit.join(PAGE_FILE),
            &unit.join(ALTO_FILE),
            &first,
            crate::qwen::stage::tests::engine(),
            quality.crop_seed,
            quality.k_crops,
            600,
            4,
        )
        .unwrap();
        job.doc_id = quality.doc_id.clone();
        job.id = job.identity().unwrap();
        std::fs::write(
            first.join(crate::qwen::JOB_FILE),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        let second = dir.path().join("staged-qwen-second");
        std::fs::create_dir_all(&second).unwrap();
        std::fs::copy(first.join(PAGE_FILE), second.join(PAGE_FILE)).unwrap();
        let mut peer = job.clone();
        peer.doc_id = "second".into();
        peer.id = peer.identity().unwrap();
        std::fs::write(
            second.join(crate::qwen::JOB_FILE),
            serde_json::to_vec(&peer).unwrap(),
        )
        .unwrap();
        let job = crate::qwen::stage::write_bundle(&qunit, &[first, second]).unwrap();
        assert_eq!(job.pages.len(), 2);
        let before = collect(&opts).unwrap();
        let mut previous_key = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        assert_eq!(before.summary.with_readings, 0);
        assert!(before.crops.iter().all(|r| r.cer_vs_vlm.is_none()));
        std::fs::create_dir_all(&opts.qwen_spool).unwrap();
        for text in [
            "the council reads this crop",
            "a different late canonical text",
        ] {
            let readings = Readings {
                v: qwen::VERSION,
                job_id: job.id.clone(),
                device: "cuda".into(),
                diag: Default::default(),
                model: Some("qwen3-vl-8b-q4km".into()),
                outcome: qwen::Outcome::Ok,
                prompt_version: qwen::PROMPT_VERSION,
                readings: job
                    .crops
                    .iter()
                    .map(|c| qwen::Reading {
                        id: c.id.clone(),
                        ms: 1,
                        text: text.into(),
                        tokens: 3,
                    })
                    .collect(),
            };
            let bytes = serde_json::to_vec(&readings).unwrap();
            let manifest = qwen::ResultManifest {
                v: qwen::VERSION,
                job_id: job.id.clone(),
                completed_at: "2026-10-14T00:00:00Z".into(),
                crops: job.crops.len() as u32,
                device: "cuda".into(),
                diag_bytes: 4,
                diag_sha256: sha256(b"diag"),
                engine: job.engine.clone(),
                engine_ms: 5,
                gpu_mem_peak_mib: None,
                host: Default::default(),
                load_ms: 1,
                model: Some("qwen3-vl-8b-q4km".into()),
                outcome: qwen::Outcome::Ok,
                peak_rss_kib: None,
                read: job.crops.len() as u32,
                readings_bytes: bytes.len() as u64,
                readings_sha256: sha256(&bytes),
                unit_error: None,
            };
            for (name, bytes) in [
                (crate::qwen::READINGS_FILE, bytes),
                (
                    crate::qwen::RESULT_FILE,
                    serde_json::to_vec(&manifest).unwrap(),
                ),
                (crate::qwen::DIAG_FILE, b"diag".to_vec()),
            ] {
                std::fs::write(
                    opts.qwen_spool
                        .join(format!("{}__file_{name}", job.workunit())),
                    bytes,
                )
                .unwrap();
            }
            let after = collect(&opts).unwrap();
            let key = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
            assert_ne!(key, previous_key);
            previous_key = key;
            assert_eq!(after.summary.with_readings, 1);
            assert!(after.crops.iter().any(|r| r.cer_vs_vlm.is_some()));
            assert_ne!(after.crops, before.crops);
            let fresh = collect(&Options {
                cache: None,
                ..opts.clone()
            })
            .unwrap();
            assert_eq!(after.pages, fresh.pages);
            assert_eq!(after.crops, fresh.crops);
        }
        assert_eq!(parts(&opts).len(), 3);
    }

    /// A single-page `usnm_qwen` unit for the fixture's page, with a canonical
    /// result reading `texts` crop by crop. Returns the job and the result's
    /// readings digest.
    fn qwen_canonical(
        opts: &Options,
        unit: &Path,
        quality: &Job,
        texts: &[&str],
    ) -> (qwen::Job, String) {
        qwen_canonical_in(opts, &opts.qwen_spool, None, unit, quality, texts)
    }

    /// [`qwen_canonical`] for a job pinned to `model`, its result in `spool`.
    fn qwen_canonical_in(
        opts: &Options,
        spool: &Path,
        model: Option<&str>,
        unit: &Path,
        quality: &Job,
        texts: &[&str],
    ) -> (qwen::Job, String) {
        use usnm_ocr_contract::boinc::sha256;
        let qunit = units(&opts.qwen_root).join("page");
        let mut job = crate::qwen::stage::local_unit(
            &unit.join(PAGE_FILE),
            &unit.join(ALTO_FILE),
            &qunit,
            crate::qwen::stage::tests::engine(),
            quality.crop_seed,
            quality.k_crops,
            600,
            4,
        )
        .unwrap();
        job.doc_id = quality.doc_id.clone();
        job.model = model.map(Into::into);
        let prompt_version = if model.is_some() {
            usnm_ocr_contract::qwen::PROMPT_VERSION_XIAOMI
        } else {
            qwen::PROMPT_VERSION
        };
        job.prompt_version = prompt_version;
        job.id = job.identity().unwrap();
        std::fs::write(
            qunit.join(crate::qwen::JOB_FILE),
            serde_json::to_vec(&job).unwrap(),
        )
        .unwrap();
        assert!(job.crops.len() >= 2, "the fixture cuts two crops or more");
        let readings = Readings {
            v: qwen::VERSION,
            job_id: job.id.clone(),
            device: "cuda".into(),
            diag: Default::default(),
            model: Some(model.unwrap_or("qwen3-vl-8b-q4km").into()),
            outcome: qwen::Outcome::Ok,
            prompt_version,
            readings: job
                .crops
                .iter()
                .enumerate()
                .map(|(i, c)| qwen::Reading {
                    id: c.id.clone(),
                    ms: 1,
                    text: texts[i % texts.len()].into(),
                    tokens: 3,
                })
                .collect(),
        };
        let bytes = serde_json::to_vec(&readings).unwrap();
        let digest = sha256(&bytes);
        let manifest = qwen::ResultManifest {
            v: qwen::VERSION,
            job_id: job.id.clone(),
            completed_at: "2026-11-04T00:00:00Z".into(),
            crops: job.crops.len() as u32,
            device: "cuda".into(),
            diag_bytes: 4,
            diag_sha256: sha256(b"diag"),
            engine: job.engine.clone(),
            engine_ms: 5,
            gpu_mem_peak_mib: None,
            host: Default::default(),
            load_ms: 1,
            model: Some(model.unwrap_or("qwen3-vl-8b-q4km").into()),
            outcome: qwen::Outcome::Ok,
            peak_rss_kib: None,
            read: job.crops.len() as u32,
            readings_bytes: bytes.len() as u64,
            readings_sha256: digest.clone(),
            unit_error: None,
        };
        std::fs::create_dir_all(spool).unwrap();
        for (name, bytes) in [
            (crate::qwen::READINGS_FILE, bytes),
            (
                crate::qwen::RESULT_FILE,
                serde_json::to_vec(&manifest).unwrap(),
            ),
            (crate::qwen::DIAG_FILE, b"diag".to_vec()),
        ] {
            std::fs::write(
                spool.join(format!("{}__file_{name}", job.workunit())),
                bytes,
            )
            .unwrap();
        }
        (job, digest)
    }

    /// Writes a crop-state record as `qwen compare --crop-states` does.
    fn crop_record(
        dir: &Path,
        name: &str,
        job: &qwen::Job,
        digests: [&str; 2],
        agree: bool,
        basis: &str,
        crops: &[(&str, &str)],
    ) {
        let parent = dir.join(&job.id);
        std::fs::create_dir_all(&parent).unwrap();
        let record = serde_json::json!({
            "v": crate::qwen::consensus::RECORD_VERSION,
            "rule": "crop-consensus-v2",
            "basis": basis,
            "job_id": job.id,
            "results": ["r_a", "r_b"],
            "readings_sha256": digests,
            "agree": agree,
            "crops": crops.iter().map(|(id, state)| serde_json::json!({"id": id, "state": state, "score": null})).collect::<Vec<_>>(),
            "accuracy_established": false,
        });
        std::fs::write(
            parent.join(format!("{name}.json")),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
    }

    fn vlm_rows(c: &Collected) -> Vec<&CropRow> {
        c.crops.iter().filter(|r| r.reader == VLM).collect()
    }

    /// A Xiaomi-OCR-0 job reads only the batch-108 spool and any other job only the batch-8
    /// one, so a page's reading is never the other reader's (#1776).
    #[tokio::test]
    async fn xiaomi_results_join_from_their_own_spool_only() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        opts.cache = None;
        let xiaomi_spool = dir.path().join("xiaomi-spool");
        let (job, _) = qwen_canonical_in(
            &opts,
            &xiaomi_spool,
            Some(crate::qwen::XIAOMI_MODEL),
            &unit,
            &quality,
            &["the xiaomi reading"],
        );
        // No batch-108 spool configured, or the result is in the batch-8 one only: no reading.
        assert_eq!(collect(&opts).unwrap().summary.with_readings, 0);
        let stray = Options {
            qwen_spool: xiaomi_spool.clone(),
            ..opts.clone()
        };
        assert_eq!(collect(&stray).unwrap().summary.with_readings, 0);
        // Configured, it joins.
        opts.qwen_xiaomi_spool = Some(xiaomi_spool.clone());
        let got = collect(&opts).unwrap();
        assert_eq!(got.summary.with_readings, 1);
        assert!(got
            .crops
            .iter()
            .filter(|r| r.vlm_text.is_some())
            .all(|r| r.vlm_text.as_deref() == Some("the xiaomi reading")
                && r.vlm_model.as_deref() == Some(crate::qwen::XIAOMI_MODEL)));
        // A Qwen job's result in the batch-108 spool is not read.
        let dir2 = tempfile::tempdir_in(".").unwrap();
        let (mut qopts, qunit, qquality) = cached_fixture(dir2.path()).await;
        qopts.cache = None;
        let qx = dir2.path().join("xiaomi-spool");
        qwen_canonical_in(&qopts, &qx, None, &qunit, &qquality, &["a qwen reading"]);
        qopts.qwen_xiaomi_spool = Some(qx);
        assert_eq!(collect(&qopts).unwrap().summary.with_readings, 0);
        assert_eq!(job.model.as_deref(), Some(crate::qwen::XIAOMI_MODEL));
    }

    #[tokio::test]
    async fn readings_join_only_on_crops_an_agreeing_pair_vouched_for() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        let (job, digest) =
            qwen_canonical(&opts, &unit, &quality, &["the council reads this crop"]);
        let crop_ids: Vec<String> = job.crops.iter().map(|c| c.id.clone()).collect();
        let states = dir.path().join("crop-states");
        opts.cache = None;
        // Without crop states, every crop joins and nothing is said about it.
        let plain = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&plain).len(), crop_ids.len());
        assert!(plain.crops.iter().all(|r| r.vlm_crop_state.is_none()));
        // With the directory and no record, the strict rule accepted the result.
        opts.qwen_crop_states = Some(states.clone());
        let strict = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&strict).len(), crop_ids.len());
        assert!(strict
            .crops
            .iter()
            .all(|r| r.vlm_crop_state.as_deref() == Some("strict-v1")));
        assert_eq!(strict.summary.with_readings, 1);
        // A disagreeing pair and an agreeing pair of other results vouch for nothing here.
        let all_agree: Vec<(&str, &str)> = crop_ids.iter().map(|c| (c.as_str(), "agree")).collect();
        // A pair that included this result but did not agree judged every crop
        // a disagreement here, so it vouches for none (and is not the strict
        // every-crop vouch either).
        let all_disagree: Vec<(&str, &str)> =
            crop_ids.iter().map(|c| (c.as_str(), "disagree")).collect();
        crop_record(
            &states,
            "a",
            &job,
            [&digest, "other"],
            false,
            "crop-consensus-v2",
            &all_disagree,
        );
        crop_record(
            &states,
            "b",
            &job,
            ["other", "else"],
            true,
            "crop-consensus-v2",
            &all_agree,
        );
        let still = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&still).len(), 0);
        // An agreeing pair that includes the canonical result vouches for its
        // `agree` crops only; a looped crop and an unjudged one lose the reading.
        crop_record(
            &states,
            "c",
            &job,
            ["peer", &digest],
            true,
            "crop-consensus-v2",
            &[
                (crop_ids[0].as_str(), "agree"),
                (crop_ids[1].as_str(), "looped"),
            ],
        );
        let v2 = collect(&opts).unwrap();
        let vlm = vlm_rows(&v2);
        assert_eq!(vlm.len(), 1);
        assert_eq!(vlm[0].crop_id, crop_ids[0]);
        assert_eq!(v2.summary.with_readings, 1);
        let state = |c: &Collected, id: &str| {
            c.crops
                .iter()
                .filter(|r| r.crop_id == id)
                .map(|r| {
                    (
                        r.vlm_crop_state.clone(),
                        r.vlm_text.is_some(),
                        r.vlm_reading_present,
                    )
                })
                .collect::<Vec<_>>()
        };
        assert!(state(&v2, &crop_ids[0])
            .iter()
            .all(|s| *s == (Some("agree".into()), true, Some(true))));
        assert!(state(&v2, &crop_ids[1])
            .iter()
            .all(|s| *s == (Some("looped".into()), false, Some(false))));
        for id in &crop_ids[2..] {
            assert!(state(&v2, id)
                .iter()
                .all(|s| *s == (Some("unjudged".into()), false, Some(false))));
        }
        // No other reader's row is lost, and none is compared with a withheld reading.
        assert_eq!(
            v2.crops.iter().filter(|r| r.reader != VLM).count(),
            strict.crops.iter().filter(|r| r.reader != VLM).count()
        );
        assert!(v2
            .crops
            .iter()
            .filter(|r| r.crop_id != crop_ids[0])
            .all(|r| r.cer_vs_vlm.is_none()));
        // A second agreeing pair adds what it vouched for.
        crop_record(
            &states,
            "d",
            &job,
            [&digest, "third"],
            true,
            "crop-consensus-v2",
            &[(crop_ids[1].as_str(), "agree")],
        );
        assert_eq!(vlm_rows(&collect(&opts).unwrap()).len(), 2);
        // A pair the strict rule decided (they did not both finish) covers every crop.
        crop_record(
            &states,
            "e",
            &job,
            [&digest, "fourth"],
            true,
            "strict-v1",
            &[],
        );
        assert_eq!(vlm_rows(&collect(&opts).unwrap()).len(), crop_ids.len());
        // A record that cannot be read stops the collection rather than guess.
        std::fs::write(states.join(&job.id).join("f.json"), b"{").unwrap();
        assert!(collect(&opts).is_err());
    }

    /// An excluded unit joins no reading, an excluded crop joins none of its own, the rest of
    /// the page's rows stay, a release brings them back, and each change re-keys the unit.
    #[tokio::test]
    async fn excluded_units_and_crops_join_no_reading_until_released() {
        use crate::quality::exclusions::Exclusions;
        use usnm_ocr_local::quality_db::ExcludedQwenReading;
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        let (job, _) = qwen_canonical(&opts, &unit, &quality, &["the council reads this crop"]);
        let all = job.crops.len();
        let row = |crop: Option<&str>| ExcludedQwenReading {
            job_id: job.id.clone(),
            wu_id: 1,
            crop_id: crop.map(Into::into),
        };
        let open = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        let plain = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&plain).len(), all);
        // One crop: its row has no reading, every other crop keeps its own.
        let first = job.crops[0].id.clone();
        opts.qwen_exclusions = Exclusions::from_rows([row(Some(&first))]);
        let one = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&one).len(), all - 1);
        assert!(vlm_rows(&one).iter().all(|r| r.crop_id != first));
        let crop_key = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        assert_ne!(crop_key, open);
        // The whole unit: no reading at all, but the page and its crops are still rows.
        opts.qwen_exclusions = Exclusions::from_rows([row(None)]);
        let whole = collect(&opts).unwrap();
        assert!(vlm_rows(&whole).is_empty());
        assert_eq!(whole.summary.with_readings, 0);
        assert_eq!(whole.pages.len(), plain.pages.len());
        assert_ne!(
            inventory(&opts, None).unwrap()["page"].fingerprint,
            crop_key
        );
        // Every crop excluded: no reading is joined, so the page counts none.
        opts.qwen_exclusions =
            Exclusions::from_rows(job.crops.iter().map(|c| row(Some(c.id.as_str()))));
        let none = collect(&opts).unwrap();
        assert!(vlm_rows(&none).is_empty());
        assert_eq!(none.summary.with_readings, 0);
        // Another job's exclusion changes nothing here.
        opts.qwen_exclusions = Exclusions::from_rows([ExcludedQwenReading {
            job_id: "0".repeat(64),
            wu_id: 2,
            crop_id: None,
        }]);
        assert_eq!(vlm_rows(&collect(&opts).unwrap()).len(), all);
        // Released (the database no longer returns the row): the readings are back.
        opts.qwen_exclusions = Exclusions::default();
        let released = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&released).len(), all);
        assert_eq!(released.crops, plain.crops);
        assert_eq!(inventory(&opts, None).unwrap()["page"].fingerprint, open);
    }

    /// Exclusions on a bundle: a whole-unit one joins nothing, a page-prefixed crop one only
    /// loses its own crop (from accepted page records and from a canonical result alike), and a
    /// page with every crop excluded counts no joined reading.
    #[tokio::test]
    async fn excluded_bundle_units_and_page_crops_join_no_reading() {
        use crate::quality::exclusions::Exclusions;
        use crate::qwen::{ledger::tests::result, validate::compare};
        use usnm_ocr_local::quality_db::ExcludedQwenReading;
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        let (_bundle, job) = qwen_bundle(&opts, &unit, &quality);
        let records = dir.path().join("qwen-pages");
        let results = dir.path().join("results");
        std::fs::create_dir_all(&results).unwrap();
        let ours = quality.doc_id.clone();
        let words = |_: &str, _: &str| "the council reads this crop".to_owned();
        let a = result(&results, &job, "a", words);
        let b = result(&results, &job, "b", words);
        compare(&a, &b, 0.5, None, Some(&records)).unwrap();
        opts.qwen_pages = Some(records);
        opts.cache = None;
        let page_crops: Vec<String> = job.pages[0].crops.iter().map(|c| c.id.clone()).collect();
        let row = |crop: Option<String>| ExcludedQwenReading {
            job_id: job.id.clone(),
            wu_id: 1,
            crop_id: crop,
        };
        let plain = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&plain).len(), page_crops.len());
        // The whole unit.
        opts.qwen_exclusions = Exclusions::from_rows([row(None)]);
        let c = collect(&opts).unwrap();
        assert!(vlm_rows(&c).is_empty());
        assert_eq!(c.summary.with_readings, 0);
        // One crop, named with its page.
        opts.qwen_exclusions =
            Exclusions::from_rows([row(Some(format!("{ours}:{}", page_crops[0])))]);
        let c = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&c).len(), page_crops.len() - 1);
        assert!(vlm_rows(&c).iter().all(|r| r.crop_id != page_crops[0]));
        assert_eq!(c.summary.with_readings, 1);
        // Every crop of the page: nothing joined, and the page does not count.
        opts.qwen_exclusions =
            Exclusions::from_rows(page_crops.iter().map(|c| row(Some(format!("{ours}:{c}")))));
        let c = collect(&opts).unwrap();
        assert!(vlm_rows(&c).is_empty());
        assert_eq!(c.summary.with_readings, 0);
        // A canonical result is filtered the same way, after it has been accepted.
        let canonical = result(&results, &job, "z", |_, _| "the canonical reading".into());
        std::fs::create_dir_all(&opts.qwen_spool).unwrap();
        for (from, name) in canonical.iter().zip(crate::qwen::outputs()) {
            std::fs::copy(
                from,
                opts.qwen_spool
                    .join(format!("{}__file_{name}", job.workunit())),
            )
            .unwrap();
        }
        opts.qwen_pages = None;
        opts.qwen_exclusions = Exclusions::default();
        assert_eq!(vlm_rows(&collect(&opts).unwrap()).len(), page_crops.len());
        opts.qwen_exclusions =
            Exclusions::from_rows([row(Some(format!("{ours}:{}", page_crops[0])))]);
        let c = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&c).len(), page_crops.len() - 1);
        assert_eq!(c.summary.with_readings, 1);
    }

    #[tokio::test]
    async fn a_page_with_no_vouched_crop_has_no_readings() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        let (job, digest) = qwen_canonical(&opts, &unit, &quality, &["[?]"]);
        let states = dir.path().join("crop-states");
        opts.cache = None;
        opts.qwen_crop_states = Some(states.clone());
        let crops: Vec<(&str, &str)> = job
            .crops
            .iter()
            .map(|c| (c.id.as_str(), "no_text"))
            .collect();
        crop_record(
            &states,
            "a",
            &job,
            [&digest, "peer"],
            true,
            "crop-consensus-v2",
            &crops,
        );
        let c = collect(&opts).unwrap();
        assert_eq!(c.summary.with_readings, 0);
        assert!(vlm_rows(&c).is_empty());
        assert!(c
            .crops
            .iter()
            .all(|r| r.vlm_crop_state.as_deref() == Some("no_text")));
    }

    /// A two-page `usnm_qwen` bundle staged as `qwen stage --bundle-size 2`
    /// stages one: the fixture's page then another, each page unit under the
    /// root's `pages/`, with no canonical result.
    fn qwen_bundle(opts: &Options, unit: &Path, quality: &Job) -> (PathBuf, qwen::Job) {
        let mut dirs = Vec::new();
        for doc in [quality.doc_id.as_str(), "sn00000000_1900-01-01_ed-1_seq-2"] {
            let page = opts.qwen_root.join("pages").join(doc);
            let mut job = crate::qwen::stage::local_unit(
                &unit.join(PAGE_FILE),
                &unit.join(ALTO_FILE),
                &page,
                crate::qwen::stage::tests::engine(),
                quality.crop_seed,
                quality.k_crops,
                600,
                4,
            )
            .unwrap();
            job.doc_id = doc.into();
            job.id = job.identity().unwrap();
            std::fs::write(
                page.join(crate::qwen::JOB_FILE),
                serde_json::to_vec(&job).unwrap(),
            )
            .unwrap();
            dirs.push(page);
        }
        let bundle = units(&opts.qwen_root).join(format!("bundle-00000-{}", quality.doc_id));
        let job = crate::qwen::stage::write_bundle(&bundle, &dirs).unwrap();
        (bundle, job)
    }

    #[tokio::test]
    async fn bundle_pages_two_hosts_agreed_on_join_without_a_canonical_result() {
        use crate::qwen::{ledger::tests::result, validate::compare};
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        let (bundle, job) = qwen_bundle(&opts, &unit, &quality);
        let records = dir.path().join("qwen-pages");
        let results = dir.path().join("results");
        std::fs::create_dir_all(&results).unwrap();
        let ours = quality.doc_id.clone();
        let words = |_: &str, _: &str| "the council reads this crop".to_owned();
        // Host b loops on the fixture's page; both read the other page alike.
        let a = result(&results, &job, "a", words);
        let b = result(&results, &job, "b", |doc, crop| {
            if doc == ours {
                "[?] ".repeat(30)
            } else {
                words(doc, crop)
            }
        });
        assert!(compare(&a, &b, 0.5, None, Some(&records)).is_err());
        let fresh = |opts: &Options| {
            collect(&Options {
                cache: None,
                ..opts.clone()
            })
            .unwrap()
        };
        let key = |opts: &Options| inventory(opts, None).unwrap()["page"].fingerprint.clone();

        // Off by default: only canonical results join, and the key is as before.
        let off = key(&opts);
        assert_eq!(collect(&opts).unwrap().summary.with_readings, 0);
        opts.qwen_pages = Some(records.clone());
        let disagreed = key(&opts);
        assert_ne!(disagreed, off);
        // A page the pair disagreed on does not join.
        let c = collect(&opts).unwrap();
        assert_eq!(c.summary.with_readings, 0);
        assert!(vlm_rows(&c).is_empty());

        // A late third result agrees with a: the record changes the key and the page joins.
        let late = result(&results, &job, "c", words);
        compare(&a, &late, 0.5, None, Some(&records)).unwrap();
        let agreed = key(&opts);
        assert_ne!(agreed, disagreed);
        let c = collect(&opts).unwrap();
        assert_eq!(c.summary.with_readings, 1);
        let page_crops = job.pages[0].crops.len();
        assert_eq!(vlm_rows(&c).len(), page_crops);
        assert!(vlm_rows(&c).iter().all(|r| {
            r.vlm_text.as_deref() == Some("the council reads this crop")
                && r.vlm_device.as_deref() == Some("vulkan")
        }));
        assert!(c
            .crops
            .iter()
            .all(|r| r.vlm_crop_state.as_deref() == Some("page-strict-v1")));
        assert_eq!(c.crops, fresh(&opts).crops);
        assert_eq!(c.pages, fresh(&opts).pages);

        // A record added during collection is caught before publication.
        let cache =
            super::super::collect_cache::Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        let other = result(&results, &job, "d", words);
        compare(&a, &other, 0.5, None, Some(&records)).unwrap();
        assert!(cache.verify_readings().is_err());
        // The run checks the page's own inputs: it publishes the page but caches no part.
        assert!(!cache.readings_unchanged(&ours).unwrap());
        assert!(cache
            .readings_unchanged("sn00000000_1900-01-01_ed-9_seq-9")
            .unwrap());

        // A membership that no longer matches the bundle keeps no page.
        let membership = bundle.join(crate::qwen::stage::MEMBERSHIP_FILE);
        let original = std::fs::read(&membership).unwrap();
        std::fs::write(&membership, b"{").unwrap();
        assert_ne!(key(&opts), agreed);
        assert!(vlm_rows(&collect(&opts).unwrap()).is_empty());
        std::fs::write(&membership, &original).unwrap();
        // So does a member page unit gone missing: it is keyed as missing, not an error.
        let member = opts
            .qwen_root
            .join("pages")
            .join(&ours)
            .join(crate::qwen::JOB_FILE);
        let moved = member.with_file_name("moved.json");
        let present = key(&opts);
        std::fs::rename(&member, &moved).unwrap();
        assert_ne!(key(&opts), present);
        assert!(vlm_rows(&collect(&opts).unwrap()).is_empty());
        std::fs::rename(&moved, &member).unwrap();
        assert_eq!(key(&opts), present);

        // A canonical result, when there is one, is joined instead.
        let canonical = result(&results, &job, "z", |_, _| "the canonical reading".into());
        std::fs::create_dir_all(&opts.qwen_spool).unwrap();
        for (from, name) in canonical.iter().zip(crate::qwen::outputs()) {
            std::fs::copy(
                from,
                opts.qwen_spool
                    .join(format!("{}__file_{name}", job.workunit())),
            )
            .unwrap();
        }
        let c = collect(&opts).unwrap();
        let rows = vlm_rows(&c);
        assert_eq!(rows.len(), page_crops);
        assert!(rows
            .iter()
            .all(|r| r.vlm_text.as_deref() == Some("the canonical reading")));
        assert!(c.crops.iter().all(|r| r.vlm_crop_state.is_none()));
        assert_eq!(c.crops, fresh(&opts).crops);

        // A `page-v1` pair settles the bundle (one page of two agreed): its
        // canonical result no longer joins, only the accepted pages do.
        let canonical_key = key(&opts);
        crate::qwen::validate::compare_with(
            &a,
            &b,
            crate::qwen::validate::Compare {
                min_dice: 0.5,
                rule: crate::qwen::validate::Rule::StrictV1,
                bundle_verdict: crate::qwen::validate::BundleVerdict::PageV1,
                log: None,
                pages: Some(&records),
                crop_states: None,
            },
        )
        .unwrap();
        assert_ne!(key(&opts), canonical_key);
        let c = collect(&opts).unwrap();
        let rows = vlm_rows(&c);
        assert_eq!(rows.len(), page_crops);
        assert!(rows
            .iter()
            .all(|r| r.vlm_text.as_deref() == Some("the council reads this crop")));
        assert!(c
            .crops
            .iter()
            .all(|r| r.vlm_crop_state.as_deref() == Some("page-strict-v1")));
        // Without the later pairs' agreeing records the page is disagreed and,
        // the bundle being settled by its pages, joins nothing.
        let page_dir = records.join(&job.id).join(&ours);
        for e in std::fs::read_dir(&page_dir).unwrap() {
            let path = e.unwrap().path();
            let r: serde_json::Value =
                serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            if r["status"] == crate::qwen::validate::PAGE_AGREED {
                std::fs::remove_file(&path).unwrap();
            }
        }
        let c = collect(&opts).unwrap();
        assert!(vlm_rows(&c).is_empty());
        assert_eq!(c.summary.with_readings, 0);
    }

    #[tokio::test]
    async fn a_retried_page_takes_its_later_bundles_canonical_reading() {
        use crate::qwen::{
            ledger::tests::result,
            validate::{compare_with, BundleVerdict, Compare, Rule},
        };
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        opts.cache = None;
        let (_, job) = qwen_bundle(&opts, &unit, &quality);
        let records = dir.path().join("qwen-pages");
        opts.qwen_pages = Some(records.clone());
        let results = dir.path().join("results");
        std::fs::create_dir_all(&results).unwrap();
        let ours = quality.doc_id.clone();
        let words = |_: &str, _: &str| "the council reads this crop".to_owned();
        let a = result(&results, &job, "a", words);
        let b = result(&results, &job, "b", |doc, crop| {
            if doc == ours {
                "[?] ".repeat(30)
            } else {
                words(doc, crop)
            }
        });
        let page_v1 = || Compare {
            min_dice: 0.5,
            rule: Rule::StrictV1,
            bundle_verdict: BundleVerdict::PageV1,
            log: None,
            pages: Some(&records),
            crop_states: None,
        };
        // The first bundle settles by its other page; ours is disagreed and joins nothing.
        compare_with(&a, &b, page_v1()).unwrap();
        assert!(vlm_rows(&collect(&opts).unwrap()).is_empty());
        // Ours packed again on its own; that bundle's pair agrees under the unit rule.
        let page = opts.qwen_root.join("pages").join(&ours);
        let retry = crate::qwen::stage::write_bundle(
            &units(&opts.qwen_root).join(format!("bundle-00001-{ours}")),
            &[page],
        )
        .unwrap();
        let canonical = result(&results, &retry, "z", |_, _| "the retry reading".into());
        std::fs::create_dir_all(&opts.qwen_spool).unwrap();
        for (from, name) in canonical.iter().zip(crate::qwen::outputs()) {
            std::fs::copy(
                from,
                opts.qwen_spool
                    .join(format!("{}__file_{name}", retry.workunit())),
            )
            .unwrap();
        }
        let rows = |opts: &Options| {
            vlm_rows(&collect(opts).unwrap())
                .iter()
                .map(|r| r.vlm_text.clone().unwrap())
                .collect::<std::collections::BTreeSet<_>>()
        };
        assert_eq!(rows(&opts), ["the retry reading".to_owned()].into());
        // A late pair of the first bundle agreeing on ours does not undo the retry's reading.
        let late = result(&results, &job, "c", words);
        compare_with(&a, &late, page_v1()).unwrap();
        assert_eq!(rows(&opts), ["the retry reading".to_owned()].into());
        // A canonical result that read none of the page's crops (a timed-out pair) replaces nothing.
        let mut empty: qwen::Readings =
            serde_json::from_slice(&std::fs::read(&canonical[0]).unwrap()).unwrap();
        empty.readings.clear();
        empty.outcome = qwen::Outcome::Timeout;
        let bytes = serde_json::to_vec(&empty).unwrap();
        let mut manifest: qwen::ResultManifest =
            serde_json::from_slice(&std::fs::read(&canonical[1]).unwrap()).unwrap();
        manifest.read = 0;
        manifest.readings_bytes = bytes.len() as u64;
        manifest.readings_sha256 = usnm_ocr_contract::boinc::sha256(&bytes);
        manifest.outcome = qwen::Outcome::Timeout;
        let spool = |name: &str| {
            opts.qwen_spool
                .join(format!("{}__file_{name}", retry.workunit()))
        };
        let names = crate::qwen::outputs();
        std::fs::write(spool(names[0]), &bytes).unwrap();
        std::fs::write(spool(names[1]), serde_json::to_vec(&manifest).unwrap()).unwrap();
        assert_eq!(
            rows(&opts),
            ["the council reads this crop".to_owned()].into()
        );
    }

    #[tokio::test]
    async fn page_records_leave_single_page_keys_alone() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (opts, unit, quality) = cached_fixture(dir.path()).await;
        qwen_canonical(&opts, &unit, &quality, &["the council reads this crop"]);
        let key = |opts: &Options| inventory(opts, None).unwrap()["page"].fingerprint.clone();
        let pages = Options {
            qwen_pages: Some(dir.path().join("qwen-pages")),
            ..opts.clone()
        };
        assert_eq!(key(&pages), key(&opts));
    }

    #[tokio::test]
    async fn crop_states_are_part_of_the_row_cache_keys() {
        let dir = tempfile::tempdir_in(".").unwrap();
        let (mut opts, unit, quality) = cached_fixture(dir.path()).await;
        let (job, digest) =
            qwen_canonical(&opts, &unit, &quality, &["the council reads this crop"]);
        let states = dir.path().join("crop-states");
        let without = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        opts.qwen_crop_states = Some(states.clone());
        // Turning crop states on changes every key.
        let empty = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        assert_ne!(empty, without);
        let first = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&first).len(), job.crops.len());
        // A record arriving later changes the key, so the cached rows are not reused.
        crop_record(
            &states,
            "a",
            &job,
            [&digest, "peer"],
            true,
            "crop-consensus-v2",
            &[(job.crops[0].id.as_str(), "agree")],
        );
        let recorded = inventory(&opts, None).unwrap()["page"].fingerprint.clone();
        assert_ne!(recorded, empty);
        let cached = collect(&opts).unwrap();
        assert_eq!(vlm_rows(&cached).len(), 1);
        let fresh = collect(&Options {
            cache: None,
            ..opts.clone()
        })
        .unwrap();
        assert_eq!(cached.crops, fresh.crops);
        assert_eq!(cached.pages, fresh.pages);
        // A record that changes in place changes the key too.
        let path = states.join(&job.id).join("a.json");
        let mut record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        record["crops"][0]["state"] = "looped".into();
        std::fs::write(&path, serde_json::to_vec(&record).unwrap()).unwrap();
        assert_ne!(
            inventory(&opts, None).unwrap()["page"].fingerprint,
            recorded
        );
        assert!(vlm_rows(&collect(&opts).unwrap()).is_empty());
        // A record added during collection is caught before publication.
        let cache =
            super::super::collect_cache::Cache::new(opts.cache.as_ref().unwrap(), &opts).unwrap();
        crop_record(
            &states,
            "b",
            &job,
            ["x", "y"],
            false,
            "crop-consensus-v2",
            &[],
        );
        assert!(cache.verify_readings().is_err());
    }

    /// A bundle unit collects the same rows from its dims.json as from its
    /// image, with the image or after it is pruned; with neither, collect
    /// says it needs page.jpg. Pruning leaves the unit's keys as they were.
    #[tokio::test]
    async fn bundle_pages_collect_the_same_from_dims_json_without_the_image() {
        use crate::quality::page_dims::{PageDims, DIMS_FILE};
        let tmp = tempfile::tempdir().unwrap();
        let qroot = tmp.path().join("quality");
        let (unit, job) = crate::quality::page_dims::tests::bundle(&qroot);
        std::fs::write(unit.join("submitted"), b"").unwrap();
        crate::quality::work::run(&unit).await.unwrap();
        let spool = tmp.path().join("spool");
        canonical(&unit, &job, &spool);
        // A Surya row on a crop of each page, so each page is cut at its own size.
        let (sroot, sspool) = (tmp.path().join("surya"), tmp.path().join("surya-spool"));
        let page_blob = std::fs::read(unit.join(PAGE_FILE)).unwrap();
        let alto_blob = std::fs::read(unit.join(ALTO_FILE)).unwrap();
        let mut cut = Vec::new();
        for bp in &job.pages {
            let crops = crate::qwen::stage::cut(
                bp.inputs.page.extract(&page_blob, "page").unwrap(),
                bp.inputs.alto.extract(&alto_blob, "alto").unwrap(),
                bp.crop_seed,
                bp.k_crops,
            )
            .unwrap();
            let mut base = crate::surya::work::tests::job();
            base.doc_id = bp.doc_id.clone();
            let cj = crate::surya::collect::tests::crop_job(
                &base,
                &crops[0].id,
                crops[0].rect,
                &crops[0].stratum,
            );
            crate::surya::collect::tests::unit_with_result(
                &sroot,
                &sspool,
                &cj,
                &[(
                    usnm_ocr_contract::surya::InputMode::Whole,
                    usnm_ocr_contract::surya::Outcome::Ok,
                    "Surya read this line\n",
                )],
            );
            cut.push(crops);
        }
        // The two pages differ in size, so their crops differ.
        assert_ne!(cut[0][0].rect, cut[1][0].rect);
        let opts = Options {
            campaign: "t".into(),
            root: qroot,
            spool,
            qwen_root: tmp.path().join("no-qwen"),
            qwen_spool: tmp.path().join("no-qwen"),
            qwen_xiaomi_spool: None,
            qwen_crop_states: None,
            qwen_exclusions: Default::default(),
            qwen_pages: None,
            surya_root: Some(sroot),
            surya_spool: Some(sspool),
            out_dir: None,
            curated_url: None,
            cache: None,
            max_dirty_units: 8,
            max_compute: None,
            warm_only: false,
            qe_dir: None,
        };
        let cached = Options {
            cache: Some(tmp.path().join("cache")),
            ..opts.clone()
        };
        let parts = || {
            std::fs::read_dir(tmp.path().join("cache"))
                .unwrap()
                .filter(|e| e.as_ref().unwrap().path().is_file())
                .count()
        };
        let name = unit.file_name().unwrap().to_str().unwrap().to_owned();
        let key = |o: &Options| inventory(o, None).unwrap()[&name].fingerprint.clone();

        // A bundle packed before stage wrote dims.json reads its image.
        let staged = std::fs::read(unit.join(DIMS_FILE)).unwrap();
        std::fs::remove_file(unit.join(DIMS_FILE)).unwrap();
        let legacy = collect(&opts).unwrap();
        assert_eq!((legacy.summary.pages, legacy.summary.with_surya), (2, 2));
        for (bp, crops) in job.pages.iter().zip(&cut) {
            let rows: Vec<&CropRow> = legacy
                .crops
                .iter()
                .filter(|r| r.doc_id == bp.doc_id)
                .collect();
            assert_eq!(rows.len(), crops.len() * bp.readers.len() + 1);
            let s_row = rows.iter().find(|r| r.reader == SURYA).unwrap();
            assert_eq!(
                (s_row.x0, s_row.y0, s_row.x1, s_row.y1),
                (
                    crops[0].rect[0],
                    crops[0].rect[1],
                    crops[0].rect[2],
                    crops[0].rect[3]
                )
            );
        }
        let legacy_key = key(&cached);

        // Backfilled: the same rows from the record; the key now names
        // the record instead of the image.
        PageDims::of_file(&job, &unit.join(PAGE_FILE), false)
            .unwrap()
            .write(&unit)
            .unwrap();
        assert_eq!(std::fs::read(unit.join(DIMS_FILE)).unwrap(), staged);
        let with_dims = collect(&cached).unwrap();
        assert_eq!(with_dims.summary, legacy.summary);
        assert_eq!(with_dims.pages, legacy.pages);
        assert_eq!(with_dims.crops, legacy.crops);
        let dims_key = key(&cached);
        assert_ne!(dims_key, legacy_key);
        let stored = parts();
        assert!(stored > 0);

        // Pruned: the same rows, the same keys, the stored part reused.
        std::fs::remove_file(unit.join(PAGE_FILE)).unwrap();
        assert_eq!(key(&cached), dims_key);
        let pruned = collect(&cached).unwrap();
        assert_eq!(parts(), stored);
        assert_eq!(pruned.pages, legacy.pages);
        assert_eq!(pruned.crops, legacy.crops);
        let fresh = collect(&opts).unwrap();
        assert_eq!(fresh.pages, legacy.pages);
        assert_eq!(fresh.crops, legacy.crops);

        // The record is in the key: another record is another key.
        let mut other: PageDims = serde_json::from_slice(&staged).unwrap();
        other.pages[1].width += 2;
        other.write(&unit).unwrap();
        assert_ne!(key(&cached), dims_key);
        // A record for other bytes is refused, not used.
        other.pages[1].sha256 = "0".repeat(64);
        other.write(&unit).unwrap();
        let e = format!("{:#}", collect(&opts).err().unwrap());
        assert!(e.contains("dims.json"), "{e}");
        // Neither record nor image: collect names what it needs.
        std::fs::remove_file(unit.join(DIMS_FILE)).unwrap();
        let e = format!("{:#}", collect(&opts).err().unwrap());
        assert!(e.contains("page.jpg"), "{e}");
    }
}
