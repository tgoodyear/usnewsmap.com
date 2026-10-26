//! The picture layer's first table: Newspaper Navigator's boxes (#1102, 04 §4.11).
//!
//! `usnm-ingest pictures` reads the five per-kind configs of
//! `biglam/newspaper-navigator` (CC0) at a pinned dataset revision, from a
//! directory or from Hugging Face, and writes one row per detected picture to
//! the curated store:
//!
//! ```text
//! pictures/newspaper-navigator/{revision}/{attempt}/{kind}-{nnnn}.parquet
//! pictures/newspaper-navigator/{revision}/{attempt}/provenance.json
//! ```
//!
//! Attempts are numbered from `0001`, written create-only and never changed,
//! as the Congress pages are (`usnm_congress::curated`); `provenance.json` is
//! written last and marks the attempt finished, so an attempt without one is
//! ignored. It records the revision, the sha256 of every input file and what
//! was written, with counts of the rows the rules here could not place.
//!
//! A row is keyed by the scan it was detected on, not by `page_key`: the
//! dataset's `page_seq_num` is the JP2's frame number on its reel, not the
//! page's place in its issue ([`url`]). The frame-to-`seq` join needs LoC's
//! file list for each issue and is a later step.

pub mod read;
pub mod source;
pub mod url;

use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{bail, Context};
use arrow_array::builder::{
    Date32Builder, FixedSizeListBuilder, Float64Builder, ListBuilder, StringBuilder, UInt32Builder,
};
use arrow_array::cast::AsArray;
use arrow_array::types::{Date32Type, Float64Type, UInt32Type};
use arrow_array::{Array, ArrayRef, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use bytes::Bytes;
use chrono::{DateTime, NaiveDate, Utc};
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use usnm_store::ObjectStore;

pub use self::read::Quality;
use self::source::Source;
use crate::source::hex;

/// The Hugging Face dataset the rows come from.
pub const DATASET: &str = "biglam/newspaper-navigator";

/// Where the dataset's tables go in the curated store.
pub const PREFIX: &str = "pictures/newspaper-navigator";

/// The file that marks an attempt finished.
pub const PROVENANCE: &str = "provenance.json";

/// The version of the rows and of `provenance.json`. A finished attempt with
/// another version doesn't count: the run writes a new attempt.
pub const SCHEMA_VERSION: u32 = 1;

/// The dataset's configs and the kind of picture each holds, in the order they
/// are read.
pub const CONFIGS: [(&str, &str); 5] = [
    ("cartoons", "cartoon"),
    ("comics", "comic"),
    ("illustrations", "illustration"),
    ("maps", "map"),
    ("photos", "photo"),
];

/// Rows in a part: a run holds about this many at a time.
pub const PART_ROWS: usize = 100_000;

/// The share of a kind's rows, in percent, that may have no scan identity or
/// no crop URL before the run stops without marking anything finished.
pub const MAX_UNPLACED_PCT: f64 = 1.0;

/// One detected picture.
#[derive(Debug, Clone, PartialEq)]
pub struct Picture {
    /// `photo`, `illustration`, `map`, `comic` or `cartoon`: from the config
    /// the row was read in.
    pub kind: String,
    /// The scan's LCCN in lower case, from `page_url`.
    pub lccn: Option<String>,
    pub date: Option<NaiveDate>,
    pub edition: Option<u32>,
    /// The batch directory (`ohi_ingstad_ver01`), from the `batch` column.
    pub batch: Option<String>,
    /// The reel directory as the path has it: digits, `print`, `no/reel`.
    pub reel: Option<String>,
    /// The JP2's number on its reel, zero padded as in its name (`0037`).
    pub frame: Option<String>,
    /// The detector's score.
    pub score: Option<f64>,
    /// The dataset's `box`, as it gives it: four fractions of the page.
    pub bbox: Option<[f64; 4]>,
    /// The caption's words (the dataset's `ocr`, LoC's OCR).
    pub caption_words: Vec<String>,
    /// The dataset's `predicted_type`, as it is.
    pub predicted_type: Option<String>,
    /// The dataset's `page_url`, as it is.
    pub page_url: Option<String>,
    /// The crop on tile.loc.gov ([`url::rewrite_crop_url`]); none when the
    /// dataset's URL is of a shape that is not placed.
    pub crop_url: Option<String>,
}

fn unix_days(d: NaiveDate) -> i32 {
    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid");
    i32::try_from((d - epoch).num_days()).expect("dates fit in i32 days")
}

fn from_unix_days(n: i32) -> NaiveDate {
    NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid") + chrono::Duration::days(i64::from(n))
}

fn schema() -> SchemaRef {
    let text = |name: &str, nullable| Field::new(name, DataType::Utf8, nullable);
    Arc::new(Schema::new(vec![
        text("kind", false),
        text("lccn", true),
        Field::new("date", DataType::Date32, true),
        Field::new("edition", DataType::UInt32, true),
        text("batch", true),
        text("reel", true),
        text("frame", true),
        Field::new("score", DataType::Float64, true),
        Field::new(
            "box",
            DataType::FixedSizeList(Arc::new(Field::new_list_field(DataType::Float64, true)), 4),
            true,
        ),
        Field::new(
            "caption_words",
            DataType::List(Arc::new(Field::new_list_field(DataType::Utf8, true))),
            false,
        ),
        text("predicted_type", true),
        text("page_url", true),
        text("crop_url", true),
    ]))
}

fn to_batch(rows: &[Picture]) -> anyhow::Result<RecordBatch> {
    let text = |f: &dyn Fn(&Picture) -> Option<&str>| {
        let mut b = StringBuilder::new();
        for r in rows {
            b.append_option(f(r));
        }
        Arc::new(b.finish()) as ArrayRef
    };
    let mut date = Date32Builder::with_capacity(rows.len());
    let mut edition = UInt32Builder::with_capacity(rows.len());
    let mut score = Float64Builder::with_capacity(rows.len());
    let mut bbox = FixedSizeListBuilder::new(Float64Builder::new(), 4);
    let mut words = ListBuilder::new(StringBuilder::new());
    for r in rows {
        date.append_option(r.date.map(unix_days));
        edition.append_option(r.edition);
        score.append_option(r.score);
        match r.bbox {
            Some(b) => {
                bbox.values().append_slice(&b);
                bbox.append(true);
            }
            None => {
                bbox.values().append_nulls(4);
                bbox.append(false);
            }
        }
        for w in &r.caption_words {
            words.values().append_value(w);
        }
        words.append(true);
    }
    let cols: Vec<ArrayRef> = vec![
        text(&|r| Some(&r.kind)),
        text(&|r| r.lccn.as_deref()),
        Arc::new(date.finish()),
        Arc::new(edition.finish()),
        text(&|r| r.batch.as_deref()),
        text(&|r| r.reel.as_deref()),
        text(&|r| r.frame.as_deref()),
        Arc::new(score.finish()),
        Arc::new(bbox.finish()),
        Arc::new(words.finish()),
        text(&|r| r.predicted_type.as_deref()),
        text(&|r| r.page_url.as_deref()),
        text(&|r| r.crop_url.as_deref()),
    ];
    Ok(RecordBatch::try_new(schema(), cols)?)
}

/// One Parquet part of `rows`.
pub fn write_part(rows: &[Picture]) -> anyhow::Result<Vec<u8>> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(ZstdLevel::try_new(6)?))
        .set_max_row_group_row_count(Some(65_536))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema(), Some(props))?;
    for chunk in rows.chunks(65_536) {
        w.write(&to_batch(chunk)?)?;
    }
    Ok(w.into_inner()?)
}

/// Every row of a part.
pub fn read_part(bytes: Bytes) -> anyhow::Result<Vec<Picture>> {
    let mut out = Vec::new();
    for batch in ParquetRecordBatchReaderBuilder::try_new(bytes)?.build()? {
        let batch = batch?;
        let (kind, lccn, batch_name, reel, frame) = (
            text(&batch, "kind")?,
            text(&batch, "lccn")?,
            text(&batch, "batch")?,
            text(&batch, "reel")?,
            text(&batch, "frame")?,
        );
        let (types, page_url, crop_url) = (
            text(&batch, "predicted_type")?,
            text(&batch, "page_url")?,
            text(&batch, "crop_url")?,
        );
        let date = column(&batch, "date")?.as_primitive::<Date32Type>();
        let edition = column(&batch, "edition")?.as_primitive::<UInt32Type>();
        let score = column(&batch, "score")?.as_primitive::<Float64Type>();
        let bbox = column(&batch, "box")?.as_fixed_size_list();
        let words = column(&batch, "caption_words")?.as_list::<i32>();
        let opt = |a: &StringArray, i: usize| (!a.is_null(i)).then(|| a.value(i).to_owned());
        for i in 0..batch.num_rows() {
            out.push(Picture {
                kind: kind.value(i).to_owned(),
                lccn: opt(lccn, i),
                date: (!date.is_null(i)).then(|| from_unix_days(date.value(i))),
                edition: (!edition.is_null(i)).then(|| edition.value(i)),
                batch: opt(batch_name, i),
                reel: opt(reel, i),
                frame: opt(frame, i),
                score: (!score.is_null(i)).then(|| score.value(i)),
                bbox: (!bbox.is_null(i)).then(|| {
                    let v = bbox.value(i);
                    let v = v.as_primitive::<Float64Type>();
                    [v.value(0), v.value(1), v.value(2), v.value(3)]
                }),
                caption_words: words
                    .value(i)
                    .as_string::<i32>()
                    .iter()
                    .flatten()
                    .map(str::to_owned)
                    .collect(),
                predicted_type: opt(types, i),
                page_url: opt(page_url, i),
                crop_url: opt(crop_url, i),
            });
        }
    }
    Ok(out)
}

fn column<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a ArrayRef> {
    batch
        .column_by_name(name)
        .with_context(|| format!("a pictures part has no `{name}` column"))
}

fn text<'a>(batch: &'a RecordBatch, name: &str) -> anyhow::Result<&'a StringArray> {
    column(batch, name)?
        .as_string_opt::<i32>()
        .with_context(|| format!("column `{name}` is not text"))
}

/// A part an attempt wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PartRef {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
    pub rows: u64,
}

/// A dataset file that was read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputRef {
    /// The file's path in the dataset (`cartoons/train-00000-of-00001.parquet`).
    pub path: String,
    pub bytes: u64,
    pub sha256: String,
    pub rows: u64,
}

/// What an attempt holds of one kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KindSummary {
    /// The dataset config the rows came from.
    pub config: String,
    pub rows: u64,
    pub parts: Vec<PartRef>,
    pub quality: Quality,
}

/// `provenance.json`: where an attempt's rows came from and what was written.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Provenance {
    pub schema: u32,
    pub source: String,
    pub license: String,
    /// The dataset revision, a full commit SHA.
    pub revision: String,
    pub attempt: u32,
    pub written_at: DateTime<Utc>,
    /// The ingest crate's version and, when the build has it, the commit.
    pub ingest_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commit: Option<String>,
    /// Every file read, in the order read.
    pub inputs: Vec<InputRef>,
    /// By kind.
    pub kinds: BTreeMap<String, KindSummary>,
    pub rows: u64,
    /// All kinds' counts of rows that did not fit.
    pub quality: Quality,
}

/// `pictures/newspaper-navigator/{revision}`.
fn revision_dir(revision: &str) -> String {
    format!("{PREFIX}/{revision}")
}

/// `pictures/newspaper-navigator/{revision}/{attempt}` (four digits).
pub fn attempt_dir(revision: &str, attempt: u32) -> String {
    format!("{}/{attempt:04}", revision_dir(revision))
}

/// A dataset revision as the Hub names it: 40 lower-case hex digits. A branch
/// or tag name would not pin the data.
pub fn parse_revision(s: &str) -> Result<String, String> {
    if s.len() == 40 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
        Ok(s.to_owned())
    } else {
        Err("a dataset revision is the full 40-character commit SHA, in lower case".to_owned())
    }
}

/// `(attempt, file)` for each object under a revision.
fn attempt_files(paths: &[String]) -> impl Iterator<Item = (u32, &str)> {
    paths.iter().filter_map(|p| {
        let rest = p.strip_prefix(PREFIX)?.strip_prefix('/')?;
        let mut it = rest.splitn(3, '/');
        let (_revision, attempt, file) = (it.next()?, it.next()?, it.next()?);
        let attempt: u32 = attempt.parse().ok().filter(|_| attempt.len() == 4)?;
        Some((attempt, file))
    })
}

/// The next attempt number at a revision: one past every attempt started.
async fn next_attempt(store: &dyn ObjectStore, revision: &str) -> anyhow::Result<u32> {
    let paths = store.list(&revision_dir(revision)).await?;
    Ok(attempt_files(&paths)
        .map(|(attempt, _)| attempt)
        .max()
        .map_or(1, |a| a + 1))
}

/// The newest finished attempt at a revision, whatever its schema version.
pub async fn finished(
    store: &dyn ObjectStore,
    revision: &str,
) -> anyhow::Result<Option<Provenance>> {
    let paths = store.list(&revision_dir(revision)).await?;
    let Some(attempt) = attempt_files(&paths)
        .filter(|(_, file)| *file == PROVENANCE)
        .map(|(attempt, _)| attempt)
        .max()
    else {
        return Ok(None);
    };
    let path = format!("{}/{PROVENANCE}", attempt_dir(revision, attempt));
    let body = store
        .get(&path)
        .await?
        .with_context(|| format!("`{path}` vanished"))?;
    Ok(Some(
        serde_json::from_slice(&body).with_context(|| format!("reading `{path}`"))?,
    ))
}

/// What a run is asked to do.
#[derive(Debug, Clone)]
pub struct Options {
    /// The dataset revision ([`parse_revision`]).
    pub revision: String,
    /// Where the files come from.
    pub source: Source,
    /// Rows in a part ([`PART_ROWS`]).
    pub part_rows: usize,
    /// [`MAX_UNPLACED_PCT`].
    pub max_unplaced_pct: f64,
    /// The commit the build was made from, for the provenance.
    pub commit: Option<String>,
}

/// What a run did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Report {
    pub revision: String,
    pub attempt: u32,
    /// The revision already had a finished attempt of this schema version:
    /// nothing was read or written.
    pub already_written: bool,
    pub rows: u64,
    pub rows_by_kind: BTreeMap<String, u64>,
    pub provenance: String,
}

impl Report {
    fn of(p: &Provenance, already_written: bool) -> Self {
        Self {
            revision: p.revision.clone(),
            attempt: p.attempt,
            already_written,
            rows: p.rows,
            rows_by_kind: p.kinds.iter().map(|(k, v)| (k.clone(), v.rows)).collect(),
            provenance: format!("{}/{PROVENANCE}", attempt_dir(&p.revision, p.attempt)),
        }
    }
}

/// A part, encoded on the reading thread.
struct Encoded {
    bytes: Vec<u8>,
    rows: u64,
}

/// Read one open file (blocking; `path` is for messages), sending its rows on
/// as parts of at most `part_rows`. A part never spans two files. Returns the
/// rows and the counts of what did not fit.
fn convert_file(
    file: std::fs::File,
    path: &std::path::Path,
    kind: &str,
    part_rows: usize,
    tx: tokio::sync::mpsc::Sender<Encoded>,
) -> anyhow::Result<(u64, Quality)> {
    let send = |rows: &[Picture]| -> anyhow::Result<()> {
        let part = Encoded {
            bytes: write_part(rows)?,
            rows: rows.len() as u64,
        };
        tx.blocking_send(part)
            .map_err(|_| anyhow::anyhow!("the part writer stopped"))
    };
    let mut held: Vec<Picture> = Vec::new();
    let quality = read::read_file(file, path, kind, |rows| {
        held.extend(rows);
        while held.len() >= part_rows {
            let rest = held.split_off(part_rows);
            send(&std::mem::replace(&mut held, rest))?;
        }
        Ok(())
    })?;
    if !held.is_empty() {
        send(&held)?;
    }
    Ok((quality.rows, quality))
}

/// Write one finished attempt of the dataset at `opts.revision`, unless the
/// revision already has one of this schema version.
pub async fn run(
    store: &dyn ObjectStore,
    opts: &Options,
    now: DateTime<Utc>,
) -> anyhow::Result<Report> {
    let revision = &opts.revision;
    parse_revision(revision).map_err(anyhow::Error::msg)?;
    if let Some(done) = finished(store, revision).await? {
        if done.schema == SCHEMA_VERSION {
            tracing::info!(
                revision,
                attempt = done.attempt,
                rows = done.rows,
                "the revision already has a finished attempt; nothing to do"
            );
            return Ok(Report::of(&done, true));
        }
    }
    let attempt = next_attempt(store, revision).await?;
    let dir = attempt_dir(revision, attempt);

    let mut inputs = Vec::new();
    let mut kinds = BTreeMap::new();
    let mut total = Quality::default();
    for (config, kind) in CONFIGS {
        let files = opts.source.list(revision, config).await?;
        let mut summary = KindSummary {
            config: config.to_owned(),
            rows: 0,
            parts: Vec::new(),
            quality: Quality::default(),
        };
        for file in files {
            let local = opts.source.get(revision, &file).await?;
            let (tx, mut rx) = tokio::sync::mpsc::channel::<Encoded>(2);
            // The handle the file was hashed through, not its path again.
            let handle = local
                .file
                .try_clone()
                .with_context(|| format!("reading `{}`", file.name))?;
            let (path, part_rows) = (local.path.clone(), opts.part_rows);
            let reader = tokio::task::spawn_blocking(move || {
                convert_file(handle, &path, kind, part_rows, tx)
            });
            while let Some(part) = rx.recv().await {
                let path = format!("{dir}/{kind}-{:04}.parquet", summary.parts.len());
                let part_ref = PartRef {
                    path: path.clone(),
                    sha256: hex(&Sha256::digest(&part.bytes)),
                    bytes: part.bytes.len() as u64,
                    rows: part.rows,
                };
                if !store
                    .put_new(&path, part.bytes, "application/vnd.apache.parquet")
                    .await?
                {
                    bail!("`{path}` already exists: another run wrote this attempt");
                }
                summary.parts.push(part_ref);
            }
            let (rows, quality) = reader
                .await
                .context("reading a dataset file stopped")?
                .with_context(|| format!("reading `{}`", file.name))?;
            tracing::info!(config, file = %file.name, rows, "dataset file read");
            inputs.push(InputRef {
                path: file.name.clone(),
                bytes: local.bytes,
                sha256: local.sha256.clone(),
                rows,
            });
            summary.rows += rows;
            summary.quality.add(&quality);
        }
        check_placed(config, &summary, opts.max_unplaced_pct)?;
        tracing::info!(
            config,
            rows = summary.rows,
            parts = summary.parts.len(),
            unplaced = summary.quality.unplaced_rows,
            "dataset config written"
        );
        total.add(&summary.quality);
        kinds.insert(kind.to_owned(), summary);
    }

    let provenance = Provenance {
        schema: SCHEMA_VERSION,
        source: DATASET.to_owned(),
        license: "cc0-1.0".to_owned(),
        revision: revision.clone(),
        attempt,
        written_at: now,
        ingest_version: env!("CARGO_PKG_VERSION").to_owned(),
        commit: opts.commit.clone(),
        inputs,
        rows: kinds.values().map(|k| k.rows).sum(),
        kinds,
        quality: total,
    };
    let path = format!("{dir}/{PROVENANCE}");
    if !store
        .put_new(
            &path,
            serde_json::to_vec_pretty(&provenance)?,
            "application/json",
        )
        .await?
    {
        bail!("`{path}` already exists: another run wrote this attempt");
    }
    Ok(Report::of(&provenance, false))
}

/// Stop when more of a kind's rows than `max_pct` percent have no scan
/// identity or crop URL: the rules here would then not fit the data, and the
/// attempt shouldn't be marked finished as if they did. The batches with the
/// most rows without a crop URL are logged, counts only.
fn check_placed(config: &str, kind: &KindSummary, max_pct: f64) -> anyhow::Result<()> {
    let unplaced = kind.quality.unplaced_rows;
    if kind.rows == 0 || (unplaced as f64) * 100.0 <= max_pct * kind.rows as f64 {
        return Ok(());
    }
    let mut worst: Vec<(&String, &u64)> = kind.quality.without_crop_by_batch.iter().collect();
    worst.sort_by(|a, b| b.1.cmp(a.1).then(a.0.cmp(b.0)));
    worst.truncate(20);
    tracing::error!(config, ?worst, "rows without a crop URL, by batch");
    bail!(
        "{unplaced} of {} `{config}` rows have no scan identity or no crop URL, more than \
         {max_pct}%: the shapes of `page_url` and `prediction_section_iiif_url` read here don't \
         fit this data. Nothing was marked finished. To write the attempt anyway and read the \
         counts in its provenance.json, run again with a higher --max-unplaced-pct",
        kind.rows
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_is_a_full_lower_case_sha() {
        let sha = "e2e585824cdd28d52755c481dca69ed0bfa992a1";
        assert_eq!(parse_revision(sha).as_deref(), Ok(sha));
        for bad in [
            "",
            "main",
            "refs/convert/parquet",
            &sha[..39],
            &format!("{sha}0"),
            &sha.to_uppercase(),
            &format!("{}g", &sha[..39]),
        ] {
            assert!(parse_revision(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn parts_read_back_as_written() {
        let full = Picture {
            kind: "map".into(),
            lccn: Some("sn85026051".into()),
            date: NaiveDate::from_ymd_opt(1850, 7, 27),
            edition: Some(2),
            batch: Some("ohi_ingstad_ver01".into()),
            reel: Some("no/reel".into()),
            frame: Some("0037".into()),
            score: Some(0.5),
            bbox: Some([0.25, 0.5, 0.75, 1.0]),
            caption_words: vec!["a".into(), "b".into()],
            predicted_type: Some("Map".into()),
            page_url: Some("https://example.test/x".into()),
            crop_url: Some("https://tile.loc.gov/x".into()),
        };
        let bare = Picture {
            kind: "photo".into(),
            lccn: None,
            date: None,
            edition: None,
            batch: None,
            reel: None,
            frame: None,
            score: None,
            bbox: None,
            caption_words: vec![],
            predicted_type: None,
            page_url: None,
            crop_url: None,
        };
        let rows = vec![full, bare];
        let bytes = write_part(&rows).unwrap();
        assert_eq!(read_part(Bytes::from(bytes)).unwrap(), rows);
        assert_eq!(
            read_part(Bytes::from(write_part(&[]).unwrap())).unwrap(),
            []
        );
    }

    #[test]
    fn attempts_are_numbered_from_the_files_listed() {
        let paths: Vec<String> = [
            "pictures/newspaper-navigator/r/0001/map-0000.parquet",
            "pictures/newspaper-navigator/r/0001/provenance.json",
            "pictures/newspaper-navigator/r/0003/map-0000.parquet",
            // Not attempts.
            "pictures/newspaper-navigator/r/x/map-0000.parquet",
            "pictures/newspaper-navigator/r/12/map-0000.parquet",
            "pictures/newspaper-navigator/r/0004",
            "other/r/0009/map-0000.parquet",
        ]
        .map(String::from)
        .into();
        let seen: Vec<(u32, &str)> = attempt_files(&paths).collect();
        assert_eq!(
            seen,
            [
                (1, "map-0000.parquet"),
                (1, "provenance.json"),
                (3, "map-0000.parquet")
            ]
        );
    }
}
