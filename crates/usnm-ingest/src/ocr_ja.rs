//! Our own OCR of the Japanese pages LoC has no text for (04 §4.8, #139).
//!
//! The OCR job (`ja-ocr/jaocr.py`) writes `ocr-ja/pages/<issue>[.<n>].parquet`
//! to the curated store, one row per page, keyed by `doc_id`. A release reads
//! every part, keeps the newest row for each page (by `ocred_at`, then part
//! path), and keeps the pages of batches in the version whose titles are in
//! the catalog. It indexes them in their own small index (`pages-ja-…`,
//! `infra/quickwit/pages-ja-index.yaml`), rebuilt at every release, and adds
//! the pages curation never had (`loc_text = missing`: no `ocr.txt` in LoC's
//! archive) to the version's baselines. Pages curation did have (empty,
//! short or garbled LoC text) are already counted there.

use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, Context};
use arrow_array::{
    Array, Date32Array, FixedSizeBinaryArray, Int16Array, LargeStringArray, StringArray,
    TimestampMicrosecondArray,
};
use bytes::Bytes;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use usnm_core::ids::PageKey;
use usnm_core::ja;
use usnm_core::time::{day_number, ym_number};
use usnm_store::ObjectStore;

use crate::catalog::{Catalog, Place, Title};
use crate::curated::from_unix_days;
use crate::source::hex;

/// Where the OCR job writes its parts in the curated store (a directory:
/// stores list it without the trailing slash).
pub const PREFIX: &str = "ocr-ja/pages";

/// The Japanese pages' index mapping.
pub const JA_TEMPLATE: &str = include_str!("../../../infra/quickwit/pages-ja-index.yaml");

/// The snapshot file recording which overlay parts a version was built from.
pub const OCR_JA_FILE: &str = "ocr_ja.json";

/// One page of our Japanese OCR.
#[derive(Debug, Clone, PartialEq)]
pub struct JaPage {
    pub key: PageKey,
    pub batch: String,
    /// Why LoC's text wasn't used: `missing`, `empty`, `short` or `garbled`.
    pub loc_text: String,
    pub ok: bool,
    pub printed: Option<String>,
    pub ocr_source: String,
    pub ocr_engine: String,
    ocred_at: i64,
    part: String,
}

impl JaPage {
    /// A page curation never saw: not in any `counts.json`, so not in the baselines.
    pub fn missing_from_curation(&self) -> bool {
        self.loc_text == "missing"
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PartRef {
    pub path: String,
    pub sha256: String,
    pub bytes: usize,
}

/// The overlay as a version uses it.
#[derive(Debug, Default)]
pub struct Overlay {
    /// The pages kept, sorted by doc_id.
    pub pages: Vec<JaPage>,
    /// Every part read, sorted by path.
    pub parts: Vec<PartRef>,
    /// Pages left out: their batch isn't in the version, or their title isn't catalogued.
    pub skipped: u64,
}

/// Read every overlay part and keep the newest row per page, for the batches in
/// `batches` (base names, as in `RunBatch::batch`) whose titles are catalogued.
pub async fn load(
    store: &dyn ObjectStore,
    batches: &BTreeSet<&str>,
    catalog: &Catalog,
) -> anyhow::Result<Overlay> {
    let mut paths: Vec<String> = store
        .list(PREFIX)
        .await?
        .into_iter()
        .filter(|p| p.starts_with(&format!("{PREFIX}/")) && p.ends_with(".parquet"))
        .collect();
    paths.sort();
    let mut newest: BTreeMap<String, JaPage> = BTreeMap::new();
    let mut parts = Vec::with_capacity(paths.len());
    for path in paths {
        let bytes = store
            .get(&path)
            .await?
            .with_context(|| format!("overlay part `{path}` vanished"))?;
        parts.push(PartRef {
            path: path.clone(),
            sha256: hex(&Sha256::digest(&bytes)),
            bytes: bytes.len(),
        });
        for page in read_part(Bytes::from(bytes), &path).with_context(|| path.clone())? {
            let doc_id = page.key.doc_id();
            let newer = newest
                .get(&doc_id)
                .is_none_or(|old| (page.ocred_at, &page.part) > (old.ocred_at, &old.part));
            if newer {
                newest.insert(doc_id, page);
            }
        }
    }
    let mut overlay = Overlay {
        parts,
        ..Overlay::default()
    };
    for (_, page) in newest {
        if batches.contains(page.batch.as_str()) && catalog.title(&page.key.lccn).is_some() {
            overlay.pages.push(page);
        } else {
            overlay.skipped += 1;
        }
    }
    Ok(overlay)
}

/// The rows of one overlay part (the schema `jaocr.py write_part` writes).
pub fn read_part(bytes: Bytes, path: &str) -> anyhow::Result<Vec<JaPage>> {
    let reader = ParquetRecordBatchReaderBuilder::try_new(bytes)?.build()?;
    let mut out = Vec::new();
    for batch in reader {
        let batch = batch?;
        let col = |name: &str| {
            batch
                .column_by_name(name)
                .with_context(|| format!("overlay part has no `{name}` column"))
        };
        macro_rules! typed {
            ($name:expr, $t:ty) => {
                col($name)?
                    .as_any()
                    .downcast_ref::<$t>()
                    .with_context(|| format!("column `{}` has an unexpected type", $name))?
            };
        }
        let doc_id = typed!("doc_id", StringArray);
        let lccn = typed!("lccn", StringArray);
        let date = typed!("date", Date32Array);
        let edition = typed!("edition", Int16Array);
        let seq = typed!("seq", Int16Array);
        let batch_name = typed!("batch", StringArray);
        let loc_text = typed!("loc_text", StringArray);
        let status = typed!("text_status", StringArray);
        let text = typed!("text", LargeStringArray);
        let source = typed!("ocr_source", StringArray);
        let engine = typed!("ocr_engine", StringArray);
        let at = typed!("ocred_at", TimestampMicrosecondArray);
        let _sha = typed!("text_sha256", FixedSizeBinaryArray);
        let unsigned = |v: i16| u16::try_from(v).context("negative edition or seq");
        for i in 0..batch.num_rows() {
            let key = PageKey::new(
                lccn.value(i),
                from_unix_days(date.value(i)),
                unsigned(edition.value(i))?,
                unsigned(seq.value(i))?,
            )?;
            if key.doc_id() != doc_id.value(i) {
                bail!(
                    "row {i}: doc_id `{}` doesn't match its page `{key}`",
                    doc_id.value(i)
                );
            }
            out.push(JaPage {
                key,
                batch: batch_name.value(i).to_owned(),
                loc_text: loc_text.value(i).to_owned(),
                ok: status.value(i) == "ok",
                printed: (!text.is_null(i)).then(|| text.value(i).to_owned()),
                ocr_source: source.value(i).to_owned(),
                ocr_engine: engine.value(i).to_owned(),
                ocred_at: at.value(i),
                part: path.to_owned(),
            });
        }
    }
    Ok(out)
}

/// The engine document for a Japanese page: the fields of the main index's
/// `page_doc`, with `text` the folded tokens of `printed`.
pub fn ja_doc(page: &JaPage, title: &Title, place: &Place) -> Value {
    let k = &page.key;
    let printed = page.printed.as_deref().unwrap_or_default();
    json!({
        "doc_id": k.doc_id(),
        "day": day_number(k.date),
        "ym": ym_number(k.date),
        "year": chrono::Datelike::year(&k.date),
        "place_id": place.id,
        "place_shard": place.ordinal % 8,
        "lccn": k.lccn,
        "state": title.state,
        "language": title.languages,
        "front_page": k.seq == 1,
        "edition": k.edition,
        "seq": k.seq,
        "sort_key": (u64::from(title.ordinal) << 32) | (u64::from(k.edition) << 16) | u64::from(k.seq),
        "date": k.date.to_string(),
        "batch": page.batch,
        "text": ja::index_text(printed),
        "printed": printed,
        "ocr_source": page.ocr_source,
        "ocr_engine": page.ocr_engine,
    })
}

/// The Japanese index for a version: `pages-v20261005-1` → `pages-ja-20261005-1`.
pub fn index_id(version: &str) -> String {
    format!(
        "pages-ja-{}",
        version.strip_prefix("pages-v").unwrap_or(version)
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_the_index_after_the_version() {
        assert_eq!(index_id("pages-v20261005-1"), "pages-ja-20261005-1");
    }
}
