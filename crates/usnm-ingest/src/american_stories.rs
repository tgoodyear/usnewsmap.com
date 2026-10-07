//! American Stories' text for our pages (#218, 04 §4.9, 05 §5.5.4).
//!
//! `jaocr.py american-stories-write` (ja-ocr/american_stories_write.py)
//! writes one row per page to the curated store,
//! `american-stories/pages/<lccn>/<year>-<nnn>.parquet`, keyed by our
//! `doc_id`, and marks each year it has finished with
//! `american-stories/years/<year>.json`, last. A release reads the listing
//! once ([`Source::list`]), keeping only the parts of finished years, then
//! loads one batch at a time ([`Source::load`]): the parts of the batch's
//! titles for the years it has pages in, and of their rows only the
//! title-days the batch has (its `counts.json`), so the map in memory is
//! about the batch's own pages. Texts are normalized as LoC's are
//! ([`usnm_core::text::normalize_ocr`]); a page whose text is then empty is
//! left out.
//!
//! The release puts a page's text in `text_as` (and its pairs in
//! `text_as_cg`), and indexes pages LoC has no usable text for when this
//! has some. Only with `--american-stories` (`USNM_AMERICAN_STORIES` in
//! the deployment): without it nothing is read and the documents are as
//! before.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use anyhow::Context;
use arrow_array::{Array, Date32Array, LargeStringArray, StringArray};
use bytes::Bytes;
use chrono::Datelike;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use parquet::arrow::ProjectionMask;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use usnm_core::text::normalize_ocr;
use usnm_core::time::{date_from_day, day_number};
use usnm_store::ObjectStore;

use crate::curated::from_unix_days;
use crate::ocr_ja::PartRef;
use crate::source::hex;
use crate::worker::Counts;

/// Where the writer puts the pages (a directory: stores list it without the
/// trailing slash).
pub const PAGES: &str = "american-stories/pages";

/// Where the writer marks the years it has finished.
pub const YEARS: &str = "american-stories/years";

/// The snapshot file recording which parts a version's new index was built from.
pub const FILE: &str = "american_stories.json";

/// The parts a release may read: those of finished years, by title and year.
#[derive(Debug, Default)]
pub struct Source {
    /// Years with a marker.
    pub years: BTreeSet<i32>,
    /// Paths by title, then year, sorted.
    parts: HashMap<String, BTreeMap<i32, Vec<String>>>,
    /// Parts of years without a marker yet (still being written): left out.
    pub unfinished_parts: u64,
}

impl Source {
    /// List the finished years and their parts.
    pub async fn list(store: &dyn ObjectStore) -> anyhow::Result<Self> {
        let mut out = Source {
            years: finished_years(store).await?,
            ..Source::default()
        };
        let mut paths = store.list(PAGES).await?;
        paths.sort();
        for path in paths {
            let Some((lccn, year)) = part_of(&path) else {
                continue;
            };
            if !out.years.contains(&year) {
                out.unfinished_parts += 1;
                continue;
            }
            out.parts
                .entry(lccn.to_owned())
                .or_default()
                .entry(year)
                .or_default()
                .push(path);
        }
        Ok(out)
    }

    /// How many parts of finished years there are.
    pub fn parts(&self) -> usize {
        self.parts
            .values()
            .flat_map(BTreeMap::values)
            .map(Vec::len)
            .sum()
    }

    /// The texts of one batch's pages: the rows of the batch's titles on the
    /// days `counts` (the batch's `counts.json`) has pages for.
    pub async fn load(&self, store: &dyn ObjectStore, counts: &Counts) -> anyhow::Result<Batch> {
        let mut out = Batch::default();
        for (lccn, days) in counts {
            let Some(by_year) = self.parts.get(lccn) else {
                continue;
            };
            let years: BTreeSet<i32> = days.keys().map(|d| date_from_day(*d).year()).collect();
            for path in years
                .iter()
                .filter_map(|y| by_year.get(y))
                .flat_map(|paths| paths.iter())
            {
                let bytes = store
                    .get(path)
                    .await?
                    .with_context(|| format!("American Stories part `{path}` vanished"))?;
                out.parts.push(PartRef {
                    path: path.clone(),
                    sha256: hex(&Sha256::digest(&bytes)),
                    bytes: bytes.len(),
                });
                read_part(Bytes::from(bytes), |row| {
                    let wanted = counts
                        .get(row.lccn)
                        .is_some_and(|d| d.contains_key(&day_number(row.date)));
                    if !wanted {
                        return;
                    }
                    let text = normalize_ocr(row.text);
                    if text.is_empty() {
                        out.empty += 1;
                        return;
                    }
                    // A page is in one part; should it be in two, the first
                    // (by path) is kept, so every release takes the same one.
                    if let std::collections::hash_map::Entry::Vacant(e) =
                        out.texts.entry(row.doc_id.to_owned())
                    {
                        out.text_bytes += text.len() as u64;
                        e.insert(text);
                    }
                })
                .with_context(|| path.clone())?;
            }
        }
        Ok(out)
    }
}

/// The years the writer has finished (those with a marker).
pub async fn finished_years(store: &dyn ObjectStore) -> anyhow::Result<BTreeSet<i32>> {
    Ok(store
        .list(YEARS)
        .await?
        .iter()
        .filter_map(|path| {
            path.strip_prefix(&format!("{YEARS}/"))?
                .strip_suffix(".json")?
                .parse()
                .ok()
        })
        .collect())
}

/// Stop early when the setting is on and there is nothing to read: a
/// release would refuse to build (a version without the text mustn't say
/// it has it), so the ingest job's `run` checks before hours of curation
/// and titles-sync.
pub async fn check_written(store: &dyn ObjectStore) -> anyhow::Result<BTreeSet<i32>> {
    let years = finished_years(store).await?;
    if years.is_empty() {
        anyhow::bail!(
            "--american-stories (USNM_AMERICAN_STORIES) is set, but the curated store has no \
             finished year of American Stories' text ({YEARS}/): run \
             `jaocr.py american-stories-write` first, or clear the setting"
        );
    }
    Ok(years)
}

/// `american-stories/pages/<lccn>/<year>-<nnn>.parquet` → (lccn, year).
fn part_of(path: &str) -> Option<(&str, i32)> {
    let (lccn, file) = path.strip_prefix(&format!("{PAGES}/"))?.split_once('/')?;
    let (year, _) = file.strip_suffix(".parquet")?.split_once('-')?;
    Some((lccn, year.parse().ok()?))
}

/// One batch's American Stories texts.
#[derive(Debug, Default)]
pub struct Batch {
    /// Normalized text by doc id, non-empty.
    pub texts: HashMap<String, String>,
    /// The parts read, in the order read.
    pub parts: Vec<PartRef>,
    /// Bytes of text held.
    pub text_bytes: u64,
    /// The batch's pages whose text was empty.
    pub empty: u64,
}

/// One row of a part, borrowed from its record batch.
pub struct Row<'a> {
    pub doc_id: &'a str,
    pub lccn: &'a str,
    pub date: chrono::NaiveDate,
    pub text: &'a str,
}

/// Every row of a part (the schema `american_stories_write.py` writes),
/// reading only the doc id, title, date and text.
pub fn read_part(bytes: Bytes, mut on_row: impl FnMut(Row<'_>)) -> anyhow::Result<()> {
    let builder = ParquetRecordBatchReaderBuilder::try_new(bytes)?;
    let schema = builder.schema().clone();
    let wanted: Vec<usize> = ["doc_id", "lccn", "date", "text"]
        .iter()
        .map(|n| {
            schema
                .index_of(n)
                .with_context(|| format!("American Stories part has no `{n}` column"))
        })
        .collect::<anyhow::Result<_>>()?;
    let mask = ProjectionMask::roots(builder.parquet_schema(), wanted);
    for batch in builder.with_projection(mask).build()? {
        let batch = batch?;
        macro_rules! typed {
            ($name:expr, $t:ty) => {
                batch
                    .column_by_name($name)
                    .with_context(|| format!("American Stories part has no `{}` column", $name))?
                    .as_any()
                    .downcast_ref::<$t>()
                    .with_context(|| format!("column `{}` has an unexpected type", $name))?
            };
        }
        let doc_id = typed!("doc_id", StringArray);
        let lccn = typed!("lccn", StringArray);
        let date = typed!("date", Date32Array);
        let text = typed!("text", LargeStringArray);
        for i in 0..batch.num_rows() {
            if doc_id.is_null(i) || lccn.is_null(i) || date.is_null(i) || text.is_null(i) {
                continue;
            }
            on_row(Row {
                doc_id: doc_id.value(i),
                lccn: lccn.value(i),
                date: from_unix_days(date.value(i)),
                text: text.value(i),
            });
        }
    }
    Ok(())
}

/// A part in the writer's schema, for tests: (doc id, lccn, date, text) rows,
/// with empty article structure and legibility.
pub fn encode_part(rows: &[(&str, &str, chrono::NaiveDate, &str)]) -> anyhow::Result<Vec<u8>> {
    use arrow_array::{ArrayRef, Int32Array, RecordBatch};
    use std::sync::Arc;
    let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).expect("valid");
    let n = rows.len();
    let doc_ids: Vec<&str> = rows.iter().map(|r| r.0).collect();
    let lccns: Vec<&str> = rows.iter().map(|r| r.1).collect();
    let batch = RecordBatch::try_from_iter([
        ("doc_id", Arc::new(StringArray::from(doc_ids)) as ArrayRef),
        ("lccn", Arc::new(StringArray::from(lccns))),
        (
            "date",
            Arc::new(Date32Array::from(
                rows.iter()
                    .map(|r| i32::try_from((r.2 - epoch).num_days()).expect("date"))
                    .collect::<Vec<_>>(),
            )) as ArrayRef,
        ),
        (
            "text",
            Arc::new(LargeStringArray::from(
                rows.iter().map(|r| r.3).collect::<Vec<_>>(),
            )),
        ),
        ("articles", Arc::new(LargeStringArray::from(vec!["[]"; n]))),
        ("legibility", Arc::new(StringArray::from(vec!["{}"; n]))),
        ("width", Arc::new(Int32Array::from(vec![0; n]))),
        ("height", Arc::new(Int32Array::from(vec![0; n]))),
    ])?;
    let mut buf = Vec::new();
    let mut w = parquet::arrow::ArrowWriter::try_new(&mut buf, batch.schema(), None)?;
    w.write(&batch)?;
    w.close()?;
    Ok(buf)
}

/// What a release took from American Stories for the index it built.
#[derive(Debug, Default)]
pub struct Record {
    parts: BTreeMap<String, PartRef>,
    /// Texts loaded for the batches indexed (some pages may not be kept: a
    /// copy in another batch, or a title-day another batch shares).
    pub loaded: u64,
    /// Documents with `text_as`.
    pub docs: u64,
    /// Of those, documents with no text of LoC's (indexed for this text alone).
    pub only: u64,
}

impl Record {
    pub fn add(&mut self, b: &Batch) {
        self.loaded += b.texts.len() as u64;
        for p in &b.parts {
            self.parts.insert(p.path.clone(), p.clone());
        }
    }

    /// The snapshot file: every part read, with its checksum.
    pub fn file(&self, source: &Source) -> Value {
        json!({
            "version": usnm_core::american_stories::VERSION,
            "years": source.years,
            "unfinished_parts": source.unfinished_parts,
            "loaded": self.loaded,
            "docs": self.docs,
            "only_american_stories": self.only,
            "parts": self.parts.values().collect::<Vec<_>>(),
        })
    }

    /// The manifest's summary (`built_from.american_stories`).
    pub fn summary(&self, source: &Source) -> Value {
        json!({
            "version": usnm_core::american_stories::VERSION,
            "years": source.years.len(),
            "parts": self.parts.len(),
            "bytes": self.parts.values().map(|p| p.bytes as u64).sum::<u64>(),
            "docs": self.docs,
            "only_american_stories": self.only,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use usnm_store::LocalStore;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn id(lccn: &str, date: NaiveDate, seq: u16) -> String {
        format!("{lccn}_{date}_ed-1_seq-{seq}")
    }

    async fn put(store: &LocalStore, path: &str, rows: &[(&str, &str, NaiveDate, &str)]) {
        store
            .put(path, encode_part(rows).unwrap(), "application/octet-stream")
            .await
            .unwrap();
    }

    #[test]
    fn parses_part_paths() {
        assert_eq!(
            part_of("american-stories/pages/sn84026749/1865-003.parquet"),
            Some(("sn84026749", 1865))
        );
        assert_eq!(part_of("american-stories/pages/sn1/x.parquet"), None);
        assert_eq!(part_of("american-stories/pages/1865-000.parquet"), None);
        assert_eq!(part_of("american-stories/years/1865.json"), None);
    }

    #[tokio::test]
    async fn loads_a_batchs_pages_of_finished_years() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let (a, b) = ("sn11111111", "sn22222222");
        let (d1, d2, d3) = (d(1865, 4, 15), d(1865, 4, 22), d(1866, 1, 6));
        let (id1, id2, id3) = (id(a, d1, 1), id(a, d1, 2), id(a, d2, 1));
        let id4 = id(a, d3, 1);
        let id5 = id(b, d1, 1);
        put(
            &store,
            "american-stories/pages/sn11111111/1865-000.parquet",
            &[
                (&id1, a, d1, "Lincoln  ﬁred upon.\nThe Presi-\ndent"),
                // Empty once normalized: left out.
                (&id2, a, d1, " \n "),
                // A day the batch has no pages on.
                (&id3, a, d2, "Another day"),
            ],
        )
        .await;
        put(
            &store,
            "american-stories/pages/sn11111111/1865-001.parquet",
            // A second copy of a page: the first part's wins.
            &[(&id1, a, d1, "a second copy")],
        )
        .await;
        // A year without a marker: still being written.
        put(
            &store,
            "american-stories/pages/sn11111111/1866-000.parquet",
            &[(&id4, a, d3, "next year")],
        )
        .await;
        // A title the batch doesn't have.
        put(
            &store,
            "american-stories/pages/sn22222222/1865-000.parquet",
            &[(&id5, b, d1, "other title")],
        )
        .await;
        store
            .put(
                "american-stories/years/1865.json",
                b"{}".to_vec(),
                "application/json",
            )
            .await
            .unwrap();

        let source = Source::list(&store).await.unwrap();
        assert_eq!(source.years, BTreeSet::from([1865]));
        assert_eq!(source.parts(), 3);
        assert_eq!(source.unfinished_parts, 1);

        let mut counts: Counts = BTreeMap::new();
        counts
            .entry(a.to_owned())
            .or_default()
            .extend([(day_number(d1), 2), (day_number(d3), 1)]);
        let batch = source.load(&store, &counts).await.unwrap();
        assert_eq!(
            batch.texts,
            HashMap::from([(id1.clone(), "Lincoln fired upon.\nThe President".to_owned())])
        );
        assert_eq!(batch.empty, 1);
        assert_eq!(batch.text_bytes, batch.texts[&id1].len() as u64);
        // Both parts of the title's 1865, none of 1866 (unfinished) or the other title.
        let paths: Vec<&str> = batch.parts.iter().map(|p| p.path.as_str()).collect();
        assert_eq!(
            paths,
            [
                "american-stories/pages/sn11111111/1865-000.parquet",
                "american-stories/pages/sn11111111/1865-001.parquet"
            ]
        );
        assert!(batch
            .parts
            .iter()
            .all(|p| p.sha256.len() == 64 && p.bytes > 0));

        let mut record = Record::default();
        record.add(&batch);
        record.add(&batch);
        record.docs = 1;
        let file = record.file(&source);
        assert_eq!(file["parts"].as_array().unwrap().len(), 2);
        assert_eq!(file["years"], json!([1865]));
        assert_eq!(file["loaded"], 2);
        let summary = record.summary(&source);
        assert_eq!(summary["parts"], 2);
        assert_eq!(summary["version"], usnm_core::american_stories::VERSION);
    }

    #[tokio::test]
    async fn nothing_written_yet_lists_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let store = LocalStore::new(dir.path());
        let source = Source::list(&store).await.unwrap();
        assert!(source.years.is_empty());
        assert_eq!(source.parts(), 0);
        let counts: Counts = BTreeMap::from([(
            "sn11111111".to_owned(),
            BTreeMap::from([(day_number(d(1865, 4, 15)), 1)]),
        )]);
        assert!(source.load(&store, &counts).await.unwrap().texts.is_empty());
    }
}
