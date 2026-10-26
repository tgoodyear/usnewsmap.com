//! Exporting curated readers and ALTO for a staging hand-off.

use crate::atomic;
use anyhow::{ensure, Context};
use arrow_array::{Array, Int32Array, LargeStringArray, RecordBatch, StringArray};
use bytes::Bytes;
use chrono::Datelike;
use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::quality::{PageSize, Region, TextReader, Texts, MAX_ALTO, MAX_TEXT};

#[derive(Debug, Clone)]
pub struct TextsExportOptions {
    pub curated_url: String,
    /// Where the American Stories page shards are read, when not from `curated_url`: a local
    /// copy of `curated` (its `american-stories/pages/...` tree), which never changes, so only
    /// the `ocr-local` overlays come from the curated store.
    pub american_stories_url: Option<String>,
    pub out_dir: PathBuf,
    pub sample: Option<PathBuf>,
    pub doc_ids: Vec<String>,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TextsExportSummary {
    pub pages: usize,
    pub written: usize,
    pub american_stories: usize,
    pub ocr_local: usize,
    pub altos: usize,
    /// Bytes read from the curated store (`curated_url`): with a local American Stories copy,
    /// only the `ocr-local` overlays and their ALTO.
    pub curated_bytes: u64,
}

/// A store that counts the bytes its reads return.
#[derive(Debug)]
struct Counted {
    inner: std::sync::Arc<dyn usnm_store::ObjectStore>,
    bytes: std::sync::atomic::AtomicU64,
}

#[async_trait::async_trait]
impl usnm_store::ObjectStore for Counted {
    async fn get(&self, path: &str) -> Result<Option<Vec<u8>>, usnm_store::StoreError> {
        let got = self.inner.get(path).await?;
        if let Some(b) = &got {
            self.bytes
                .fetch_add(b.len() as u64, std::sync::atomic::Ordering::Relaxed);
        }
        Ok(got)
    }
    async fn put_new(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<bool, usnm_store::StoreError> {
        self.inner.put_new(path, body, content_type).await
    }
    async fn put(
        &self,
        path: &str,
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<(), usnm_store::StoreError> {
        self.inner.put(path, body, content_type).await
    }
    async fn list(&self, prefix: &str) -> Result<Vec<String>, usnm_store::StoreError> {
        self.inner.list(prefix).await
    }
    async fn list_info(
        &self,
        prefix: &str,
    ) -> Result<Vec<usnm_store::ObjectInfo>, usnm_store::StoreError> {
        self.inner.list_info(prefix).await
    }
}

#[derive(serde::Deserialize)]
struct AsArticle {
    #[serde(default)]
    bboxes: Vec<Vec<Option<i64>>>,
    #[serde(default)]
    legibility: Vec<String>,
}

fn str_col<'a>(batch: &'a RecordBatch, name: &str, row: usize) -> Option<&'a str> {
    let col = batch.column_by_name(name)?;
    if col.is_null(row) {
        return None;
    }
    if let Some(s) = col.as_any().downcast_ref::<StringArray>() {
        Some(s.value(row))
    } else if let Some(s) = col.as_any().downcast_ref::<LargeStringArray>() {
        Some(s.value(row))
    } else {
        None
    }
}

fn int_col(batch: &RecordBatch, name: &str, row: usize) -> Option<i32> {
    let col = batch.column_by_name(name)?;
    if col.is_null(row) {
        return None;
    }
    col.as_any()
        .downcast_ref::<Int32Array>()
        .map(|a| a.value(row))
}

fn parse_as_regions(articles_json: &str) -> Vec<Region> {
    let Ok(articles) = serde_json::from_str::<Vec<AsArticle>>(articles_json) else {
        return Vec::new();
    };
    let mut regions = Vec::new();
    for a in articles {
        let legibility = ["illegible", "borderline", "legible"]
            .into_iter()
            .find(|k| a.legibility.iter().any(|l| l == k))
            .map(str::to_owned);
        for b in a.bboxes {
            if b.len() < 4 {
                continue;
            }
            let (Some(x0), Some(y0), Some(x1), Some(y1)) = (b[0], b[1], b[2], b[3]) else {
                continue;
            };
            let (Ok(x0), Ok(y0), Ok(x1), Ok(y1)) = (
                u32::try_from(x0),
                u32::try_from(y0),
                u32::try_from(x1),
                u32::try_from(y1),
            ) else {
                continue;
            };
            if x0 < x1 && y0 < y1 {
                regions.push(Region {
                    rect: [x0, y0, x1, y1],
                    kind: Some("article".into()),
                    legibility: legibility.clone(),
                });
            }
        }
    }
    regions
}

fn ours_reader_name(engine: &str, source: &str) -> String {
    let low = format!("{engine} {source}").to_ascii_lowercase();
    if low.contains("surya") {
        "ours-surya".into()
    } else if low.contains("tesseract") {
        "ours-tesseract".into()
    } else {
        let slug: String = engine
            .to_ascii_lowercase()
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();
        let slug = slug.trim_matches('-');
        let name = if slug.is_empty() {
            "ours".to_owned()
        } else {
            format!("ours-{slug}")
        };
        name.chars()
            .take(32)
            .collect::<String>()
            .trim_end_matches('-')
            .to_owned()
    }
}

/// Export each requested page's non-LoC texts (American Stories and `ocr-local`)
/// and our ALTO from the curated store into `opts.out_dir`.
pub async fn texts_export(opts: &TextsExportOptions) -> anyhow::Result<TextsExportSummary> {
    use usnm_store::ObjectStore as _;
    let mut wanted: BTreeSet<String> = BTreeSet::new();
    for doc_id in &opts.doc_ids {
        crate::assess::check_doc_id(doc_id)?;
        wanted.insert(doc_id.clone());
    }
    if let Some(sample) = &opts.sample {
        for row in crate::assess::read_sample(sample)? {
            crate::assess::check_doc_id(&row.doc_id)?;
            wanted.insert(row.doc_id);
        }
    }
    ensure!(!wanted.is_empty(), "no pages: give --sample or --doc-id");
    let mut by_lccn_year: BTreeMap<String, BTreeSet<i32>> = BTreeMap::new();
    for doc_id in &wanted {
        let key = PageKey::from_doc_id(doc_id)?;
        by_lccn_year
            .entry(key.lccn)
            .or_default()
            .insert(key.date.year());
    }

    let curated = Counted {
        inner: usnm_store::open(&opts.curated_url)?,
        bytes: Default::default(),
    };
    let american_stories: std::sync::Arc<dyn usnm_store::ObjectStore> =
        match &opts.american_stories_url {
            Some(url) => usnm_store::open(url)?,
            None => std::sync::Arc::new(Counted {
                inner: curated.inner.clone(),
                bytes: Default::default(),
            }),
        };
    std::fs::create_dir_all(&opts.out_dir)?;

    let mut as_by_doc: BTreeMap<String, (Option<PageSize>, TextReader)> = BTreeMap::new();
    for (lccn, years) in &by_lccn_year {
        let prefix = format!("{}/{lccn}", usnm_ingest::american_stories::PAGES);
        let mut paths = american_stories.list(&prefix).await?;
        paths.sort();
        let dir_prefix = format!("{prefix}/");
        for path in paths {
            let Some(file) = path
                .strip_prefix(&dir_prefix)
                .and_then(|s| s.strip_suffix(".parquet"))
            else {
                continue;
            };
            let Some((y_str, _)) = file.split_once('-') else {
                continue;
            };
            let Ok(year) = y_str.parse::<i32>() else {
                continue;
            };
            if !years.contains(&year) {
                continue;
            }
            let Some(bytes) = american_stories.get(&path).await? else {
                continue;
            };
            if opts.american_stories_url.is_none() {
                curated
                    .bytes
                    .fetch_add(bytes.len() as u64, std::sync::atomic::Ordering::Relaxed);
            }
            let reader = ParquetRecordBatchReaderBuilder::try_new(Bytes::from(bytes))
                .with_context(|| format!("reading {path}"))?
                .build()
                .with_context(|| format!("opening {path}"))?;
            for batch in reader {
                let b = batch.with_context(|| format!("decoding {path}"))?;
                for i in 0..b.num_rows() {
                    let Some(doc_id) = str_col(&b, "doc_id", i) else {
                        continue;
                    };
                    if !wanted.contains(doc_id) || as_by_doc.contains_key(doc_id) {
                        continue;
                    }
                    let Some(raw_text) = str_col(&b, "text", i) else {
                        continue;
                    };
                    let text = usnm_core::text::normalize_ocr(raw_text);
                    if text.is_empty() || text.len() > MAX_TEXT {
                        continue;
                    }
                    let regions = str_col(&b, "articles", i)
                        .filter(|s| !s.is_empty())
                        .map(parse_as_regions)
                        .unwrap_or_default();
                    let page_size = match (int_col(&b, "width", i), int_col(&b, "height", i)) {
                        (Some(w), Some(h)) if w > 0 && h > 0 => Some(PageSize {
                            width: w as u32,
                            height: h as u32,
                        }),
                        _ => None,
                    };
                    as_by_doc.insert(
                        doc_id.to_owned(),
                        (
                            page_size,
                            TextReader {
                                alto: false,
                                name: "as".into(),
                                regions,
                                source: "american-stories".into(),
                                text,
                            },
                        ),
                    );
                }
            }
        }
    }

    let revoked = usnm_ingest::ocr_local::replace::revocations(&curated).await?;
    let listing = usnm_ingest::ocr_local::replace::Listing::list(&curated).await?;
    let loaded = listing
        .load(&curated, by_lccn_year.keys().map(String::as_str), &revoked)
        .await?;
    let mut ocr_candidates: BTreeMap<String, usnm_ingest::ocr_local::replace::Candidate> =
        BTreeMap::new();
    for (doc_id, mut cand) in loaded.pages {
        if !wanted.contains(&doc_id) {
            continue;
        }
        cand.text = usnm_core::text::normalize_ocr(&cand.text);
        if !cand.text.is_empty() && cand.text.len() <= MAX_TEXT {
            ocr_candidates.insert(doc_id, cand);
        }
    }

    let mut altos_by_doc: BTreeMap<String, Vec<String>> = BTreeMap::new();
    if !ocr_candidates.is_empty() {
        let alto_root = format!("{}/alto", usnm_ingest::ocr_local::ROOT);
        let alto_prefix = format!("{alto_root}/");
        let mut alto_paths = curated.list(&alto_root).await?;
        alto_paths.sort();
        for path in alto_paths {
            let Some(rest) = path.strip_prefix(&alto_prefix) else {
                continue;
            };
            if !rest.ends_with(".xml") {
                continue;
            }
            let Some((doc_id, _)) = rest.split_once('.') else {
                continue;
            };
            if ocr_candidates.contains_key(doc_id) {
                altos_by_doc
                    .entry(doc_id.to_owned())
                    .or_default()
                    .push(path);
            }
        }
    }

    let mut summary = TextsExportSummary {
        pages: wanted.len(),
        ..TextsExportSummary::default()
    };
    let mut ours_by_doc: BTreeMap<String, TextReader> = BTreeMap::new();
    for (doc_id, cand) in ocr_candidates {
        let mut has_alto = false;
        if let Some(paths) = altos_by_doc.get(&doc_id) {
            let node_needle = format!(
                "{}/alto/{doc_id}.{}.",
                usnm_ingest::ocr_local::ROOT,
                cand.row.node
            );
            let chosen = paths
                .iter()
                .rev()
                .find(|p| p.starts_with(&node_needle))
                .or_else(|| paths.last());
            if let Some(alto_path) = chosen {
                if let Some(bytes) = curated.get(alto_path).await? {
                    if !bytes.is_empty() && bytes.len() <= MAX_ALTO {
                        atomic(&opts.out_dir.join(format!("{doc_id}.ours.xml")), &bytes)?;
                        has_alto = true;
                        summary.altos += 1;
                    }
                }
            }
        }
        let name = ours_reader_name(&cand.row.ocr_engine, &cand.row.ocr_source);
        let source = usnm_ocr_contract::quality::normalize_source(&format!(
            "ocr-local:{}",
            cand.row.ocr_engine
        ));
        ours_by_doc.insert(
            doc_id,
            TextReader {
                alto: has_alto,
                name,
                regions: vec![],
                source,
                text: cand.text,
            },
        );
    }

    for doc_id in &wanted {
        let mut page = None;
        let mut readers = Vec::new();
        if let Some((sz, r)) = as_by_doc.remove(doc_id) {
            page = sz;
            readers.push(r);
            summary.american_stories += 1;
        }
        if let Some(r) = ours_by_doc.remove(doc_id) {
            readers.push(r);
            summary.ocr_local += 1;
        }
        if !readers.is_empty() {
            let texts = Texts { page, readers };
            atomic(
                &opts.out_dir.join(format!("{doc_id}.json")),
                &serde_json::to_vec_pretty(&texts)?,
            )?;
            summary.written += 1;
        }
    }
    summary.curated_bytes = curated.bytes.load(std::sync::atomic::Ordering::Relaxed);
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn copy_tree(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let dest = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy_tree(&entry.path(), &dest);
            } else {
                std::fs::copy(entry.path(), dest).unwrap();
            }
        }
    }

    #[tokio::test]
    async fn exports_american_stories_and_ocr_local_texts_from_curated() {
        use arrow_array::builder::{Int32Builder, LargeStringBuilder, StringBuilder};
        use arrow_array::RecordBatch;
        use arrow_schema::{DataType, Field, Schema};
        use parquet::arrow::ArrowWriter;
        use std::sync::Arc;
        use usnm_ingest::ocr_local::pages::{encode as encode_ocr_local, Imported, Scores};
        use usnm_overlay::OverlayRow;

        let tmp = tempfile::tempdir().unwrap();
        let curated_dir = tmp.path().join("curated");
        let curated = usnm_store::open(curated_dir.to_str().unwrap()).unwrap();
        let doc_id = "sn83045462_1890-05-03_ed-1_seq-1";

        let as_schema = Arc::new(Schema::new(vec![
            Field::new("doc_id", DataType::Utf8, false),
            Field::new("text", DataType::LargeUtf8, false),
            Field::new("articles", DataType::LargeUtf8, true),
            Field::new("width", DataType::Int32, true),
            Field::new("height", DataType::Int32, true),
        ]));
        let mut b_doc = StringBuilder::new();
        let mut b_txt = LargeStringBuilder::new();
        let mut b_art = LargeStringBuilder::new();
        let mut b_w = Int32Builder::new();
        let mut b_h = Int32Builder::new();
        b_doc.append_value(doc_id);
        b_txt.append_value("American Stories text");
        b_art.append_value(r#"[{"bboxes":[[10,20,300,400]],"legibility":["legible","unknown"]}]"#);
        b_w.append_value(1000);
        b_h.append_value(2000);
        let batch = RecordBatch::try_new(
            as_schema.clone(),
            vec![
                Arc::new(b_doc.finish()),
                Arc::new(b_txt.finish()),
                Arc::new(b_art.finish()),
                Arc::new(b_w.finish()),
                Arc::new(b_h.finish()),
            ],
        )
        .unwrap();
        let mut as_parquet = Vec::new();
        let mut w = ArrowWriter::try_new(&mut as_parquet, as_schema, None).unwrap();
        w.write(&batch).unwrap();
        w.close().unwrap();
        curated
            .put(
                "american-stories/pages/sn83045462/1890-000.parquet",
                as_parquet,
                "application/vnd.apache.parquet",
            )
            .await
            .unwrap();

        let imported = Imported {
            row: OverlayRow {
                key: PageKey::from_doc_id(doc_id).unwrap(),
                batch: "dlc_abc".into(),
                ocr_source: "usnm-tesseract".into(),
                ocr_engine: "tesseract/5.5.1 profile=eng".into(),
                loc_text: "bulk".into(),
                text: "Our Tesseract text".into(),
                image_url: "https://example.org/p".into(),
                ocred_at: chrono::DateTime::from_timestamp(1, 0).unwrap(),
            },
            batch_version: 1,
            source_part: "ocr-out/pages/node1/part.parquet".into(),
            node: "node1".into(),
            reason: "bulk".into(),
            ours: Scores {
                lang: Some("eng".into()),
                decision: Some("ok".into()),
                damage_rate: Some(0.01),
                function_share: Some(0.35),
                head: Some(100),
            },
            loc: Scores {
                lang: Some("eng".into()),
                decision: Some("damaged".into()),
                damage_rate: Some(0.25),
                function_share: Some(0.20),
                head: Some(80),
            },
        };
        let ocr_parquet = encode_ocr_local(&[imported]).unwrap();
        curated
            .put(
                "ocr-local/pages/sn83045462/2026-04-01T00-00-00Z-a1.parquet",
                ocr_parquet,
                "application/vnd.apache.parquet",
            )
            .await
            .unwrap();
        let alto_xml = b"<alto>ours</alto>";
        curated
            .put(
                &format!("ocr-local/alto/{doc_id}.node1.20260401T000000Z.xml"),
                alto_xml.to_vec(),
                "application/xml",
            )
            .await
            .unwrap();

        let out_dir = tmp.path().join("texts");
        let sum = texts_export(&TextsExportOptions {
            curated_url: curated_dir.to_str().unwrap().into(),
            american_stories_url: None,
            out_dir: out_dir.clone(),
            sample: None,
            doc_ids: vec![doc_id.into()],
        })
        .await
        .unwrap();
        let all_bytes = sum.curated_bytes;
        assert!(all_bytes > 0);
        assert_eq!(
            sum,
            TextsExportSummary {
                pages: 1,
                written: 1,
                american_stories: 1,
                ocr_local: 1,
                altos: 1,
                curated_bytes: all_bytes,
            }
        );
        // With a local copy of the American Stories shards, the same texts, and the curated
        // store gives only the overlays.
        let local = tmp.path().join("local-copy");
        let shards = curated_dir.join(usnm_ingest::american_stories::PAGES);
        let to = local.join(usnm_ingest::american_stories::PAGES);
        copy_tree(&shards, &to);
        std::fs::remove_dir_all(&shards).unwrap();
        let out2 = tmp.path().join("texts-local");
        let local_sum = texts_export(&TextsExportOptions {
            curated_url: curated_dir.to_str().unwrap().into(),
            american_stories_url: Some(local.to_str().unwrap().into()),
            out_dir: out2.clone(),
            sample: None,
            doc_ids: vec![doc_id.into()],
        })
        .await
        .unwrap();
        assert_eq!(
            (
                local_sum.written,
                local_sum.american_stories,
                local_sum.altos
            ),
            (1, 1, 1)
        );
        assert!(local_sum.curated_bytes < all_bytes);
        assert_eq!(
            std::fs::read(out2.join(format!("{doc_id}.json"))).unwrap(),
            std::fs::read(out_dir.join(format!("{doc_id}.json"))).unwrap()
        );

        let exported: Texts =
            serde_json::from_slice(&std::fs::read(out_dir.join(format!("{doc_id}.json"))).unwrap())
                .unwrap();
        assert_eq!(
            exported.page,
            Some(PageSize {
                width: 1000,
                height: 2000
            })
        );
        assert_eq!(exported.readers.len(), 2);
        assert_eq!(exported.readers[0].name, "as");
        assert_eq!(exported.readers[0].source, "american-stories");
        assert!(!exported.readers[0].alto);
        assert_eq!(exported.readers[0].text, "American Stories text");
        assert_eq!(
            exported.readers[0].regions,
            vec![Region {
                rect: [10, 20, 300, 400],
                kind: Some("article".into()),
                legibility: Some("legible".into()),
            }]
        );
        assert_eq!(exported.readers[1].name, "ours-tesseract");
        assert_eq!(
            exported.readers[1].source,
            "ocr-local:tesseract/5.5.1;profile=eng"
        );
        assert!(exported.readers[1].alto);
        assert_eq!(exported.readers[1].text, "Our Tesseract text");
        assert_eq!(
            std::fs::read(out_dir.join(format!("{doc_id}.ours.xml"))).unwrap(),
            alto_xml
        );
    }
}
