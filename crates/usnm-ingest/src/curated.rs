//! Curated page rows (04 §4.3.1) and their Parquet encoding.
//!
//! Curated rows hold only immutable page facts. Title-derived attributes
//! (place, state, language) are joined in from the titles snapshot when an
//! index or reference snapshot is built.

use std::sync::Arc;

use anyhow::Context;
use arrow_array::builder::{
    BooleanBuilder, Date32Builder, FixedSizeBinaryBuilder, Int16Builder, Int32Builder, Int8Builder,
    StringBuilder, TimestampMicrosecondBuilder,
};
use arrow_array::{
    Array, ArrayRef, BooleanArray, Date32Array, FixedSizeBinaryArray, Int16Array, RecordBatch,
    StringArray,
};
use arrow_schema::{DataType, Field, Schema, SchemaRef, TimeUnit};
use bytes::Bytes;
use chrono::{DateTime, Datelike, NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::{ArrowWriter, ProjectionMask};
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use sha2::{Digest, Sha256};
use usnm_core::ids::PageKey;
use usnm_core::text::{normalize_ocr, text_status, TextStatus};

/// Rows per Parquet row group.
const ROW_GROUP_ROWS: usize = 4_096;

/// Start a new part once this much text is buffered, so that each part stays
/// well under the object size the stores read in one piece.
pub const PART_TEXT_BYTES: usize = 160 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedRow {
    pub key: PageKey,
    pub batch: String,
    pub batch_version: u16,
    pub ocr_source: String,
    pub status: TextStatus,
    /// Normalized text; `None` unless `status` is `Ok` (04 §4.5).
    pub text: Option<String>,
    pub text_chars: i32,
    pub word_count: i32,
    pub text_sha256: [u8; 32],
    pub ingested_at: DateTime<Utc>,
}

impl CuratedRow {
    /// Normalize one page's raw OCR text into a curated row.
    pub fn from_ocr(
        key: PageKey,
        raw: &str,
        batch: &str,
        batch_version: u16,
        ocr_source: &str,
        ingested_at: DateTime<Utc>,
    ) -> Self {
        let text = normalize_ocr(raw);
        let status = text_status(&text);
        let clamp = |n: usize| i32::try_from(n).unwrap_or(i32::MAX);
        Self {
            key,
            batch: batch.to_owned(),
            batch_version,
            ocr_source: ocr_source.to_owned(),
            status,
            text_chars: clamp(text.chars().count()),
            word_count: clamp(text.split_whitespace().count()),
            text_sha256: Sha256::digest(text.as_bytes()).into(),
            text: (status == TextStatus::Ok).then_some(text),
            ingested_at,
        }
    }
}

fn status_str(s: TextStatus) -> &'static str {
    match s {
        TextStatus::Ok => "ok",
        TextStatus::Short => "short",
        TextStatus::Empty => "empty",
    }
}

fn parse_status(s: &str) -> anyhow::Result<TextStatus> {
    Ok(match s {
        "ok" => TextStatus::Ok,
        "short" => TextStatus::Short,
        "empty" => TextStatus::Empty,
        _ => anyhow::bail!("unknown text_status `{s}`"),
    })
}

pub fn schema() -> SchemaRef {
    let s = |n: &str| Field::new(n, DataType::Utf8, false);
    Arc::new(Schema::new(vec![
        s("doc_id"),
        s("page_key"),
        s("lccn"),
        Field::new("date", DataType::Date32, false),
        Field::new("year", DataType::Int16, false),
        Field::new("month", DataType::Int8, false),
        Field::new("edition", DataType::Int16, false),
        Field::new("seq", DataType::Int16, false),
        Field::new("front_page", DataType::Boolean, false),
        s("batch"),
        Field::new("batch_version", DataType::Int16, false),
        s("ocr_source"),
        s("text_status"),
        Field::new("text", DataType::LargeUtf8, true),
        Field::new("text_chars", DataType::Int32, false),
        Field::new("word_count", DataType::Int32, false),
        Field::new("text_sha256", DataType::FixedSizeBinary(32), false),
        s("resource_url"),
        Field::new(
            "ingested_at",
            DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
            false,
        ),
    ]))
}

fn unix_days(d: NaiveDate) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid");
    i32::try_from((d - epoch).num_days()).expect("dates fit in i32 days")
}

fn from_unix_days(n: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid") + chrono::Duration::days(i64::from(n))
}

fn to_batch(rows: &[CuratedRow]) -> anyhow::Result<RecordBatch> {
    let n = rows.len();
    let mut doc_id = StringBuilder::new();
    let mut page_key = StringBuilder::new();
    let mut lccn = StringBuilder::new();
    let mut date = Date32Builder::with_capacity(n);
    let mut year = Int16Builder::with_capacity(n);
    let mut month = Int8Builder::with_capacity(n);
    let mut edition = Int16Builder::with_capacity(n);
    let mut seq = Int16Builder::with_capacity(n);
    let mut front = BooleanBuilder::with_capacity(n);
    let mut batch = StringBuilder::new();
    let mut batch_version = Int16Builder::with_capacity(n);
    let mut ocr_source = StringBuilder::new();
    let mut status = StringBuilder::new();
    let mut text = arrow_array::builder::LargeStringBuilder::new();
    let mut chars = Int32Builder::with_capacity(n);
    let mut words = Int32Builder::with_capacity(n);
    let mut sha = FixedSizeBinaryBuilder::with_capacity(n, 32);
    let mut url = StringBuilder::new();
    let mut at = TimestampMicrosecondBuilder::with_capacity(n).with_timezone("UTC");
    let small = |v: u16| i16::try_from(v).context("edition, seq or version above 32767");
    for r in rows {
        let k = &r.key;
        doc_id.append_value(k.doc_id());
        page_key.append_value(k.to_string());
        lccn.append_value(&k.lccn);
        date.append_value(unix_days(k.date));
        year.append_value(i16::try_from(k.date.year()).context("year")?);
        month.append_value(i8::try_from(k.date.month()).expect("month fits"));
        edition.append_value(small(k.edition)?);
        seq.append_value(small(k.seq)?);
        front.append_value(k.seq == 1);
        batch.append_value(&r.batch);
        batch_version.append_value(small(r.batch_version)?);
        ocr_source.append_value(&r.ocr_source);
        status.append_value(status_str(r.status));
        text.append_option(r.text.as_deref());
        chars.append_value(r.text_chars);
        words.append_value(r.word_count);
        sha.append_value(r.text_sha256)?;
        url.append_value(k.viewer_url(None));
        at.append_value(r.ingested_at.timestamp_micros());
    }
    let cols: Vec<ArrayRef> = vec![
        Arc::new(doc_id.finish()),
        Arc::new(page_key.finish()),
        Arc::new(lccn.finish()),
        Arc::new(date.finish()),
        Arc::new(year.finish()),
        Arc::new(month.finish()),
        Arc::new(edition.finish()),
        Arc::new(seq.finish()),
        Arc::new(front.finish()),
        Arc::new(batch.finish()),
        Arc::new(batch_version.finish()),
        Arc::new(ocr_source.finish()),
        Arc::new(status.finish()),
        Arc::new(text.finish()),
        Arc::new(chars.finish()),
        Arc::new(words.finish()),
        Arc::new(sha.finish()),
        Arc::new(url.finish()),
        Arc::new(at.finish()),
    ];
    Ok(RecordBatch::try_new(schema(), cols)?)
}

/// Writes curated rows as a sequence of Parquet parts held in memory; the
/// caller uploads each finished part.
pub struct PartWriter {
    writer: Option<ArrowWriter<Vec<u8>>>,
    pending: Vec<CuratedRow>,
    part_text: usize,
}

impl Default for PartWriter {
    fn default() -> Self {
        Self::new()
    }
}

impl PartWriter {
    pub fn new() -> Self {
        Self {
            writer: None,
            pending: Vec::new(),
            part_text: 0,
        }
    }

    fn props() -> WriterProperties {
        WriterProperties::builder()
            .set_compression(Compression::ZSTD(
                ZstdLevel::try_new(6).expect("valid level"),
            ))
            .set_max_row_group_row_count(Some(ROW_GROUP_ROWS))
            .build()
    }

    fn flush(&mut self) -> anyhow::Result<()> {
        if self.pending.is_empty() {
            return Ok(());
        }
        let batch = to_batch(&self.pending)?;
        self.pending.clear();
        if self.writer.is_none() {
            self.writer = Some(ArrowWriter::try_new(
                Vec::new(),
                schema(),
                Some(Self::props()),
            )?);
        }
        self.writer.as_mut().expect("just set").write(&batch)?;
        Ok(())
    }

    /// Add a row; returns a finished part when this one has grown large enough.
    pub fn push(&mut self, row: CuratedRow) -> anyhow::Result<Option<Vec<u8>>> {
        self.part_text += row.text.as_ref().map_or(0, String::len);
        self.pending.push(row);
        if self.pending.len() >= ROW_GROUP_ROWS {
            self.flush()?;
        }
        if self.part_text >= PART_TEXT_BYTES {
            return self.finish_part();
        }
        Ok(None)
    }

    /// Close the current part, if it has any rows.
    pub fn finish_part(&mut self) -> anyhow::Result<Option<Vec<u8>>> {
        self.flush()?;
        self.part_text = 0;
        match self.writer.take() {
            Some(w) => Ok(Some(w.into_inner()?)),
            None => Ok(None),
        }
    }
}

/// Read every row of a part, one record batch at a time. `with_text: false`
/// skips decoding the text column (the other columns are small).
pub fn read_part(
    bytes: Bytes,
    with_text: bool,
    mut on_row: impl FnMut(CuratedRow) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)?;
    let schema = builder.schema().clone();
    let wanted: Vec<usize> = (0..schema.fields().len())
        .filter(|&i| with_text || schema.field(i).name() != "text")
        .collect();
    let mask = ProjectionMask::roots(builder.parquet_schema(), wanted);
    let reader = builder.with_projection(mask).build()?;
    for batch in reader {
        let batch = batch?;
        let col = |name: &str| {
            batch
                .column_by_name(name)
                .with_context(|| format!("curated part has no `{name}` column"))
        };
        macro_rules! typed {
            ($name:expr, $t:ty) => {
                col($name)?
                    .as_any()
                    .downcast_ref::<$t>()
                    .with_context(|| format!("column `{}` has an unexpected type", $name))?
            };
        }
        let lccn = typed!("lccn", StringArray);
        let date = typed!("date", Date32Array);
        let edition = typed!("edition", Int16Array);
        let seq = typed!("seq", Int16Array);
        let _front = typed!("front_page", BooleanArray);
        let batch_name = typed!("batch", StringArray);
        let batch_version = typed!("batch_version", Int16Array);
        let ocr_source = typed!("ocr_source", StringArray);
        let status = typed!("text_status", StringArray);
        let chars = typed!("text_chars", arrow_array::Int32Array);
        let words = typed!("word_count", arrow_array::Int32Array);
        let sha = typed!("text_sha256", FixedSizeBinaryArray);
        let at = typed!("ingested_at", arrow_array::TimestampMicrosecondArray);
        let text = if with_text {
            Some(typed!("text", arrow_array::LargeStringArray))
        } else {
            None
        };
        let unsigned = |v: i16| u16::try_from(v).context("negative edition, seq or version");
        for i in 0..batch.num_rows() {
            let key = PageKey::new(
                lccn.value(i),
                from_unix_days(date.value(i)),
                unsigned(edition.value(i))?,
                unsigned(seq.value(i))?,
            )?;
            on_row(CuratedRow {
                key,
                batch: batch_name.value(i).to_owned(),
                batch_version: unsigned(batch_version.value(i))?,
                ocr_source: ocr_source.value(i).to_owned(),
                status: parse_status(status.value(i))?,
                text: text.and_then(|t| (!t.is_null(i)).then(|| t.value(i).to_owned())),
                text_chars: chars.value(i),
                word_count: words.value(i),
                text_sha256: sha.value(i).try_into().context("sha256 width")?,
                ingested_at: DateTime::from_timestamp_micros(at.value(i))
                    .context("ingested_at out of range")?,
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(seq: u16, raw: &str) -> CuratedRow {
        let key = PageKey::new(
            "sn84026749",
            NaiveDate::from_ymd_opt(1896, 7, 10).unwrap(),
            1,
            seq,
        )
        .unwrap();
        let at = DateTime::from_timestamp(1_790_000_000, 0).unwrap();
        CuratedRow::from_ocr(key, raw, "batch_x", 2, "ndnp-original", at)
    }

    #[test]
    fn normalizes_and_classifies_pages() {
        let r = row(
            1,
            "The indus-\n  try of ﬁne  cotton was the subject of the day.",
        );
        assert_eq!(r.status, TextStatus::Ok);
        assert_eq!(
            r.text.as_deref(),
            Some("The industry of fine cotton was the subject of the day.")
        );
        assert_eq!(r.word_count, 11);
        assert_eq!(row(2, "  \n ").status, TextStatus::Empty);
        let short = row(3, "a few words");
        assert_eq!((short.status, short.text), (TextStatus::Short, None));
    }

    #[test]
    fn parquet_round_trip_with_and_without_text() {
        let rows: Vec<CuratedRow> = (1..=5)
            .map(|s| {
                row(
                    s,
                    if s == 4 {
                        ""
                    } else {
                        "a page with enough words to be indexed"
                    },
                )
            })
            .collect();
        let mut w = PartWriter::new();
        for r in rows.clone() {
            assert!(w.push(r).unwrap().is_none());
        }
        let part = w.finish_part().unwrap().unwrap();
        assert!(w.finish_part().unwrap().is_none());
        let mut back = Vec::new();
        read_part(Bytes::from(part.clone()), true, |r| {
            back.push(r);
            Ok(())
        })
        .unwrap();
        assert_eq!(back, rows);
        let mut no_text = Vec::new();
        read_part(Bytes::from(part), false, |r| {
            no_text.push(r);
            Ok(())
        })
        .unwrap();
        assert!(no_text.iter().all(|r| r.text.is_none()));
        assert_eq!(no_text.len(), 5);
    }
}
