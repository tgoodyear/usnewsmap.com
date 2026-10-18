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
use usnm_ocr_quality::{align, crops, hat, report::round6};

/// Our ALTO of the page, when a reader of `texts.json` is marked `alto`.
pub const OURS_FILE: &str = "ours.xml";
/// The vision-language reader's name in the crop tables.
pub const VLM: &str = "vlm";

#[derive(Debug, Clone)]
pub struct Options {
    pub campaign: String,
    /// The `usnm_quality` staging root and spool.
    pub root: PathBuf,
    pub spool: PathBuf,
    /// The `usnm_qwen` staging root and spool.
    pub qwen_root: PathBuf,
    pub qwen_spool: PathBuf,
    /// A local directory instead of the curated store.
    pub out_dir: Option<PathBuf>,
    pub curated_url: Option<String>,
}

#[derive(Debug, Default, PartialEq)]
pub struct Summary {
    /// Pages whose `usnm_quality` result is in.
    pub pages: usize,
    /// Of them, pages whose `usnm_qwen` readings are in too.
    pub with_readings: usize,
    /// Submitted units with no canonical result yet.
    pub pending: usize,
    /// Units BOINC gave up on.
    pub failed: usize,
    pub page_rows: usize,
    pub crop_rows: usize,
}

/// One row of `crops.parquet`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct CropRow {
    pub doc_id: String,
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
}

/// What the crop join needs for one page.
pub struct CropInputs<'a> {
    pub doc_id: &'a str,
    /// LoC's ALTO in page pixels.
    pub loc: &'a Page,
    /// Our ALTO in page pixels, when a reader is marked `alto`.
    pub ours: Option<&'a Page>,
    pub texts: &'a Texts,
    /// The reader order of the job.
    pub readers: &'a [String],
    pub crops: &'a [Crop],
    pub readings: Option<&'a Readings>,
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
        let vlm = readings
            .get(crop.id.as_str())
            .filter(|r| !r.text.trim().is_empty());
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
        // The crop-level hat over the readers with text in the box, plus
        // the vision-language reading.
        let mut hat_members: Vec<(&str, &str)> = texts
            .iter()
            .filter_map(|(n, t, _, _, _)| t.as_deref().map(|t| (n.as_str(), t)))
            .collect();
        if let Some(v) = vlm {
            hat_members.push((VLM, &v.text));
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
            crop_id: crop.id.clone(),
            stratum: crop.stratum.clone(),
            x0: crop.rect[0],
            y0: crop.rect[1],
            x1: crop.rect[2],
            y1: crop.rect[3],
            reader,
            text: None,
            vlm_text: vlm.map(|v| v.text.clone()),
            vlm_model: inputs.readings.and_then(|r| r.model.clone()),
            vlm_device: inputs.readings.map(|r| r.device.clone()),
            cer_vs_vlm: None,
            dice_vs_vlm: None,
            hat_est_cer: None,
            hat_readers: hat_members.len(),
            region_overlap: false,
            region_kind: None,
            region_legibility: None,
        };
        for (name, text, overlap, kind, legibility) in &texts {
            let mut row = base(name.clone());
            row.text = text.clone();
            row.region_overlap = *overlap;
            row.region_kind = kind.clone();
            row.region_legibility = legibility.clone();
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
            row.cer_vs_vlm = Some(0.0);
            row.dice_vs_vlm = Some(1.0);
            row.hat_est_cer = est(VLM);
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
        .or_else(|| lay.and_then(|l| l.line_height_px))
        .unwrap_or(34.0)
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
                bg_tile_std: img.map(|i| i.bg_tile_std),
                otsu_local_diff: img.map(|i| i.otsu_local_diff),
                img_columns: img.map(|i| i.img_columns),
                gutter_clarity: img.map(|i| i.gutter_clarity),
                est_line_height_px: img.and_then(|i| i.est_line_height_px),
                vertical_stroke_ratio: img.map(|i| i.vertical_stroke_ratio),
                blocks: lay.map(|l| l.blocks),
                lines: lay.map(|l| l.lines),
                line_words: lay.map(|l| l.words),
                line_height_px: lay.and_then(|l| l.line_height_px),
                columns: lay.map(|l| l.columns),
                mean_word_conf: lay.and_then(|l| l.mean_word_conf),
                total_ms: r.timings.total_ms,
                plan_class: r.host.plan_class.clone(),
                with_readings,
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
                bg_tile_std: img.map(|i| i.bg_tile_std),
                otsu_local_diff: img.map(|i| i.otsu_local_diff),
                img_columns: img.map(|i| i.img_columns),
                gutter_clarity: img.map(|i| i.gutter_clarity),
                est_line_height_px: img.and_then(|i| i.est_line_height_px),
                vertical_stroke_ratio: img.map(|i| i.vertical_stroke_ratio),
                blocks: lay.map(|l| l.blocks),
                lines: lay.map(|l| l.lines),
                line_words: lay.map(|l| l.words),
                line_height_px: lay.and_then(|l| l.line_height_px),
                columns: lay.map(|l| l.columns),
                mean_word_conf: lay.and_then(|l| l.mean_word_conf),
                total_ms: pr.timings.total_ms,
                plan_class: plan_class.clone(),
                with_readings,
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
        let col = if cells.iter().all(|v| v.is_null() || v.is_string()) {
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
    let mut out = format!("# Quality campaign {campaign}\n\nPages with a result: {}; with readings: {}; pending: {}; failed: {}.\n\n", s.pages, s.with_readings, s.pending, s.failed);
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

fn spool_files(spool: &Path, workunit: &str, names: &[&str]) -> Vec<PathBuf> {
    names
        .iter()
        .map(|n| spool.join(format!("{workunit}__file_{n}")))
        .collect()
}

fn gave_up(spool: &Path, workunit: &str) -> bool {
    spool.join(format!("{workunit}_error")).exists()
        || spool.join(format!("{workunit}_no_output_files")).exists()
}

/// A page's `usnm_qwen` crops and canonical readings, when both exist.
struct QwenPage {
    crops: Vec<Crop>,
    readings: Option<Readings>,
}

/// Every submitted `usnm_qwen` unit by doc id (supporting both single-page
/// and multi-page bundle units), with its readings where the result is in.
fn qwen_pages(root: &Path, spool: &Path) -> anyhow::Result<BTreeMap<String, QwenPage>> {
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
        let files = spool_files(spool, &wu, &crate::qwen::outputs());
        let readings = if files.iter().all(|f| f.exists()) {
            let got = crate::qwen::validate::load(&files)
                .with_context(|| format!("canonical result {wu}"))?;
            anyhow::ensure!(
                got.result.job_id == job.id,
                "{wu}: result does not belong to its job"
            );
            Some(got.readings)
        } else {
            None
        };
        if job.pages.is_empty() {
            out.insert(
                job.doc_id.clone(),
                QwenPage {
                    crops: job.crops,
                    readings,
                },
            );
        } else {
            for bp in &job.pages {
                let prefix = format!("{}:", bp.doc_id);
                let page_readings = readings.as_ref().map(|r| {
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
                out.insert(
                    bp.doc_id.clone(),
                    QwenPage {
                        crops: bp.crops.clone(),
                        readings: page_readings,
                    },
                );
            }
        }
    }
    Ok(out)
}

/// The three tables of a campaign, built from the roots and spools.
pub struct Collected {
    pub summary: Summary,
    pub pages: Vec<PageRow>,
    pub crops: Vec<CropRow>,
}

pub fn collect(opts: &Options) -> anyhow::Result<Collected> {
    let mut summary = Summary::default();
    let mut pages = Vec::new();
    let mut crop_table = Vec::new();
    let qwen = qwen_pages(&opts.qwen_root, &opts.qwen_spool)?;
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
        let got = validate::load(&files).with_context(|| format!("canonical result {wu}"))?;
        anyhow::ensure!(
            got.result.job_id == job.id && got.result.engine == job.engine,
            "{wu}: result does not belong to its job"
        );
        if job.pages.is_empty() {
            summary.pages += 1;
            let q = qwen.get(&job.doc_id);
            let readings = q.and_then(|q| q.readings.as_ref());
            if readings.is_some() {
                summary.with_readings += 1;
            }
            let texts_opt: Option<Texts> = read(&unit.join(TEXTS_FILE), MAX_TEXTS)
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok());
            let mut unit_rows = page_rows(&job, &got.result, readings.is_some());
            if let Some(ref texts) = texts_opt {
                enrich_rows_with_damage(&mut unit_rows, texts, None);
            }
            pages.extend(unit_rows);
            if let Some(q) = q {
                let page = read(&unit.join(PAGE_FILE), MAX_SCAN)?;
                let (w, h) = image::load_from_memory(&page)
                    .with_context(|| format!("{wu}: page.jpg"))?
                    .dimensions();
                let loc = parse_alto(
                    &read(&unit.join(ALTO_FILE), MAX_ALTO)?,
                    f64::from(w),
                    f64::from(h),
                )?;
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
                crop_table.extend(crop_rows(&CropInputs {
                    doc_id: &job.doc_id,
                    loc: &loc,
                    ours: ours.as_ref(),
                    texts: &texts,
                    readers: &readers,
                    crops: &q.crops,
                    readings,
                }));
            }
        } else {
            anyhow::ensure!(
                got.result.pages.len() == job.pages.len(),
                "{wu}: bundle result has {} pages, expected {}",
                got.result.pages.len(),
                job.pages.len()
            );
            let has_qwen = job.pages.iter().any(|bp| qwen.contains_key(&bp.doc_id));
            let bundle_page = if has_qwen {
                Some(read(&unit.join(PAGE_FILE), MAX_BUNDLE_SCAN)?)
            } else {
                None
            };
            let bundle_alto = if has_qwen {
                Some(read(&unit.join(ALTO_FILE), MAX_BUNDLE_ALTO)?)
            } else {
                None
            };
            let bundle_texts = read(&unit.join(TEXTS_FILE), MAX_BUNDLE_TEXTS).ok();
            for (bp, pr) in job.pages.iter().zip(&got.result.pages) {
                anyhow::ensure!(
                    bp.doc_id == pr.doc_id,
                    "{wu}: bundle page doc_id mismatch ({} vs {})",
                    bp.doc_id,
                    pr.doc_id
                );
                summary.pages += 1;
                let q = qwen.get(&bp.doc_id);
                let readings = q.and_then(|q| q.readings.as_ref());
                if readings.is_some() {
                    summary.with_readings += 1;
                }
                let mut unit_rows = bundle_page_rows(
                    &job,
                    bp,
                    pr,
                    got.result.host.plan_class.clone(),
                    readings.is_some(),
                );
                if let Some(ref bt) = bundle_texts {
                    if let Ok(t_bytes) = bp.inputs.texts.extract(bt, "texts") {
                        if let Ok(page_texts) = serde_json::from_slice::<Texts>(t_bytes) {
                            enrich_rows_with_damage(&mut unit_rows, &page_texts, None);
                        }
                    }
                }
                pages.extend(unit_rows);
                if let (Some(q), Some(bp_page), Some(bp_alto)) =
                    (q, bundle_page.as_ref(), bundle_alto.as_ref())
                {
                    let bt = bundle_texts
                        .as_ref()
                        .ok_or_else(|| anyhow::anyhow!("{wu}: missing {TEXTS_FILE}"))?;
                    let p_bytes = bp.inputs.page.extract(bp_page, "page")?;
                    let a_bytes = bp.inputs.alto.extract(bp_alto, "alto")?;
                    let t_bytes = bp.inputs.texts.extract(bt, "texts")?;
                    let (w, h) = image::load_from_memory(p_bytes)
                        .with_context(|| format!("{wu} ({}): page.jpg", bp.doc_id))?
                        .dimensions();
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
                    crop_table.extend(crop_rows(&CropInputs {
                        doc_id: &bp.doc_id,
                        loc: &loc,
                        ours: ours.as_ref(),
                        texts: &texts,
                        readers: &readers,
                        crops: &q.crops,
                        readings,
                    }));
                }
            }
        }
    }
    summary.page_rows = pages.len();
    summary.crop_rows = crop_table.len();
    Ok(Collected {
        summary,
        pages,
        crops: crop_table,
    })
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
            to_parquet(&c.crops)?,
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
        opts.out_dir.is_some() != opts.curated_url.is_some(),
        "one of --out-dir and --curated-url"
    );
    anyhow::ensure!(
        usnm_store::is_safe_segment(&opts.campaign),
        "invalid campaign name {:?}",
        opts.campaign
    );
    let c = collect(opts)?;
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
mod tests {
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
            loc: &loc,
            ours: None,
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: Some(&readings),
        });
        // Three readers plus the vision-language row per crop.
        assert_eq!(rows.len(), crops.len() * 4);
        let loc_row = rows.iter().find(|r| r.reader == "loc").unwrap();
        assert!(loc_row.text.as_deref().is_some_and(|t| !t.is_empty()));
        assert!(loc_row.cer_vs_vlm.is_some());
        // Only LoC and the reading have text in the box: the hat has two
        // members and no estimate.
        assert_eq!(loc_row.hat_readers, 2);
        assert_eq!(loc_row.hat_est_cer, None);
        let as_row = rows.iter().find(|r| r.reader == "as").unwrap();
        assert_eq!(as_row.text, None);
        assert_eq!(as_row.cer_vs_vlm, None);
        let vlm_row = rows.iter().find(|r| r.reader == VLM).unwrap();
        assert_eq!(vlm_row.dice_vs_vlm, Some(1.0));
        // Without readings there are reader rows and nothing to compare.
        let rows = crop_rows(&CropInputs {
            doc_id: "p1",
            loc: &loc,
            ours: Some(&loc),
            texts: &l.texts,
            readers: &readers,
            crops: &crops,
            readings: None,
        });
        assert_eq!(rows.len(), crops.len() * 3);
        assert!(rows.iter().all(|r| r.vlm_text.is_none()));
        assert!(overlaps([0, 0, 10, 10], [5, 5, 20, 20]));
        assert!(!overlaps([0, 0, 10, 10], [10, 0, 20, 20]));
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
}
