//! `usnm-ocr-worker loc-alto-stats` (#962): LoC's own word confidence per page, from
//! Chronicling America's bulk OCR archives (`bulk-ocr`'s `<batch>_verNN.tar.bz2`, each page's
//! `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`, ALTO with `WC` on every `String`).
//!
//! For each archive it streams the tar through bzip2, reads each page's ALTO with a pull
//! parser (no page is held whole), and writes one Parquet file per archive,
//! `<out>/<archive stem>.parquet`, with one row a page: the word count, the words that
//! carry a confidence, their mean and median, the share under `--threshold`, and the OCR
//! software the ALTO names. A file is written to `.part` and renamed, beside a
//! `<stem>.done` that records the archive's size and the threshold, so a rerun skips
//! archives already read and re-reads one whose size or threshold changed. It only reads the
//! archives, and streams each page's ALTO from the tar entry.

use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use arrow_array::{ArrayRef, Float64Array, Int16Array, Int32Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use serde::Serialize;
pub use usnm_corpus_signals::confidence::{page_stats, PageStats};

/// `sn84021930/1898/11/23/ed-1/seq-1/ocr.xml` -> `sn84021930_1898-11-23_ed-1_seq-1`.
pub fn doc_id_of(path: &str) -> Option<String> {
    let parts: Vec<&str> = path.trim_start_matches("./").split('/').collect();
    let [lccn, y, m, d, ed, seq, "ocr.xml"] = parts.as_slice() else {
        return None;
    };
    let ok = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
    if lccn.is_empty()
        || !lccn
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        || !ok(y, 4)
        || !ok(m, 2)
        || !ok(d, 2)
    {
        return None;
    }
    let num = |s: &str, p: &str| {
        s.strip_prefix(p)
            .filter(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
            .map(str::to_owned)
    };
    let (ed, seq) = (num(ed, "ed-")?, num(seq, "seq-")?);
    Some(format!("{lccn}_{y}-{m}-{d}_ed-{ed}_seq-{seq}"))
}

/// `ak_albatross_ver01.tar.bz2` -> (`ak_albatross`, 1).
pub fn batch_of(archive: &str) -> Option<(String, i16)> {
    let stem = archive.strip_suffix(".tar.bz2")?;
    let (batch, ver) = stem.rsplit_once("_ver")?;
    let v: i16 = ver.parse().ok()?;
    (!batch.is_empty() && v > 0).then(|| (batch.to_owned(), v))
}

/// Every page of one archive.
pub fn archive_stats(path: &Path, threshold: f64) -> anyhow::Result<(Vec<PageStats>, u64)> {
    let file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let bz = bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(1 << 20, file));
    let mut tar = tar::Archive::new(bz);
    let mut out = Vec::new();
    let mut skipped = 0u64;
    for entry in tar.entries()? {
        let mut entry = entry?;
        let p = entry.path()?.to_string_lossy().into_owned();
        let Some(doc) = doc_id_of(&p) else { continue };
        // Streamed straight from the tar entry, capped so a broken entry can't run away.
        let xml = std::io::BufReader::with_capacity(64 << 10, entry.by_ref().take(64 << 20));
        match page_stats(doc, xml, threshold) {
            Ok(s) => out.push(s),
            Err(e) => {
                skipped += 1;
                tracing::warn!(archive = %path.display(), error = %e, "page left out");
            }
        }
    }
    out.sort_by(|a, b| a.doc_id.cmp(&b.doc_id));
    out.dedup_by(|a, b| a.doc_id == b.doc_id);
    Ok((out, skipped))
}

pub fn schema() -> SchemaRef {
    Arc::new(Schema::new(vec![
        Field::new("batch", DataType::Utf8, false),
        Field::new("batch_version", DataType::Int16, false),
        Field::new("doc_id", DataType::Utf8, false),
        Field::new("words", DataType::Int32, false),
        Field::new("words_with_wc", DataType::Int32, false),
        Field::new("wc_mean", DataType::Float64, true),
        Field::new("wc_median", DataType::Float64, true),
        Field::new("wc_below", DataType::Float64, true),
        Field::new("wc_threshold", DataType::Float64, false),
        Field::new("software", DataType::Utf8, true),
        Field::new("archive", DataType::Utf8, false),
    ]))
}

pub fn encode(
    batch: &str,
    version: i16,
    archive: &str,
    threshold: f64,
    rows: &[PageStats],
) -> anyhow::Result<Vec<u8>> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(6).expect("valid level"),
        ))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema(), Some(props))?;
    let n = rows.len();
    let i32_of = |v: u32| i32::try_from(v).unwrap_or(i32::MAX);
    let cols: Vec<ArrayRef> = vec![
        Arc::new(StringArray::from(vec![batch; n])),
        Arc::new(Int16Array::from(vec![version; n])),
        Arc::new(StringArray::from_iter_values(
            rows.iter().map(|r| r.doc_id.as_str()),
        )),
        Arc::new(Int32Array::from_iter_values(
            rows.iter().map(|r| i32_of(r.words)),
        )),
        Arc::new(Int32Array::from_iter_values(
            rows.iter().map(|r| i32_of(r.words_with_wc)),
        )),
        Arc::new(Float64Array::from_iter(rows.iter().map(|r| r.wc_mean))),
        Arc::new(Float64Array::from_iter(rows.iter().map(|r| r.wc_median))),
        Arc::new(Float64Array::from_iter(rows.iter().map(|r| r.wc_below))),
        Arc::new(Float64Array::from(vec![threshold; n])),
        Arc::new(StringArray::from_iter(
            rows.iter().map(|r| r.software.as_deref()),
        )),
        Arc::new(StringArray::from(vec![archive; n])),
    ];
    w.write(&RecordBatch::try_new(schema(), cols)?)?;
    Ok(w.into_inner()?)
}

#[derive(Debug, Clone)]
pub struct Options {
    pub archives: PathBuf,
    pub out: PathBuf,
    pub threshold: f64,
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub archives: usize,
    pub read: usize,
    pub already: usize,
    pub pages: u64,
    pub pages_left_out: u64,
}

/// Reads every finished archive in `archives` not read yet (or whose size changed).
pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    std::fs::create_dir_all(&opts.out)?;
    let mut names: Vec<String> = std::fs::read_dir(&opts.archives)?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".tar.bz2") && !n.starts_with('.'))
        .collect();
    names.sort();
    let mut s = Summary {
        archives: names.len(),
        ..Summary::default()
    };
    for name in names {
        if opts.limit.is_some_and(|l| s.read >= l) {
            break;
        }
        let Some((batch, version)) = batch_of(&name) else {
            continue;
        };
        let path = opts.archives.join(&name);
        let size = std::fs::metadata(&path)?.len();
        let stem = name.trim_end_matches(".tar.bz2");
        let done = opts.out.join(format!("{stem}.done"));
        // The archive's size and the threshold: a new threshold reads every archive again (an
        // older size-only marker never matches).
        let signature = format!("{size} {}", opts.threshold);
        if std::fs::read_to_string(&done)
            .ok()
            .is_some_and(|d| d.trim() == signature)
        {
            s.already += 1;
            continue;
        }
        let (rows, left_out) = archive_stats(&path, opts.threshold)?;
        let body = encode(&batch, version, &name, opts.threshold, &rows)?;
        let part = opts.out.join(format!(".{stem}.parquet.part"));
        std::fs::write(&part, body)?;
        std::fs::rename(&part, opts.out.join(format!("{stem}.parquet")))?;
        std::fs::write(&done, format!("{signature}\n"))?;
        s.read += 1;
        s.pages += rows.len() as u64;
        s.pages_left_out += left_out;
        tracing::info!(archive = %name, pages = rows.len(), left_out, "LoC word confidence read");
    }
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALTO: &str = r#"<?xml version="1.0"?><alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
<Description><OCRProcessing><ocrProcessingStep><processingSoftware><softwareName>ABBYY FineReader</softwareName></processingSoftware></ocrProcessingStep></OCRProcessing></Description>
<Layout><Page><PrintSpace><TextBlock><TextLine>
<String CONTENT="The" WC="0.9"/><SP/><String CONTENT="mayor" WC="0.3"/><String CONTENT="of" WC="0.7"/><String CONTENT="x"/>
</TextLine></TextBlock></PrintSpace></Page></Layout></alto>"#;

    #[test]
    fn page_stats_from_alto() {
        let s = page_stats("d".into(), ALTO.as_bytes(), 0.5).unwrap();
        assert_eq!((s.words, s.words_with_wc), (4, 3));
        assert_eq!(s.wc_mean, Some(0.633333));
        assert_eq!(s.wc_median, Some(0.7));
        assert_eq!(s.wc_below, Some(0.333333));
        assert_eq!(s.software.as_deref(), Some("ABBYY FineReader"));
        let empty = page_stats("e".into(), "<alto/>".as_bytes(), 0.5).unwrap();
        assert_eq!((empty.words, empty.wc_mean), (0, None));
    }

    #[test]
    fn names() {
        assert_eq!(
            doc_id_of("sn84021930/1898/11/23/ed-1/seq-12/ocr.xml").as_deref(),
            Some("sn84021930_1898-11-23_ed-1_seq-12")
        );
        assert_eq!(doc_id_of("sn84021930/1898/11/23/ed-1/seq-1/ocr.txt"), None);
        assert_eq!(doc_id_of("../x/1898/11/23/ed-1/seq-1/ocr.xml"), None);
        assert_eq!(
            batch_of("ak_albatross_ver01.tar.bz2"),
            Some(("ak_albatross".into(), 1))
        );
        assert_eq!(batch_of("x.tar.bz2"), None);
    }

    #[test]
    fn an_archive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("in");
        std::fs::create_dir(&arch).unwrap();
        let mut tarbuf = tar::Builder::new(Vec::new());
        for (p, body) in [
            ("sn1/1900/01/02/ed-1/seq-1/ocr.xml", ALTO),
            ("sn1/1900/01/02/ed-1/seq-1/ocr.txt", "text"),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            tarbuf.append_data(&mut h, p, body.as_bytes()).unwrap();
        }
        let raw = tarbuf.into_inner().unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        std::io::Write::write_all(&mut bz, &raw).unwrap();
        std::fs::write(arch.join("b_x_ver02.tar.bz2"), bz.finish().unwrap()).unwrap();
        let opts = Options {
            archives: arch,
            out: dir.path().join("out"),
            threshold: 0.5,
            limit: None,
        };
        let s = run(&opts).unwrap();
        assert_eq!((s.read, s.pages), (1, 1));
        assert!(opts.out.join("b_x_ver02.parquet").exists());
        let again = run(&opts).unwrap();
        assert_eq!((again.read, again.already), (0, 1));
        let other = Options {
            threshold: 0.7,
            ..opts.clone()
        };
        assert_eq!(
            run(&other).unwrap().read,
            1,
            "a new threshold reads it again"
        );
    }
}
