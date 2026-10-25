//! Newspaper Navigator's Parquet files as picture rows (#1102).
//!
//! The per-kind configs of `biglam/newspaper-navigator` are read as they come:
//! `box` is a list of fixed-point decimals whose precision differs from file
//! to file (the combined config has doubles), and the other columns' widths are
//! not relied on either. A column whose type is none of the expected ones fails
//! the run on the first batch; a row that does not fit is kept, with what could
//! not be read left null and counted in [`Quality`].

use std::collections::BTreeMap;
use std::fs::File;
use std::path::Path;

use anyhow::Context;
use arrow_array::cast::AsArray;
use arrow_array::types::{
    Decimal128Type, Float32Type, Float64Type, Int16Type, Int32Type, Int64Type, Int8Type,
    UInt16Type, UInt32Type, UInt64Type, UInt8Type,
};
use arrow_array::{Array, RecordBatch};
use arrow_schema::DataType;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ProjectionMask;
use serde::{Deserialize, Serialize};

use super::url::{scan_of_page_url, OldCrop};
use super::Picture;

/// Rows per record batch read.
const BATCH_ROWS: usize = 8192;

/// The columns read: all of them must be in a file.
const COLUMNS: [&str; 8] = [
    "batch",
    "box",
    "ocr",
    "page_seq_num",
    "page_url",
    "predicted_type",
    "prediction_section_iiif_url",
    "score",
];

/// What did not fit in the rows read. Counts only, so a run's provenance says
/// how much of the data the rules here placed without holding any of it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quality {
    pub rows: u64,
    /// Rows with no complete scan identity (a `page_url` that is no scan, or an
    /// empty `batch`) or no `crop_url`, whatever the reason: what the run's
    /// check that the rules here fit the data counts.
    pub unplaced_rows: u64,
    /// `batch` empty: the row has no batch in its identity.
    pub batch_missing: u64,
    /// `page_url` is not `…/{lccn}/{reel}/{issue}/{frame}.jp2`: the row has no
    /// scan identity.
    pub page_url_unplaced: u64,
    /// No crop URL in the row.
    pub crop_url_missing: u64,
    /// A crop URL of a shape the rewrite does not place (no frame in its image
    /// id, an unexpected path): the row has no `crop_url`.
    pub crop_url_unplaced: u64,
    /// A crop URL of another scan or batch than the row's own columns name:
    /// the row has no `crop_url`.
    pub crop_url_mismatch: u64,
    /// `box` is not four finite numbers.
    pub box_unusable: u64,
    /// `score` is empty or not finite.
    pub score_missing: u64,
    /// `page_seq_num` is empty.
    pub page_seq_num_missing: u64,
    /// `page_seq_num` is a number other than the frame in `page_url`. The
    /// dataset's `page_seq_num` is the frame, so this is expected to be 0.
    pub frame_differs_from_page_seq_num: u64,
    /// Rows without a `crop_url` for an unplaced or mismatched one, by batch
    /// (`(none)` for a row without a batch).
    pub without_crop_by_batch: BTreeMap<String, u64>,
}

impl Quality {
    pub fn add(&mut self, other: &Quality) {
        self.rows += other.rows;
        self.unplaced_rows += other.unplaced_rows;
        self.batch_missing += other.batch_missing;
        self.page_url_unplaced += other.page_url_unplaced;
        self.crop_url_missing += other.crop_url_missing;
        self.crop_url_unplaced += other.crop_url_unplaced;
        self.crop_url_mismatch += other.crop_url_mismatch;
        self.box_unusable += other.box_unusable;
        self.score_missing += other.score_missing;
        self.page_seq_num_missing += other.page_seq_num_missing;
        self.frame_differs_from_page_seq_num += other.frame_differs_from_page_seq_num;
        for (batch, n) in &other.without_crop_by_batch {
            *self.without_crop_by_batch.entry(batch.clone()).or_default() += n;
        }
    }
}

/// Read one open Parquet file of `kind`, handing the rows to `emit` a record
/// batch at a time. `path` is for messages. Returns the counts of what did not
/// fit.
pub fn read_file(
    file: File,
    path: &Path,
    kind: &str,
    mut emit: impl FnMut(Vec<Picture>) -> anyhow::Result<()>,
) -> anyhow::Result<Quality> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(file)
        .with_context(|| format!("reading {} as Parquet", path.display()))?
        .with_batch_size(BATCH_ROWS);
    let schema = builder.schema().clone();
    let wanted = COLUMNS
        .iter()
        .map(|name| {
            schema
                .index_of(name)
                .with_context(|| format!("the file has no `{name}` column"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), wanted);
    let mut quality = Quality::default();
    for batch in builder.with_projection(mask).build()? {
        emit(pictures(&batch?, kind, &mut quality)?)?;
    }
    Ok(quality)
}

/// The rows of one record batch.
fn pictures(batch: &RecordBatch, kind: &str, q: &mut Quality) -> anyhow::Result<Vec<Picture>> {
    let batches = string_column(batch, "batch")?;
    let page_urls = string_column(batch, "page_url")?;
    let old_crops = string_column(batch, "prediction_section_iiif_url")?;
    let types = string_column(batch, "predicted_type")?;
    let scores = number_column(batch, "score", floats)?;
    let seq_nums = number_column(batch, "page_seq_num", ints)?;
    let boxes = boxes(column(batch, "box")?)?;
    let captions = words(column(batch, "ocr")?)?;

    let mut out = Vec::with_capacity(batch.num_rows());
    for i in 0..batch.num_rows() {
        q.rows += 1;
        let batch_name = non_empty(batches[i]);
        let page_url = non_empty(page_urls[i]);
        let scan = page_url.and_then(scan_of_page_url);
        if batch_name.is_none() {
            q.batch_missing += 1;
        }
        if scan.is_none() {
            q.page_url_unplaced += 1;
        }

        let crop_url = match non_empty(old_crops[i]) {
            None => {
                q.crop_url_missing += 1;
                None
            }
            Some(old) => match OldCrop::parse(old) {
                None => {
                    q.crop_url_unplaced += 1;
                    count_without_crop(q, batch_name);
                    None
                }
                Some(crop) if !crop.is_of(batch_name, scan.as_ref()) => {
                    q.crop_url_mismatch += 1;
                    count_without_crop(q, batch_name);
                    None
                }
                Some(crop) => Some(crop.tile_url()),
            },
        };

        // The batch is part of the scan's identity, as the `page_url` is: a
        // crop of the right scan does not make up for a row without one.
        if scan.is_none() || batch_name.is_none() || crop_url.is_none() {
            q.unplaced_rows += 1;
        }

        let score = scores[i].filter(|s| s.is_finite());
        if score.is_none() {
            q.score_missing += 1;
        }
        if boxes[i].is_none() {
            q.box_unusable += 1;
        }
        match (seq_nums[i], &scan) {
            (None, _) => q.page_seq_num_missing += 1,
            (Some(n), Some(scan)) if scan.frame.parse::<i64>().ok() != Some(n) => {
                q.frame_differs_from_page_seq_num += 1;
            }
            _ => {}
        }

        out.push(Picture {
            kind: kind.to_owned(),
            lccn: scan.as_ref().map(|s| s.lccn_key()),
            date: scan.as_ref().map(|s| s.date),
            edition: scan.as_ref().map(|s| s.edition),
            batch: batch_name.map(str::to_owned),
            reel: scan.as_ref().map(|s| s.reel.clone()),
            frame: scan.as_ref().map(|s| s.frame.clone()),
            score,
            bbox: boxes[i],
            caption_words: captions[i].clone(),
            predicted_type: non_empty(types[i]).map(str::to_owned),
            page_url: page_url.map(str::to_owned),
            crop_url,
        });
    }
    Ok(out)
}

/// A column of the record batch, by name.
fn column<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a dyn Array> {
    Ok(batch
        .column_by_name(name)
        .with_context(|| format!("the file has no `{name}` column"))?
        .as_ref())
}

fn string_column<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<Vec<Option<&'a str>>> {
    let col = column(batch, name)?;
    strings(col).with_context(|| format!("`{name}` has the unsupported type {:?}", col.data_type()))
}

fn number_column<T>(
    batch: &RecordBatch,
    name: &str,
    read: fn(&dyn Array) -> Option<Vec<Option<T>>>,
) -> anyhow::Result<Vec<Option<T>>> {
    let col = column(batch, name)?;
    read(col).with_context(|| format!("`{name}` has the unsupported type {:?}", col.data_type()))
}

/// A string that is there and not empty.
fn non_empty(s: Option<&str>) -> Option<&str> {
    s.filter(|s| !s.is_empty())
}

fn count_without_crop(q: &mut Quality, batch: Option<&str>) {
    *q.without_crop_by_batch
        .entry(batch.unwrap_or("(none)").to_owned())
        .or_default() += 1;
}

/// A string column's values, whatever its width.
fn strings(col: &dyn Array) -> Option<Vec<Option<&str>>> {
    match col.data_type() {
        DataType::Utf8 => Some(col.as_string::<i32>().iter().collect()),
        DataType::LargeUtf8 => Some(col.as_string::<i64>().iter().collect()),
        DataType::Utf8View => Some(col.as_string_view().iter().collect()),
        _ => None,
    }
}

/// A number column's values as doubles: floats, or fixed-point decimals
/// scaled back.
fn floats(col: &dyn Array) -> Option<Vec<Option<f64>>> {
    match col.data_type() {
        DataType::Float64 => Some(col.as_primitive::<Float64Type>().iter().collect()),
        DataType::Float32 => Some(
            col.as_primitive::<Float32Type>()
                .iter()
                .map(|v| v.map(f64::from))
                .collect(),
        ),
        DataType::Decimal128(_, scale) => {
            let divisor = 10f64.powi(i32::from(*scale));
            Some(
                col.as_primitive::<Decimal128Type>()
                    .iter()
                    .map(|v| v.map(|v| v as f64 / divisor))
                    .collect(),
            )
        }
        _ => None,
    }
}

/// An integer column's values, whatever its width.
fn ints(col: &dyn Array) -> Option<Vec<Option<i64>>> {
    macro_rules! widen {
        ($t:ty) => {
            Some(
                col.as_primitive::<$t>()
                    .iter()
                    .map(|v| v.and_then(|v| i64::try_from(v).ok()))
                    .collect(),
            )
        };
    }
    match col.data_type() {
        DataType::Int8 => widen!(Int8Type),
        DataType::Int16 => widen!(Int16Type),
        DataType::Int32 => widen!(Int32Type),
        DataType::Int64 => widen!(Int64Type),
        DataType::UInt8 => widen!(UInt8Type),
        DataType::UInt16 => widen!(UInt16Type),
        DataType::UInt32 => widen!(UInt32Type),
        DataType::UInt64 => widen!(UInt64Type),
        _ => None,
    }
}

/// The rows of a list column as arrays, with `None` for a null list.
fn lists(col: &dyn Array) -> Option<Vec<Option<arrow_array::ArrayRef>>> {
    match col.data_type() {
        DataType::List(_) => Some(col.as_list::<i32>().iter().collect()),
        DataType::LargeList(_) => Some(col.as_list::<i64>().iter().collect()),
        DataType::FixedSizeList(_, _) => Some(col.as_fixed_size_list().iter().collect()),
        _ => None,
    }
}

/// `box`: four finite numbers per row, `None` for any other list.
fn boxes(col: &dyn Array) -> anyhow::Result<Vec<Option<[f64; 4]>>> {
    let rows = lists(col)
        .with_context(|| format!("`box` has the unsupported type {:?}", col.data_type()))?;
    rows.into_iter()
        .map(|row| {
            let Some(row) = row else { return Ok(None) };
            let values = floats(row.as_ref()).with_context(|| {
                format!("`box` holds the unsupported type {:?}", row.data_type())
            })?;
            Ok(<[Option<f64>; 4]>::try_from(values).ok().and_then(|v| {
                let v = v.map(|x| x.filter(|x| x.is_finite()));
                Some([v[0]?, v[1]?, v[2]?, v[3]?])
            }))
        })
        .collect()
}

/// `ocr`: the caption's words, empty for a null list or a null word's place.
fn words(col: &dyn Array) -> anyhow::Result<Vec<Vec<String>>> {
    let rows = lists(col)
        .with_context(|| format!("`ocr` has the unsupported type {:?}", col.data_type()))?;
    rows.into_iter()
        .map(|row| {
            let Some(row) = row else {
                return Ok(Vec::new());
            };
            let words = strings(row.as_ref()).with_context(|| {
                format!("`ocr` holds the unsupported type {:?}", row.data_type())
            })?;
            Ok(words.into_iter().flatten().map(str::to_owned).collect())
        })
        .collect()
}
