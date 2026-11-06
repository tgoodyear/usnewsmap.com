//! `usnm-ocr-worker loc-software`: which software made LoC's OCR, one row per bulk OCR
//! archive (`<batch>_verNN.tar.bz2`).
//!
//! LoC's software is per batch, so the job reads only the first page's ALTO header
//! (`<Description>`) from each archive. It streams the tar through bzip2 and stops at the
//! first `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`, and in that file at the end of
//! `<Description>`, so an archive costs one bzip2 block or two. It writes one Parquet file,
//! `--out`, to `.part` and renames it; `load.py locsw` loads it into `quality.loc_software`.
//!
//! The raw fields keep what the header says: every `processingSoftware` in document order,
//! with its `OCRProcessing` block, its step (`pre`, `ocr`, `post`), creator, name, version and
//! application description; the processing agencies; the step descriptions; and the engines
//! named in `processingStepSettings`. A version belongs to the software it sits beside in the
//! XML, never to another.
//!
//! The derived fields name the engine that read the page and what drove it. The rules are in
//! [`derive`].

use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::Context;
use arrow_array::{ArrayRef, Int16Array, RecordBatch, StringArray};
use arrow_schema::{DataType, Field, Schema, SchemaRef};
use parquet::arrow::ArrowWriter;
use parquet::basic::{Compression, ZstdLevel};
use parquet::file::properties::WriterProperties;
use quick_xml::events::Event;
use serde::Serialize;

use crate::loc_alto::{batch_of, doc_id_of};

/// Text fields longer than this are cut (a step description can run to a paragraph).
const MAX_TEXT: usize = 200;

/// One `processingSoftware` element.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Software {
    /// The `OCRProcessing` block it is in, from 1.
    pub block: u16,
    /// `pre`, `ocr` or `post`: the processing step it is in.
    pub step: &'static str,
    pub creator: Option<String>,
    pub name: Option<String>,
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What a page's `<Description>` says about how it was made.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Header {
    pub software: Vec<Software>,
    /// Each distinct `processingAgency`, in order.
    pub agencies: Vec<String>,
    /// Each distinct `processingStepDescription`, in order.
    pub steps: Vec<String>,
    /// Each distinct engine named in an OCR step's `processingStepSettings`, in order:
    /// `abbyy9 9.0.0.7394-3` (from `abbyy9.version:`), `abbyy8` (from `Engine:Abbyy8`) or
    /// `abbyy6`, `iris`, `scansoft` (from `Abbyy6OCREngine STAT BLOCK` and the like).
    pub settings_engines: Vec<String>,
    /// The same engines with the `OCRProcessing` block whose OCR step named them, so
    /// [`derive`] reads only the selected framework's settings.
    pub settings_by_block: Vec<(u16, String)>,
}

/// The engine behind a batch and what drove it, from [`derive`].
#[derive(Debug, Clone, PartialEq)]
pub struct Derived {
    /// `abbyy`, `tesseract`, `primeocr`, `omnipage`, `newpah` (iArchives' multi-engine
    /// vote), `iarchives-unknown` or `unknown`.
    pub engine_family: &'static str,
    pub engine_name: Option<String>,
    pub engine_version: Option<String>,
    pub wrapper: Option<String>,
    pub wrapper_version: Option<String>,
    pub vendor: Option<String>,
}

fn cut(s: &str) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    s.chars().take(MAX_TEXT).collect()
}

fn push_distinct(v: &mut Vec<String>, s: String) {
    if !s.is_empty() && !v.contains(&s) {
        v.push(s);
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Open {
    None,
    Creator,
    Name,
    Version,
    Description,
    Agency,
    Step,
    Settings,
}

/// Reads the `<Description>` of one ALTO stream, stopping at its end (or at `<Layout>`, for
/// a file with none). A file with no description gives an empty header; a stream with no ALTO
/// root, or one that ends inside `<Description>`, is an error, so a truncated `ocr.xml` never
/// passes for a batch with no software.
pub fn read_header<R: BufRead>(xml: R) -> anyhow::Result<Header> {
    let mut reader = quick_xml::Reader::from_reader(xml);
    let mut buf = Vec::new();
    let mut h = Header::default();
    let mut block = 0u16;
    let mut step: &'static str = "";
    let mut in_software = false;
    let mut open = Open::None;
    let mut text = String::new();
    let (mut saw_root, mut in_description) = (false, false);
    loop {
        let event = reader
            .read_event_into(&mut buf)
            .map_err(|e| anyhow::anyhow!("unreadable ALTO: {e}"))?;
        match event {
            Event::Start(ref e) => {
                text.clear();
                open = Open::None;
                match e.local_name().as_ref() {
                    n if n.eq_ignore_ascii_case("alto") => saw_root = true,
                    "Description" => in_description = true,
                    "Layout" => break,
                    "OCRProcessing" => block += 1,
                    "preProcessingStep" => step = "pre",
                    "ocrProcessingStep" => step = "ocr",
                    "postProcessingStep" => step = "post",
                    "processingSoftware" => {
                        in_software = true;
                        h.software.push(Software {
                            block: block.max(1),
                            step,
                            ..Software::default()
                        });
                    }
                    "softwareCreator" if in_software => open = Open::Creator,
                    "softwareName" if in_software => open = Open::Name,
                    "softwareVersion" if in_software => open = Open::Version,
                    "applicationDescription" if in_software => open = Open::Description,
                    "processingAgency" => open = Open::Agency,
                    "processingStepDescription" => open = Open::Step,
                    "processingStepSettings" => open = Open::Settings,
                    _ => {}
                }
            }
            Event::Text(ref t) if open != Open::None => {
                text.push_str(&t.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::CData(ref t) if open != Open::None => {
                text.push_str(&t.xml_content(quick_xml::XmlVersion::Implicit1_0));
            }
            Event::GeneralRef(ref r) if open != Open::None => {
                let s = crate::loc_alto_words::entity(r)
                    .ok_or_else(|| anyhow::anyhow!("undefined entity"))?;
                text.push_str(&s);
            }
            Event::End(ref e) => {
                let name = e.local_name();
                let s = text.trim();
                let field = (!s.is_empty()).then(|| cut(s));
                match open {
                    Open::Creator | Open::Name | Open::Version | Open::Description => {
                        if let Some(sw) = h.software.last_mut() {
                            let slot = match open {
                                Open::Creator => &mut sw.creator,
                                Open::Name => &mut sw.name,
                                Open::Version => &mut sw.version,
                                _ => &mut sw.description,
                            };
                            if slot.is_none() {
                                *slot = field;
                            }
                        }
                    }
                    Open::Agency => push_distinct(&mut h.agencies, field.unwrap_or_default()),
                    Open::Step => push_distinct(&mut h.steps, field.unwrap_or_default()),
                    Open::Settings if step == "ocr" => {
                        for engine in settings_engines(s) {
                            let entry = (block.max(1), engine.clone());
                            if !h.settings_by_block.contains(&entry) {
                                h.settings_by_block.push(entry);
                            }
                            push_distinct(&mut h.settings_engines, engine);
                        }
                    }
                    _ => {}
                }
                text.clear();
                open = Open::None;
                match name.as_ref() {
                    "Description" => break,
                    "processingSoftware" => in_software = false,
                    "preProcessingStep" | "ocrProcessingStep" | "postProcessingStep" => step = "",
                    _ => {}
                }
            }
            Event::Eof => {
                anyhow::ensure!(saw_root, "no ALTO root");
                anyhow::ensure!(!in_description, "ALTO ends inside <Description>");
                break;
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(h)
}

/// Engine names a token may start with, once lowercased; anything run on before it (iArchives
/// settings run together, as in `...:trueabbyy9.version:`) is dropped.
const SETTINGS_ENGINES: [&str; 6] = [
    "abbyy",
    "omnipage",
    "tesseract",
    "prime",
    "iris",
    "scansoft",
];

fn engine_token(raw: &str) -> Option<String> {
    let low = raw.to_ascii_lowercase();
    let at = SETTINGS_ENGINES.iter().filter_map(|p| low.rfind(p)).max()?;
    let t = &low[at..];
    (!t.is_empty()).then(|| t.to_owned())
}

/// The alphanumeric run that ends at byte `end` of `s`.
fn word_before(s: &str, end: usize) -> &str {
    // Step past the separator by its UTF-8 length: it may be multibyte, like a no-break space.
    let start = s[..end]
        .char_indices()
        .rev()
        .find(|&(_, c)| !c.is_ascii_alphanumeric())
        .map_or(0, |(i, c)| i + c.len_utf8());
    &s[start..end]
}

/// The engines an OCR step's `processingStepSettings` names, in order:
/// `<engine>.version:<v>`, `Engine:<engine>` and `<Engine>OCREngine STAT BLOCK`.
pub fn settings_engines(s: &str) -> Vec<String> {
    let mut found: Vec<(usize, String)> = Vec::new();
    for (at, _) in s.match_indices(".version:") {
        let Some(name) = engine_token(word_before(s, at)) else {
            continue;
        };
        let rest = &s[at + ".version:".len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_digit() && c != '.' && c != '-')
            .unwrap_or(rest.len());
        let v = &rest[..end];
        found.push((
            at,
            if v.is_empty() {
                name
            } else {
                format!("{name} {v}")
            },
        ));
    }
    for (at, _) in s.match_indices("Engine:") {
        // `Engine:` itself, not the end of `OCREngine:` or a longer key.
        if s[..at].ends_with(|c: char| c.is_ascii_alphanumeric()) {
            continue;
        }
        let rest = &s[at + "Engine:".len()..];
        let end = rest
            .find(|c: char| !c.is_ascii_alphanumeric())
            .unwrap_or(rest.len());
        if let Some(name) = engine_token(&rest[..end]) {
            found.push((at, name));
        }
    }
    for (at, _) in s.match_indices("OCREngine STAT BLOCK") {
        if let Some(name) = engine_token(word_before(s, at)) {
            found.push((at, name));
        }
    }
    found.sort_by_key(|(at, _)| *at);
    let mut out = Vec::new();
    for (_, name) in found {
        // `abbyy9` after `abbyy9 9.0.0.7394-3` adds nothing.
        if !out
            .iter()
            .any(|o: &String| o == &name || o.split(' ').next() == Some(name.as_str()))
        {
            out.push(name);
        }
    }
    out
}

/// The engine family a `softwareName` names, if it names an engine.
pub fn family_of_name(name: &str) -> Option<&'static str> {
    let l = name.to_ascii_lowercase();
    if l.contains("finereader") || l.starts_with("abbyy") {
        Some("abbyy")
    } else if l.contains("tesseract") {
        Some("tesseract")
    } else if l.contains("primeocr") || l.contains("prime recognition") {
        Some("primeocr")
    } else if l.contains("omnipage") {
        Some("omnipage")
    } else {
        None
    }
}

/// The family of a settings engine (`abbyy9 9.0.0.7394-3`, `iris`).
fn family_of_setting(engine: &str) -> &'static str {
    let t = engine.split(' ').next().unwrap_or("");
    if t.starts_with("abbyy") {
        "abbyy"
    } else if t.starts_with("omnipage") {
        "omnipage"
    } else if t.starts_with("tesseract") {
        "tesseract"
    } else if t.starts_with("prime") {
        "primeocr"
    } else if t.starts_with("iris") {
        "iris"
    } else {
        "scansoft"
    }
}

/// The version a settings engine gives: after the space, else the digits ending its name
/// (`abbyy8` -> `8`).
fn version_of_setting(engine: &str) -> Option<String> {
    if let Some((_, v)) = engine.split_once(' ') {
        return Some(v.to_owned());
    }
    let digits = engine.trim_start_matches(|c: char| !c.is_ascii_digit());
    (!digits.is_empty()).then(|| digits.to_owned())
}

fn is_named(s: &Software) -> bool {
    s.name
        .as_deref()
        .is_some_and(|n| !n.eq_ignore_ascii_case("NA"))
}

/// A short, stable name for the firm that did the conversion.
pub fn vendor_name(raw: &str) -> String {
    const KNOWN: [(&str, &str); 8] = [
        ("apex", "Apex CoVantage"),
        (
            "ccs content conversion",
            "CCS Content Conversion Specialists",
        ),
        ("htc global", "HTC Global Services"),
        ("nmt corporation", "NMT Corporation"),
        ("iarchives", "iArchives"),
        ("library of congress", "Library of Congress"),
        ("perfect image", "Perfect Image"),
        ("digital divide data", "Digital Divide Data"),
    ];
    let l = raw.to_ascii_lowercase();
    KNOWN
        .iter()
        .find(|(k, _)| l.contains(k))
        .map_or_else(|| raw.trim().to_owned(), |(_, v)| (*v).to_owned())
}

/// A software creator that is an engine's maker, not a conversion vendor.
fn is_engine_maker(creator: &str) -> bool {
    let l = creator.to_ascii_lowercase();
    ["abbyy", "google", "prime", "tesseract"]
        .iter()
        .any(|k| l.contains(k))
}

/// The engine, wrapper and vendor of a header. The rules, in order:
///
/// 1. **Engine.** The first software in an OCR step whose name names an engine
///    ([`family_of_name`]: FineReader or a name starting ABBYY, Tesseract, PrimeOCR or Prime
///    Recognition, OmniPage). Its own name and version are `engine_name` and
///    `engine_version`; the version is the one beside that name in the XML, which for ABBYY
///    Recognition Server and FineReader Server is the server's version.
/// 2. **A framework in the OCR step.** Otherwise the first named software in an OCR step
///    (iArchives OCR Framework, Newpah, Forepah, PerfectOCR) is the wrapper, and the engine
///    comes from that step's settings ([`settings_engines`]). One family there is the engine:
///    its version is the settings' version (`9.0.0.7394-3`), else the digits of its name
///    (`8`), and `engine_name` is the settings' name (`abbyy9`). More than one family, as
///    Newpah's ABBYY, IRIS and ScanSoft vote, is `newpah` (or `multiple` for another
///    framework), named and versioned as the framework. None is `iarchives-unknown` for an
///    iArchives framework and `unknown` otherwise.
/// 3. **Wrapper.** Under rule 1, the first other named software in the engine's
///    `OCRProcessing` block, in document order: the CCS docWorks or docWizz preprocessing,
///    Apex's `apex-alto`, HTC's Image Converter, NMT's PolishImage, LoC's `ndnp-open-ocr`,
///    DDD's ZED. A batch whose header names only the engine has none.
/// 4. **Vendor.** The first `processingAgency`, else the wrapper's creator, else the
///    engine's creator unless that is the engine's maker (ABBYY, Google, Prime), shortened by
///    [`vendor_name`].
pub fn derive(h: &Header) -> Derived {
    let ocr_named = || h.software.iter().filter(|s| s.step == "ocr" && is_named(s));
    let engine = ocr_named().find(|s| s.name.as_deref().and_then(family_of_name).is_some());
    let mut d = Derived {
        engine_family: "unknown",
        engine_name: None,
        engine_version: None,
        wrapper: None,
        wrapper_version: None,
        vendor: None,
    };
    let mut wrapper_sw: Option<&Software> = None;
    let mut engine_creator: Option<&str> = None;
    if let Some(e) = engine {
        d.engine_family = e
            .name
            .as_deref()
            .and_then(family_of_name)
            .unwrap_or("unknown");
        d.engine_name = e.name.clone();
        d.engine_version = e.version.clone();
        engine_creator = e.creator.as_deref();
        wrapper_sw = h
            .software
            .iter()
            .filter(|s| s.block == e.block && is_named(s))
            .find(|s| s.name != e.name);
    } else if let Some(fw) = ocr_named().next() {
        wrapper_sw = Some(fw);
        // Only the settings of the framework's own block: another block may name another engine.
        let own: Vec<&String> = h
            .settings_by_block
            .iter()
            .filter(|(b, _)| *b == fw.block)
            .map(|(_, e)| e)
            .collect();
        let mut families: Vec<&'static str> = Vec::new();
        for s in &own {
            let f = family_of_setting(s);
            if !families.contains(&f) {
                families.push(f);
            }
        }
        let name = fw.name.as_deref().unwrap_or("");
        let iarchives = fw
            .creator
            .as_deref()
            .is_some_and(|c| c.to_ascii_lowercase().contains("iarchives"))
            || name.to_ascii_lowercase().contains("iarchives");
        match families.as_slice() {
            [one] => {
                let s = own
                    .iter()
                    .find(|s| family_of_setting(s) == *one)
                    .expect("a settings engine of that family");
                let token = s.split(' ').next().unwrap_or(s);
                d.engine_family = *one;
                d.engine_name = Some(token.to_owned());
                // An explicit version only from the same engine token: `abbyy8` never takes
                // `abbyy9.version:`'s number.
                d.engine_version = own
                    .iter()
                    .filter(|e| e.split(' ').next() == Some(token))
                    .find_map(|e| e.split_once(' ').map(|(_, v)| v.to_owned()))
                    .or_else(|| version_of_setting(s));
            }
            [] => {
                d.engine_family = if iarchives {
                    "iarchives-unknown"
                } else {
                    "unknown"
                }
            }
            _ => {
                let l = name.to_ascii_lowercase();
                d.engine_family = if l == "newpah" || l == "forepah" {
                    "newpah"
                } else {
                    "multiple"
                };
                d.engine_name = fw.name.clone();
                d.engine_version = fw.version.clone();
            }
        }
    }
    if let Some(w) = wrapper_sw {
        d.wrapper = w.name.clone();
        d.wrapper_version = w.version.clone();
    }
    d.vendor = h
        .agencies
        .first()
        .map(String::as_str)
        .or_else(|| wrapper_sw.and_then(|w| w.creator.as_deref()))
        .or_else(|| engine_creator.filter(|c| !is_engine_maker(c)))
        .map(vendor_name);
    d
}

/// One archive's row.
#[derive(Debug, Clone)]
pub struct Row {
    pub batch: String,
    pub batch_version: i16,
    pub archive: String,
    /// The page whose header was read, `None` if the archive has no page.
    pub header_doc: Option<String>,
    pub header: Header,
    pub derived: Derived,
    pub read_at: String,
}

/// The first page's header in one archive, and that page's `doc_id`.
pub fn archive_header(path: &Path) -> anyhow::Result<Option<(String, Header)>> {
    let file = std::fs::File::open(path).with_context(|| path.display().to_string())?;
    let bz = bzip2::read::MultiBzDecoder::new(BufReader::with_capacity(1 << 16, file));
    let mut tar = tar::Archive::new(bz);
    for entry in tar.entries()? {
        let entry = entry?;
        let p = entry.path()?.to_string_lossy().into_owned();
        let Some(doc) = doc_id_of(&p) else { continue };
        // Capped as `loc-alto-stats` caps a page, though the header ends long before.
        let xml = BufReader::with_capacity(64 << 10, entry.take(64 << 20));
        let h = read_header(xml).with_context(|| format!("{}: {p}", path.display()))?;
        return Ok(Some((doc, h)));
    }
    Ok(None)
}

fn json<T: Serialize>(v: &T) -> String {
    serde_json::to_string(v).expect("plain data serializes")
}

pub fn schema() -> SchemaRef {
    let s = |n: &str, null: bool| Field::new(n, DataType::Utf8, null);
    Arc::new(Schema::new(vec![
        s("batch", false),
        Field::new("batch_version", DataType::Int16, false),
        s("archive", false),
        s("header_doc", true),
        s("software", false),
        s("agencies", false),
        s("steps", false),
        s("settings_engines", false),
        s("engine_family", false),
        s("engine_name", true),
        s("engine_version", true),
        s("wrapper", true),
        s("wrapper_version", true),
        s("vendor", true),
        s("read_at", false),
    ]))
}

pub fn encode(rows: &[Row]) -> anyhow::Result<Vec<u8>> {
    let props = WriterProperties::builder()
        .set_compression(Compression::ZSTD(
            ZstdLevel::try_new(6).expect("valid level"),
        ))
        .build();
    let mut w = ArrowWriter::try_new(Vec::new(), schema(), Some(props))?;
    let col = |f: fn(&Row) -> Option<String>| -> ArrayRef {
        Arc::new(StringArray::from_iter(rows.iter().map(f)))
    };
    let cols: Vec<ArrayRef> = vec![
        col(|r| Some(r.batch.clone())),
        Arc::new(Int16Array::from_iter_values(
            rows.iter().map(|r| r.batch_version),
        )),
        col(|r| Some(r.archive.clone())),
        col(|r| r.header_doc.clone()),
        col(|r| Some(json(&r.header.software))),
        col(|r| Some(json(&r.header.agencies))),
        col(|r| Some(json(&r.header.steps))),
        col(|r| Some(json(&r.header.settings_engines))),
        col(|r| Some(r.derived.engine_family.to_owned())),
        col(|r| r.derived.engine_name.clone()),
        col(|r| r.derived.engine_version.clone()),
        col(|r| r.derived.wrapper.clone()),
        col(|r| r.derived.wrapper_version.clone()),
        col(|r| r.derived.vendor.clone()),
        col(|r| Some(r.read_at.clone())),
    ];
    w.write(&RecordBatch::try_new(schema(), cols)?)?;
    Ok(w.into_inner()?)
}

#[derive(Debug, Clone)]
pub struct Options {
    pub archives: PathBuf,
    pub out: PathBuf,
    pub workers: usize,
    pub limit: Option<usize>,
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub archives: usize,
    pub rows: usize,
    pub failed: usize,
    pub no_page: usize,
    pub families: std::collections::BTreeMap<String, usize>,
}

fn read_one(dir: &Path, name: &str) -> anyhow::Result<Option<Row>> {
    let Some((batch, batch_version)) = batch_of(name) else {
        return Ok(None);
    };
    let found = archive_header(&dir.join(name))?;
    let (header_doc, header) = match found {
        Some((d, h)) => (Some(d), h),
        None => (None, Header::default()),
    };
    let derived = derive(&header);
    Ok(Some(Row {
        batch,
        batch_version,
        archive: name.to_owned(),
        header_doc,
        header,
        derived,
        read_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
    }))
}

/// Reads every archive in `archives` and writes one row each to `out`.
pub fn run(opts: &Options) -> anyhow::Result<Summary> {
    let mut names: Vec<String> = std::fs::read_dir(&opts.archives)
        .with_context(|| opts.archives.display().to_string())?
        .filter_map(|e| e.ok()?.file_name().into_string().ok())
        .filter(|n| n.ends_with(".tar.bz2") && !n.starts_with('.') && batch_of(n).is_some())
        .collect();
    names.sort();
    if let Some(l) = opts.limit {
        names.truncate(l);
    }
    let next = AtomicUsize::new(0);
    let rows = Mutex::new(Vec::with_capacity(names.len()));
    let failed = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        for _ in 0..opts.workers.max(1) {
            scope.spawn(|| loop {
                let i = next.fetch_add(1, Ordering::Relaxed);
                let Some(name) = names.get(i) else { break };
                match read_one(&opts.archives, name) {
                    Ok(Some(r)) => rows.lock().expect("not poisoned").push(r),
                    Ok(None) => {}
                    Err(e) => {
                        failed.fetch_add(1, Ordering::Relaxed);
                        tracing::warn!(archive = %name, error = %e, "archive left out");
                    }
                }
            });
        }
    });
    let mut rows = rows.into_inner().expect("not poisoned");
    rows.sort_by(|a, b| a.archive.cmp(&b.archive));
    let mut s = Summary {
        archives: names.len(),
        rows: rows.len(),
        failed: failed.into_inner(),
        no_page: rows.iter().filter(|r| r.header_doc.is_none()).count(),
        ..Summary::default()
    };
    for r in &rows {
        *s.families
            .entry(r.derived.engine_family.to_owned())
            .or_default() += 1;
    }
    if let Some(dir) = opts.out.parent().filter(|p| !p.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir)?;
    }
    let file = opts
        .out
        .file_name()
        .context("--out: a file name")?
        .to_string_lossy();
    // A scan with any failed archive publishes nothing: the loader would take the partial file
    // as complete, dropping or leaving stale the rows of the batches that failed.
    anyhow::ensure!(
        s.failed == 0,
        "{} of {} archives failed; {} left unchanged",
        s.failed,
        s.archives,
        opts.out.display()
    );
    let part = opts.out.with_file_name(format!(".{file}.part"));
    std::fs::write(&part, encode(&rows)?)?;
    std::fs::rename(&part, &opts.out)?;
    Ok(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use arrow_array::Array;

    fn alto(desc: &str) -> String {
        format!(
            r#"<?xml version="1.0"?><alto xmlns="http://www.loc.gov/standards/alto/ns-v2#"><Description><MeasurementUnit>inch1200</MeasurementUnit>{desc}</Description><Layout><Page><String CONTENT="x"/></Page></Layout></alto>"#
        )
    }

    fn sw(creator: &str, name: &str, version: Option<&str>) -> String {
        let c = if creator.is_empty() {
            String::new()
        } else {
            format!("<softwareCreator>{creator}</softwareCreator>")
        };
        let v = version.map_or(String::new(), |v| {
            format!("<softwareVersion>{v}</softwareVersion>")
        });
        format!(
            "<processingSoftware>{c}<softwareName>{name}</softwareName>{v}</processingSoftware>"
        )
    }

    fn of(desc: &str) -> (Header, Derived) {
        let h = read_header(alto(desc).as_bytes()).unwrap();
        let d = derive(&h);
        (h, d)
    }

    fn check(
        d: &Derived,
        family: &str,
        name: Option<&str>,
        version: Option<&str>,
        wrapper: Option<(&str, Option<&str>)>,
        vendor: Option<&str>,
    ) {
        assert_eq!(d.engine_family, family, "{d:?}");
        assert_eq!(d.engine_name.as_deref(), name, "{d:?}");
        assert_eq!(d.engine_version.as_deref(), version, "{d:?}");
        assert_eq!(d.wrapper.as_deref(), wrapper.map(|w| w.0), "{d:?}");
        assert_eq!(
            d.wrapper_version.as_deref(),
            wrapper.and_then(|w| w.1),
            "{d:?}"
        );
        assert_eq!(d.vendor.as_deref(), vendor, "{d:?}");
    }

    #[test]
    fn apex_alto_over_abbyy_and_prime() {
        for (creator, name, family) in [
            ("ABBYY", "ABBYY FineReader", "abbyy"),
            ("ABBYY", "FineReader", "abbyy"),
            ("Prime Recognition", "PrimeOCR", "primeocr"),
        ] {
            let desc = format!(
                "<OCRProcessing ID=\"OCR1\"><ocrProcessingStep><processingAgency>Apex CoVantage</processingAgency>{}</ocrProcessingStep><postProcessingStep><processingAgency>Apex CoVantage</processingAgency><processingStepDescription>Apex -&gt; ALTO</processingStepDescription>{}</postProcessingStep></OCRProcessing>",
                sw(creator, name, None),
                sw("Apex CoVantage", "apex-alto", Some("2.0"))
            );
            let (h, d) = of(&desc);
            assert_eq!(h.steps, ["Apex -> ALTO"]);
            assert_eq!(h.agencies, ["Apex CoVantage"]);
            assert_eq!(h.software.len(), 2);
            assert_eq!((h.software[1].step, h.software[1].block), ("post", 1));
            check(
                &d,
                family,
                Some(name),
                None,
                Some(("apex-alto", Some("2.0"))),
                Some("Apex CoVantage"),
            );
        }
    }

    #[test]
    fn ccs_docworks_over_finereader_and_tesseract() {
        let ccs = "CCS Content Conversion Specialists GmbH, Hamburg, Germany";
        let (h, d) = of(&format!(
            "<OCRProcessing ID=\"OCRPROCESSING_1\"><preProcessingStep><processingStepDescription>Align</processingStepDescription>{}</preProcessingStep><ocrProcessingStep>{}</ocrProcessingStep><postProcessingStep><processingStepDescription>CCS OCR Postprocessing Filter</processingStepDescription></postProcessingStep></OCRProcessing>",
            sw(ccs, "CCS docWorks", Some("7.0-1.52")),
            sw("ABBYY (BIT Software), Russia", "FineReader", Some("11.0"))
        ));
        // Each version stays with its own software.
        assert_eq!(h.software[0].version.as_deref(), Some("7.0-1.52"));
        assert_eq!(h.software[1].version.as_deref(), Some("11.0"));
        check(
            &d,
            "abbyy",
            Some("FineReader"),
            Some("11.0"),
            Some(("CCS docWorks", Some("7.0-1.52"))),
            Some("CCS Content Conversion Specialists"),
        );
        let (_, d) = of(&format!(
            "<OCRProcessing><preProcessingStep>{}</preProcessingStep><ocrProcessingStep><processingStepDescription>OCR: Engine statistics: 130 suspicious characters</processingStepDescription>{}</ocrProcessingStep></OCRProcessing>",
            sw(ccs, "CCS docWizz", Some("8.0-0.38")),
            sw("Google Inc. Apache License, Version 2.0", "Tesseract", Some("4.1"))
        ));
        check(
            &d,
            "tesseract",
            Some("Tesseract"),
            Some("4.1"),
            Some(("CCS docWizz", Some("8.0-0.38"))),
            Some("CCS Content Conversion Specialists"),
        );
        // A second block's engine, or its `NA`, doesn't displace the first's.
        let (h, d) = of(&format!(
            "<OCRProcessing ID=\"A\"><preProcessingStep>{a}</preProcessingStep><ocrProcessingStep>{b}</ocrProcessingStep></OCRProcessing><OCRProcessing ID=\"B\"><preProcessingStep>{a}</preProcessingStep><ocrProcessingStep>{c}</ocrProcessingStep></OCRProcessing>",
            a = sw(ccs, "CCS docWizz", Some("8.0-0.32")),
            b = sw("ABBYY (BIT Software), Russia", "FineReader", Some("12.0")),
            c = sw("Google Inc. Apache License, Version 2.0", "Tesseract", Some("4.1"))
        ));
        assert_eq!(h.software[3].block, 2);
        check(
            &d,
            "abbyy",
            Some("FineReader"),
            Some("12.0"),
            Some(("CCS docWizz", Some("8.0-0.32"))),
            Some("CCS Content Conversion Specialists"),
        );
        let (_, d) = of(&format!(
            "<OCRProcessing><ocrProcessingStep>{}</ocrProcessingStep></OCRProcessing>",
            sw("ABBYY (BIT Software), Russia", "NA", None)
        ));
        assert_eq!(d.engine_family, "unknown");
    }

    #[test]
    fn abbyy_named_by_iarchives_directly() {
        let (h, d) = of(
            "<OCRProcessing ID=\"OCR.0\"><ocrProcessingStep><processingStepSettings>abbyy8.version:8.1.0.6184-12 Conf:0.475 Lang:en_US xdpi:300</processingStepSettings><processingSoftware><softwareCreator>iArchives</softwareCreator><softwareName>Abbyy FineReader</softwareName><softwareVersion>8.1.0.6184-12</softwareVersion><applicationDescription>iArchives OCR Framework [Abbyy FineReader 8.1.0.6184-12]</applicationDescription></processingSoftware></ocrProcessingStep></OCRProcessing>"
        );
        assert_eq!(h.settings_engines, ["abbyy8 8.1.0.6184-12"]);
        assert_eq!(
            h.software[0].description.as_deref(),
            Some("iArchives OCR Framework [Abbyy FineReader 8.1.0.6184-12]")
        );
        check(
            &d,
            "abbyy",
            Some("Abbyy FineReader"),
            Some("8.1.0.6184-12"),
            None,
            Some("iArchives"),
        );
    }

    #[test]
    fn an_empty_or_truncated_header_is_an_error() {
        assert!(read_header(&b""[..]).is_err());
        assert!(read_header(&b"<?xml version=\"1.0\"?>"[..]).is_err());
        let cut = r#"<alto><Description><OCRProcessing><ocrProcessingStep><processingSoftware><softwareName>ABBYY"#;
        assert!(read_header(cut.as_bytes()).is_err());
        let none = r#"<alto><Layout><Page/></Layout></alto>"#;
        assert_eq!(read_header(none.as_bytes()).unwrap(), Header::default());
    }

    #[test]
    fn a_multibyte_separator_before_a_version_does_not_panic() {
        assert_eq!(
            settings_engines("Lang:eng\u{a0}abbyy9.version:9.0.0.7394-3"),
            ["abbyy9 9.0.0.7394-3"]
        );
        assert_eq!(settings_engines("é.version:1.0"), Vec::<String>::new());
    }

    #[test]
    fn only_the_framework_blocks_settings_choose_the_engine() {
        let fw = sw("iArchives", "iArchives OCR Framework", Some("Multiple"));
        let block = |id: &str, settings: &str| {
            format!(
                "<OCRProcessing ID=\"{id}\"><ocrProcessingStep><processingStepSettings>{settings}</processingStepSettings>{fw}</ocrProcessingStep></OCRProcessing>"
            )
        };
        let (h, d) = of(&format!(
            "{}{}",
            block("OCR.0", "abbyy9.version:9.0.0.7394-3"),
            block("OCR.1", "Engine:OmniPage15")
        ));
        assert_eq!(h.settings_engines.len(), 2, "{h:?}");
        assert_eq!(d.engine_family, "abbyy", "{d:?}");
        assert_eq!(d.engine_version.as_deref(), Some("9.0.0.7394-3"), "{d:?}");
    }

    #[test]
    fn a_version_belongs_to_its_own_engine_token() {
        let fw = sw("iArchives", "iArchives OCR Framework", Some("Multiple"));
        let (_, d) = of(&format!(
            "<OCRProcessing ID=\"OCR.0\"><ocrProcessingStep><processingStepSettings>Engine:Abbyy8 abbyy9.version:9.0</processingStepSettings>{fw}</ocrProcessingStep></OCRProcessing>"
        ));
        assert_eq!(d.engine_family, "abbyy", "{d:?}");
        assert_eq!(d.engine_name.as_deref(), Some("abbyy8"), "{d:?}");
        assert_eq!(d.engine_version.as_deref(), Some("8"), "{d:?}");
    }

    #[test]
    fn iarchives_framework_by_its_settings() {
        let fw = sw("iArchives", "iArchives OCR Framework", Some("Multiple"));
        let step = |settings: &str| {
            format!(
                "<OCRProcessing ID=\"OCR.0\"><ocrProcessingStep><processingStepSettings>{settings}</processingStepSettings>{fw}</ocrProcessingStep></OCRProcessing>"
            )
        };
        let wrapper = Some(("iArchives OCR Framework", Some("Multiple")));
        let (h, d) = of(&step(
            "abbyy9.version:9.0.0.7394-3Abbyy9.cache-check.image-CRC:ef19Lang:eng spaabbyy9.option.ocr-auto-pictures:trueConf:0.991Engine:Abbyy9",
        ));
        assert_eq!(h.settings_engines, ["abbyy9 9.0.0.7394-3"]);
        check(
            &d,
            "abbyy",
            Some("abbyy9"),
            Some("9.0.0.7394-3"),
            wrapper,
            Some("iArchives"),
        );
        // Run on from the setting before it.
        let (h, _) = of(&step(
            "ocr-auto-pictures:trueabbyy9.version:9.0.0.7394-3Lang:eng",
        ));
        assert_eq!(h.settings_engines, ["abbyy9 9.0.0.7394-3"]);
        // A blank page names the engine only as `Engine:`.
        let (_, d) = of(&step(
            "Predicted Word Accuracy:0% Lang:ja Engine:Abbyy8 Word Count:0",
        ));
        check(
            &d,
            "abbyy",
            Some("abbyy8"),
            Some("8"),
            wrapper,
            Some("iArchives"),
        );
        let (_, d) = of(&step("omnipage155.version:1550 Lang:en"));
        check(
            &d,
            "omnipage",
            Some("omnipage155"),
            Some("1550"),
            wrapper,
            Some("iArchives"),
        );
        let (_, d) = of(&step("Lang:en xdpi:300"));
        check(
            &d,
            "iarchives-unknown",
            None,
            None,
            wrapper,
            Some("iArchives"),
        );
    }

    #[test]
    fn newpah_and_forepah() {
        let step = |name: &str, version: &str, settings: &str| {
            format!(
                "<OCRProcessing ID=\"ID0\"><ocrProcessingStep><processingStepSettings>{settings}</processingStepSettings>{}</ocrProcessingStep></OCRProcessing>",
                sw("iArchives", name, Some(version))
            )
        };
        let (h, d) = of(&step(
            "Newpah",
            "v2.21",
            "useAbbyy4:0 Abbyy6OCREngine Character Count:41348 Abbyy6OCREngine STAT BLOCK:STATBLOCK_Abbyy6OCREngine;41348 IrisOCREngine STAT BLOCK:STATBLOCK_IrisOCREngine;34352 version:Newpah v2.21 MatB ScansoftOCREngine STAT BLOCK:STATBLOCK_ScansoftOCREngine;34033",
        ));
        assert_eq!(h.settings_engines, ["abbyy6", "iris", "scansoft"]);
        check(
            &d,
            "newpah",
            Some("Newpah"),
            Some("v2.21"),
            Some(("Newpah", Some("v2.21"))),
            Some("iArchives"),
        );
        let (_, d) = of(&step(
            "Forepah",
            "v2.45",
            "useIris:0 version:Forepah v2.45 Abbyy8OCREngine STAT BLOCK:STATBLOCK_Abbyy8OCREngine;52911 useAbbyy6:0",
        ));
        check(
            &d,
            "abbyy",
            Some("abbyy8"),
            Some("8"),
            Some(("Forepah", Some("v2.45"))),
            Some("iArchives"),
        );
    }

    #[test]
    fn perfectocr_over_abbyy() {
        let (_, d) = of(&format!(
            "<OCRProcessing ID=\"OCR0\"><ocrProcessingStep><processingStepSettings>Dictionary Words:5292 abbyy9.version:9.0.1.733 Lang:en abbyy9.cache-check.wrapper-version:1.13</processingStepSettings>{}</ocrProcessingStep></OCRProcessing>",
            sw("Perfect Image, Inc.", "PerfectOCR", Some("1.01"))
        ));
        check(
            &d,
            "abbyy",
            Some("abbyy9"),
            Some("9.0.1.733"),
            Some(("PerfectOCR", Some("1.01"))),
            Some("Perfect Image"),
        );
    }

    #[test]
    fn htc_image_converter() {
        for (creator, name, version, family) in [
            ("ABBYY", "ABBYY Recognition", "11.0", "abbyy"),
            ("Prime", "Prime Recognition", "4.3", "primeocr"),
        ] {
            let (h, d) = of(&format!(
                "<OCRProcessing ID=\"ID0\"><preProcessingStep><processingAgency>HTC Global Services</processingAgency><processingStepSettings>Auto Despeckle on</processingStepSettings><processingSoftware><softwareCreator>HTC Global Services</softwareCreator><softwareName>Image Converter</softwareName><softwareVersion>v1.0</softwareVersion><applicationDescription>Application used to Apply the Auto De-speckle option</applicationDescription></processingSoftware></preProcessingStep><ocrProcessingStep><processingAgency>HTC Global Services</processingAgency><processingStepSettings>Auto despeckle on, Turn on six OCR engines, Lexical checks on</processingStepSettings>{}</ocrProcessingStep><postProcessingStep>{}</postProcessingStep></OCRProcessing>",
                sw(creator, name, Some(version)),
                sw("HTC Global Services", "ALTO XML Creation", Some("1.0"))
            ));
            assert!(h.settings_engines.is_empty(), "{:?}", h.settings_engines);
            check(
                &d,
                family,
                Some(name),
                Some(version),
                Some(("Image Converter", Some("v1.0"))),
                Some("HTC Global Services"),
            );
        }
    }

    #[test]
    fn nmt_polishimage_and_bldalto() {
        for (name, version) in [
            ("ABBYY FineReader Engine", "8.04.1162.20"),
            ("ABBYY ABBYY FineReader Engine", "8.04.1162.20"),
            ("ABBYY Recognition Server", "3.5.1.343"),
            ("ABBYY Finereader Server", "14.0.1.69"),
        ] {
            let (_, d) = of(&format!(
                "<OCRProcessing ID=\"OCR001\"><preProcessingStep><processingStepDescription>Deskew, crop, paginate</processingStepDescription>{}</preProcessingStep><ocrProcessingStep><processingStepSettings> width:5717 height:7648 xdpi:400 ydpi:400 </processingStepSettings>{}</ocrProcessingStep><postProcessingStep>{}</postProcessingStep></OCRProcessing>",
                sw("NMT Corporation, La Crosse, WI", "PolishImage", Some("v1.99.32b")),
                sw("ABBYY Software LLC", name, Some(version)),
                sw("NMT Corporation, La Crosse, WI", "bldalto", Some("3.1.7a"))
            ));
            check(
                &d,
                "abbyy",
                Some(name),
                Some(version),
                Some(("PolishImage", Some("v1.99.32b"))),
                Some("NMT Corporation"),
            );
        }
    }

    #[test]
    fn ndnp_open_ocr_over_tesseract() {
        let (_, d) = of(&format!(
            "<OCRProcessing ID=\"OCR_0\"><ocrProcessingStep>{}</ocrProcessingStep><postProcessingStep><processingDateTime>2024-08-20T15:19:23</processingDateTime><processingAgency>Library of Congress</processingAgency>{}</postProcessingStep></OCRProcessing>",
            sw("", "Tesseract Open Source OCR Engine", Some("5.4.1")),
            sw("Library of Congress", "ndnp-open-ocr", Some("1.2.0"))
        ));
        check(
            &d,
            "tesseract",
            Some("Tesseract Open Source OCR Engine"),
            Some("5.4.1"),
            Some(("ndnp-open-ocr", Some("1.2.0"))),
            Some("Library of Congress"),
        );
    }

    #[test]
    fn ddd_zed_and_the_mislabelled_creator() {
        let (_, d) = of(&format!(
            "<OCRProcessing><preProcessingStep>{}</preProcessingStep><ocrProcessingStep>{}</ocrProcessingStep></OCRProcessing>",
            sw("Digital Divide Data and Apsotech Services Pvt Ltd, India", "DDD ZED", Some("3.8.1.8")),
            sw("ABBYY (BIT Software), Russia", "FineReader", Some("11.0"))
        ));
        check(
            &d,
            "abbyy",
            Some("FineReader"),
            Some("11.0"),
            Some(("DDD ZED", Some("3.8.1.8"))),
            Some("Digital Divide Data"),
        );
        // CCS once named Google as the creator of ABBYY's engine: the name decides.
        let (_, d) = of(&format!(
            "<OCRProcessing><ocrProcessingStep>{}</ocrProcessingStep></OCRProcessing>",
            sw(
                "Google Inc. Apache License, Version 2.0",
                "ABBYY FineReader Engine",
                Some("8.1")
            )
        ));
        check(
            &d,
            "abbyy",
            Some("ABBYY FineReader Engine"),
            Some("8.1"),
            None,
            None,
        );
    }

    #[test]
    fn nothing_named() {
        let (h, d) = of(
            "<OCRProcessing ID=\"OP0001\"><ocrProcessingStep><processingSoftware><applicationDescription>NM20002alto.exe</applicationDescription></processingSoftware></ocrProcessingStep></OCRProcessing>",
        );
        assert_eq!(h.software.len(), 1);
        check(&d, "unknown", None, None, None, None);
        let h = read_header("<alto><Layout/></alto>".as_bytes()).unwrap();
        assert_eq!(h, Header::default());
        assert_eq!(derive(&h).engine_family, "unknown");
    }

    #[test]
    fn json_keeps_the_pairs_in_order() {
        let (h, _) = of(&format!(
            "<OCRProcessing><preProcessingStep>{}</preProcessingStep><ocrProcessingStep>{}</ocrProcessingStep></OCRProcessing>",
            sw("C", "CCS docWorks", Some("7.0-1.52")),
            sw("A", "FineReader", Some("11.0"))
        ));
        assert_eq!(
            json(&h.software),
            r#"[{"block":1,"step":"pre","creator":"C","name":"CCS docWorks","version":"7.0-1.52"},{"block":1,"step":"ocr","creator":"A","name":"FineReader","version":"11.0"}]"#
        );
    }

    #[test]
    fn an_archive_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let arch = dir.path().join("in");
        std::fs::create_dir(&arch).unwrap();
        let page = alto(&format!(
            "<OCRProcessing><ocrProcessingStep>{}</ocrProcessingStep></OCRProcessing>",
            sw("ABBYY", "FineReader", Some("12.0"))
        ));
        let mut t = tar::Builder::new(Vec::new());
        for (p, body) in [
            ("sn1/1900/01/02/ed-1/seq-1/ocr.txt", "text".to_owned()),
            ("sn1/1900/01/02/ed-1/seq-1/ocr.xml", page.clone()),
            ("sn1/1900/01/02/ed-1/seq-2/ocr.xml", "<broken".to_owned()),
        ] {
            let mut h = tar::Header::new_gnu();
            h.set_size(body.len() as u64);
            h.set_mode(0o644);
            h.set_cksum();
            t.append_data(&mut h, p, body.as_bytes()).unwrap();
        }
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        std::io::Write::write_all(&mut bz, &t.into_inner().unwrap()).unwrap();
        std::fs::write(arch.join("b_x_ver02.tar.bz2"), bz.finish().unwrap()).unwrap();
        let mut bz = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
        std::io::Write::write_all(
            &mut bz,
            &tar::Builder::new(Vec::new()).into_inner().unwrap(),
        )
        .unwrap();
        std::fs::write(arch.join("b_y_ver01.tar.bz2"), bz.finish().unwrap()).unwrap();
        std::fs::write(arch.join("not-a-batch.tar.bz2"), b"x").unwrap();
        let out = dir.path().join("out/loc-software.parquet");
        let s = run(&Options {
            archives: arch,
            out: out.clone(),
            workers: 2,
            limit: None,
        })
        .unwrap();
        assert_eq!((s.archives, s.rows, s.failed, s.no_page), (2, 2, 0, 1));
        assert_eq!(s.families.get("abbyy"), Some(&1));
        assert!(out.exists());
        assert!(!dir.path().join("out/.loc-software.parquet.part").exists());
        let f = std::fs::File::open(&out).unwrap();
        let r = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(f)
            .unwrap()
            .build()
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(r.num_rows(), 2);
        let docs = r
            .column_by_name("header_doc")
            .unwrap()
            .as_any()
            .downcast_ref::<StringArray>()
            .unwrap();
        assert_eq!(docs.value(0), "sn1_1900-01-02_ed-1_seq-1");
        assert!(docs.is_null(1));

        // A batch archive that can't be read fails the run, and the published file stays.
        let before = std::fs::read(&out).unwrap();
        let arch = dir.path().join("in");
        std::fs::write(arch.join("b_z_ver01.tar.bz2"), b"not bzip2").unwrap();
        let err = run(&Options {
            archives: arch,
            out: out.clone(),
            workers: 2,
            limit: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("1 of 3 archives failed"), "{err}");
        assert_eq!(std::fs::read(&out).unwrap(), before);
        assert!(!dir.path().join("out/.loc-software.parquet.part").exists());
    }
}
