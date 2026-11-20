//! Page-mode ALTO assembly with per-block provenance (#1998; design `#1997`, "Assembly" and
//! "Per-block provenance").
//!
//! The page starts from LoC's ALTO in the scan's pixels, as [`crate::lines::merge`] does for
//! Cherokee lines. Each region of the unit keeps LoC's geometry (block box, line boxes, reading
//! order). A region whose pair judged it `agree` takes the canonical result's Xiaomi-OCR-0 text,
//! line by line; every other region keeps LoC's text, for one reason:
//!
//! | Reason     | When                                                                           |
//! | ---------- | ------------------------------------------------------------------------------ |
//! | `empty`    | the canonical reading has no letter or digit and LoC's region does             |
//! | `looped`   | either side of the pair looped on it                                           |
//! | `disagree` | the pair's readings fall under the crop-consensus-v2 floor                     |
//! | `small`    | not read (under the minimum size, or a line the cut left out)                  |
//! | `failed`   | not read (no reading, or no region state: the request failed or timed out)     |
//!
//! The result is ALTO v4 ([`usnm_alto::write_v4`]): one `Processing` step for Xiaomi-OCR-0 and
//! one for LoC's OCR, and a `PROCESSINGREFS` on each block naming the one that made its text.
//! The same facts go to a `blocks.json` sidecar ([`Blocks`]), so analysis never parses ALTO.
//! The page is HELD: nothing here writes `ocr-local` or `ocr-out`.
use super::consensus::CropState;
use super::page_mode::{letters, line_text, scan_ids};
use anyhow::{ensure, Context};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};
use usnm_alto::{Block, BlockNote, Granularity, Line, Page, Processing, Rect, Word};
use usnm_ocr_contract::qwen::Region;

/// The ids of the `Processing` steps blocks refer to.
pub const PROC_XIAOMI: &str = "PROC_XIAOMI";
pub const PROC_LOC: &str = "PROC_LOC";
pub const PROC_ASSEMBLY: &str = "PROC_ASSEMBLY";
/// `blocks.json`'s version.
pub const BLOCKS_VERSION: u32 = 1;

/// Why a region kept LoC's text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Keep {
    Empty,
    Looped,
    Disagree,
    Small,
    Failed,
}

impl Keep {
    pub fn as_str(self) -> &'static str {
        match self {
            Keep::Empty => "empty",
            Keep::Looped => "looped",
            Keep::Disagree => "disagree",
            Keep::Small => "small",
            Keep::Failed => "failed",
        }
    }
}

/// How a region's lines were fitted to LoC's.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Fit {
    /// The reading has as many lines as LoC's region: line `i` goes to LoC line `i`.
    Lines,
    /// Otherwise the words are spread over LoC's lines in proportion to each one's characters.
    Reflow,
}

/// One region of the unit with what the pair made of it.
#[derive(Debug, Clone)]
pub struct RegionIn<'a> {
    pub region: &'a Region,
    /// The crop id of the region within its result: the page's own, without a bundle's
    /// `<doc_id>:` prefix.
    pub crop: &'a str,
    /// The canonical result's reading, when it has one.
    pub reading: Option<&'a str>,
    /// What an agreeing pair judged, when it judged the region.
    pub state: Option<(CropState, Option<f64>)>,
}

/// What produced the Xiaomi text, for the `Processing` step and the notes.
#[derive(Debug, Clone)]
pub struct Facts<'a> {
    /// The `ocr_engine` tag (13 §13.8.1).
    pub tag: &'a str,
    pub model: &'a str,
    /// The upstream revision, short.
    pub revision: &'a str,
    pub prompt_version: u32,
    pub image: &'a str,
    pub llama: &'a str,
    pub weights: &'a str,
    /// The canonical result's `completed_at`, for the step's time: the ALTO is the same bytes
    /// each time.
    pub completed_at: Option<&'a str>,
}

pub struct Inputs<'a> {
    pub doc_id: &'a str,
    pub loc_alto: &'a [u8],
    pub dims: (u32, u32),
    /// The page's image URL for the ALTO's `fileName`.
    pub file_name: &'a str,
    /// ISO 639-3.
    pub language: &'a str,
    pub regions: Vec<RegionIn<'a>>,
    pub facts: Facts<'a>,
    /// Page-level provenance, as `fileIdentifier`s (location, value): `usnm_engine_image`,
    /// `usnm_job_id`, `usnm_llama_image`, ... (the notes of `retry::splice_notes`).
    pub notes: Vec<(String, String)>,
}

/// One block of the assembled page, as `blocks.json` lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BlockEntry {
    /// The ALTO `TextBlock` id: LoC's, or `<LoC id>.<n>` when LoC's block was cut.
    pub id: String,
    pub loc_block: String,
    /// The region (crop id) the text was read for; none for a stretch the cut left to LoC.
    pub region: Option<String>,
    /// `xiaomi` or `loc`.
    pub source: &'static str,
    /// Why LoC's text was kept, for `source = loc`.
    pub kept: Option<Keep>,
    /// The crop-consensus-v2 similarity of the pair's readings, where they were compared.
    pub score: Option<f64>,
    pub fit: Option<Fit>,
    pub loc_chars: usize,
    pub xiaomi_chars: Option<usize>,
}

/// `blocks.json`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Blocks {
    pub v: u32,
    pub doc_id: String,
    pub engine: String,
    pub regions: usize,
    pub xiaomi: usize,
    pub loc_by_reason: BTreeMap<&'static str, usize>,
    /// Regions with text (the pair compared them or one side looped) that agreed, by count
    /// and by LoC characters: the page's agreeing share, which acceptance reads.
    pub agree_share_regions: Option<f64>,
    pub agree_share_chars: Option<f64>,
    pub blocks: Vec<BlockEntry>,
}

pub struct Assembled {
    /// ALTO v4.
    pub alto: String,
    pub blocks: Blocks,
    /// The page's text, as `usnm_alto::text` gives it.
    pub text: String,
}

/// The segments of a LoC block: a region, or a stretch of lines no region has.
enum Segment<'a> {
    Region(&'a RegionIn<'a>),
    Gap,
}

fn alnum(s: &str) -> bool {
    s.chars().any(char::is_alphanumeric)
}

/// Why a region's LoC text stays, or `None` when it takes the reading.
fn keep_reason(r: &RegionIn<'_>, loc_has_text: bool) -> Option<Keep> {
    let (Some(reading), Some((state, _))) = (r.reading, r.state) else {
        return Some(Keep::Failed);
    };
    // The empty guard comes first: a reading with no letter or digit never replaces LoC's text.
    if !alnum(reading) && loc_has_text {
        return Some(Keep::Empty);
    }
    match state {
        CropState::Agree => None,
        CropState::Looped => Some(Keep::Looped),
        CropState::Disagree => Some(Keep::Disagree),
        CropState::NoText => Some(Keep::Empty),
    }
}

/// A line's words, each with the line's own box: the model returns no coordinates, so none are
/// invented (the parent-box rule of docs/design/13-ocr-engines.md), and the page declares
/// that its geometry is no finer than a line's.
fn split_words(rect: Rect, text: &str) -> Vec<Word> {
    text.split_whitespace()
        .map(|w| Word {
            rect,
            text: w.to_owned(),
            conf: None,
        })
        .collect()
}

/// `words` spread over lines in proportion to `weights` (each LoC line's characters): a word
/// goes to the line whose share of the characters its middle falls in.
fn reflow<'a>(words: &[&'a str], weights: &[usize]) -> Vec<Vec<&'a str>> {
    // The characters and the spaces between the words, as `used` counts them.
    let total_chars: usize =
        words.iter().map(|w| w.chars().count()).sum::<usize>() + words.len().saturating_sub(1);
    let total_w: usize = weights.iter().map(|w| (*w).max(1)).sum();
    let mut out: Vec<Vec<&str>> = vec![Vec::new(); weights.len()];
    let (mut used, mut cum_w, mut line) = (0usize, 0usize, 0usize);
    for w in words {
        let len = w.chars().count();
        while line + 1 < weights.len() {
            let end = (total_chars * (cum_w + weights[line].max(1))).div_ceil(total_w.max(1));
            if used * 2 + len <= end * 2 {
                break;
            }
            cum_w += weights[line].max(1);
            line += 1;
        }
        out[line].push(w);
        used += len + 1;
    }
    out
}

/// The lines of a region with its reading put in: `(lines, fit)`.
fn fitted(lines: &[(String, Line)], reading: &str) -> (Vec<Line>, Fit) {
    let read_lines: Vec<&str> = reading
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let (texts, fit): (Vec<String>, Fit) = if read_lines.len() == lines.len() {
        (
            read_lines.iter().map(|l| (*l).to_owned()).collect(),
            Fit::Lines,
        )
    } else {
        let words: Vec<&str> = reading.split_whitespace().collect();
        let weights: Vec<usize> = lines
            .iter()
            .map(|(_, l)| line_text(l).chars().count())
            .collect();
        (
            reflow(&words, &weights)
                .into_iter()
                .map(|w| w.join(" "))
                .collect(),
            Fit::Reflow,
        )
    };
    let out = lines
        .iter()
        .zip(texts)
        .map(|((_, loc), text)| {
            let mut line = loc.clone();
            line.words = split_words(loc.rect, &text);
            // LoC's end-of-line hyphen belongs to its own text and breaks: any hyphen the
            // model read is already in the new text, and a reflowed line ends elsewhere.
            line.hyphen = None;
            line
        })
        .collect();
    (out, fit)
}

fn union_rect(lines: &[(String, Line)]) -> Option<Rect> {
    Rect::union_all(lines.iter().map(|(_, l)| &l.rect))
}

fn loc_software(alto: &[u8]) -> usnm_alto::Software {
    use quick_xml::{events::Event, Reader};
    let mut sw = usnm_alto::Software::default();
    let Ok(text) = std::str::from_utf8(alto) else {
        return sw;
    };
    let mut reader = Reader::from_str(text);
    let (mut inside, mut field) = (false, None::<&'static str>);
    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => match e.local_name().as_ref() {
                "processingSoftware" => inside = true,
                "softwareCreator" if inside => field = Some("creator"),
                "softwareName" if inside => field = Some("name"),
                "softwareVersion" if inside => field = Some("version"),
                _ => {}
            },
            Ok(Event::Text(t)) => {
                if let Some(f) = field {
                    let v = t
                        .xml_content(quick_xml::XmlVersion::Implicit1_0)
                        .trim()
                        .to_owned();
                    match f {
                        "creator" => sw.creator = v,
                        "name" => sw.name = v,
                        _ => sw.version = v,
                    }
                }
            }
            Ok(Event::End(e)) => {
                field = None;
                if e.local_name().as_ref() == "processingSoftware" && !sw.name.is_empty() {
                    break;
                }
            }
            Ok(Event::Eof) | Err(_) => break,
            _ => {}
        }
    }
    if sw.creator.is_empty() && sw.name.is_empty() {
        sw.creator = "Library of Congress".into();
        sw.name = "LoC OCR".into();
    }
    sw
}

/// Assembles a page-mode result into held ALTO and its `blocks.json`.
pub fn assemble(inputs: &Inputs<'_>) -> anyhow::Result<Assembled> {
    let alto = usnm_ocr_quality::slot::normalize_alto_ns(inputs.loc_alto);
    let (w, h) = inputs.dims;
    let loc = crate::quality::parse_alto(&alto, f64::from(w), f64::from(h))?;
    let ids = scan_ids(&alto)?;
    ensure!(
        ids.len() == loc.blocks.len()
            && ids
                .iter()
                .zip(&loc.blocks)
                .all(|(s, b)| { s.text == b.kind.is_text() && s.lines.len() == b.lines.len() }),
        "the ALTO's ids do not line up with its blocks"
    );
    // The regions of each LoC block, in the unit's order.
    let mut by_block: BTreeMap<&str, Vec<&RegionIn<'_>>> = BTreeMap::new();
    for r in &inputs.regions {
        by_block.entry(r.region.block.as_str()).or_default().push(r);
    }
    let known: BTreeSet<&str> = ids.iter().filter_map(|s| s.id.as_deref()).collect();
    for b in by_block.keys() {
        ensure!(known.contains(b), "region block {b} is not in this ALTO");
    }

    let mut blocks: Vec<Block> = Vec::new();
    let mut notes: Vec<Option<BlockNote>> = Vec::new();
    let mut entries: Vec<BlockEntry> = Vec::new();
    let mut took_xiaomi = false;
    for (scan, block) in ids.iter().zip(&loc.blocks) {
        let Some(block_id) = scan.id.as_deref().filter(|_| scan.text) else {
            // An illustration or rule, as LoC has it.
            blocks.push(block.clone());
            notes.push(None);
            continue;
        };
        let lines: Vec<(String, Line)> = scan
            .lines
            .iter()
            .zip(&block.lines)
            .map(|(id, l)| {
                Ok((
                    id.clone()
                        .with_context(|| format!("a line of {block_id} has no ID"))?,
                    l.clone(),
                ))
            })
            .collect::<anyhow::Result<_>>()?;
        let regions = by_block.get(block_id).cloned().unwrap_or_default();
        // Cut the block's lines into segments: each region's run, and what lies between.
        let index: BTreeMap<&str, usize> = lines
            .iter()
            .enumerate()
            .map(|(i, (id, _))| (id.as_str(), i))
            .collect();
        let mut segments: Vec<(Vec<usize>, Segment<'_>)> = Vec::new();
        let mut claimed = vec![false; lines.len()];
        for r in regions.iter() {
            let at: Vec<usize> = r
                .region
                .lines
                .iter()
                .map(|l| {
                    index.get(l.as_str()).copied().with_context(|| {
                        format!("region {}: line {l} is not in {block_id}", r.crop)
                    })
                })
                .collect::<anyhow::Result<_>>()?;
            ensure!(
                at.windows(2).all(|p| p[0] + 1 == p[1]) && at.iter().all(|&i| !claimed[i]),
                "region {} is not a run of {block_id}'s lines",
                r.crop
            );
            at.iter().for_each(|&i| claimed[i] = true);
            segments.push((at, Segment::Region(r)));
        }
        let mut gap: Vec<usize> = Vec::new();
        for (i, c) in claimed.iter().enumerate() {
            if *c {
                if !gap.is_empty() {
                    segments.push((std::mem::take(&mut gap), Segment::Gap));
                }
            } else {
                gap.push(i);
            }
        }
        if !gap.is_empty() {
            segments.push((gap, Segment::Gap));
        }
        segments.sort_by_key(|(at, _)| at.first().copied().unwrap_or(usize::MAX));
        if segments.is_empty() {
            // No line at all: the writer leaves such a block out.
            blocks.push(block.clone());
            notes.push(None);
            continue;
        }
        let cut = segments.len() > 1;
        for (k, (at, seg)) in segments.iter().enumerate() {
            let seg_lines: Vec<(String, Line)> = at.iter().map(|&i| lines[i].clone()).collect();
            let id = if cut {
                format!("{block_id}.{}", k + 1)
            } else {
                block_id.to_owned()
            };
            let loc_text: String = seg_lines
                .iter()
                .map(|(_, l)| line_text(l))
                .collect::<Vec<_>>()
                .join("\n");
            let loc_chars = letters(&loc_text);
            let mut entry = BlockEntry {
                id: id.clone(),
                loc_block: block_id.to_owned(),
                region: None,
                source: "loc",
                kept: Some(Keep::Small),
                score: None,
                fit: None,
                loc_chars,
                xiaomi_chars: None,
            };
            let mut out_lines: Vec<Line> = seg_lines.iter().map(|(_, l)| l.clone()).collect();
            if let Segment::Region(r) = seg {
                entry.region = Some(r.crop.to_owned());
                entry.score = r.state.and_then(|(_, s)| s);
                match keep_reason(r, loc_chars > 0) {
                    None => {
                        let reading = r.reading.unwrap_or_default();
                        let (new, fit) = fitted(&seg_lines, reading);
                        out_lines = new;
                        entry.source = "xiaomi";
                        entry.kept = None;
                        entry.fit = Some(fit);
                        entry.xiaomi_chars = Some(letters(reading));
                        took_xiaomi = true;
                    }
                    Some(why) => entry.kept = Some(why),
                }
            }
            let rect = if cut {
                union_rect(&seg_lines).unwrap_or(block.rect)
            } else {
                block.rect
            };
            blocks.push(Block {
                kind: block.kind,
                rect,
                lang: block.lang.clone(),
                lines: out_lines,
            });
            notes.push(Some(BlockNote {
                id,
                processing: if entry.source == "xiaomi" {
                    PROC_XIAOMI
                } else {
                    PROC_LOC
                }
                .to_owned(),
            }));
            entries.push(entry);
        }
    }
    let mut page = Page {
        width: loc.width,
        height: loc.height,
        granularity: loc.granularity,
        blocks,
    };
    // A line the model read has proportional word boxes: the page's geometry is no finer than a
    // line's.
    if took_xiaomi && page.granularity < Granularity::Line {
        page.granularity = Granularity::Line;
    }
    let (read, xiaomi) = (
        entries.iter().filter(|e| e.region.is_some()).count(),
        entries.iter().filter(|e| e.source == "xiaomi").count(),
    );
    let mut loc_by_reason: BTreeMap<&'static str, usize> = BTreeMap::new();
    for e in entries.iter().filter(|e| e.region.is_some()) {
        if let Some(k) = e.kept {
            *loc_by_reason.entry(k.as_str()).or_default() += 1;
        }
    }
    // The agreeing share: of the regions with text, those that agree.
    let with_text = |e: &BlockEntry| {
        inputs
            .regions
            .iter()
            .find(|r| Some(r.crop) == e.region.as_deref())
            .and_then(|r| r.state)
            .is_some_and(|(s, _)| s != CropState::NoText)
            && e.kept != Some(Keep::Failed)
    };
    let counted: Vec<&BlockEntry> = entries.iter().filter(|e| with_text(e)).collect();
    let share = |num: usize, den: usize| (den > 0).then(|| num as f64 / den as f64);
    let agree_regions = counted.iter().filter(|e| e.source == "xiaomi").count();
    let agree_chars: usize = counted
        .iter()
        .filter(|e| e.source == "xiaomi")
        .map(|e| e.loc_chars)
        .sum();
    let all_chars: usize = counted.iter().map(|e| e.loc_chars).sum();
    // A reading that agreed but was empty is not agreement with text: `empty` took it back.
    let blocks_json = Blocks {
        v: BLOCKS_VERSION,
        doc_id: inputs.doc_id.to_owned(),
        engine: inputs.facts.tag.to_owned(),
        regions: read,
        xiaomi,
        loc_by_reason: loc_by_reason.clone(),
        agree_share_regions: share(agree_regions, counted.len()),
        agree_share_chars: share(agree_chars, all_chars),
        blocks: entries,
    };

    let f = &inputs.facts;
    let when = f
        .completed_at
        .and_then(|t| chrono::DateTime::parse_from_rfc3339(t).ok())
        .map(|t| {
            t.with_timezone(&chrono::Utc)
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        });
    let counts = |k: &str| loc_by_reason.get(k).copied().unwrap_or(0).to_string();
    let steps = vec![
        Processing {
            id: PROC_XIAOMI.into(),
            category: "contentGeneration".into(),
            step: usnm_alto::Step {
                date_time: when.clone(),
                settings: vec![
                    ("ocr_engine".into(), f.tag.into()),
                    ("model".into(), f.model.into()),
                    ("request_version".into(), f.prompt_version.to_string()),
                    ("image".into(), f.image.into()),
                    ("llama".into(), f.llama.into()),
                    ("weights".into(), f.weights.into()),
                ],
                software: usnm_alto::Software {
                    creator: "Xiaomi".into(),
                    name: "Xiaomi-OCR-0".into(),
                    version: f.revision.into(),
                },
            },
        },
        Processing {
            id: PROC_LOC.into(),
            category: "contentGeneration".into(),
            step: usnm_alto::Step {
                date_time: None,
                settings: vec![("source".into(), "LoC ALTO".into())],
                software: loc_software(inputs.loc_alto),
            },
        },
        Processing {
            id: PROC_ASSEMBLY.into(),
            category: "postOperation".into(),
            step: usnm_alto::Step {
                date_time: when,
                settings: vec![
                    ("granularity".into(), page.granularity.as_str().into()),
                    ("regions_read".into(), read.to_string()),
                    ("regions_xiaomi".into(), xiaomi.to_string()),
                    ("loc_empty".into(), counts("empty")),
                    ("loc_looped".into(), counts("looped")),
                    ("loc_disagree".into(), counts("disagree")),
                    ("loc_small".into(), counts("small")),
                    ("loc_failed".into(), counts("failed")),
                    (
                        "agree_share_chars".into(),
                        blocks_json
                            .agree_share_chars
                            .map_or_else(|| "none".to_owned(), |s| format!("{s:.4}")),
                    ),
                ],
                software: usnm_alto::Software {
                    creator: "usnewsmap.com".into(),
                    name: "usnm-boinc page assembly".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
            },
        },
    ];
    let text = usnm_alto::text(&page);
    let alto_out = usnm_alto::write_v4(
        &page,
        inputs.file_name,
        &inputs.notes,
        &steps,
        &notes,
        inputs.language,
    );
    Ok(Assembled {
        alto: alto_out,
        blocks: blocks_json,
        text,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALTO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><MeasurementUnit>pixel</MeasurementUnit><OCRProcessing ID="O"><ocrProcessingStep><processingSoftware><softwareCreator>iArchives</softwareCreator><softwareName>ABBYY</softwareName><softwareVersion>9</softwareVersion></processingSoftware></ocrProcessingStep></OCRProcessing></Description><Layout><Page ID="P1" WIDTH="1000" HEIGHT="1400" PHYSICAL_IMG_NR="1"><PrintSpace>
<TextBlock ID="B1" HPOS="0" VPOS="0" WIDTH="1000" HEIGHT="200">
<TextLine ID="B1L1" HPOS="0" VPOS="0" WIDTH="600" HEIGHT="30"><String CONTENT="The" HPOS="0" VPOS="0" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="shlp" HPOS="110" VPOS="0" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="salled" HPOS="220" VPOS="0" WIDTH="150" HEIGHT="30"/></TextLine>
<TextLine ID="B1L2" HPOS="0" VPOS="40" WIDTH="600" HEIGHT="30"><String CONTENT="for" HPOS="0" VPOS="40" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="Englaud" HPOS="110" VPOS="40" WIDTH="200" HEIGHT="30"/></TextLine>
<TextLine ID="B1L3" HPOS="0" VPOS="80" WIDTH="600" HEIGHT="30"><String CONTENT="on" HPOS="0" VPOS="80" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="Tuesday" HPOS="110" VPOS="80" WIDTH="200" HEIGHT="30"/></TextLine>
</TextBlock>
<Illustration ID="I1" HPOS="0" VPOS="300" WIDTH="500" HEIGHT="300"/>
<TextBlock ID="B2" HPOS="0" VPOS="700" WIDTH="1000" HEIGHT="200">
<TextLine ID="B2L1" HPOS="0" VPOS="700" WIDTH="600" HEIGHT="30"><String CONTENT="Eggs" HPOS="0" VPOS="700" WIDTH="100" HEIGHT="30"/><SP/><String CONTENT="twelve" HPOS="110" VPOS="700" WIDTH="150" HEIGHT="30"/></TextLine>
</TextBlock>
<TextBlock ID="B3" HPOS="0" VPOS="900" WIDTH="1000" HEIGHT="200">
<TextLine ID="B3L1" HPOS="0" VPOS="900" WIDTH="600" HEIGHT="30"><String CONTENT="Coal" HPOS="0" VPOS="900" WIDTH="100" HEIGHT="30"/></TextLine>
</TextBlock>
</PrintSpace></Page></Layout></alto>"#;

    fn region(id: &str, block: &str, lines: &[&str]) -> Region {
        Region {
            id: id.into(),
            block: block.into(),
            lines: lines.iter().map(|s| (*s).to_owned()).collect(),
        }
    }

    fn facts() -> Facts<'static> {
        Facts {
            tag: "xiaomi-ocr-0/bbd8cf8d21f2 profile=q8_0.rv5 image=sha256:aaaaaaaaaaaa models=sha256:bbbbbbbbbbbb route=boinc-page@r1 contract=0",
            model: "xiaomi-ocr-0-q8",
            revision: "bbd8cf8d21f2",
            prompt_version: 5,
            image: "sha256:aa",
            llama: "sha256:bb",
            weights: "sha256:cc",
            completed_at: Some("2026-11-20T10:00:00Z"),
        }
    }

    fn run(regions: Vec<RegionIn<'_>>) -> Assembled {
        assemble(&Inputs {
            doc_id: "sn1_1900-01-01_ed-1_seq-1",
            loc_alto: ALTO.as_bytes(),
            dims: (1000, 1400),
            file_name: "https://tile.loc.gov/x",
            language: "eng",
            regions,
            facts: facts(),
            notes: vec![("usnm_job_id".into(), "j".into())],
        })
        .unwrap()
    }

    fn input<'a>(
        r: &'a Region,
        reading: Option<&'a str>,
        state: Option<CropState>,
    ) -> RegionIn<'a> {
        RegionIn {
            region: r,
            crop: &r.id,
            reading,
            state: state.map(|s| (s, Some(0.9))),
        }
    }

    #[test]
    fn agreeing_regions_take_the_reading_and_the_rest_keep_loc_for_a_reason() {
        let r1 = region("r0001", "B1", &["B1L1", "B1L2", "B1L3"]);
        let r2 = region("r0002", "B2", &["B2L1"]);
        let r3 = region("r0003", "B3", &["B3L1"]);
        let got = run(vec![
            input(
                &r1,
                Some("The ship sailed\nfor England\non Tuesday"),
                Some(CropState::Agree),
            ),
            input(&r2, Some("Eggs twelve"), Some(CropState::Disagree)),
            input(&r3, Some("Coal"), Some(CropState::Looped)),
        ]);
        let b = &got.blocks;
        assert_eq!((b.regions, b.xiaomi), (3, 1));
        assert_eq!(b.loc_by_reason["disagree"], 1);
        assert_eq!(b.loc_by_reason["looped"], 1);
        let first = &b.blocks[0];
        assert_eq!(
            (first.id.as_str(), first.source, first.fit),
            ("B1", "xiaomi", Some(Fit::Lines))
        );
        assert_eq!(first.xiaomi_chars, Some(32));
        // Text: the model's words, LoC's elsewhere.
        assert!(got
            .text
            .contains("The ship sailed\nfor England\non Tuesday"));
        assert!(got.text.contains("Eggs twelve") && got.text.contains("Coal"));
        assert!(!got.text.contains("shlp"));
        // Provenance: each block names who made it, and ALTO and sidecar agree.
        assert!(got.alto.contains("ns-v4#"));
        assert!(
            got.alto.contains("<TextBlock ID=\"B1\"")
                && got.alto.contains("PROCESSINGREFS=\"PROC_XIAOMI\"")
        );
        assert_eq!(
            got.alto.matches("PROCESSINGREFS=\"PROC_XIAOMI\"").count(),
            1
        );
        assert_eq!(got.alto.matches("PROCESSINGREFS=\"PROC_LOC\"").count(), 2);
        assert!(got.alto.contains("<softwareName>ABBYY</softwareName>"));
        assert!(got
            .alto
            .contains("fileIdentifierLocation=\"usnm_job_id\">j<"));
        assert!(got.alto.contains("<Illustration ID=\"IL"));
        let parsed =
            usnm_alto::parse(got.alto.as_bytes(), &usnm_alto::ParseOptions::default()).unwrap();
        assert_eq!(parsed.blocks.len(), 4);
        assert!(usnm_ocr_local::checks::check_alto(got.alto.as_bytes()).is_ok());
        // The model has no coordinates: every word has its line's box, no WC, and the page
        // declares line granularity.
        let words: Vec<_> = parsed.blocks[0].lines[0].words.iter().collect();
        assert_eq!(words.len(), 3);
        assert!(words
            .iter()
            .all(|w| w.rect == parsed.blocks[0].lines[0].rect && w.conf.is_none()));
        assert!(got.alto.contains("granularity=line"));
        assert_eq!(parsed.granularity, usnm_alto::Granularity::Line);
        // One processingStepSettings element a step.
        assert_eq!(got.alto.matches("<processingStepSettings>").count(), 3);
        // 2 of 3 regions with text did not agree: the agreeing share is the first's share.
        assert_eq!(b.agree_share_regions.map(|s| (s * 3.0).round()), Some(1.0));
        assert!(b.agree_share_chars.unwrap() > 0.5);
    }

    #[test]
    fn the_empty_guard_comes_before_the_state_and_failures_keep_loc() {
        let r1 = region("r0001", "B1", &["B1L1", "B1L2", "B1L3"]);
        let r2 = region("r0002", "B2", &["B2L1"]);
        let r3 = region("r0003", "B3", &["B3L1"]);
        let got = run(vec![
            // An agreeing pair of empty readings, against text in LoC.
            input(&r1, Some("  \n"), Some(CropState::Agree)),
            // No reading at all, and a reading with no state.
            input(&r2, None, None),
            input(&r3, Some("Coal"), None),
        ]);
        assert_eq!(got.blocks.xiaomi, 0);
        assert_eq!(got.blocks.loc_by_reason["empty"], 1);
        assert_eq!(got.blocks.loc_by_reason["failed"], 2);
        assert!(!got.alto.contains("PROCESSINGREFS=\"PROC_XIAOMI\""));
        assert!(got.text.contains("shlp"));
    }

    #[test]
    fn a_reading_with_other_lines_is_spread_over_loc_s_in_proportion() {
        let r1 = region("r0001", "B1", &["B1L1", "B1L2", "B1L3"]);
        let got = run(vec![input(
            &r1,
            Some("The ship sailed for England on Tuesday"),
            Some(CropState::Agree),
        )]);
        assert_eq!(got.blocks.blocks[0].fit, Some(Fit::Reflow));
        let lines: Vec<&str> = got.text.lines().take(3).collect();
        assert_eq!(lines.join(" "), "The ship sailed for England on Tuesday");
        assert!(lines.iter().all(|l| !l.is_empty()), "{lines:?}");
        // Blocks without a region stay as LoC has them, listed as `small`.
        let small: Vec<_> = got
            .blocks
            .blocks
            .iter()
            .filter(|b| b.region.is_none())
            .collect();
        assert_eq!(small.len(), 2);
        assert!(small.iter().all(|b| b.kept == Some(Keep::Small)));
    }

    #[test]
    fn a_block_cut_into_regions_gets_dotted_ids_and_the_lines_between_stay_loc() {
        let r1 = region("r0001", "B1", &["B1L1"]);
        let r2 = region("r0002", "B1", &["B1L3"]);
        let got = run(vec![
            input(&r1, Some("The ship sailed"), Some(CropState::Agree)),
            input(&r2, Some("on Tuesday"), Some(CropState::Agree)),
        ]);
        let ids: Vec<&str> = got
            .blocks
            .blocks
            .iter()
            .take(3)
            .map(|b| b.id.as_str())
            .collect();
        assert_eq!(ids, ["B1.1", "B1.2", "B1.3"]);
        let sources: Vec<&str> = got.blocks.blocks.iter().take(3).map(|b| b.source).collect();
        assert_eq!(sources, ["xiaomi", "loc", "xiaomi"]);
        assert!(got.alto.contains("ID=\"B1.2\""));
        // The sidecar and the ALTO list the same blocks.
        for e in &got.blocks.blocks {
            assert!(got.alto.contains(&format!("ID=\"{}\"", e.id)), "{}", e.id);
        }
        let lines: Vec<&str> = got.text.lines().filter(|l| !l.is_empty()).take(3).collect();
        assert_eq!(lines, ["The ship sailed", "for Englaud", "on Tuesday"]);
    }

    #[test]
    fn reflow_counts_the_spaces_and_a_replaced_line_loses_the_hyphen_it_inherited() {
        // 20 one-character words over two equal lines split 10 and 10, not 5 and 15.
        let words: Vec<&str> = vec!["a"; 20];
        let lines = reflow(&words, &[10, 10]);
        assert_eq!((lines[0].len(), lines[1].len()), (10, 10));
        let line = Line {
            rect: Rect::new(0.0, 0.0, 100.0, 10.0),
            words: Vec::new(),
            hyphen: Some("-".into()),
        };
        let (out, _) = fitted(&[("L1".into(), line)], "end of the line");
        assert_eq!(out[0].hyphen, None);
    }

    #[test]
    fn a_region_that_is_not_in_the_alto_is_refused() {
        let bad = region("r0001", "B9", &["B9L1"]);
        let err = assemble(&Inputs {
            doc_id: "d",
            loc_alto: ALTO.as_bytes(),
            dims: (1000, 1400),
            file_name: "x",
            language: "eng",
            regions: vec![input(&bad, Some("x"), Some(CropState::Agree))],
            facts: facts(),
            notes: vec![],
        })
        .err()
        .unwrap();
        assert!(format!("{err:#}").contains("B9"));
        let wrong_line = region("r0001", "B1", &["B1L2", "B1L1"]);
        assert!(assemble(&Inputs {
            doc_id: "d",
            loc_alto: ALTO.as_bytes(),
            dims: (1000, 1400),
            file_name: "x",
            language: "eng",
            regions: vec![input(&wrong_line, Some("x"), Some(CropState::Agree))],
            facts: facts(),
            notes: vec![],
        })
        .is_err());
    }
}
