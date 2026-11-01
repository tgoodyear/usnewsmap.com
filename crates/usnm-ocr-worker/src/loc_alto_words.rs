//! `usnm-ocr-worker loc-alto-words` (#1424): every word LoC's OCR read, in Parquet.
//!
//! The sibling of [`crate::loc_alto`], over the same archives: `loc-alto-stats` keeps a line a
//! page, this keeps a row a `String`, with its place in the page, confidence and box. Each
//! archive becomes `<out>/<archive stem>.parquet`. The schema, mapped to ALTO elements, is in
//! `docs/loc-alto-words.md`.
//!
//! Archives are read in parallel, each by one thread that streams the tar through bzip2 and
//! pull-parses each page's ALTO. Rows go to the Parquet writer as each page ends, so a worker
//! holds its current page and one row group, never an archive. A page whose XML breaks is left
//! out whole. A file is written to `.part`, synced and renamed, then a `<stem>.done` records the
//! archive's size and the schema version, so a rerun skips what's finished and reads an archive
//! again if its size or the schema changed.

use std::collections::HashSet;
use std::io::{BufReader, BufWriter, Read};
use std::ops::Range;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use arrow_array::builder::{
    BooleanBuilder, Float32Builder, Int16Builder, Int32Builder, StringBuilder,
};
use arrow_array::{ArrayRef, RecordBatch};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, Encoding, ZstdLevel};
use parquet::file::properties::{WriterProperties, WriterPropertiesBuilder, WriterVersion};
use parquet::schema::types::ColumnPath;
use quick_xml::events::{BytesRef, BytesStart, Event};
use serde::Serialize;

use crate::loc_alto::{batch_of, doc_id_of};

/// Goes into each `.done`: a change that alters the rows bumps it, and every archive is read
/// again.
pub const SCHEMA_VERSION: u32 = 1;

/// Rows between hand-offs to the Parquet writer, and in a row group.
const FLUSH_ROWS: usize = 1 << 17;
const ROW_GROUP_ROWS: usize = 1 << 20;

/// Coordinates a page needs before its grid can name a dpi.
const MIN_GRID_COORDS: usize = 40;

/// Between readings in `alternatives`. Unit separator: no XML 1.0 document can hold it.
pub const ALTERNATIVE_SEPARATOR: char = '\u{1f}';

/// How a page's ALTO units became pixels.
#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    /// `None`: nothing on the page gives the image's dpi, so its boxes stay in ALTO units.
    pub dpi: Option<f64>,
    pub source: &'static str,
    /// The unit the boxes and page size are stored in: `pixel`, or the ALTO unit.
    pub coord_unit: String,
    factor: f64,
}

#[derive(Debug, Default, Clone, PartialEq)]
pub struct PageHead {
    pub alto_version: Option<String>,
    pub unit: Option<String>,
    /// The first `softwareName` anywhere, as `loc-alto-stats` reports it.
    pub software: Option<String>,
    pub software_version: Option<String>,
    /// The `softwareName` of the `ocrProcessingStep`, which can differ: `software` is often a
    /// preprocessing step's.
    pub ocr_software: Option<String>,
    pub ocr_version: Option<String>,
    /// `abbyy8 8.1.0.6184-12` from `processingStepSettings`: where a wrapper's software names
    /// itself and its version is "Multiple", the engine that read the page.
    pub ocr_engine: Option<String>,
    pub alto_width: Option<f64>,
    pub alto_height: Option<f64>,
    pub pc: Option<f32>,
    pub accuracy: Option<f32>,
    /// `xdpi:` in `processingStepSettings`.
    xdpi: Option<f64>,
    /// `width:` in `processingStepSettings`: the image's width in pixels.
    image_width: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Default)]
struct Word {
    block: i32,
    line: i32,
    word: i32,
    text: Range<usize>,
    lang: Option<Range<usize>>,
    cc: Option<Range<usize>>,
    subs_content: Option<Range<usize>>,
    subs_type: Option<Range<usize>>,
    alternatives: Option<Range<usize>>,
    wc: Option<f32>,
    hyphen: bool,
    x: Option<f64>,
    y: Option<f64>,
    w: Option<f64>,
    h: Option<f64>,
}

/// One page's words. The strings share one buffer, reused from page to page.
#[derive(Debug, Default)]
pub struct Page {
    pub head: PageHead,
    words: Vec<Word>,
    buf: String,
}

impl Page {
    fn clear(&mut self) {
        self.head = PageHead::default();
        self.words.clear();
        self.buf.clear();
    }

    fn keep(&mut self, s: &str) -> Range<usize> {
        let start = self.buf.len();
        self.buf.push_str(s);
        start..self.buf.len()
    }

    /// `ALTERNATIVE` readings run after their word in the buffer, a separator between them,
    /// so nothing else can land in the middle.
    fn open_alternative(&mut self) {
        let start = self.buf.len();
        if let Some(w) = self.words.last_mut() {
            match &mut w.alternatives {
                Some(r) => {
                    self.buf.push(ALTERNATIVE_SEPARATOR);
                    r.end = self.buf.len();
                }
                None => w.alternatives = Some(start..start),
            }
        }
    }

    fn extend_alternative(&mut self, s: &str) {
        if let Some(r) = self.words.last_mut().and_then(|w| w.alternatives.as_mut()) {
            self.buf.push_str(s);
            r.end = self.buf.len();
        }
    }

    pub fn word_count(&self) -> usize {
        self.words.len()
    }

    /// The dpi and unit for the page's boxes: what the ALTO states, else the image width it
    /// states, else the grid its coordinates sit on.
    pub fn scale(&self) -> Scale {
        let unit = self.head.unit.as_deref().unwrap_or("").trim();
        let per_inch = match unit.to_ascii_lowercase().as_str() {
            "pixel" => {
                return Scale {
                    dpi: None,
                    source: "pixel",
                    coord_unit: "pixel".into(),
                    factor: 1.0,
                }
            }
            "inch1200" => 1200.0,
            "mm10" => 254.0,
            _ => {
                return Scale {
                    dpi: None,
                    source: "none",
                    coord_unit: if unit.is_empty() { "unknown" } else { unit }.into(),
                    factor: 1.0,
                }
            }
        };
        let stated = self.head.xdpi.filter(|d| *d > 0.0).map(|d| (d, "settings"));
        let sized = || {
            let (px, units) = (self.head.image_width?, self.head.alto_width?);
            (px > 0.0 && units > 0.0).then(|| ((px * per_inch / units).round(), "image_width"))
        };
        let grid = || {
            (unit.eq_ignore_ascii_case("inch1200"))
                .then(|| self.grid_dpi())
                .flatten()
        };
        match stated
            .or_else(sized)
            .or_else(|| grid().map(|d| (d, "grid")))
        {
            Some((dpi, source)) => Scale {
                dpi: Some(dpi),
                source,
                coord_unit: "pixel".into(),
                factor: dpi / per_inch,
            },
            None => Scale {
                dpi: None,
                source: "none",
                coord_unit: unit.into(),
                factor: 1.0,
            },
        }
    }

    /// An inch1200 scan at 300 dpi has every coordinate a multiple of 4, at 400 of 3, at 600
    /// of 2. A page that's a multiple of 12 could be either, and names nothing.
    fn grid_dpi(&self) -> Option<f64> {
        let coords = || {
            self.words
                .iter()
                .flat_map(|w| [w.x, w.y, w.w, w.h])
                .flatten()
        };
        if coords().count() < MIN_GRID_COORDS {
            return None;
        }
        let mut g = 0u64;
        for v in coords() {
            if v.fract() != 0.0 || v < 0.0 {
                return None;
            }
            g = gcd(g, v as u64);
            if g == 1 {
                return None;
            }
        }
        match (
            g.is_multiple_of(4),
            g.is_multiple_of(3),
            g.is_multiple_of(2),
        ) {
            (true, false, _) => Some(300.0),
            (false, true, _) => Some(400.0),
            (false, false, true) => Some(600.0),
            _ => None,
        }
    }
}

fn gcd(mut a: u64, mut b: u64) -> u64 {
    while b != 0 {
        (a, b) = (b, a % b);
    }
    a
}

fn number(v: &str) -> Option<f64> {
    v.trim().parse::<f64>().ok().filter(|x| x.is_finite())
}

/// `CONTENT` and its like, with entities decoded. Most values have none, so they skip the
/// decoder.
fn value_of(a: &quick_xml::events::attributes::Attribute<'_>) -> String {
    if !a.value.contains(['&', '\t', '\n', '\r']) {
        return a.value.to_string();
    }
    match a.normalized_value(quick_xml::XmlVersion::Implicit1_0) {
        Ok(v) => v.into_owned(),
        Err(_) => a.value.to_string(),
    }
}

/// `key:` followed by a number, anywhere in `processingStepSettings` (some producers run the
/// settings together with no line breaks, so `key` must not follow a letter).
fn setting(s: &str, key: &str) -> Option<f64> {
    let mut from = 0;
    while let Some(i) = s[from..].find(key) {
        let at = from + i;
        from = at + key.len();
        if at > 0 && s.as_bytes()[at - 1].is_ascii_alphabetic() {
            continue;
        }
        let rest = &s[at + key.len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.')
            .unwrap_or(rest.len());
        if let Ok(v) = rest[..end].trim_end_matches('.').parse::<f64>() {
            return Some(v);
        }
    }
    None
}

/// A reference in element text. ALTO text has only the XML-defined ones.
fn entity(r: &BytesRef<'_>) -> Option<String> {
    if let Ok(Some(c)) = r.resolve_char_ref() {
        return Some(c.to_string());
    }
    let s = match &**r {
        "amp" => "&",
        "lt" => "<",
        "gt" => ">",
        "quot" => "\"",
        "apos" => "'",
        _ => return None,
    };
    Some(s.to_owned())
}

/// The first `<name>.version:<number>` in `processingStepSettings`.
fn engine_of(s: &str) -> Option<String> {
    let at = s.find(".version:")?;
    let start = s[..at]
        .rfind(|c: char| !c.is_ascii_alphanumeric())
        .map_or(0, |i| i + 1);
    let rest = &s[at + ".version:".len()..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
        .unwrap_or(rest.len());
    let (name, version) = (&s[start..at], &rest[..end]);
    (!name.is_empty() && !version.is_empty()).then(|| format!("{name} {version}"))
}

#[derive(Clone, Copy, PartialEq)]
enum Open {
    None,
    Unit,
    Software,
    Version,
    Settings,
    Alternative,
}

/// The ALTO version from the root element: `2.0` from a LoC namespace or a schema's file name,
/// else the namespace itself, so a producer's own dialect stays visible.
fn alto_version(root: &BytesStart<'_>) -> Option<String> {
    let (mut ns, mut schema) = (None, None);
    for a in root.attributes().flatten() {
        match a.key.as_ref() {
            "xmlns" => ns = Some(value_of(&a)),
            "xsi:schemaLocation" | "xsi:noNamespaceSchemaLocation" => {
                schema = Some(value_of(&a));
            }
            _ => {}
        }
    }
    if let Some(v) = schema.as_deref().and_then(schema_version) {
        return Some(v);
    }
    ns.map(|n| match n.split_once("/ns-v") {
        Some((_, v)) => v.trim_end_matches('#').to_owned(),
        None => n,
    })
}

/// `.../alto-1-4.xsd` -> `1.4`; `.../alto-v2.0.xsd` -> `2.0`.
fn schema_version(s: &str) -> Option<String> {
    let file = s.split_whitespace().last()?.rsplit('/').next()?;
    let v = file.strip_prefix("alto-")?.strip_suffix(".xsd")?;
    let v = v.strip_prefix('v').unwrap_or(v).replace('-', ".");
    v.bytes()
        .all(|b| b.is_ascii_digit() || b == b'.')
        .then_some(v)
}

/// Reads one ALTO stream into `page`, which is cleared first.
pub fn read_page<R: std::io::BufRead>(xml: R, page: &mut Page) -> anyhow::Result<()> {
    page.clear();
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let (mut block, mut line, mut word) = (-1i32, -1i32, -1i32);
    let mut lang: Option<Range<usize>> = None;
    let mut open = Open::None;
    let mut in_ocr_step = false;
    let mut seen_root = false;
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| anyhow::anyhow!("unreadable ALTO: {e}"))?;
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                let is_start = matches!(event, Event::Start(_));
                match e.local_name().as_ref() {
                    "alto" if !seen_root => {
                        seen_root = true;
                        page.head.alto_version = alto_version(e);
                    }
                    "String" => {
                        word += 1;
                        let w = read_string(e, page, block, line, word, lang.clone());
                        page.words.push(w);
                    }
                    "SP" => {}
                    "ALTERNATIVE" if is_start => {
                        open = Open::Alternative;
                        page.open_alternative();
                    }
                    "HYP" => {
                        if let Some(w) = page.words.last_mut() {
                            w.hyphen = true;
                        }
                    }
                    "TextLine" => {
                        line += 1;
                        word = -1;
                    }
                    "TextBlock" => {
                        block += 1;
                        line = -1;
                        word = -1;
                        lang = None;
                        for a in e.attributes().flatten() {
                            let k = a.key.as_ref();
                            if k == "language" || k == "LANG" {
                                let v = value_of(&a);
                                lang = Some(page.keep(&v));
                            }
                        }
                    }
                    "Page" => read_page_element(e, &mut page.head),
                    "ocrProcessingStep" if is_start => in_ocr_step = true,
                    "MeasurementUnit" if is_start => open = Open::Unit,
                    "softwareName" if is_start => open = Open::Software,
                    "softwareVersion" if is_start => open = Open::Version,
                    "processingStepSettings" if is_start => open = Open::Settings,
                    _ => {}
                }
            }
            Event::Text(ref t) if open == Open::Alternative => {
                page.extend_alternative(&t.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(ref r) if open == Open::Alternative => {
                if let Some(s) = entity(r) {
                    page.extend_alternative(&s);
                }
            }
            Event::Text(ref t) if open != Open::None => {
                let s = t.xml_content(quick_xml::XmlVersion::Implicit1_0);
                let s = s.trim();
                if !s.is_empty() {
                    read_description_text(open, s, in_ocr_step, &mut page.head);
                }
            }
            Event::End(ref e) => {
                open = Open::None;
                if e.local_name().as_ref() == "ocrProcessingStep" {
                    in_ocr_step = false;
                }
            }
            Event::Eof => break,
            _ => {}
        }
        buf.clear();
    }
    anyhow::ensure!(seen_root, "no alto element");
    Ok(())
}

fn read_description_text(open: Open, s: &str, in_ocr_step: bool, head: &mut PageHead) {
    let short = || s.chars().take(64).collect::<String>();
    match open {
        Open::Unit => head.unit = Some(s.to_owned()),
        Open::Software => {
            head.software.get_or_insert_with(short);
            if in_ocr_step {
                head.ocr_software.get_or_insert_with(short);
            }
        }
        Open::Version => {
            head.software_version.get_or_insert_with(short);
            if in_ocr_step {
                head.ocr_version.get_or_insert_with(short);
            }
        }
        Open::Settings => {
            head.xdpi = head.xdpi.or_else(|| setting(s, "xdpi:"));
            head.image_width = head.image_width.or_else(|| setting(s, "width:"));
            if head.ocr_engine.is_none() {
                head.ocr_engine = engine_of(s);
            }
        }
        Open::None | Open::Alternative => {}
    }
}

fn read_page_element(e: &BytesStart<'_>, head: &mut PageHead) {
    for a in e.attributes().flatten() {
        match a.key.as_ref() {
            "WIDTH" => head.alto_width = head.alto_width.or(number(&a.value)),
            "HEIGHT" => head.alto_height = head.alto_height.or(number(&a.value)),
            "PC" => head.pc = head.pc.or(number(&a.value).map(|v| v as f32)),
            "ACCURACY" => head.accuracy = head.accuracy.or(number(&a.value).map(|v| v as f32)),
            _ => {}
        }
    }
}

fn read_string(
    e: &BytesStart<'_>,
    page: &mut Page,
    block: i32,
    line: i32,
    word: i32,
    lang: Option<Range<usize>>,
) -> Word {
    let mut w = Word {
        block,
        line,
        word,
        lang,
        ..Word::default()
    };
    for a in e.attributes().flatten() {
        match a.key.as_ref() {
            "CONTENT" => w.text = page.keep(&value_of(&a)),
            "WC" => {
                // As `loc-alto-stats` counts it: outside 0 to 1 is no confidence.
                w.wc = number(&a.value)
                    .filter(|v| (0.0..=1.0).contains(v))
                    .map(|v| v as f32);
            }
            "CC" => w.cc = Some(page.keep(&value_of(&a))),
            "SUBS_CONTENT" => w.subs_content = Some(page.keep(&value_of(&a))),
            "SUBS_TYPE" => w.subs_type = Some(page.keep(&value_of(&a))),
            "HPOS" => w.x = number(&a.value),
            "VPOS" => w.y = number(&a.value),
            "WIDTH" => w.w = number(&a.value),
            "HEIGHT" => w.h = number(&a.value),
            _ => {}
        }
    }
    w
}

pub fn schema() -> SchemaRef {
    use DataType::{Boolean, Float32, Int16, Int32, Utf8};
    let f = |n: &str, t: DataType, null: bool| Field::new(n, t, null);
    Arc::new(Schema::new(vec![
        f("batch", Utf8, false),
        f("batch_version", Int16, false),
        f("doc_id", Utf8, false),
        f("block", Int32, false),
        f("line", Int32, false),
        f("word", Int32, false),
        f("text", Utf8, true),
        f("wc", Float32, true),
        f("cc", Utf8, true),
        f("hyphen", Boolean, false),
        f("subs_content", Utf8, true),
        f("subs_type", Utf8, true),
        f("alternatives", Utf8, true),
        f("lang", Utf8, true),
        f("x", Int32, true),
        f("y", Int32, true),
        f("w", Int32, true),
        f("h", Int32, true),
        f("page_width", Int32, true),
        f("page_height", Int32, true),
        f("coord_unit", Utf8, false),
        f("alto_unit", Utf8, true),
        f("dpi", Float32, true),
        f("dpi_source", Utf8, false),
        f("alto_version", Utf8, true),
        f("software", Utf8, true),
        f("software_version", Utf8, true),
        f("ocr_software", Utf8, true),
        f("ocr_version", Utf8, true),
        f("ocr_engine", Utf8, true),
        f("page_pc", Float32, true),
        f("page_accuracy", Float32, true),
    ]))
}

fn writer_properties() -> WriterProperties {
    // A word's `x` and `y` sit close to the last word's: 10% off the file on a sampled archive.
    let delta = |b: WriterPropertiesBuilder, name: &str| {
        b.set_column_dictionary_enabled(ColumnPath::from(name), false)
            .set_column_encoding(ColumnPath::from(name), Encoding::DELTA_BINARY_PACKED)
    };
    delta(delta(WriterProperties::builder(), "x"), "y")
        .set_writer_version(WriterVersion::PARQUET_2_0)
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(6).expect("valid level"),
        ))
        .set_max_row_group_row_count(Some(ROW_GROUP_ROWS))
        .build()
}

/// The columns being filled, handed to the Parquet writer every [`FLUSH_ROWS`] rows.
struct Rows {
    batch: StringBuilder,
    batch_version: Int16Builder,
    doc_id: StringBuilder,
    block: Int32Builder,
    line: Int32Builder,
    word: Int32Builder,
    text: StringBuilder,
    wc: Float32Builder,
    cc: StringBuilder,
    hyphen: BooleanBuilder,
    subs_content: StringBuilder,
    subs_type: StringBuilder,
    alternatives: StringBuilder,
    lang: StringBuilder,
    x: Int32Builder,
    y: Int32Builder,
    w: Int32Builder,
    h: Int32Builder,
    page_width: Int32Builder,
    page_height: Int32Builder,
    coord_unit: StringBuilder,
    alto_unit: StringBuilder,
    dpi: Float32Builder,
    dpi_source: StringBuilder,
    alto_version: StringBuilder,
    software: StringBuilder,
    software_version: StringBuilder,
    ocr_software: StringBuilder,
    ocr_version: StringBuilder,
    ocr_engine: StringBuilder,
    page_pc: Float32Builder,
    page_accuracy: Float32Builder,
    len: usize,
}

impl Rows {
    fn new() -> Self {
        Self {
            batch: StringBuilder::new(),
            batch_version: Int16Builder::new(),
            doc_id: StringBuilder::new(),
            block: Int32Builder::new(),
            line: Int32Builder::new(),
            word: Int32Builder::new(),
            text: StringBuilder::new(),
            wc: Float32Builder::new(),
            cc: StringBuilder::new(),
            hyphen: BooleanBuilder::new(),
            subs_content: StringBuilder::new(),
            subs_type: StringBuilder::new(),
            alternatives: StringBuilder::new(),
            lang: StringBuilder::new(),
            x: Int32Builder::new(),
            y: Int32Builder::new(),
            w: Int32Builder::new(),
            h: Int32Builder::new(),
            page_width: Int32Builder::new(),
            page_height: Int32Builder::new(),
            coord_unit: StringBuilder::new(),
            alto_unit: StringBuilder::new(),
            dpi: Float32Builder::new(),
            dpi_source: StringBuilder::new(),
            alto_version: StringBuilder::new(),
            software: StringBuilder::new(),
            software_version: StringBuilder::new(),
            ocr_software: StringBuilder::new(),
            ocr_version: StringBuilder::new(),
            ocr_engine: StringBuilder::new(),
            page_pc: Float32Builder::new(),
            page_accuracy: Float32Builder::new(),
            len: 0,
        }
    }

    fn head(&mut self, doc: &str, batch: &str, version: i16, page: &Page, scale: &Scale) {
        let h = &page.head;
        let px = |v: Option<f64>| v.and_then(|v| to_i32(v * scale.factor));
        self.batch.append_value(batch);
        self.batch_version.append_value(version);
        self.doc_id.append_value(doc);
        self.page_width.append_option(px(h.alto_width));
        self.page_height.append_option(px(h.alto_height));
        self.coord_unit.append_value(&scale.coord_unit);
        self.alto_unit.append_option(h.unit.as_deref());
        self.dpi.append_option(scale.dpi.map(|d| d as f32));
        self.dpi_source.append_value(scale.source);
        self.alto_version.append_option(h.alto_version.as_deref());
        self.software.append_option(h.software.as_deref());
        self.software_version
            .append_option(h.software_version.as_deref());
        self.ocr_software.append_option(h.ocr_software.as_deref());
        self.ocr_version.append_option(h.ocr_version.as_deref());
        self.ocr_engine.append_option(h.ocr_engine.as_deref());
        self.page_pc.append_option(h.pc);
        self.page_accuracy.append_option(h.accuracy);
        self.len += 1;
    }

    /// One row a word. A page with none still gets a row, with null `text` and `-1` ordinals,
    /// so the file names every page.
    fn page(&mut self, doc: &str, batch: &str, version: i16, page: &Page) {
        let scale = page.scale();
        let px = |v: Option<f64>| v.and_then(|v| to_i32(v * scale.factor));
        let sub = |r: &Option<Range<usize>>| r.clone().map(|r| &page.buf[r]);
        if page.words.is_empty() {
            self.head(doc, batch, version, page, &scale);
            for c in [&mut self.block, &mut self.line, &mut self.word] {
                c.append_value(-1);
            }
            self.text.append_null();
            self.wc.append_null();
            self.cc.append_null();
            self.hyphen.append_value(false);
            self.subs_content.append_null();
            self.subs_type.append_null();
            self.alternatives.append_null();
            self.lang.append_null();
            for c in [&mut self.x, &mut self.y, &mut self.w, &mut self.h] {
                c.append_null();
            }
        }
        for w in &page.words {
            self.head(doc, batch, version, page, &scale);
            self.block.append_value(w.block);
            self.line.append_value(w.line);
            self.word.append_value(w.word);
            self.text.append_value(&page.buf[w.text.clone()]);
            self.wc.append_option(w.wc);
            self.cc.append_option(sub(&w.cc));
            self.hyphen.append_value(w.hyphen);
            self.subs_content.append_option(sub(&w.subs_content));
            self.subs_type.append_option(sub(&w.subs_type));
            self.alternatives.append_option(sub(&w.alternatives));
            self.lang.append_option(sub(&w.lang));
            self.x.append_option(px(w.x));
            self.y.append_option(px(w.y));
            self.w.append_option(px(w.w));
            self.h.append_option(px(w.h));
        }
    }

    fn finish(&mut self) -> anyhow::Result<RecordBatch> {
        self.len = 0;
        let cols: Vec<ArrayRef> = vec![
            Arc::new(self.batch.finish()),
            Arc::new(self.batch_version.finish()),
            Arc::new(self.doc_id.finish()),
            Arc::new(self.block.finish()),
            Arc::new(self.line.finish()),
            Arc::new(self.word.finish()),
            Arc::new(self.text.finish()),
            Arc::new(self.wc.finish()),
            Arc::new(self.cc.finish()),
            Arc::new(self.hyphen.finish()),
            Arc::new(self.subs_content.finish()),
            Arc::new(self.subs_type.finish()),
            Arc::new(self.alternatives.finish()),
            Arc::new(self.lang.finish()),
            Arc::new(self.x.finish()),
            Arc::new(self.y.finish()),
            Arc::new(self.w.finish()),
            Arc::new(self.h.finish()),
            Arc::new(self.page_width.finish()),
            Arc::new(self.page_height.finish()),
            Arc::new(self.coord_unit.finish()),
            Arc::new(self.alto_unit.finish()),
            Arc::new(self.dpi.finish()),
            Arc::new(self.dpi_source.finish()),
            Arc::new(self.alto_version.finish()),
            Arc::new(self.software.finish()),
            Arc::new(self.software_version.finish()),
            Arc::new(self.ocr_software.finish()),
            Arc::new(self.ocr_version.finish()),
            Arc::new(self.ocr_engine.finish()),
            Arc::new(self.page_pc.finish()),
            Arc::new(self.page_accuracy.finish()),
        ];
        Ok(RecordBatch::try_new(schema(), cols)?)
    }
}

fn to_i32(v: f64) -> Option<i32> {
    let r = v.round();
    (r.is_finite() && r.abs() < f64::from(i32::MAX)).then_some(r as i32)
}

#[derive(Debug, Default, Clone, Copy, Serialize)]
pub struct ArchiveCount {
    pub pages: u64,
    pub words: u64,
    pub pages_left_out: u64,
    pub duplicate_pages: u64,
}

/// Streams one archive into `part`.
pub fn write_archive(
    path: &Path,
    part: &Path,
    batch: &str,
    version: i16,
) -> anyhow::Result<ArchiveCount> {
    let file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let bz = bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(1 << 20, file));
    let mut tar = tar::Archive::new(bz);
    let out = BufWriter::with_capacity(1 << 20, std::fs::File::create(part)?);
    let mut w = ArrowWriter::try_new(out, schema(), Some(writer_properties()))?;
    let mut rows = Rows::new();
    let mut page = Page::default();
    let mut seen: HashSet<String> = HashSet::new();
    let mut n = ArchiveCount::default();
    for entry in tar.entries()? {
        let mut entry = entry?;
        let p = entry.path()?.to_string_lossy().into_owned();
        let Some(doc) = doc_id_of(&p) else { continue };
        if seen.contains(&doc) {
            n.duplicate_pages += 1;
            continue;
        }
        // Capped as `loc-alto-stats` caps it, so a broken entry can't run away.
        let xml = BufReader::with_capacity(64 << 10, entry.by_ref().take(64 << 20));
        if let Err(e) = read_page(xml, &mut page) {
            n.pages_left_out += 1;
            tracing::warn!(archive = %path.display(), doc = %doc, error = %e, "page left out");
            continue;
        }
        rows.page(&doc, batch, version, &page);
        n.pages += 1;
        n.words += page.word_count() as u64;
        seen.insert(doc);
        if rows.len >= FLUSH_ROWS {
            w.write(&rows.finish()?)?;
        }
    }
    if rows.len > 0 {
        w.write(&rows.finish()?)?;
    }
    let out = w.into_inner()?;
    let file = out.into_inner().map_err(|e| e.into_error())?;
    file.sync_all()?;
    Ok(n)
}

#[derive(Debug, Clone)]
pub struct Options {
    pub archives: PathBuf,
    pub out: PathBuf,
    /// Archives read at once, each on one thread.
    pub workers: usize,
    pub limit: Option<usize>,
    /// Stops starting archives when the output's filesystem has less free than this.
    pub min_free_bytes: u64,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub archives: usize,
    pub read: usize,
    pub already: usize,
    pub failed: usize,
    pub stopped_low_disk: bool,
    pub pages: u64,
    pub words: u64,
    pub bytes: u64,
    pub pages_left_out: u64,
    pub duplicate_pages: u64,
}

struct Job {
    name: String,
    stem: String,
    batch: String,
    version: i16,
    signature: String,
}

fn free_bytes(dir: &Path) -> anyhow::Result<u64> {
    let s = rustix::fs::statvfs(dir)?;
    Ok(s.f_bavail.saturating_mul(s.f_frsize))
}

/// Reads every finished archive in `archives` not read yet (or whose size changed), in
/// `workers` threads. A failed archive doesn't stop the others; the run then returns an error.
pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    anyhow::ensure!(opts.workers > 0, "--workers: at least 1");
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
    let mut jobs = Vec::new();
    for name in names {
        if opts.limit.is_some_and(|l| jobs.len() >= l) {
            break;
        }
        let Some((batch, version)) = batch_of(&name) else {
            continue;
        };
        let size = std::fs::metadata(opts.archives.join(&name))?.len();
        let stem = name.trim_end_matches(".tar.bz2").to_owned();
        let signature = format!("{size} {SCHEMA_VERSION}");
        let done = opts.out.join(format!("{stem}.done"));
        if std::fs::read_to_string(&done)
            .ok()
            .is_some_and(|d| d.trim() == signature)
        {
            s.already += 1;
            continue;
        }
        jobs.push(Job {
            name,
            stem,
            batch,
            version,
            signature,
        });
    }
    let next = AtomicUsize::new(0);
    let low_disk = AtomicBool::new(false);
    let total = Mutex::new(s);
    std::thread::scope(|scope| {
        for _ in 0..opts.workers {
            scope.spawn(|| loop {
                let Some(job) = jobs.get(next.fetch_add(1, Ordering::Relaxed)) else {
                    return;
                };
                match free_bytes(&opts.out) {
                    Ok(f) if f >= opts.min_free_bytes => {}
                    Ok(f) => {
                        tracing::error!(free = f, "stopping: too little disk free");
                        low_disk.store(true, Ordering::Relaxed);
                        next.store(jobs.len(), Ordering::Relaxed);
                        return;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "stopping: no free-space reading");
                        low_disk.store(true, Ordering::Relaxed);
                        next.store(jobs.len(), Ordering::Relaxed);
                        return;
                    }
                }
                let got = one_archive(opts, job);
                let mut t = total.lock().expect("no thread panics holding it");
                match got {
                    Ok((n, bytes)) => {
                        t.read += 1;
                        t.pages += n.pages;
                        t.words += n.words;
                        t.bytes += bytes;
                        t.pages_left_out += n.pages_left_out;
                        t.duplicate_pages += n.duplicate_pages;
                        tracing::info!(archive = %job.name, pages = n.pages, words = n.words,
                            bytes, left_out = n.pages_left_out, "LoC ALTO words written");
                    }
                    Err(e) => {
                        t.failed += 1;
                        tracing::error!(archive = %job.name, error = %format!("{e:#}"),
                            "archive failed");
                    }
                }
            });
        }
    });
    let mut s = total.into_inner().expect("no thread panicked");
    s.stopped_low_disk = low_disk.load(Ordering::Relaxed);
    Ok(s)
}

fn one_archive(opts: &Options, job: &Job) -> anyhow::Result<(ArchiveCount, u64)> {
    let part = opts.out.join(format!(".{}.parquet.part", job.stem));
    let done = opts.out.join(format!("{}.done", job.stem));
    let name = opts.out.join(format!("{}.parquet", job.stem));
    let path = opts.archives.join(&job.name);
    let before = std::fs::metadata(&path)?.len();
    let got = write_archive(&path, &part, &job.batch, job.version);
    let n = match got {
        Ok(n) => n,
        Err(e) => {
            std::fs::remove_file(&part).ok();
            return Err(e);
        }
    };
    // An archive replaced under us isn't what the signature says.
    if std::fs::metadata(&path)?.len() != before {
        std::fs::remove_file(&part).ok();
        anyhow::bail!("{} changed while being read", job.name);
    }
    let bytes = std::fs::metadata(&part)?.len();
    std::fs::rename(&part, &name)?;
    std::fs::write(&done, format!("{}\n", job.signature))?;
    Ok((n, bytes))
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::cast::AsArray;
    use arrow_array::types::{Float32Type, Int32Type};
    use arrow_array::Array;
    use parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder;

    const V2: &str = r#"<?xml version="1.0"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v2#">
<Description><MeasurementUnit>inch1200</MeasurementUnit>
<OCRProcessing ID="O"><preProcessingStep><processingSoftware><softwareName>PolishImage</softwareName><softwareVersion>1.99</softwareVersion></processingSoftware></preProcessingStep>
<ocrProcessingStep><processingStepSettings>
width:5000
xdpi:320
</processingStepSettings><processingSoftware><softwareName>ABBYY FineReader</softwareName><softwareVersion>8.1</softwareVersion></processingSoftware></ocrProcessingStep></OCRProcessing></Description>
<Layout><Page ID="P" WIDTH="18750" HEIGHT="24000" PC="0.8" ACCURACY="91.5"><PrintSpace>
<TextBlock ID="T1" language="eng"><TextLine>
<String HPOS="750" VPOS="1500.0" WIDTH="375" HEIGHT="150" CONTENT="Fish &amp; chips" WC="0.91" CC="0 1"/><SP/><String CONTENT="sup-" SUBS_CONTENT="supper" SUBS_TYPE="HypPart1" WC="1.5" HPOS="1"/><HYP CONTENT="-"/>
</TextLine><TextLine><String CONTENT="per"/></TextLine></TextBlock>
<ComposedBlock><TextBlock ID="T2"><TextLine><String CONTENT="deux" WC="0"><ALTERNATIVE>dcux</ALTERNATIVE><ALTERNATIVE>d&amp;ux</ALTERNATIVE></String></TextLine></TextBlock></ComposedBlock>
</PrintSpace></Page></Layout></alto>"#;

    fn parse(xml: &str) -> Page {
        let mut p = Page::default();
        read_page(xml.as_bytes(), &mut p).unwrap();
        p
    }

    #[test]
    fn a_page_reads_in_order_with_its_description() {
        let p = parse(V2);
        let h = &p.head;
        assert_eq!(h.alto_version.as_deref(), Some("2"));
        assert_eq!(h.unit.as_deref(), Some("inch1200"));
        assert_eq!(h.software.as_deref(), Some("PolishImage"));
        assert_eq!(h.software_version.as_deref(), Some("1.99"));
        assert_eq!(h.ocr_software.as_deref(), Some("ABBYY FineReader"));
        assert_eq!(h.ocr_version.as_deref(), Some("8.1"));
        assert_eq!(
            (h.alto_width, h.alto_height),
            (Some(18750.0), Some(24000.0))
        );
        assert_eq!((h.pc, h.accuracy), (Some(0.8), Some(91.5)));
        assert_eq!(p.word_count(), 4);
        let at: Vec<_> = p.words.iter().map(|w| (w.block, w.line, w.word)).collect();
        assert_eq!(at, [(0, 0, 0), (0, 0, 1), (0, 1, 0), (1, 0, 0)]);
        let w = &p.words[0];
        assert_eq!(&p.buf[w.text.clone()], "Fish & chips");
        assert_eq!(w.wc, Some(0.91));
        assert_eq!(&p.buf[w.cc.clone().unwrap()], "0 1");
        assert_eq!(&p.buf[w.lang.clone().unwrap()], "eng");
        assert_eq!(
            (w.x, w.y, w.w, w.h),
            (Some(750.0), Some(1500.0), Some(375.0), Some(150.0))
        );
        let w = &p.words[1];
        assert!(w.hyphen);
        assert_eq!(w.wc, None, "out of range counts as no confidence");
        assert_eq!(&p.buf[w.subs_content.clone().unwrap()], "supper");
        assert_eq!(&p.buf[w.subs_type.clone().unwrap()], "HypPart1");
        assert_eq!(p.words[3].wc, Some(0.0), "zero is a confidence");
        assert_eq!(
            &p.buf[p.words[3].alternatives.clone().unwrap()],
            "dcux\u{1f}d&ux"
        );
        assert_eq!(p.words[3].lang, None, "the language is its own block's");
    }

    #[test]
    fn scale_prefers_what_the_alto_states() {
        let s = parse(V2).scale();
        assert_eq!((s.dpi, s.source), (Some(320.0), "settings"));
        assert_eq!(s.coord_unit, "pixel");
        assert!((s.factor - 320.0 / 1200.0).abs() < 1e-12);

        let sized = V2.replace("xdpi:320", "");
        let s = parse(&sized).scale();
        assert_eq!((s.dpi, s.source), (Some(320.0), "image_width"));
    }

    fn grid_page(step: u32) -> String {
        let strings: String = (1..=12)
            .map(|i| {
                let v = i * step;
                format!(r#"<String CONTENT="a" HPOS="{v}" VPOS="{v}" WIDTH="{v}" HEIGHT="{v}"/>"#)
            })
            .collect();
        format!(
            "<alto><Description><MeasurementUnit>inch1200</MeasurementUnit></Description>\
             <Layout><Page><TextBlock><TextLine>{strings}</TextLine></TextBlock></Page></Layout></alto>"
        )
    }

    #[test]
    fn scale_falls_back_to_the_coordinate_grid() {
        assert_eq!(parse(&grid_page(4)).scale().dpi, Some(300.0));
        assert_eq!(parse(&grid_page(3)).scale().dpi, Some(400.0));
        assert_eq!(parse(&grid_page(2)).scale().dpi, Some(600.0));
        let either = parse(&grid_page(12)).scale();
        assert_eq!((either.dpi, either.source), (None, "none"));
        assert_eq!(either.coord_unit, "inch1200");
        assert_eq!(parse(&grid_page(1)).scale().dpi, None);
        let few = parse(
            r#"<alto><Description><MeasurementUnit>inch1200</MeasurementUnit></Description><String HPOS="4"/></alto>"#,
        );
        assert_eq!(few.scale().dpi, None);
    }

    #[test]
    fn other_units_and_dialects() {
        let p = parse(
            r#"<alto xmlns="http://schema.ccs-gmbh.com/ALTO" xmlns:xsi="x" xsi:schemaLocation="http://schema.ccs-gmbh.com/ALTO http://www.loc.gov/ndnp/xml/alto-1-4.xsd"><Description><MeasurementUnit>pixel</MeasurementUnit></Description></alto>"#,
        );
        assert_eq!(p.head.alto_version.as_deref(), Some("1.4"));
        let s = p.scale();
        assert_eq!(
            (s.dpi, s.source, s.coord_unit.as_str()),
            (None, "pixel", "pixel")
        );
        let p = parse(r#"<alto xmlns="http://schema.ccs-gmbh.com/ALTO"/>"#);
        assert_eq!(
            p.head.alto_version.as_deref(),
            Some("http://schema.ccs-gmbh.com/ALTO")
        );
        assert_eq!(p.scale().coord_unit, "unknown");
        let p = parse(
            r#"<alto><Description><MeasurementUnit>mm10</MeasurementUnit><OCRProcessing><ocrProcessingStep><processingStepSettings>xdpi:254</processingStepSettings></ocrProcessingStep></OCRProcessing></Description></alto>"#,
        );
        assert!((p.scale().factor - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_engine_behind_a_wrapper() {
        assert_eq!(
            engine_of("abbyy9.version:9.0.0.7394-3Abbyy9.cache-check.image-CRC:add1").as_deref(),
            Some("abbyy9 9.0.0.7394-3")
        );
        assert_eq!(
            engine_of("Dictionary Words:1\nabbyy8.version:8.1.0.6184-12\nLang:en").as_deref(),
            Some("abbyy8 8.1.0.6184-12")
        );
        assert_eq!(engine_of("xdpi:300"), None);
    }

    #[test]
    fn settings_run_together() {
        let s = "abbyy9.version:9Lang:engConf:0.8xdpi:300ydpi:300width:5241height:6840";
        assert_eq!(setting(s, "xdpi:"), Some(300.0));
        assert_eq!(setting("height:9 width:", "width:"), None);
        assert_eq!(setting("page width:12", "width:"), Some(12.0));
    }

    #[test]
    fn a_stale_page_buffer_never_leaks() {
        let mut p = Page::default();
        read_page(V2.as_bytes(), &mut p).unwrap();
        read_page("<alto/>".as_bytes(), &mut p).unwrap();
        assert_eq!(p.word_count(), 0);
        assert_eq!(p.head.software, None);
        assert!(read_page("<alto><String></alto>".as_bytes(), &mut p).is_err());
        assert!(
            read_page("".as_bytes(), &mut p).is_err(),
            "no root, no page"
        );
    }

    fn archive(dir: &Path, name: &str, pages: &[(&str, &str)]) {
        let mut t = tar::Builder::new(Vec::new());
        for (p, body) in pages {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            t.append_data(&mut h, p, body.as_bytes()).unwrap();
        }
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        std::io::Write::write_all(&mut bz, &t.into_inner().unwrap()).unwrap();
        std::fs::write(dir.join(name), bz.finish().unwrap()).unwrap();
    }

    fn rows_of(path: &Path) -> RecordBatch {
        let f = std::fs::File::open(path).unwrap();
        let b = ParquetRecordBatchReaderBuilder::try_new(f).unwrap();
        let mut all: Vec<_> = b.build().unwrap().collect::<Result<_, _>>().unwrap();
        assert_eq!(all.len(), 1, "a small file reads as one batch");
        all.remove(0)
    }

    #[test]
    fn archives_round_trip_resume_and_skip_bad_pages() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("in");
        std::fs::create_dir(&arch).unwrap();
        archive(
            &arch,
            "b_x_ver02.tar.bz2",
            &[
                ("sn1/1900/01/02/ed-1/seq-1/ocr.xml", V2),
                ("sn1/1900/01/02/ed-1/seq-1/ocr.txt", "text"),
                ("sn1/1900/01/02/ed-1/seq-2/ocr.xml", "<alto/>"),
                ("sn1/1900/01/02/ed-1/seq-3/ocr.xml", "<alto><String></alto>"),
                ("sn1/1900/01/02/ed-1/seq-1/ocr.xml", V2),
            ],
        );
        archive(
            &arch,
            "b_y_ver01.tar.bz2",
            &[("sn2/1900/01/02/ed-1/seq-1/ocr.xml", V2)],
        );
        let opts = Options {
            archives: arch.clone(),
            out: dir.path().join("out"),
            workers: 2,
            limit: None,
            min_free_bytes: 0,
        };
        let s = run(&opts).unwrap();
        assert_eq!((s.read, s.failed, s.pages, s.words), (2, 0, 3, 8));
        assert_eq!((s.pages_left_out, s.duplicate_pages), (1, 1));

        let r = rows_of(&opts.out.join("b_x_ver02.parquet"));
        assert_eq!(r.num_rows(), 5, "four words and the empty page's row");
        let text = r.column_by_name("text").unwrap().as_string::<i32>();
        assert_eq!(text.value(0), "Fish & chips");
        assert!(text.is_null(4));
        let int = |n: &str| {
            r.column_by_name(n)
                .unwrap()
                .as_primitive::<Int32Type>()
                .clone()
        };
        assert_eq!(int("x").value(0), 200, "750 inch1200 at 320 dpi");
        assert_eq!(int("y").value(0), 400);
        assert_eq!(int("w").value(0), 100);
        assert_eq!(int("h").value(0), 40);
        assert_eq!(int("page_width").value(0), 5000);
        assert_eq!(int("word").value(4), -1);
        let wc = r
            .column_by_name("wc")
            .unwrap()
            .as_primitive::<Float32Type>();
        assert_eq!(wc.value(0), 0.91);
        assert!(wc.is_null(1));
        let doc = r.column_by_name("doc_id").unwrap().as_string::<i32>();
        assert_eq!(doc.value(4), "sn1_1900-01-02_ed-1_seq-2");

        let again = run(&opts).unwrap();
        assert_eq!((again.read, again.already), (0, 2));
        let leftovers: Vec<_> = std::fs::read_dir(&opts.out)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .filter(|n| n.ends_with(".part"))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");

        archive(
            &arch,
            "b_y_ver01.tar.bz2",
            &[
                ("sn2/1900/01/02/ed-1/seq-1/ocr.xml", V2),
                ("sn2/1900/01/02/ed-1/seq-2/ocr.xml", V2),
            ],
        );
        let changed = run(&opts).unwrap();
        assert_eq!((changed.read, changed.already, changed.pages), (1, 1, 2));
    }

    #[test]
    fn a_truncated_archive_fails_alone_and_leaves_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("in");
        std::fs::create_dir(&arch).unwrap();
        archive(
            &arch,
            "b_a_ver01.tar.bz2",
            &[("sn1/1900/01/02/ed-1/seq-1/ocr.xml", V2)],
        );
        let body = std::fs::read(arch.join("b_a_ver01.tar.bz2")).unwrap();
        std::fs::write(arch.join("b_a_ver01.tar.bz2"), &body[..body.len() / 2]).unwrap();
        archive(
            &arch,
            "b_b_ver01.tar.bz2",
            &[("sn1/1900/01/02/ed-1/seq-1/ocr.xml", V2)],
        );
        let opts = Options {
            archives: arch,
            out: dir.path().join("out"),
            workers: 1,
            limit: None,
            min_free_bytes: 0,
        };
        let s = run(&opts).unwrap();
        assert_eq!((s.read, s.failed), (1, 1));
        let names: Vec<_> = std::fs::read_dir(&opts.out)
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert!(!opts.out.join("b_a_ver01.done").exists());
    }

    #[test]
    fn a_full_disk_stops_before_the_next_archive() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("in");
        std::fs::create_dir(&arch).unwrap();
        archive(
            &arch,
            "b_a_ver01.tar.bz2",
            &[("sn1/1900/01/02/ed-1/seq-1/ocr.xml", V2)],
        );
        let opts = Options {
            archives: arch,
            out: dir.path().join("out"),
            workers: 1,
            limit: None,
            min_free_bytes: u64::MAX,
        };
        let s = run(&opts).unwrap();
        assert!(s.stopped_low_disk);
        assert_eq!(s.read, 0);
    }
}
