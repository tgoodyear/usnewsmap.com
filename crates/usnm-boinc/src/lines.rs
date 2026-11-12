//! The Cherokee line re-read (#1621): a line job reads the Cherokee lines of one LoC page with
//! a model trained on the print, one LoC ALTO line box at a time, and LoC's text stays for
//! everything else.
//!
//! - `export --line-read chr` reads the page's LoC ALTO (`--loc-alto`, the bulk OCR archives
//!   unpacked), picks its Cherokee lines ([`select_lines`]) and puts their boxes, in scan pixels,
//!   in the job. It keeps the ALTO beside the job ([`LOC_ALTO_FILE`], [`LINE_READ_FILE`]).
//! - The worker ([`ocr_lines`]) decodes the scan, crops each box with the padding
//!   `scripts/ocr-survey/cherokee_hybrid.py` uses, and reads the crops at PSM 13. Its
//!   `text.txt` holds one reading per box, in the job's order; its `layout.xml` holds the boxes
//!   and readings as ALTO lines.
//! - `import` turns the result into the page ([`merge`]): LoC's lines in reading order, each
//!   Cherokee line the model's reading unless it fails
//!   (`usnm_core::cherokee_type::merge_lines`), with the counts in [`MERGE_FILE`]. The page
//!   then enters the release as our OCR, LoC's text kept as `text_loc`.
use anyhow::{ensure, Context};
use async_trait::async_trait;
use image::GrayImage;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Instant,
};
use usnm_alto::{Block, BlockKind, Granularity, Line, Page, Rect, Word};
use usnm_core::cherokee_type::{self, is_syllabary, LineSource};
use usnm_core::ids::PageKey;
use usnm_ocr_contract::boinc::{self, Job, LineBox, ResultManifest};
use usnm_ocr_worker::engine::{LineReading, Tesseract};

pub mod script;
pub mod stage;

/// Padding around a line box, in scan pixels, left and right.
pub const PAD_X: u32 = 12;
/// Padding around a line box, in scan pixels, above and below.
pub const PAD_Y: u32 = 8;
/// A line job's sidecars, in its journal directory: everything `import` needs besides the
/// result, in one directory so the hand-off's collect (`handoff get --prefix
/// dispatch/jobs/<id>/line-read`) and a retry carry them together.
pub const LINE_READ_DIR: &str = "line-read";
/// LoC's ALTO for the page, kept beside a line job by `export`.
pub const LOC_ALTO_FILE: &str = "line-read/loc.xml";
/// What `export` read it with ([`LineReadRecord`]).
pub const LINE_READ_FILE: &str = "line-read/record.json";
/// The merge's counts, written by `import`.
pub const MERGE_FILE: &str = "line-merge.json";
/// LoC's ALTO is in inch1200; read it at the job's DPI.
const ALTO_DPI: f64 = 300.0;

/// What `export` kept about a line job's page, beside [`LOC_ALTO_FILE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineReadRecord {
    pub loc_alto_sha256: String,
    pub scan_height: u32,
    pub scan_width: u32,
    /// The title's catalog languages, for rule v1's judgement ([`report`]).
    pub title_languages: Vec<String>,
}

/// Where LoC's bulk OCR keeps a page's ALTO: `<lccn>/<yyyy>/<mm>/<dd>/ed-<n>/seq-<m>/ocr.xml`.
pub fn loc_alto_path(dir: &Path, key: &PageKey) -> anyhow::Result<PathBuf> {
    let page = key.to_string();
    let parts: Vec<&str> = page.split('/').collect();
    let [lccn, date, ed, seq] = parts.as_slice() else {
        anyhow::bail!("unexpected page key {page}");
    };
    let ymd: Vec<&str> = date.split('-').collect();
    let [y, m, d] = ymd.as_slice() else {
        anyhow::bail!("unexpected page date {date}");
    };
    Ok(dir
        .join(lccn)
        .join(y)
        .join(m)
        .join(d)
        .join(ed)
        .join(seq)
        .join("ocr.xml"))
}

/// A scan's width and height without decoding it: a JP2's SIZ marker, or the image header.
pub fn scan_size(bytes: &[u8]) -> anyhow::Result<(u32, u32)> {
    if let Some(size) = usnm_ocr_contract::jp2::dimensions(bytes) {
        return Ok(size);
    }
    Ok(image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()?
        .into_dimensions()?)
}

/// LoC's ALTO, in early NDNP batches' namespace too, with its blocks' languages.
fn parse_loc(alto: &[u8]) -> anyhow::Result<Page> {
    Ok(usnm_alto::parse_with_languages(
        &usnm_ocr_quality::slot::normalize_alto_ns(alto),
        &usnm_alto::ParseOptions {
            dpi: Some(ALTO_DPI),
            ..usnm_alto::ParseOptions::default()
        },
    )?)
}

/// The id of text block `block`'s line `line` in [`usnm_alto::parse`]'s order.
fn line_id(block: usize, line: usize) -> String {
    format!("b{block}l{line}")
}

/// A line's text as LoC's ALTO has it: its words, and its hyphen at the end.
fn line_text(line: &Line) -> String {
    let mut s = line
        .words
        .iter()
        .map(|w| w.text.as_str())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(h) = &line.hyphen {
        s.push_str(h);
    }
    s
}

/// Whether most of a line's words are syllabary: a word counts when most of its letters are.
fn mostly_syllabary(line: &Line) -> bool {
    let (mut words, mut syllabary) = (0usize, 0usize);
    for w in &line.words {
        let letters = w.text.chars().filter(|c| c.is_alphabetic()).count();
        if letters == 0 {
            continue;
        }
        words += 1;
        if w.text.chars().filter(|c| is_syllabary(*c)).count() * 2 > letters {
            syllabary += 1;
        }
    }
    words > 0 && syllabary * 2 > words
}

/// The text blocks' lines of LoC's page, with their ids, in reading order.
fn page_lines(page: &Page) -> Vec<(String, &Block, &Line)> {
    page.blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind.is_text())
        .flat_map(|(bi, b)| {
            b.lines
                .iter()
                .enumerate()
                .map(move |(li, l)| (line_id(bi, li), b, l))
        })
        .collect()
}

/// The lines of LoC's `alto` to read again in `language`: those of its TextBlocks marked
/// `LANGUAGE="<language>"`, and any line whose words are mostly syllabary. Their boxes are in
/// pixels of a scan `scan` wide and high, without padding. Lines with no area are left out.
pub fn select_lines(alto: &[u8], scan: (u32, u32), language: &str) -> anyhow::Result<Vec<LineBox>> {
    let page = parse_loc(alto)?;
    ensure!(
        page.width > 0.0 && page.height > 0.0,
        "LoC's ALTO has no page size"
    );
    let (sx, sy) = (
        f64::from(scan.0) / page.width,
        f64::from(scan.1) / page.height,
    );
    let mut out = Vec::new();
    for (id, block, line) in page_lines(&page) {
        if block.lang.as_deref() != Some(language) && !mostly_syllabary(line) {
            continue;
        }
        let r = line.rect.map(sx, sy, 0.0, 0.0);
        let px = |v: f64, hi: u32| (v.round().max(0.0) as u32).min(hi);
        let rect = [
            px(r.x0, scan.0),
            px(r.y0, scan.1),
            px(r.x1, scan.0),
            px(r.y1, scan.1),
        ];
        if rect[0] < rect[2] && rect[1] < rect[3] {
            out.push(LineBox { id, rect });
        }
    }
    Ok(out)
}

/// `rect` with [`PAD_X`] and [`PAD_Y`] around it, clamped to an image `w` by `h`; none when
/// nothing of it is on the image.
pub fn padded(rect: [u32; 4], w: u32, h: u32) -> Option<(u32, u32, u32, u32)> {
    let [x0, y0, x1, y1] = rect;
    let (x0, y0) = (x0.saturating_sub(PAD_X), y0.saturating_sub(PAD_Y));
    let (x1, y1) = (
        x1.saturating_add(PAD_X).min(w),
        y1.saturating_add(PAD_Y).min(h),
    );
    (x0 < x1 && y0 < y1).then_some((x0, y0, x1, y1))
}

/// The scan as 8-bit grey: a JP2 through `hayro-jpeg2000`, anything else through `image`.
pub fn decode_gray(bytes: &[u8]) -> anyhow::Result<GrayImage> {
    let jp2 = bytes.starts_with(b"\x00\x00\x00\x0cjP  ") || bytes.starts_with(b"\xff\x4f\xff\x51");
    if !jp2 {
        return Ok(image::load_from_memory(bytes)?.to_luma8());
    }
    let image = hayro_jpeg2000::Image::new(bytes, &hayro_jpeg2000::DecodeSettings::default())
        .map_err(|e| anyhow::anyhow!("decoding the JP2: {e}"))?;
    let (w, h) = (image.width(), image.height());
    let channels = usize::from(image.color_space().num_channels()) + usize::from(image.has_alpha());
    let mut ctx = hayro_jpeg2000::DecoderContext::default();
    let decoded = image
        .decode(&mut ctx)
        .map_err(|e| anyhow::anyhow!("decoding the JP2: {e}"))?;
    let data = decoded.data_u8();
    let pixels = w as usize * h as usize;
    ensure!(
        channels >= 1 && data.len() == pixels * channels,
        "decoded JP2 has {} bytes for {w}x{h} with {channels} channels",
        data.len()
    );
    let gray = if channels < 3 {
        data.chunks_exact(channels).map(|p| p[0]).collect()
    } else {
        data.chunks_exact(channels)
            .map(|p| {
                let y = 0.299 * f64::from(p[0]) + 0.587 * f64::from(p[1]) + 0.114 * f64::from(p[2]);
                y.round().clamp(0.0, 255.0) as u8
            })
            .collect()
    };
    GrayImage::from_raw(w, h, gray).context("decoded JP2 size")
}

/// Reads line crops: the image's Tesseract, or a stand-in in tests.
#[async_trait]
pub trait LineEngine: Send + Sync {
    async fn read_lines(
        &self,
        crops: &[PathBuf],
        language: &str,
        work: &Path,
    ) -> anyhow::Result<Vec<LineReading>>;
}

#[async_trait]
impl LineEngine for Tesseract {
    async fn read_lines(
        &self,
        crops: &[PathBuf],
        language: &str,
        work: &Path,
    ) -> anyhow::Result<Vec<LineReading>> {
        Tesseract::read_lines(self, crops, language, work)
            .await
            .map_err(anyhow::Error::from)
    }
}

/// `text`'s words, each with its line's box: a line read whole has no word geometry, and the
/// stored ALTO's finer elements inherit their parent's box (13 §13.8.2), never a guess.
fn line_words(rect: Rect, text: &str, conf: Option<f64>) -> Vec<Word> {
    text.split_whitespace()
        .map(|w| Word {
            rect,
            text: w.to_owned(),
            conf,
        })
        .collect()
}

/// The ALTO's description. `date_time` is the result's completion time, never the clock's, so
/// the same result always gives the same bytes.
fn description(
    job: &Job,
    step: &str,
    date_time: Option<&str>,
) -> anyhow::Result<usnm_alto::Description> {
    Ok(usnm_alto::Description {
        file_name: job.scan.url.clone(),
        file_identifiers: Vec::new(),
        ocr: usnm_alto::Step {
            date_time: date_time
                .map(chrono::DateTime::parse_from_rfc3339)
                .transpose()?
                .map(|t| {
                    t.with_timezone(&chrono::Utc)
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
                }),
            settings: vec![
                ("ocr_engine".into(), job.engine_tag()?.to_string()),
                ("read".into(), step.into()),
                ("psm".into(), job.engine.psm.to_string()),
                ("dpi".into(), job.engine.dpi.to_string()),
            ],
            software: usnm_alto::Software {
                creator: "tesseract-ocr".into(),
                name: "tesseract".into(),
                version: job.engine.version.clone(),
            },
        },
        post: None,
    })
}

/// Reads a line job's lines on `scan` (its bytes, checked against the job), as a single-page
/// unit returns them: the result manifest, `text.txt` (one reading per line box, in order, each
/// ending in a newline) and `layout.xml` (the boxes as ALTO lines with the readings).
pub async fn ocr_lines(
    engine: &dyn LineEngine,
    job: &Job,
    scan: &[u8],
) -> anyhow::Result<(ResultManifest, String, Vec<u8>)> {
    let lines = job.lines.as_ref().context("not a line job")?;
    let started = Instant::now();
    let image = decode_gray(scan)?;
    let (w, h) = image.dimensions();
    let scratch = tempfile::tempdir()?;
    let mut crops = Vec::with_capacity(lines.lines.len());
    for (i, line) in lines.lines.iter().enumerate() {
        let (x0, y0, x1, y1) = padded(line.rect, w, h)
            .with_context(|| format!("line {} is off the {w}x{h} scan", line.id))?;
        let path = scratch.path().join(format!("line-{i:04}.png"));
        image::imageops::crop_imm(&image, x0, y0, x1 - x0, y1 - y0)
            .to_image()
            .save(&path)?;
        crops.push(path);
    }
    drop(image);
    let readings = if job.engine.profile == boinc::LINE_WORDS_PROFILE {
        read_words(engine, &crops, &lines.language, scratch.path())
            .await?
            .0
    } else {
        engine
            .read_lines(&crops, &lines.language, scratch.path())
            .await?
    };
    ensure!(
        readings.len() == lines.lines.len(),
        "{} readings for {} lines",
        readings.len(),
        lines.lines.len()
    );
    let mut text = String::new();
    let mut out_lines = Vec::with_capacity(readings.len());
    for (line, reading) in lines.lines.iter().zip(&readings) {
        let one = reading
            .text
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        text.push_str(&one);
        text.push('\n');
        let [x0, y0, x1, y1] = line.rect.map(f64::from);
        let rect = Rect::new(x0, y0, x1, y1);
        out_lines.push(Line {
            rect,
            words: line_words(rect, &one, reading.conf),
            hyphen: None,
        });
    }
    ensure!(text.len() <= boinc::MAX_TEXT, "result over limit");
    let completed_at = chrono::Utc::now().to_rfc3339();
    let page = Page {
        width: f64::from(w),
        height: f64::from(h),
        granularity: Granularity::Line,
        blocks: vec![Block {
            kind: BlockKind::Text,
            rect: Rect::new(0.0, 0.0, f64::from(w), f64::from(h)),
            lang: None,
            lines: out_lines,
        }],
    };
    let mut desc = description(
        job,
        "LoC ALTO line boxes, one crop each",
        Some(&completed_at),
    )?;
    desc.ocr
        .settings
        .push(("granularity".into(), page.granularity.as_str().into()));
    let alto = usnm_alto::write(&page, &desc, &lines.language).into_bytes();
    ensure!(alto.len() <= boinc::MAX_ALTO, "result over limit");
    usnm_alto::check(&alto, &usnm_alto::Limits::default())?;
    let result = ResultManifest {
        v: job.v,
        job_id: job.id.clone(),
        scan_sha256: job.scan.sha256.clone(),
        engine: job.engine.clone(),
        text_sha256: boinc::sha256(text.as_bytes()),
        alto_sha256: boinc::sha256(&alto),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        completed_at,
    };
    Ok((result, text, alto))
}

/// The language the Latin words of a words-route line job are read in (#1666).
pub const LATIN_LANGUAGE: &str = "eng";

/// Reads `crops` (line crops in `work`) in `language`, then reads again in [`LATIN_LANGUAGE`] the
/// words of each line that [`script`] finds Latin, and puts those readings in place of the
/// syllabary model's (#1666). A line with no such word is the syllabary model's reading as it
/// was. Each line's words keep their order; the line's confidence is the model's. The log has
/// one entry for each word that was read again, whatever the verdict.
pub async fn read_words(
    engine: &dyn LineEngine,
    crops: &[PathBuf],
    language: &str,
    work: &Path,
) -> anyhow::Result<(Vec<LineReading>, Vec<WordLog>)> {
    let mut readings = engine.read_lines(crops, language, work).await?;
    anyhow::ensure!(readings.len() == crops.len(), "one reading per crop");
    // The words worth a second reading, and the crop of each.
    let mut asked: Vec<(usize, usize)> = Vec::new();
    let mut word_crops: Vec<PathBuf> = Vec::new();
    for (i, reading) in readings.iter().enumerate() {
        let words = script_words(reading);
        if words.is_empty() {
            continue;
        }
        let image = image::open(&crops[i])?.to_luma8();
        let want = script::candidates(&script::Ink::new(&image), &words);
        for (k, (word, want)) in words.iter().zip(want).enumerate() {
            if !want {
                continue;
            }
            let [x0, y0, x1, y1] = word.rect;
            let (w, h) = image.dimensions();
            let pad = script::WORD_PAD;
            let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
            let (x1, y1) = (x1.saturating_add(pad).min(w), y1.saturating_add(pad).min(h));
            if x0 >= x1 || y0 >= y1 {
                continue;
            }
            let path = work.join(format!("word-{i:04}-{k:03}.png"));
            image::imageops::crop_imm(&image, x0, y0, x1 - x0, y1 - y0)
                .to_image()
                .save(&path)?;
            asked.push((i, k));
            word_crops.push(path);
        }
    }
    let mut log = Vec::new();
    if word_crops.is_empty() {
        return Ok((readings, log));
    }
    let english = engine.read_lines(&word_crops, LATIN_LANGUAGE, work).await?;
    anyhow::ensure!(
        english.len() == word_crops.len(),
        "one reading per word crop"
    );
    let mut at = 0;
    while at < asked.len() {
        let line = asked[at].0;
        let mut eng: Vec<Option<script::English>> = vec![None; readings[line].words.len()];
        while at < asked.len() && asked[at].0 == line {
            let reading = &english[at];
            let conf = reading
                .words
                .iter()
                .filter_map(|w| w.conf)
                .fold(None, |m: Option<f64>, c| Some(m.map_or(c, |m| m.min(c))))
                .or(reading.conf)
                .unwrap_or(0.0);
            eng[asked[at].1] = Some(script::English {
                text: reading.text.clone(),
                conf: conf * 100.0,
            });
            at += 1;
        }
        let words = script_words(&readings[line]);
        let image = image::open(&crops[line])?.to_luma8();
        let latin = script::verdicts(&script::Ink::new(&image), &words, &eng);
        for (k, e) in eng.iter().enumerate() {
            if let Some(e) = e {
                log.push(WordLog {
                    line,
                    word: k,
                    rect: words[k].rect,
                    syllabary: words[k].text.clone(),
                    syllabary_conf: words[k].conf,
                    english: e.text.clone(),
                    english_conf: e.conf,
                    latin: latin[k],
                });
            }
        }
        if latin.iter().any(|l| *l) {
            readings[line].text = script::assemble(&words, &eng, &latin);
        }
    }
    Ok((readings, log))
}

/// A word [`read_words`] read again in English, and what it decided.
#[derive(Debug, Clone, Serialize)]
pub struct WordLog {
    /// The line's index among the crops, and the word's in its reading.
    pub line: usize,
    pub word: usize,
    /// The word's box in the crop.
    pub rect: [u32; 4],
    pub syllabary: String,
    pub syllabary_conf: Option<f64>,
    pub english: String,
    pub english_conf: f64,
    pub latin: bool,
}

/// A reading's words as the classifier takes them, confidences 0 to 100.
fn script_words(reading: &LineReading) -> Vec<script::Word> {
    reading
        .words
        .iter()
        .map(|w| script::Word {
            text: w.text.clone(),
            conf: w.conf.map(|c| c * 100.0),
            rect: w.rect,
        })
        .collect()
}

/// How a page's lines were merged, written to [`MERGE_FILE`].
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct MergeStats {
    /// Cherokee lines the model read.
    pub lines: usize,
    /// Of them, lines that took the model's reading.
    pub model: usize,
    /// Of them, lines that kept LoC's text, by why.
    pub fallback: BTreeMap<&'static str, usize>,
    /// Each line that kept LoC's text, by id, and why.
    pub fallback_lines: BTreeMap<String, &'static str>,
    /// Latin words in the lines that took the model's reading: what a words-route job's
    /// English reads put in them (#1666). Left out of `line-merge.json` when none.
    #[serde(skip_serializing_if = "is_zero")]
    pub latin_words: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// The rule that holds a line job's reading against LoC's text: `line-merge-v2` for a words
/// route (#1666), else `line-merge-v1`.
fn merge_rule(job: &Job) -> cherokee_type::MergeRule {
    if job.engine.profile == boinc::LINE_WORDS_PROFILE {
        cherokee_type::MergeRule::V2
    } else {
        cherokee_type::MergeRule::V1
    }
}

/// A line job's page after the merge.
pub struct Merged {
    /// The page's text, as `text.txt` holds a page's, before normalization.
    pub text: String,
    /// The page's ALTO, in the scan's pixels.
    pub alto: String,
    pub stats: MergeStats,
}

/// A line job's page from LoC's ALTO and the job's result text: LoC's lines in reading order,
/// each line the model read replaced by its reading unless the reading fails. `completed_at`
/// is the result's (`ResultManifest::completed_at`): the ALTO is the same bytes each time.
pub fn merge(
    job: &Job,
    loc_alto: &[u8],
    record: &LineReadRecord,
    result_text: &str,
    completed_at: Option<&str>,
) -> anyhow::Result<Merged> {
    ensure!(
        boinc::sha256(loc_alto) == record.loc_alto_sha256,
        "LoC's ALTO changed since export"
    );
    let readings: BTreeMap<&str, &str> = job
        .line_texts(result_text)?
        .context("not a line job")?
        .into_iter()
        .collect();
    let mut page = parse_loc(loc_alto)?;
    let ids: Vec<(usize, usize, String)> = page
        .blocks
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind.is_text())
        .flat_map(|(bi, b)| (0..b.lines.len()).map(move |li| (bi, li, line_id(bi, li))))
        .collect();
    let texts: Vec<String> = ids
        .iter()
        .map(|(bi, li, _)| line_text(&page.blocks[*bi].lines[*li]))
        .collect();
    let pairs: Vec<(&str, Option<&str>)> = ids
        .iter()
        .zip(&texts)
        .map(|((_, _, id), loc)| (loc.as_str(), readings.get(id.as_str()).copied()))
        .collect();
    ensure!(
        pairs.iter().filter(|(_, r)| r.is_some()).count() == readings.len(),
        "the job's lines are not this page's"
    );
    let merged = cherokee_type::merge_lines_with(merge_rule(job), &pairs);
    for (((bi, li, _), source), text) in ids.iter().zip(&merged.sources).zip(&merged.lines) {
        if *source != LineSource::Model {
            continue;
        }
        let line = &mut page.blocks[*bi].lines[*li];
        line.words = line_words(line.rect, text, None);
        if text.ends_with(['-', '⸗', '¬', '‐']) {
            line.hyphen = None;
        }
    }
    // A line the model read has its words at the line's box: the page's geometry is no finer.
    if merged.model() > 0 && page.granularity < Granularity::Line {
        page.granularity = Granularity::Line;
    }
    page.rescale(f64::from(record.scan_width), f64::from(record.scan_height));
    let latin_words = merged
        .lines
        .iter()
        .zip(&merged.sources)
        .filter(|(_, s)| **s == LineSource::Model)
        .map(|(text, _)| script::latin_words(text))
        .sum();
    let stats = MergeStats {
        lines: readings.len(),
        model: merged.model(),
        latin_words,
        fallback: merged
            .fallbacks()
            .into_iter()
            .map(|(why, n)| (why.as_str(), n))
            .collect(),
        fallback_lines: ids
            .iter()
            .zip(&merged.sources)
            .filter_map(|((_, _, id), source)| match source {
                LineSource::Fallback(why) => Some((id.clone(), why.as_str())),
                _ => None,
            })
            .collect(),
    };
    let mut desc = description(
        job,
        "LoC ALTO; Cherokee lines read again one crop each",
        completed_at,
    )?;
    desc.ocr
        .settings
        .push(("granularity".into(), page.granularity.as_str().into()));
    desc.post = Some(usnm_alto::Step {
        date_time: None,
        settings: vec![
            ("lines_read".into(), stats.lines.to_string()),
            ("lines_model".into(), stats.model.to_string()),
            (
                "lines_fallback".into(),
                (stats.lines - stats.model).to_string(),
            ),
        ],
        software: usnm_alto::Software {
            creator: "usnewsmap.com".into(),
            name: "usnm-boinc line merge".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    });
    Ok(Merged {
        text: usnm_alto::text(&page),
        alto: usnm_alto::write(&page, &desc, &job.message.language),
        stats,
    })
}

/// A line job's sidecars from its journal directory, checked against each other.
pub fn sidecars(job_dir: &Path) -> anyhow::Result<(Vec<u8>, LineReadRecord)> {
    let loc = crate::read(&job_dir.join(LOC_ALTO_FILE), boinc::MAX_ALTO).with_context(|| {
        format!(
            "a line job's LoC ALTO, {} under its journal directory",
            LOC_ALTO_FILE
        )
    })?;
    let record: LineReadRecord = serde_json::from_slice(
        &crate::read(&job_dir.join(LINE_READ_FILE), boinc::MAX_JSON).with_context(|| {
            format!(
                "a line job's line-read record, {} under its journal directory",
                LINE_READ_FILE
            )
        })?,
    )?;
    ensure!(
        boinc::sha256(&loc) == record.loc_alto_sha256,
        "a line job's LoC ALTO is not the one it was exported with"
    );
    Ok((loc, record))
}

/// Copies a line job's sidecars from `from` to `to` (a retry's journal directory), checked.
pub fn copy_sidecars(from: &Path, to: &Path) -> anyhow::Result<()> {
    let (loc, record) = sidecars(from)?;
    crate::atomic(&to.join(LOC_ALTO_FILE), &loc)?;
    crate::atomic(
        &to.join(LINE_READ_FILE),
        &serde_json::to_vec_pretty(&record)?,
    )?;
    Ok(())
}

/// The merged page of the line job journaled in `job_dir`, from its result text.
pub fn merge_in(
    job_dir: &Path,
    job: &Job,
    result_text: &[u8],
    completed_at: Option<&str>,
) -> anyhow::Result<Merged> {
    let (loc, record) = sidecars(job_dir)?;
    merge(
        job,
        &loc,
        &record,
        std::str::from_utf8(result_text)?,
        completed_at,
    )
}

/// Rule `line-merge-v1`'s verdict on a page, as `line-merge.json` records it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Admission {
    pub rule: &'static str,
    /// `admit`, or why LoC's text stays.
    pub verdict: &'static str,
    pub cherokee: usize,
    pub model: usize,
}

/// A line's text as an ALTO gives it: its non-empty words joined by spaces, and with
/// `hyphen` its hyphen at the end.
fn alto_line_text(line: &Line, hyphen: bool) -> String {
    let mut s = line
        .words
        .iter()
        .map(|w| w.text.as_str())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let (true, Some(h)) = (hyphen, &line.hyphen) {
        s.push_str(h);
    }
    s
}

/// Rule `line-merge-v1` (`usnm_ocr_local::rule::line_merge`) on `published`, the ALTO a line
/// job's page is published with, checked against LoC's own ALTO rather than the merge: each
/// of LoC's lines, scaled to the scan, is found in `published` by its box, and its text there
/// compared with LoC's. A line of `published` that is none of LoC's counts as a change.
pub fn admission(
    job: &Job,
    loc_alto: &[u8],
    record: &LineReadRecord,
    published: &str,
    result_text: &str,
) -> anyhow::Result<Admission> {
    use usnm_ocr_local::rule::line_merge;
    ensure!(
        boinc::sha256(loc_alto) == record.loc_alto_sha256,
        "LoC's ALTO changed since export"
    );
    let readings: BTreeMap<&str, &str> = job
        .line_texts(result_text)?
        .context("not a line job")?
        .into_iter()
        .collect();
    let mut loc = parse_loc(loc_alto)?;
    let ids: Vec<String> = page_lines(&loc).into_iter().map(|(id, ..)| id).collect();
    loc.rescale(f64::from(record.scan_width), f64::from(record.scan_height));
    let ours = usnm_alto::parse(published.as_bytes(), &usnm_alto::ParseOptions::default())?;
    let key = |r: &Rect| [r.x0, r.y0, r.x1, r.y1].map(|v| v.round() as i64);
    // Each line's text with and without its hyphen: a line the job read is compared by its
    // words, since the merge keeps LoC's hyphen after a reading that has none.
    type Texts = std::collections::VecDeque<(String, String)>;
    let mut by_box: BTreeMap<[i64; 4], Texts> = BTreeMap::new();
    for b in ours.blocks.iter().filter(|b| b.kind.is_text()) {
        for l in &b.lines {
            by_box
                .entry(key(&l.rect))
                .or_default()
                .push_back((alto_line_text(l, true), alto_line_text(l, false)));
        }
    }
    let texts: Vec<(String, String, Option<&str>)> = page_lines(&loc)
        .into_iter()
        .zip(&ids)
        .map(|((_, _, line), id)| {
            let (full, words) = by_box
                .get_mut(&key(&line.rect))
                .and_then(|q| q.pop_front())
                .unwrap_or_default();
            match readings.get(id.as_str()).copied() {
                Some(read) => (alto_line_text(line, false), words, Some(read)),
                None => (alto_line_text(line, true), full, None),
            }
        })
        .collect();
    let mut lines: Vec<line_merge::Line<'_>> = texts
        .iter()
        .map(|(loc, ours, read)| line_merge::Line {
            loc,
            ours,
            read: *read,
        })
        .collect();
    // A line in what is published that is none of LoC's.
    let extra: Vec<&String> = by_box.values().flatten().map(|(full, _)| full).collect();
    for e in &extra {
        lines.push(line_merge::Line {
            loc: "",
            ours: e,
            read: None,
        });
    }
    let (counts, verdict) = line_merge::verdict(&lines);
    Ok(Admission {
        rule: match merge_rule(job) {
            cherokee_type::MergeRule::V1 => line_merge::VERSION,
            cherokee_type::MergeRule::V2 => line_merge::VERSION_V2,
        },
        verdict: match verdict {
            Ok(()) => "admit",
            Err(keep) => keep.as_str(),
        },
        cherokee: counts.cherokee,
        model: counts.model,
    })
}

/// [`admission`] for the line job journaled in `job_dir`.
pub fn admission_in(
    job_dir: &Path,
    job: &Job,
    published: &str,
    result_text: &[u8],
) -> anyhow::Result<Admission> {
    let (loc, record) = sidecars(job_dir)?;
    admission(
        job,
        &loc,
        &record,
        published,
        std::str::from_utf8(result_text)?,
    )
}

/// One line job in [`report`].
#[derive(Debug, Clone, Serialize)]
pub struct ReportRow {
    pub job: String,
    pub page: String,
    pub stats: MergeStats,
    /// Rule v1's verdict on the merged page against LoC's text: `pass`, or why LoC's stays.
    pub verdict: String,
    /// Rule `line-merge-v1`'s verdict, the rule that decides a line job's page.
    pub line_merge: Admission,
    /// The language and damage rate rule v1 found for the merged page and for LoC's text.
    pub ours: (Option<String>, Option<f64>),
    pub loc: (Option<String>, Option<f64>),
}

/// Rule v1 (`usnm_ocr_local::rule::judge`) on each line job under `root` with a result: the
/// accepted one, or else the canonical files in `spool`. LoC's text is its ALTO's.
pub fn report(root: &Path, spool: Option<&Path>) -> anyhow::Result<Vec<ReportRow>> {
    let mut rows = Vec::new();
    let Ok(entries) = std::fs::read_dir(root.join("jobs")) else {
        return Ok(rows);
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|id| boinc::is_sha256(id))
        .collect();
    ids.sort();
    for id in ids {
        let record = crate::load(root, &id)?;
        if record.job.lines.is_none() {
            continue;
        }
        let job_dir = crate::dir(root, &id)?;
        let accepted = job_dir.join("accepted").join("text.txt");
        let text = if accepted.exists() {
            crate::read(&accepted, boinc::MAX_TEXT)?
        } else if let Some(spool) = spool {
            let path = spool.join(format!("{}__file_text.txt", record.job.workunit()));
            if !path.exists() {
                continue;
            }
            crate::read(&path, boinc::MAX_TEXT)?
        } else {
            continue;
        };
        let merged = merge_in(&job_dir, &record.job, &text, None)?;
        let line_merge = admission_in(&job_dir, &record.job, &merged.alto, &text)?;
        let line_read: LineReadRecord = serde_json::from_slice(&crate::read(
            &job_dir.join(LINE_READ_FILE),
            boinc::MAX_JSON,
        )?)?;
        let loc = parse_loc(&crate::read(&job_dir.join(LOC_ALTO_FILE), boinc::MAX_ALTO)?)?;
        let loc_text = usnm_alto::text(&loc);
        let ours = crate::normalized_text(&record.job, merged.text.as_bytes())?;
        let languages: Vec<&str> = line_read
            .title_languages
            .iter()
            .map(String::as_str)
            .collect();
        let (ours_scores, loc_scores, verdict) =
            usnm_ocr_local::rule::judge(&ours, Some(&loc_text), &languages);
        rows.push(ReportRow {
            job: id,
            page: record.job.message.page.clone(),
            stats: merged.stats,
            verdict: match verdict {
                Ok(()) => "pass".into(),
                Err(keep) => keep.as_str().into(),
            },
            line_merge,
            ours: (ours_scores.lang, ours_scores.damage_rate),
            loc: (loc_scores.lang, loc_scores.damage_rate),
        });
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// LoC-style ALTO (inch1200 at 300 dpi: 4 units a pixel) for a 1000 by 1000 pixel page:
    /// an English block, a block marked Cherokee, and an unmarked block with a syllabary line.
    pub(crate) const LOC_ALTO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://schema.ccs-gmbh.com/ALTO">
<Description><MeasurementUnit>inch1200</MeasurementUnit></Description>
<Layout><Page ID="P1" HEIGHT="4000" WIDTH="4000" PHYSICAL_IMG_NR="1"><PrintSpace>
<TextBlock ID="TB1" HPOS="400" VPOS="400" WIDTH="3200" HEIGHT="400" LANGUAGE="eng">
<TextLine ID="TL1" HPOS="400" VPOS="400" WIDTH="3200" HEIGHT="160"><String CONTENT="CHEROKEE" HPOS="400" VPOS="400" WIDTH="1200" HEIGHT="160" WC="0.9"/><SP/><String CONTENT="PHOENIX." HPOS="1700" VPOS="400" WIDTH="1200" HEIGHT="160" WC="0.9"/></TextLine>
</TextBlock>
<TextBlock ID="TB2" HPOS="400" VPOS="1200" WIDTH="3200" HEIGHT="800" LANGUAGE="chr">
<TextLine ID="TL2" HPOS="400" VPOS="1200" WIDTH="3200" HEIGHT="160"><String CONTENT="ᎦᎪᎯ" HPOS="400" VPOS="1200" WIDTH="800" HEIGHT="160" WC="0.5"/><SP/><String CONTENT="ᎠᎴ" HPOS="1300" VPOS="1200" WIDTH="600" HEIGHT="160" WC="0.5"/></TextLine>
<TextLine ID="TL3" HPOS="400" VPOS="1600" WIDTH="3200" HEIGHT="160"><String CONTENT="Tbe" HPOS="400" VPOS="1600" WIDTH="800" HEIGHT="160" WC="0.2"/></TextLine>
</TextBlock>
<TextBlock ID="TB3" HPOS="400" VPOS="2400" WIDTH="3200" HEIGHT="800">
<TextLine ID="TL4" HPOS="400" VPOS="2400" WIDTH="3200" HEIGHT="160"><String CONTENT="ᎦᏙᎯ" HPOS="400" VPOS="2400" WIDTH="800" HEIGHT="160" WC="0.5"/></TextLine>
<TextLine ID="TL5" HPOS="400" VPOS="2800" WIDTH="3200" HEIGHT="160"><String CONTENT="The" HPOS="400" VPOS="2800" WIDTH="800" HEIGHT="160" WC="0.9"/><SP/><String CONTENT="end." HPOS="1300" VPOS="2800" WIDTH="800" HEIGHT="160" WC="0.9"/></TextLine>
</TextBlock>
</PrintSpace></Page></Layout></alto>"#;

    #[test]
    fn selects_marked_and_syllabary_lines_in_scan_pixels() {
        // A scan of twice the ALTO's 300 dpi frame.
        let lines = select_lines(LOC_ALTO.as_bytes(), (2000, 2000), "chr").unwrap();
        assert_eq!(
            lines,
            [
                LineBox {
                    id: "b1l0".into(),
                    rect: [200, 600, 1800, 680]
                },
                LineBox {
                    id: "b1l1".into(),
                    rect: [200, 800, 1800, 880]
                },
                LineBox {
                    id: "b2l0".into(),
                    rect: [200, 1200, 1800, 1280]
                },
            ]
        );
        // Without a block marked in the language, only the mostly syllabary lines.
        let ids: Vec<String> = select_lines(LOC_ALTO.as_bytes(), (2000, 2000), "yid")
            .unwrap()
            .into_iter()
            .map(|l| l.id)
            .collect();
        assert_eq!(ids, ["b1l0", "b2l0"]);
    }

    #[test]
    fn page_paths_and_padding() {
        let key: PageKey = "sn83020866/1828-03-06/ed-1/seq-4".parse().unwrap();
        assert_eq!(
            loc_alto_path(Path::new("/b"), &key).unwrap(),
            Path::new("/b/sn83020866/1828/03/06/ed-1/seq-4/ocr.xml")
        );
        assert_eq!(padded([20, 20, 80, 40], 100, 100), Some((8, 12, 92, 48)));
        assert_eq!(padded([5, 5, 95, 95], 100, 100), Some((0, 0, 100, 100)));
        assert_eq!(padded([200, 5, 300, 9], 100, 100), None);
    }

    /// A 60 by 40 grey JP2 (lossless, `opj_compress`), black where x is 10 to 49 and y 10 to 29.
    const JP2: &[u8] = include_bytes!("../tests/fixtures/line-60x40.jp2");

    fn synthetic() -> GrayImage {
        GrayImage::from_fn(60, 40, |x, y| {
            image::Luma([if (10..50).contains(&x) && (10..30).contains(&y) {
                0
            } else {
                255
            }])
        })
    }

    #[test]
    fn decodes_jp2_and_other_scans_to_grey() {
        assert_eq!(scan_size(JP2).unwrap(), (60, 40));
        assert_eq!(decode_gray(JP2).unwrap(), synthetic());
        let mut png = Vec::new();
        synthetic()
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        assert_eq!(scan_size(&png).unwrap(), (60, 40));
        assert_eq!(decode_gray(&png).unwrap(), synthetic());
    }

    /// Reads each crop as its size and how much of it is black.
    struct Measure(Mutex<Vec<String>>);

    #[async_trait]
    impl LineEngine for Measure {
        async fn read_lines(
            &self,
            crops: &[PathBuf],
            language: &str,
            work: &Path,
        ) -> anyhow::Result<Vec<LineReading>> {
            assert_eq!(language, "chr");
            Ok(crops
                .iter()
                .map(|c| {
                    assert_eq!(c.parent(), Some(work));
                    let img = image::open(c).unwrap().to_luma8();
                    let black = img.pixels().filter(|p| p.0[0] == 0).count();
                    self.0
                        .lock()
                        .unwrap()
                        .push(format!("{}x{}", img.width(), img.height()));
                    LineReading {
                        text: if black > 0 {
                            format!("Ꭰ{black}\nᎦ")
                        } else {
                            String::new()
                        },
                        conf: Some(0.5),
                        words: Vec::new(),
                    }
                })
                .collect())
        }
    }

    fn line_job(scan: &[u8], boxes: &[(&str, [u32; 4])]) -> Job {
        let mut job: Job = serde_json::from_str(include_str!(
            "../../usnm-ocr-contract/src/fixtures/boinc-tesseract.json"
        ))
        .unwrap();
        job.v = boinc::LINES_VERSION;
        job.scan.bytes = scan.len() as u64;
        job.scan.sha256 = boinc::sha256(scan);
        job.engine.profile = "chr_phoenix".into();
        job.engine.psm = boinc::LINE_PSM;
        job.lines = Some(boinc::LineRead {
            language: "chr".into(),
            psm: boinc::LINE_PSM,
            lines: boxes
                .iter()
                .map(|(id, rect)| LineBox {
                    id: (*id).into(),
                    rect: *rect,
                })
                .collect(),
        });
        job.id = job.identity().unwrap();
        job.validate().unwrap();
        job
    }

    #[tokio::test]
    async fn reads_each_padded_crop_and_returns_one_line_each() {
        let job = line_job(JP2, &[("a", [20, 15, 40, 25]), ("b", [20, 38, 40, 40])]);
        let engine = Measure(Mutex::new(Vec::new()));
        let (result, text, alto) = ocr_lines(&engine, &job, JP2).await.unwrap();
        // 20+24 by 10+16 with all 40 by 20 black pixels; the second is clamped at the scan's
        // edge and has no ink.
        assert_eq!(*engine.0.lock().unwrap(), ["44x26", "44x10"]);
        assert_eq!(text, "Ꭰ800 Ꭶ\n\n");
        result.matches(&job, text.as_bytes(), &alto).unwrap();
        assert_eq!(
            job.line_texts(&text).unwrap().unwrap(),
            [("a", "Ꭰ800 Ꭶ"), ("b", "")]
        );
        let page = usnm_alto::parse(&alto, &usnm_alto::ParseOptions::default()).unwrap();
        // The empty reading has no words, so the stored ALTO has no line for it.
        assert_eq!(page.blocks[0].lines.len(), 1);
        assert_eq!(
            page.blocks[0].lines[0].rect,
            Rect::new(20.0, 15.0, 40.0, 25.0)
        );
        assert_eq!(page.blocks[0].lines[0].words.len(), 2);
        for line in &page.blocks[0].lines {
            for word in &line.words {
                assert_eq!(word.rect, line.rect);
            }
        }
        // A box off the scan fails the page.
        let off = line_job(JP2, &[("a", [80, 15, 90, 25])]);
        assert!(ocr_lines(&engine, &off, JP2).await.is_err());
    }

    #[test]
    fn merges_model_lines_into_loc_text_and_counts_fallbacks() {
        let scan = b"scan";
        let alto = LOC_ALTO.as_bytes();
        let boxes: Vec<(String, [u32; 4])> = select_lines(alto, (2000, 2000), "chr")
            .unwrap()
            .into_iter()
            .map(|l| (l.id, l.rect))
            .collect();
        let refs: Vec<(&str, [u32; 4])> = boxes.iter().map(|(i, r)| (i.as_str(), *r)).collect();
        let job = line_job(scan, &refs);
        let record = LineReadRecord {
            loc_alto_sha256: boinc::sha256(alto),
            scan_height: 2000,
            scan_width: 2000,
            title_languages: vec!["eng".into(), "chr".into()],
        };
        // b1l0 takes the model's reading; b1l1 (LoC "Tbe") too, LoC had no syllabary;
        // b2l0 loses a New Testament word, so LoC's stays.
        let result = "ᎦᏙᎯ ᎠᎴ\nᎠᏍᎦᏯ\nᎦᎪᎯ\n";
        let merged = merge(&job, alto, &record, result, None).unwrap();
        assert_eq!(
            merged.text,
            "CHEROKEE PHOENIX.\n\nᎦᏙᎯ ᎠᎴ\nᎠᏍᎦᏯ\n\nᎦᏙᎯ\nThe end."
        );
        assert_eq!(
            merged.stats,
            MergeStats {
                lines: 3,
                model: 2,
                fallback: BTreeMap::from([("fewer_known", 1)]),
                fallback_lines: BTreeMap::from([("b2l0".to_owned(), "fewer_known")]),
                latin_words: 0,
            }
        );
        let page =
            usnm_alto::parse(merged.alto.as_bytes(), &usnm_alto::ParseOptions::default()).unwrap();
        assert_eq!((page.width, page.height), (2000.0, 2000.0));
        assert_eq!(usnm_alto::text(&page), merged.text);
        // Every word lies within its line, and the page claims line geometry, no finer.
        assert!(merged.alto.contains("granularity=line"), "{}", merged.alto);
        for line in page.blocks.iter().flat_map(|b| &b.lines) {
            for word in &line.words {
                let (w, l) = (word.rect, line.rect);
                assert!(
                    w.x0 >= l.x0 && w.y0 >= l.y0 && w.x1 <= l.x1 && w.y1 <= l.y1,
                    "{word:?} outside {l:?}"
                );
            }
        }
        // The same result merges to the same bytes, whenever: the ALTO's date is the result's.
        let at = Some("2026-11-09T12:00:00Z");
        let once = merge(&job, alto, &record, result, at).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        let again = merge(&job, alto, &record, result, at).unwrap();
        assert_eq!(once.alto, again.alto);
        assert!(once.alto.contains("2026-11-09T12:00:00Z"));
        // line-merge-v1, against LoC's ALTO: 2 of 3 lines took the model's reading.
        let admit = admission(&job, alto, &record, &merged.alto, result).unwrap();
        assert_eq!(
            (admit.verdict, admit.cherokee, admit.model),
            ("admit", 3, 2)
        );
        // An English line changed in what is published is refused.
        let changed = merged.alto.replace("PHOENIX.", "PHOENIX,");
        assert_ne!(changed, merged.alto);
        let refused = admission(&job, alto, &record, &changed, result).unwrap();
        assert_eq!(refused.verdict, "line_changed");
        // Under half the Cherokee lines from the model.
        let mostly_loc = "\nᎠ\nᎦᎪᎯ\n";
        let page = merge(&job, alto, &record, mostly_loc, None).unwrap();
        let few = admission(&job, alto, &record, &page.alto, mostly_loc).unwrap();
        assert_eq!((few.verdict, few.model), ("model_share", 1));
        // A result for other lines, or LoC's ALTO changed, is refused.
        assert!(merge(&job, alto, &record, "a\nb\n", None).is_err());
        let mut changed = record.clone();
        changed.loc_alto_sha256 = boinc::sha256(b"other");
        assert!(merge(&job, alto, &changed, result, None).is_err());
        let other = line_job(scan, &[("b9l9", [1, 1, 2, 2])]);
        assert!(merge(&other, alto, &record, "Ꭰ\n", None).is_err());
    }

    /// Reads crops as a line model and the English model would: each crop's reading is its
    /// width, and a word crop is read by the `eng` stand-in.
    struct Both {
        chr: Vec<Vec<(&'static str, f64, [u32; 4])>>,
        eng: Mutex<Vec<String>>,
        said: Vec<(&'static str, f64)>,
    }

    #[async_trait]
    impl LineEngine for Both {
        async fn read_lines(
            &self,
            crops: &[PathBuf],
            language: &str,
            work: &Path,
        ) -> anyhow::Result<Vec<LineReading>> {
            for c in crops {
                assert_eq!(c.parent(), Some(work));
            }
            if language == "chr" {
                return Ok(self
                    .chr
                    .iter()
                    .map(|words| {
                        let words: Vec<usnm_ocr_worker::engine::WordReading> = words
                            .iter()
                            .map(|(t, c, r)| usnm_ocr_worker::engine::WordReading {
                                text: (*t).into(),
                                conf: Some(c / 100.0),
                                rect: *r,
                            })
                            .collect();
                        LineReading {
                            text: words
                                .iter()
                                .map(|w| w.text.as_str())
                                .collect::<Vec<_>>()
                                .join(" "),
                            conf: Some(0.5),
                            words,
                        }
                    })
                    .collect());
            }
            assert_eq!(language, "eng");
            let mut seen = self.eng.lock().unwrap();
            Ok(crops
                .iter()
                .enumerate()
                .map(|(i, c)| {
                    let img = image::open(c).unwrap().to_luma8();
                    seen.push(format!("{}x{}", img.width(), img.height()));
                    let (text, conf) = self.said[i];
                    LineReading {
                        text: text.into(),
                        conf: Some(conf / 100.0),
                        words: vec![usnm_ocr_worker::engine::WordReading {
                            text: text.into(),
                            conf: Some(conf / 100.0),
                            rect: [0, 0, 1, 1],
                        }],
                    }
                })
                .collect())
        }
    }

    #[tokio::test]
    async fn words_the_classifier_finds_latin_are_read_in_english() {
        let dir = tempfile::tempdir().unwrap();
        // One line crop, 300 by 60, with a block of ink for each of its three words.
        let mut img = image::GrayImage::from_pixel(300, 60, image::Luma([255]));
        for (x0, x1) in [(10u32, 70u32), (90, 150), (170, 290)] {
            for y in 20..40 {
                for x in x0..x1 {
                    img.put_pixel(x, y, image::Luma([0]));
                }
            }
        }
        let crop = dir.path().join("line-0000.png");
        img.save(&crop).unwrap();
        let engine = Both {
            chr: vec![vec![
                ("ᎤᏪᏟᏌᏁᏯ", 92.0, [8, 18, 72, 42]),
                ("ᏣᎳ", 20.0, [88, 18, 152, 42]),
                ("ᎤᏪᏟᏌᏁᏯ", 93.0, [168, 18, 292, 42]),
            ]],
            eng: Mutex::new(Vec::new()),
            said: vec![("Judge", 90.0)],
        };
        let (readings, log) = read_words(&engine, std::slice::from_ref(&crop), "chr", dir.path())
            .await
            .unwrap();
        // Only the unsure word is read again, from its box with ten pixels around it.
        assert_eq!(engine.eng.lock().unwrap().as_slice(), ["84x44"]);
        assert_eq!(readings[0].text, "ᎤᏪᏟᏌᏁᏯ Judge ᎤᏪᏟᏌᏁᏯ");
        assert_eq!(log.len(), 1);
        assert!(log[0].latin && log[0].word == 1);
        // The same line when the English reading isn't sure: the syllabary model's stands.
        let engine = Both {
            said: vec![("Judge", 20.0)],
            ..engine
        };
        let (readings, log) = read_words(&engine, &[crop], "chr", dir.path())
            .await
            .unwrap();
        assert_eq!(readings[0].text, "ᎤᏪᏟᏌᏁᏯ ᏣᎳ ᎤᏪᏟᏌᏁᏯ");
        assert!(!log[0].latin);
    }

    #[test]
    fn a_words_route_page_keeps_a_line_with_latin_words_under_rule_v2() {
        let scan = b"scan";
        let alto = LOC_ALTO.as_bytes();
        let boxes: Vec<(String, [u32; 4])> = select_lines(alto, (2000, 2000), "chr")
            .unwrap()
            .into_iter()
            .map(|l| (l.id, l.rect))
            .collect();
        let refs: Vec<(&str, [u32; 4])> = boxes.iter().map(|(i, r)| (i.as_str(), *r)).collect();
        let mut job = line_job(scan, &refs);
        let record = LineReadRecord {
            loc_alto_sha256: boinc::sha256(alto),
            scan_height: 2000,
            scan_width: 2000,
            title_languages: vec!["eng".into(), "chr".into()],
        };
        // LoC's b1l0 is two syllabary words, five letters. The reading keeps the second and has
        // a Latin first word: under v1 it has under half of LoC's letters.
        let result = "ᎠᎴ (Cherokee)\nᎠᏍᎦᏯ\nᎦᏙᎯ\n";
        let v1 = merge(&job, alto, &record, result, None).unwrap();
        assert_eq!(v1.stats.fallback.get("few_letters"), Some(&1));
        assert!(!v1.text.contains("Cherokee"));
        job.engine.profile = boinc::LINE_WORDS_PROFILE.into();
        let v2 = merge(&job, alto, &record, result, None).unwrap();
        assert_eq!(v2.stats.fallback.get("few_letters"), None);
        assert!(v2.text.contains("ᎠᎴ (Cherokee)"), "{}", v2.text);
        assert_eq!(v2.stats.latin_words, 1);
        let admit = admission(&job, alto, &record, &v2.alto, result).unwrap();
        assert_eq!(admit.rule, "line-merge-v2");
        assert_eq!(admit.verdict, "admit");
    }
}
