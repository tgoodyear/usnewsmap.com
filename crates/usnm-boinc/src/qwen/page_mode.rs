//! `qwen stage --page-mode` (#1998): page-mode `usnm_qwen` units for Xiaomi-OCR-0 (see
//! `docs/boinc/apps/quality.md`, "Why page mode, and how the pilot was chosen").
//!
//! A unit reads every text region of a page, not the 64-crop sample [`super::stage`] cuts.
//!
//! - **Regions** come from LoC's ALTO in the staged `usnm_quality` page (`loc.xml`, beside
//!   `page.jpg`), so the stager never asks LoC for anything. A region is one `TextBlock`, or,
//!   when its LoC text is over a character budget or its box over a pixel cap, a run of whole
//!   consecutive `TextLine`s of one. Its image is the union of its lines' boxes with the
//!   padding the line path uses ([`crate::lines::PAD_X`]). Illustrations and rules are not
//!   read, and nor is a block with fewer than a minimum of LoC characters: both stay as LoC
//!   has them.
//! - **Ids.** The crop of a region is `r0001`, `r0002`, ... in LoC's reading order (crop ids
//!   are lower-case names); [`Region::block`] and [`Region::lines`] are LoC's own `ID`s, so a
//!   reading can be put back in LoC's ALTO. A page with a block or line without an `ID`, or
//!   with the same block `ID` twice, is not staged.
//! - **Time.** The estimate of a page is its LoC characters over a class's measured characters
//!   a second: the fastest admitted class's rate decides whether the page fills a unit
//!   ([`Params::min_unit_secs`]), the slowest's sets the page's deadline. Pages that would
//!   not fill a unit are packed, up to [`Params::pack_pages`] of them, into the bundle format
//!   with regions in place of crops; a page whose deadline would pass `MAX_DEADLINE_SECS` is
//!   not staged.
//! - **Provenance.** The campaign id is the job's `group`, and every page unit and bundle
//!   carries a `page-mode.json` with the campaign, the digests of the page, the ALTO and the
//!   quality unit it was cut from, the parameters, the counts, the estimate and, when the
//!   pilot selection names them, the page's stratum.
use super::stage::{
    bundle_json_len, bundle_lock, check_retained_source, longest_readings_len, publish,
    retain_source, same_settings, write_bundle_with, TOO_LARGE,
};
use super::{check_no_hold, units, JOB_FILE, PAGE_FILE};
use crate::{atomic, read};
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
};
use usnm_ocr_contract::{
    boinc::sha256,
    quality::{
        Job as QualityJob, MAX_ALTO, MAX_BUNDLE_SCAN, MAX_JOB_JSON as MAX_QUALITY_JOB, MAX_SCAN,
    },
    qwen::{
        Crop, Engine, Job, PageMode, Region, MAX_CROP_TEXT, MAX_DEADLINE_SECS, MAX_JOB_JSON,
        MAX_PAGE_REGIONS, MAX_READINGS, PROMPT_VERSION_XIAOMI, VERSION,
    },
};

/// The sidecar of a page unit and of a bundle: [`PageProvenance`] or [`BundleProvenance`].
pub const PROVENANCE_FILE: &str = "page-mode.json";
/// The most lines the contract lets a region list (`Job::validate`).
pub const MAX_REGION_LINES: usize = 1024;
/// The marker on a page with no region to read.
pub const NO_REGIONS: &str = "no_regions";
/// The marker on a page whose deadline would pass `MAX_DEADLINE_SECS`.
pub const OVER_DEADLINE: &str = "over_deadline";
/// The marker on a page whose ALTO has no usable ids, or too many regions.
pub const UNSTAGEABLE: &str = "unstageable";

/// A region's crop id: `r` and the ordinal, four digits.
fn crop_id(ordinal: usize) -> String {
    format!("r{ordinal:04}")
}

/// The knobs of the cut and the estimate. The defaults are the design's proposals; the rates
/// and the pixel cap wait for the calibration set (#1997, "Measurement plan for slots").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Params {
    /// The most LoC characters in a region (a block over it is split at line boundaries).
    pub region_chars: usize,
    /// A block with fewer LoC characters (not counting white space) than this is not read.
    pub min_chars: usize,
    /// The longest side, in page pixels, of a region's padded box.
    pub max_side_px: u32,
    /// Characters a second of the fastest admitted class at its usual slot count.
    pub fastest_cps: f64,
    /// The same for the slowest admitted class.
    pub slowest_cps: f64,
    /// A unit should take at least this long on the fastest class.
    pub min_unit_secs: u32,
    /// The most pages in a bundle.
    pub pack_pages: usize,
}
impl Default for Params {
    fn default() -> Self {
        Self {
            region_chars: 1500,
            min_chars: 3,
            // Not measured yet: set from the first units' `gpu_mem_peak_mib`.
            max_side_px: 2048,
            // About 22 characters a second with one request in flight, the measured figure.
            fastest_cps: 22.0,
            slowest_cps: 10.0,
            min_unit_secs: 300,
            pack_pages: MAX_PACK_PAGES,
        }
    }
}

/// The most pages one page-mode bundle holds (#1997, "Packing").
pub const MAX_PACK_PAGES: usize = 4;
/// A page's deadline is its estimate on the slowest class times this, plus [`LOAD_ALLOWANCE_SECS`].
pub const DEADLINE_MARGIN: f64 = 1.5;
/// Seconds allowed for starting llama-server and loading the model.
pub const LOAD_ALLOWANCE_SECS: f64 = 300.0;

impl Params {
    pub fn check(&self) -> anyhow::Result<()> {
        ensure!(
            (1..=4000).contains(&self.region_chars),
            "--region-chars must be 1 to 4000"
        );
        ensure!(
            (1..=1 << 16).contains(&self.max_side_px),
            "--max-side-px must be 1 to 65536"
        );
        for (rate, name) in [
            (self.fastest_cps, "--fastest-cps"),
            (self.slowest_cps, "--slowest-cps"),
        ] {
            ensure!(rate.is_finite() && rate > 0.0, "{name} must be above 0");
        }
        ensure!(
            self.slowest_cps <= self.fastest_cps,
            "--slowest-cps is above --fastest-cps"
        );
        ensure!(
            (1..=MAX_PACK_PAGES).contains(&self.pack_pages),
            "--pack-pages must be 1 to {MAX_PACK_PAGES}"
        );
        Ok(())
    }

    /// Seconds the fastest class needs for `chars` LoC characters.
    pub fn fast_secs(&self, chars: usize) -> u32 {
        (chars as f64 / self.fastest_cps).ceil() as u32
    }

    /// The deadline of a page with `chars` LoC characters: the slowest class's time, with a
    /// margin, and the load allowance.
    pub fn deadline_secs(&self, chars: usize) -> u64 {
        ((chars as f64 / self.slowest_cps) * DEADLINE_MARGIN + LOAD_ALLOWANCE_SECS).ceil() as u64
    }
}

#[derive(Debug, Clone)]
pub struct Options {
    /// The `usnm_quality` staging root whose pages to cut regions from.
    pub quality_root: PathBuf,
    pub root: PathBuf,
    pub engine: Engine,
    /// The campaign id (`x08-eng-pilot`): the units' `group`, and in every provenance file.
    pub campaign: String,
    pub params: Params,
    pub max_tokens_per_1000px2: u32,
    pub max_pages: usize,
    /// The pilot selection's strata, by doc id, recorded in each page's provenance.
    pub strata: Option<PathBuf>,
    /// Publish a bundle that could not fill a unit, instead of holding it for the next run.
    pub flush_short: bool,
}

/// Where a page of the pilot sits in the selection (`pilot_select.py`'s `pilot.csv`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stratum {
    pub decade: i32,
    pub band: String,
    pub tess_read: bool,
    #[serde(default)]
    pub wf1415: bool,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    /// Pages staged, alone or for a bundle.
    pub staged: usize,
    /// Units written for one page that fills a unit by itself.
    pub single: usize,
    pub bundles: usize,
    /// Pages in a bundle.
    pub bundled: usize,
    /// Bundles written with the fastest class's estimate under the unit floor.
    pub short_bundles: usize,
    /// Small pages waiting for others to fill a unit.
    pub held: usize,
    pub already: usize,
    pub no_regions: usize,
    pub over_deadline: usize,
    pub unstageable: usize,
    pub too_large: usize,
    pub regions: usize,
}

/// What the cut left out of a page and what it split.
#[derive(Debug, Default, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Stats {
    /// Text blocks in the ALTO.
    pub text_blocks: usize,
    /// Illustrations and rules, left as LoC has them.
    pub non_text_blocks: usize,
    /// Text blocks under the minimum, left as LoC has them.
    pub small_blocks: usize,
    /// Text blocks (or runs) with no line that has an area, or no letters at all.
    pub empty_blocks: usize,
    /// Blocks cut into more than one region.
    pub split_blocks: usize,
    /// Lines wider or taller than the pixel cap, kept whole in a region of their own.
    pub oversized_lines: usize,
    pub regions: usize,
    /// LoC characters in the regions, white space not counted.
    pub chars: usize,
}

/// One region as planned: its crop and its LoC ids.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub crop: Crop,
    pub region: Region,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PagePlan {
    pub regions: Vec<Planned>,
    pub stats: Stats,
}

/// A block of the ALTO as its ids see it, in the order [`usnm_alto::parse`] lists blocks.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ScanBlock {
    pub(crate) text: bool,
    pub(crate) id: Option<String>,
    pub(crate) lines: Vec<Option<String>>,
}

/// The `ID`s of the ALTO's blocks and lines, in the order `usnm_alto::parse` lists them. The
/// parser keeps no ids, so this follows its rules for what a block and a line are: a
/// `TextBlock` ends the open one, a `TextLine` counts when it has an end tag inside a block,
/// and an `Illustration` or `GraphicalElement` is a block of its own.
pub(crate) fn scan_ids(xml: &[u8]) -> anyhow::Result<Vec<ScanBlock>> {
    use quick_xml::events::{BytesStart, Event};
    fn id(e: &BytesStart<'_>) -> anyhow::Result<Option<String>> {
        for a in e.attributes().with_checks(true) {
            let a = a.map_err(|e| anyhow::anyhow!("ALTO attribute: {e}"))?;
            if a.key.as_ref() == "ID" {
                let v = a
                    .normalized_value(quick_xml::XmlVersion::Implicit1_0)
                    .map_err(|e| anyhow::anyhow!("ALTO attribute: {e}"))?;
                return Ok(Some(v.into_owned()));
            }
        }
        Ok(None)
    }
    // Only elements in the document's own ALTO namespace count, as in the parser: ALTO allows
    // foreign XML (under `Tags/XmlData`) with elements of the same names.
    let version = usnm_alto::check(xml, &usnm_alto::ParseOptions::default().limits)
        .map_err(|e| anyhow::anyhow!("ALTO: {e}"))?
        .version;
    let ns_uri = usnm_alto::NAMESPACES
        .iter()
        .find(|(_, v)| *v == version)
        .map(|(n, _)| *n)
        .context("ALTO is in no namespace this reader knows")?;
    let text = std::str::from_utf8(xml).context("ALTO is not UTF-8")?;
    let mut reader = quick_xml::NsReader::from_str(text);
    let mut out: Vec<ScanBlock> = Vec::new();
    let mut block: Option<ScanBlock> = None;
    let mut line: Option<Option<String>> = None;
    loop {
        let (ns, event) = reader
            .read_resolved_event()
            .map_err(|e| anyhow::anyhow!("ALTO: {e}"))?;
        let empty = matches!(event, Event::Empty(_));
        let ours = matches!(&ns, quick_xml::name::ResolveResult::Bound(quick_xml::name::Namespace(u)) if *u == ns_uri);
        match &event {
            Event::Start(e) | Event::Empty(e) if ours => match e.local_name().as_ref() {
                "TextBlock" => {
                    let ob = ScanBlock {
                        text: true,
                        id: id(e)?,
                        lines: Vec::new(),
                    };
                    out.extend(block.take());
                    line = None;
                    if empty {
                        out.push(ob);
                    } else {
                        block = Some(ob);
                    }
                }
                "TextLine" if block.is_some() => {
                    if !empty {
                        line = Some(id(e)?);
                    }
                }
                "Illustration" | "GraphicalElement" => out.push(ScanBlock {
                    text: false,
                    id: None,
                    lines: Vec::new(),
                }),
                _ => {}
            },
            Event::End(e) if ours => match e.local_name().as_ref() {
                "TextLine" => {
                    if let (Some(l), Some(b)) = (line.take(), block.as_mut()) {
                        b.lines.push(l);
                    }
                }
                "TextBlock" => out.extend(block.take()),
                _ => {}
            },
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(out)
}

/// A LoC `ID` as the contract takes it (`Region::block`, `Region::lines`): ASCII letters,
/// digits and `_ - .`, 1 to 64 bytes. Checked here so a page with another fails alone, as
/// `unstageable`, instead of stopping the run at job validation.
fn check_loc_id(s: &str, what: &str) -> anyhow::Result<()> {
    ensure!(
        !s.is_empty()
            && s.len() <= 64
            && s.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"_-.".contains(&b)),
        "the {what} ID {s:?} is not 1 to 64 of letters, digits and _ - ."
    );
    Ok(())
}

/// A line's LoC text: its non-empty words, and its hyphen at the end.
pub(crate) fn line_text(line: &usnm_alto::Line) -> String {
    let mut s = line
        .words
        .iter()
        .map(|w| w.text.as_str())
        .filter(|w| !w.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if let Some(h) = &line.hyphen {
        s.push_str(h);
    }
    s
}

pub(crate) fn letters(text: &str) -> usize {
    text.chars().filter(|c| !c.is_whitespace()).count()
}

/// A line's box in page pixels, rounded, if it has an area.
fn px_rect(r: &usnm_alto::Rect, (w, h): (u32, u32)) -> Option<[u32; 4]> {
    let px = |v: f64, hi: u32| (v.round().max(0.0) as u32).min(hi);
    let rect = [px(r.x0, w), px(r.y0, h), px(r.x1, w), px(r.y1, h)];
    (rect[0] < rect[2] && rect[1] < rect[3]).then_some(rect)
}

fn union(a: [u32; 4], b: [u32; 4]) -> [u32; 4] {
    [
        a[0].min(b[0]),
        a[1].min(b[1]),
        a[2].max(b[2]),
        a[3].max(b[3]),
    ]
}

/// The padded box of `rect`, as the line path pads a line.
fn padded(rect: [u32; 4], dims: (u32, u32)) -> Option<[u32; 4]> {
    crate::lines::padded(rect, dims.0, dims.1).map(|(x0, y0, x1, y1)| [x0, y0, x1, y1])
}

/// One line of a block as the cut sees it.
struct CutLine {
    id: String,
    text: String,
    chars: usize,
    rect: Option<[u32; 4]>,
}

/// Plans the regions of a page of `dims` pixels from LoC's `alto`, in LoC's reading order.
pub fn plan(alto: &[u8], dims: (u32, u32), params: &Params) -> anyhow::Result<PagePlan> {
    params.check()?;
    let alto = usnm_ocr_quality::slot::normalize_alto_ns(alto);
    let page = crate::quality::parse_alto(&alto, f64::from(dims.0), f64::from(dims.1))?;
    let scanned = scan_ids(&alto)?;
    ensure!(
        scanned.len() == page.blocks.len()
            && scanned.iter().zip(&page.blocks).all(|(s, b)| {
                s.text == matches!(b.kind, usnm_alto::BlockKind::Text)
                    && s.lines.len() == b.lines.len()
            }),
        "the ALTO's ids do not line up with its blocks"
    );
    let mut seen = BTreeSet::new();
    let mut seen_lines = BTreeSet::new();
    let mut stats = Stats::default();
    let mut regions: Vec<Planned> = Vec::new();
    for (scan, block) in scanned.iter().zip(&page.blocks) {
        if !scan.text {
            stats.non_text_blocks += 1;
            continue;
        }
        stats.text_blocks += 1;
        let block_id = scan.id.as_deref().context("a TextBlock has no ID")?;
        check_loc_id(block_id, "TextBlock")?;
        ensure!(
            seen.insert(block_id.to_owned()),
            "the TextBlock ID {block_id} is used twice"
        );
        let mut lines = Vec::with_capacity(block.lines.len());
        for (sl, line) in scan.lines.iter().zip(&block.lines) {
            let text = line_text(line);
            let line_id = sl
                .clone()
                .with_context(|| format!("a line of {block_id} has no ID"))?;
            check_loc_id(&line_id, "TextLine")?;
            ensure!(
                seen_lines.insert(line_id.clone()),
                "the TextLine ID {line_id} is used twice"
            );
            lines.push(CutLine {
                id: line_id,
                chars: text.chars().count(),
                text,
                rect: px_rect(&line.rect, dims),
            });
        }
        let block_letters: usize = lines.iter().map(|l| letters(&l.text)).sum();
        if block_letters < params.min_chars {
            stats.small_blocks += 1;
            continue;
        }
        let runs = split(&lines, dims, params, &mut stats);
        let parts = runs.len();
        let mut kept = 0usize;
        for run in runs {
            let Some(rect) = run
                .iter()
                .filter_map(|&i| lines[i].rect)
                .reduce(union)
                .and_then(|r| padded(r, dims))
            else {
                stats.empty_blocks += 1;
                continue;
            };
            let text = run
                .iter()
                .map(|&i| lines[i].text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
            let run_letters = letters(&text);
            if run_letters == 0 {
                stats.empty_blocks += 1;
                continue;
            }
            kept += 1;
            stats.chars += run_letters;
            let id = crop_id(regions.len() + 1);
            regions.push(Planned {
                crop: Crop {
                    rect,
                    id: id.clone(),
                    loc_text: text,
                    stratum: "region".into(),
                },
                region: Region {
                    id,
                    block: block_id.to_owned(),
                    lines: run.iter().map(|&i| lines[i].id.clone()).collect(),
                },
            });
        }
        if parts > 1 && kept > 0 {
            stats.split_blocks += 1;
        }
    }
    stats.regions = regions.len();
    Ok(PagePlan { regions, stats })
}

/// A block's lines cut into runs of consecutive lines, each within the character budget, the
/// pixel cap and the contract's lines to a region ([`MAX_REGION_LINES`]). A line over either on its own is a run of its own.
fn split(
    lines: &[CutLine],
    dims: (u32, u32),
    params: &Params,
    stats: &mut Stats,
) -> Vec<Vec<usize>> {
    let side = |r: [u32; 4]| padded(r, dims).map(|p| (p[2] - p[0]).max(p[3] - p[1]));
    let mut runs: Vec<Vec<usize>> = Vec::new();
    let mut run: Vec<usize> = Vec::new();
    let mut chars = 0usize;
    let mut rect: Option<[u32; 4]> = None;
    for (i, line) in lines.iter().enumerate() {
        if line
            .rect
            .and_then(side)
            .is_some_and(|s| s > params.max_side_px)
        {
            stats.oversized_lines += 1;
        }
        let joined = match (rect, line.rect) {
            (Some(a), Some(b)) => Some(union(a, b)),
            (a, b) => a.or(b),
        };
        let fits = run.is_empty()
            || (run.len() < MAX_REGION_LINES
                && chars + line.chars < params.region_chars
                && joined
                    .and_then(side)
                    .is_none_or(|s| s <= params.max_side_px));
        if !fits {
            runs.push(std::mem::take(&mut run));
            chars = 0;
            rect = None;
        }
        chars += line.chars + usize::from(!run.is_empty());
        rect = match (rect, line.rect) {
            (Some(a), Some(b)) => Some(union(a, b)),
            (a, b) => a.or(b),
        };
        run.push(i);
    }
    if !run.is_empty() {
        runs.push(run);
    }
    runs
}

/// What a page unit and a bundle say about where their regions came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageProvenance {
    pub schema: u8,
    pub campaign: String,
    pub doc_id: String,
    /// The `usnm_quality` unit's `quality.json`, `loc.xml` and `page.jpg` the regions were cut
    /// from, by checksum.
    pub quality_sha256: String,
    pub loc_alto_sha256: String,
    pub page_sha256: String,
    pub page_width: u32,
    pub page_height: u32,
    pub params: Params,
    pub stats: Stats,
    /// Seconds the fastest class needs, and the deadline the page has.
    pub est_fast_secs: u32,
    pub deadline_secs: u32,
    pub stratum: Option<Stratum>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BundleProvenance {
    pub schema: u8,
    pub campaign: String,
    pub est_fast_secs: u32,
    pub min_unit_secs: u32,
    pub pages: Vec<PageProvenance>,
}

const SCHEMA: u8 = 1;

/// Every page-mode marker a staged page can carry.
fn markers() -> [&'static str; 5] {
    [JOB_FILE, NO_REGIONS, OVER_DEADLINE, UNSTAGEABLE, TOO_LARGE]
}

fn is_staged(dir: &Path) -> bool {
    markers().iter().any(|m| dir.join(m).exists())
}

fn load_strata(path: Option<&Path>) -> anyhow::Result<BTreeMap<String, Stratum>> {
    match path {
        Some(p) => Ok(serde_json::from_slice(&read(p, 64 * 1024 * 1024)?)
            .with_context(|| format!("{}: strata", p.display()))?),
        None => Ok(BTreeMap::new()),
    }
}

/// Stages the pages of `quality_root` (those named by `members`, in its order, when given)
/// as page-mode units, then packs the small ones. Idempotent: a page staged is skipped, and a
/// small page already waiting is packed when others arrive.
pub fn run(opts: &Options, members: Option<&Path>) -> anyhow::Result<Summary> {
    opts.params.check()?;
    super::check_digest(&opts.engine.image, "--engine-image")?;
    super::check_digest(&opts.engine.llama, "--engine-llama")?;
    super::check_digest(&opts.engine.weights, "--engine-weights")?;
    usnm_ocr_contract::quality::check_name(&opts.campaign, "campaign")?;
    ensure!(
        opts.engine.models.len() == 1
            && usnm_ocr_contract::qwen::is_xiaomi_model(&opts.engine.models[0].name),
        "page mode reads with Xiaomi-OCR-0 alone"
    );
    check_no_hold(&opts.root)?;
    let units_dir = units(&opts.root);
    let pages_dir = opts.root.join("pages");
    std::fs::create_dir_all(&units_dir)?;
    std::fs::create_dir_all(&pages_dir)?;
    // One staging run at a time, from staging a page to publishing the last bundle.
    let _held = crate::Lock::take(&bundle_lock(&units_dir)?)
        .context("another staging run is using this root")?;
    bind_campaign(&opts.root, &opts.campaign)?;
    let strata = load_strata(opts.strata.as_deref())?;
    let src = {
        let p = opts.quality_root.join("pages");
        if p.exists() {
            p
        } else {
            crate::quality::units(&opts.quality_root)
        }
    };
    let mut sources: Vec<PathBuf> = match members {
        Some(m) => {
            let docs = crate::selection::members(m)?;
            ensure!(
                docs.len() <= opts.max_pages,
                "membership exceeds --max-pages"
            );
            docs.into_iter()
                .map(|doc| {
                    let mut page = src.join(&doc);
                    if !page.join(crate::quality::JOB_FILE).is_file() {
                        page = crate::quality::units(&opts.quality_root).join(&doc);
                    }
                    ensure!(
                        page.join(crate::quality::JOB_FILE).is_file(),
                        "{doc}: missing selected quality job"
                    );
                    Ok(page)
                })
                .collect::<anyhow::Result<_>>()?
        }
        None => std::fs::read_dir(&src)
            .with_context(|| format!("reading {}", src.display()))?
            .filter_map(|e| e.ok())
            .map(|e| e.path())
            .filter(|p| p.join(crate::quality::JOB_FILE).exists())
            .collect(),
    };
    if members.is_none() {
        sources.sort();
    }
    let mut summary = Summary::default();
    let mut done = 0usize;
    for page in sources {
        if done >= opts.max_pages {
            break;
        }
        let name = page
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let single = units_dir.join(&name);
        let waiting = pages_dir.join(&name);
        let existing = [&single, &waiting].into_iter().find(|d| is_staged(d));
        if let Some(existing) = existing {
            check_retained_source(&opts.quality_root, &page, existing)?;
            check_same_campaign(existing, &opts.campaign)?;
            summary.already += 1;
            continue;
        }
        done += 1;
        stage_page(opts, &page, &name, &strata, &mut summary)?;
    }
    pack(opts, &mut summary)?;
    Ok(summary)
}

/// The root's campaign, written by the first run: a root belongs to one campaign, whatever pages
/// a later run names.
pub const CAMPAIGN_FILE: &str = "page-mode-campaign.json";

fn bind_campaign(root: &Path, campaign: &str) -> anyhow::Result<()> {
    let path = root.join(CAMPAIGN_FILE);
    let bound = serde_json::json!({ "schema": SCHEMA, "campaign": campaign });
    if path.is_file() {
        let have: serde_json::Value = serde_json::from_slice(&read(&path, 4096)?)
            .with_context(|| format!("{}", path.display()))?;
        ensure!(
            have == bound,
            "{}: this root is for campaign {}, not {campaign}; stage each campaign into its own --root",
            root.display(),
            have["campaign"]
        );
        return Ok(());
    }
    // A root first staged with the sample path holds units this run would call staged.
    for dir in ["units", "pages"] {
        let d = root.join(dir);
        ensure!(
            !d.is_dir() || std::fs::read_dir(&d)?.next().is_none(),
            "{}: already holds units staged without --page-mode; stage page mode into its own --root",
            root.display()
        );
    }
    atomic(&path, &serde_json::to_vec_pretty(&bound)?)
}

/// Refuses a page staged for another campaign: a root belongs to one.
fn check_same_campaign(unit: &Path, campaign: &str) -> anyhow::Result<()> {
    if let Ok(bytes) = read(&unit.join(JOB_FILE), MAX_JOB_JSON) {
        let page_mode = serde_json::from_slice::<Job>(&bytes).is_ok_and(|j| j.page_mode.is_some());
        ensure!(
            page_mode,
            "{}: staged without --page-mode (a sample unit); stage page mode into its own --root",
            unit.display()
        );
    }
    let path = unit.join(PROVENANCE_FILE);
    if !path.is_file() {
        return Ok(());
    }
    let p: PageProvenance = serde_json::from_slice(&read(&path, 1 << 20)?)
        .with_context(|| format!("{}", path.display()))?;
    ensure!(
        p.campaign == campaign,
        "{}: staged for campaign {}, not {campaign}; stage each campaign into its own --root",
        unit.display(),
        p.campaign
    );
    Ok(())
}

fn stage_page(
    opts: &Options,
    page: &Path,
    name: &str,
    strata: &BTreeMap<String, Stratum>,
    summary: &mut Summary,
) -> anyhow::Result<()> {
    let quality_bytes = read(&page.join(crate::quality::JOB_FILE), MAX_QUALITY_JOB)?;
    let quality: QualityJob =
        serde_json::from_slice(&quality_bytes).with_context(|| format!("{name}: quality.json"))?;
    quality.validate()?;
    crate::quality::page_dims::require_image(page, "qwen stage --page-mode")?;
    let bytes = read(&page.join(crate::quality::PAGE_FILE), MAX_SCAN)?;
    ensure!(
        quality.inputs.page.matches(&bytes),
        "{name}: staged page changed"
    );
    let alto = read(&page.join(crate::quality::ALTO_FILE), MAX_ALTO)?;
    ensure!(
        quality.inputs.alto.matches(&alto),
        "{name}: staged ALTO changed"
    );
    let dims = crate::lines::scan_size(&bytes).with_context(|| format!("{name}: page size"))?;
    let params = &opts.params;
    // Where a page that does not stage is marked, and which marker.
    let mark = |dir: &Path, marker: &str, why: String| -> anyhow::Result<()> {
        std::fs::create_dir_all(dir)?;
        check_retained_source(&opts.quality_root, page, dir)?;
        retain_source(&opts.quality_root, dir, &quality, &quality_bytes)?;
        eprintln!("{name}: {why}; marked {marker}");
        atomic(&dir.join(marker), format!("{why}\n").as_bytes())
    };
    let waiting = opts.root.join("pages").join(name);
    let plan = match plan(&alto, dims, params) {
        Ok(p) => p,
        Err(e) => {
            mark(&waiting, UNSTAGEABLE, format!("{e:#}"))?;
            summary.unstageable += 1;
            return Ok(());
        }
    };
    if plan.regions.is_empty() {
        mark(
            &waiting,
            NO_REGIONS,
            "the ALTO has no text region to read".into(),
        )?;
        summary.no_regions += 1;
        return Ok(());
    }
    if plan.regions.len() > MAX_PAGE_REGIONS {
        mark(
            &waiting,
            UNSTAGEABLE,
            format!(
                "{} regions, over the {MAX_PAGE_REGIONS} a unit may have",
                plan.regions.len()
            ),
        )?;
        summary.unstageable += 1;
        return Ok(());
    }
    if let Some(big) = plan
        .regions
        .iter()
        .find(|p| p.crop.loc_text.len() > MAX_CROP_TEXT)
    {
        // A line over the limit on its own cannot be split; the page is kept out like a
        // sample page whose crop text is too long.
        mark(
            &waiting,
            TOO_LARGE,
            format!(
                "region {} has {} bytes of LoC text, over the {MAX_CROP_TEXT}-byte limit",
                big.crop.id,
                big.crop.loc_text.len()
            ),
        )?;
        summary.too_large += 1;
        return Ok(());
    }
    let chars = plan.stats.chars;
    let deadline = params.deadline_secs(chars);
    if deadline > u64::from(MAX_DEADLINE_SECS) {
        mark(
            &waiting,
            OVER_DEADLINE,
            format!("{chars} characters would need {deadline} s, over {MAX_DEADLINE_SECS} s"),
        )?;
        summary.over_deadline += 1;
        return Ok(());
    }
    let deadline = u32::try_from(deadline).context("deadline")?;
    let est = params.fast_secs(chars);
    let alone = est >= params.min_unit_secs;
    let unit = if alone {
        super::units(&opts.root).join(name)
    } else {
        waiting
    };
    let (crops, regions): (Vec<Crop>, Vec<Region>) = plan
        .regions
        .iter()
        .map(|p| (p.crop.clone(), p.region.clone()))
        .unzip();
    let mut job = Job {
        v: VERSION,
        id: String::new(),
        cpu_timeout_secs: deadline,
        crops,
        doc_id: quality.doc_id.clone(),
        engine: opts.engine.clone(),
        gpu_timeout_secs: deadline,
        group: opts.campaign.clone(),
        input: quality.inputs.page.clone(),
        language: quality.language.clone(),
        max_tokens_per_1000px2: opts.max_tokens_per_1000px2,
        pages: vec![],
        prompt_version: PROMPT_VERSION_XIAOMI,
        model: Some(opts.engine.models[0].name.clone()),
        page_mode: Some(PageMode {
            regions,
            slots: None,
        }),
    };
    job.id = job.identity()?;
    job.validate()
        .with_context(|| format!("{name}: staged page-mode job"))?;
    let provenance = PageProvenance {
        schema: SCHEMA,
        campaign: opts.campaign.clone(),
        doc_id: quality.doc_id.clone(),
        quality_sha256: sha256(&quality_bytes),
        loc_alto_sha256: quality.inputs.alto.sha256.clone(),
        page_sha256: quality.inputs.page.sha256.clone(),
        page_width: dims.0,
        page_height: dims.1,
        params: params.clone(),
        stats: plan.stats.clone(),
        est_fast_secs: est,
        deadline_secs: deadline,
        stratum: strata.get(&quality.doc_id).cloned(),
    };
    std::fs::create_dir_all(&unit)?;
    check_retained_source(&opts.quality_root, page, &unit)?;
    retain_source(&opts.quality_root, &unit, &quality, &quality_bytes)?;
    atomic(&unit.join(PAGE_FILE), &bytes)?;
    atomic(
        &unit.join(PROVENANCE_FILE),
        &serde_json::to_vec_pretty(&provenance)?,
    )?;
    if !publish(&unit, &job)? {
        summary.too_large += 1;
        return Ok(());
    }
    summary.staged += 1;
    summary.regions += job.crops.len();
    if alone {
        summary.single += 1;
    }
    Ok(())
}

/// One small page waiting to be packed.
struct Waiting {
    dir: PathBuf,
    job: Job,
    provenance: PageProvenance,
}

/// Packs the waiting pages (`pages/<doc>` with a job and no bundle yet) into bundles, in doc
/// order: a bundle closes once the fastest class would need `min_unit_secs` for it, or at
/// `pack_pages` pages, or when the next page would not fit one unit's limits. A last group
/// that could not fill a unit waits for the next run, unless `flush_short`.
fn pack(opts: &Options, summary: &mut Summary) -> anyhow::Result<()> {
    let units_dir = units(&opts.root);
    let pages_dir = opts.root.join("pages");
    let mut bundled = BTreeSet::new();
    let mut existing = 0usize;
    for entry in std::fs::read_dir(&units_dir)?.filter_map(|e| e.ok()) {
        let p = entry.path();
        let is_bundle = p
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("bundle-"));
        if !is_bundle {
            continue;
        }
        existing += 1;
        if let Ok(bytes) = read(&p.join(JOB_FILE), MAX_JOB_JSON) {
            if let Ok(job) = serde_json::from_slice::<Job>(&bytes) {
                bundled.extend(job.pages.iter().map(|bp| bp.doc_id.clone()));
            }
        }
    }
    let mut waiting: Vec<Waiting> = Vec::new();
    for entry in std::fs::read_dir(&pages_dir)?.filter_map(|e| e.ok()) {
        let dir = entry.path();
        if !dir.join(JOB_FILE).is_file() || dir.join(TOO_LARGE).exists() {
            continue;
        }
        let job: Job = serde_json::from_slice(&read(&dir.join(JOB_FILE), MAX_JOB_JSON)?)
            .with_context(|| format!("reading {}/qwen.json", dir.display()))?;
        if bundled.contains(&job.doc_id) || job.page_mode.is_none() {
            continue;
        }
        let provenance: PageProvenance =
            serde_json::from_slice(&read(&dir.join(PROVENANCE_FILE), 1 << 20)?)
                .with_context(|| format!("{}: {PROVENANCE_FILE}", dir.display()))?;
        ensure!(
            provenance.campaign == opts.campaign,
            "{}: staged for campaign {}, not {}",
            dir.display(),
            provenance.campaign,
            opts.campaign
        );
        waiting.push(Waiting {
            dir,
            job,
            provenance,
        });
    }
    waiting.sort_by(|a, b| a.job.doc_id.cmp(&b.job.doc_id));
    let mut group: Vec<Waiting> = Vec::new();
    let mut created = 0usize;
    let mut write = |group: &mut Vec<Waiting>, summary: &mut Summary| -> anyhow::Result<()> {
        let est: u32 = group.iter().map(|w| w.provenance.est_fast_secs).sum();
        created += 1;
        let first = group[0].job.doc_id.clone();
        let dir = units_dir.join(format!("bundle-{:05}-{first}", existing + created));
        let provenance = BundleProvenance {
            schema: SCHEMA,
            campaign: opts.campaign.clone(),
            est_fast_secs: est,
            min_unit_secs: opts.params.min_unit_secs,
            pages: group.iter().map(|w| w.provenance.clone()).collect(),
        };
        let dirs: Vec<PathBuf> = group.iter().map(|w| w.dir.clone()).collect();
        write_bundle_with(
            &dir,
            &dirs,
            &[(PROVENANCE_FILE, serde_json::to_vec_pretty(&provenance)?)],
        )?;
        summary.bundles += 1;
        summary.bundled += group.len();
        if est < opts.params.min_unit_secs {
            summary.short_bundles += 1;
        }
        group.clear();
        Ok(())
    };
    for w in waiting {
        if !fits(&[], &w, &opts.params)? {
            // Alone in a bundle the page is over a limit (its crops are listed twice there):
            // keep it out, as a sample page whose bundle would be too large is.
            let why = "a one-page bundle would pass the qwen.json, readings.json or deadline limit";
            eprintln!("{}: {why}; marked {TOO_LARGE}", w.job.doc_id);
            atomic(&w.dir.join(TOO_LARGE), format!("{why}\n").as_bytes())?;
            summary.too_large += 1;
            continue;
        }
        if !group.is_empty() && !fits(&group, &w, &opts.params)? {
            // The group cannot take another page: it is as full as it gets.
            write(&mut group, summary)?;
        }
        group.push(w);
        let est: u32 = group.iter().map(|w| w.provenance.est_fast_secs).sum();
        if est >= opts.params.min_unit_secs || group.len() >= opts.params.pack_pages {
            write(&mut group, summary)?;
        }
    }
    if !group.is_empty() {
        if opts.flush_short {
            write(&mut group, summary)?;
        } else {
            summary.held += group.len();
        }
    }
    Ok(())
}

/// Whether `next` can join `group` in one unit: pages, regions, scan bytes, `qwen.json`, the
/// longest `readings.json` and the summed deadlines (a bundle's is its pages' summed, and may
/// not pass `MAX_DEADLINE_SECS`) within the limits the workers read them with.
fn fits(group: &[Waiting], next: &Waiting, params: &Params) -> anyhow::Result<bool> {
    let mut jobs: Vec<Job> = group.iter().map(|w| w.job.clone()).collect();
    jobs.push(next.job.clone());
    if jobs.len() > params.pack_pages {
        return Ok(false);
    }
    // A bundle runs with one engine, token allowance and request version: pages staged by
    // runs with other settings are not packed together.
    if !same_settings(&jobs[0], &next.job) {
        return Ok(false);
    }
    let regions: usize = jobs.iter().map(|j| j.crops.len()).sum();
    let bytes = jobs
        .iter()
        .try_fold(0usize, |n, j| {
            n.checked_add(usize::try_from(j.input.bytes).ok()?)
        })
        .unwrap_or(usize::MAX);
    let deadline = |f: fn(&Job) -> u32| jobs.iter().map(|j| u64::from(f(j))).sum::<u64>();
    Ok(regions <= MAX_PAGE_REGIONS
        && deadline(|j| j.gpu_timeout_secs) <= u64::from(MAX_DEADLINE_SECS)
        && deadline(|j| j.cpu_timeout_secs) <= u64::from(MAX_DEADLINE_SECS)
        && bytes <= MAX_BUNDLE_SCAN
        && bundle_json_len(&jobs)? <= MAX_JOB_JSON
        && longest_readings_len(&jobs)? <= MAX_READINGS)
}

#[cfg(test)]
mod tests {
    use super::super::stage::tests::engine as sample_engine;
    use super::*;

    const DIMS: (u32, u32) = (1000, 1400);

    /// An ALTO page of `DIMS` pixels with the given blocks' markup.
    fn alto(body: &str) -> Vec<u8> {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><MeasurementUnit>pixel</MeasurementUnit></Description><Layout><Page ID="P1" WIDTH="1000" HEIGHT="1400" PHYSICAL_IMG_NR="1"><PrintSpace>{body}</PrintSpace></Page></Layout></alto>"#
        )
        .into_bytes()
    }

    /// A line `(id, x, y, width, text)`, 30 pixels high, one String a word.
    type L<'a> = (&'a str, u32, u32, u32, &'a str);

    fn block(id: &str, lines: &[L<'_>]) -> String {
        let mut s =
            format!(r#"<TextBlock ID="{id}" HPOS="0" VPOS="0" WIDTH="1000" HEIGHT="1400">"#);
        for (lid, x, y, w, text) in lines {
            s.push_str(&format!(
                r#"<TextLine ID="{lid}" HPOS="{x}" VPOS="{y}" WIDTH="{w}" HEIGHT="30">"#
            ));
            for word in text.split(' ') {
                s.push_str(&format!(
                    r#"<String CONTENT="{word}" HPOS="{x}" VPOS="{y}" WIDTH="{w}" HEIGHT="30"/><SP/>"#
                ));
            }
            s.push_str("</TextLine>");
        }
        s.push_str("</TextBlock>");
        s
    }

    fn ids(plan: &PagePlan) -> Vec<(String, String, Vec<String>)> {
        plan.regions
            .iter()
            .map(|p| {
                (
                    p.region.id.clone(),
                    p.region.block.clone(),
                    p.region.lines.clone(),
                )
            })
            .collect()
    }

    fn strs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn cuts_text_blocks_and_leaves_the_rest_to_loc() {
        let body = format!(
            "{}{}<Illustration ID=\"I1\" HPOS=\"0\" VPOS=\"600\" WIDTH=\"500\" HEIGHT=\"300\"/>{}",
            block(
                "TB1",
                &[
                    ("TL1", 100, 100, 800, "Mayor Jones spoke"),
                    ("TL2", 100, 140, 800, "to the council")
                ]
            ),
            block("TB2", &[("TL3", 100, 300, 20, "a")]),
            block("TB3", &[("TL4", 100, 1000, 800, "Weather report")]),
        );
        let plan = plan(&alto(&body), DIMS, &Params::default()).unwrap();
        assert_eq!(
            ids(&plan),
            vec![
                ("r0001".into(), "TB1".into(), strs(&["TL1", "TL2"])),
                ("r0002".into(), "TB3".into(), strs(&["TL4"])),
            ]
        );
        // The union of the two lines, with the line path's padding.
        assert_eq!(plan.regions[0].crop.rect, [88, 92, 912, 178]);
        assert_eq!(
            plan.regions[0].crop.loc_text,
            "Mayor Jones spoke\nto the council"
        );
        assert_eq!(plan.regions[0].crop.stratum, "region");
        assert_eq!(
            plan.stats,
            Stats {
                text_blocks: 3,
                non_text_blocks: 1,
                small_blocks: 1,
                regions: 2,
                chars: "MayorJonesspoketothecouncil".len() + "Weatherreport".len(),
                ..Stats::default()
            }
        );
    }

    #[test]
    fn splits_a_long_block_at_line_boundaries_under_the_character_budget() {
        let lines: Vec<L<'_>> = ["L1", "L2", "L3", "L4", "L5"]
            .iter()
            .enumerate()
            .map(|(i, id)| (*id, 100, 100 + 40 * i as u32, 800, "abcdefghi jklmnopqr"))
            .collect();
        let params = Params {
            region_chars: 40,
            ..Params::default()
        };
        let plan = plan(&alto(&block("TB1", &lines)), DIMS, &params).unwrap();
        // 19 characters a line: two fit in 40 with the newline, three do not.
        assert_eq!(
            ids(&plan),
            vec![
                ("r0001".into(), "TB1".into(), strs(&["L1", "L2"])),
                ("r0002".into(), "TB1".into(), strs(&["L3", "L4"])),
                ("r0003".into(), "TB1".into(), strs(&["L5"])),
            ]
        );
        assert_eq!(plan.stats.split_blocks, 1);
        assert_eq!(plan.stats.text_blocks, 1);
        assert!(plan.regions.iter().all(|p| p.crop.loc_text.len() <= 40));
    }

    #[test]
    fn splits_at_the_pixel_cap_and_counts_a_line_over_it() {
        let lines: Vec<L<'_>> = vec![
            ("L1", 100, 100, 60, "one two"),
            ("L2", 100, 140, 60, "three four"),
            ("L3", 100, 180, 60, "five six"),
            ("L4", 100, 220, 400, "seven eight"),
        ];
        let params = Params {
            // Two lines are 70 high with 16 of padding: 86. Three are 126.
            max_side_px: 120,
            ..Params::default()
        };
        let plan = plan(&alto(&block("TB1", &lines)), DIMS, &params).unwrap();
        assert_eq!(
            ids(&plan)
                .into_iter()
                .map(|(_, _, l)| l)
                .collect::<Vec<_>>(),
            vec![strs(&["L1", "L2"]), strs(&["L3"]), strs(&["L4"])]
        );
        for p in &plan.regions[..2] {
            let r = p.crop.rect;
            assert!(r[2] - r[0] <= 120 && r[3] - r[1] <= 120, "{r:?}");
        }
        // L4 is 424 wide on its own: kept whole, and counted.
        assert_eq!(plan.stats.oversized_lines, 1);
    }

    #[test]
    fn a_block_over_the_budget_is_not_small_and_a_tiny_one_is() {
        let body = format!(
            "{}{}",
            block("TB1", &[("L1", 100, 100, 100, "ab")]),
            block("TB2", &[("L2", 100, 200, 100, "abc")])
        );
        let plan = plan(&alto(&body), DIMS, &Params::default()).unwrap();
        assert_eq!(plan.stats.small_blocks, 1);
        assert_eq!(ids(&plan)[0].1, "TB2");
    }

    #[test]
    fn refuses_alto_without_usable_ids() {
        let params = Params::default();
        let no_block = block("TB1", &[("L1", 100, 100, 100, "word")]).replace("ID=\"TB1\"", "");
        let err = plan(&alto(&no_block), DIMS, &params).unwrap_err();
        assert!(format!("{err:#}").contains("no ID"), "{err:#}");
        let no_line = block("TB1", &[("L1", 100, 100, 100, "word")]).replace("ID=\"L1\"", "");
        let err = plan(&alto(&no_line), DIMS, &params).unwrap_err();
        assert!(format!("{err:#}").contains("no ID"), "{err:#}");
        let twice = format!(
            "{}{}",
            block("TB1", &[("L1", 100, 100, 100, "word")]),
            block("TB1", &[("L2", 100, 200, 100, "more")])
        );
        let err = plan(&alto(&twice), DIMS, &params).unwrap_err();
        assert!(format!("{err:#}").contains("used twice"), "{err:#}");
    }

    #[test]
    fn follows_the_parsers_idea_of_a_line() {
        // An empty TextLine element is no line to the parser, so it is none here.
        let body = block("TB1", &[("L1", 100, 100, 300, "first line")])
            .replace("</TextBlock>", "<TextLine ID=\"L9\"/></TextBlock>");
        let plan = plan(&alto(&body), DIMS, &Params::default()).unwrap();
        assert_eq!(ids(&plan)[0].2, strs(&["L1"]));
    }

    #[test]
    fn the_fastest_class_sets_the_floor_and_the_slowest_the_deadline() {
        let p = Params::default();
        assert_eq!(p.fast_secs(2200), 100);
        assert_eq!(p.fast_secs(2201), 101);
        // 1.5 x 1000 / 10 + 300.
        assert_eq!(p.deadline_secs(1000), 450);
        assert!(p.deadline_secs(40_000) < u64::from(MAX_DEADLINE_SECS));
        assert!(p.deadline_secs(100_000) > u64::from(MAX_DEADLINE_SECS));
        assert!(Params {
            slowest_cps: 30.0,
            ..p.clone()
        }
        .check()
        .is_err());
        assert!(Params { pack_pages: 5, ..p }.check().is_err());
    }

    fn options(tmp: &Path, name: &str, params: Params) -> Options {
        let (models, _) =
            super::super::stage_reader(Some(super::super::XIAOMI_MODEL), None).unwrap();
        Options {
            quality_root: tmp.join("quality"),
            root: tmp.join(name),
            engine: Engine {
                models,
                ..sample_engine()
            },
            campaign: "x08-test".into(),
            params,
            max_tokens_per_1000px2: 4,
            max_pages: 20,
            strata: None,
            flush_short: false,
        }
    }

    fn quality_pages(tmp: &Path, n: usize) -> Vec<String> {
        let docs: Vec<String> = (1..=n)
            .map(|i| format!("sn83045462_1890-05-03_ed-1_seq-{i}"))
            .collect();
        for doc_id in &docs {
            let u = tmp.join("quality").join("pages").join(doc_id);
            let mut qjob = crate::quality::work::tests::unit(&u);
            qjob.doc_id = doc_id.clone();
            qjob.id = qjob.identity().unwrap();
            std::fs::write(
                u.join(crate::quality::JOB_FILE),
                serde_json::to_vec_pretty(&qjob).unwrap(),
            )
            .unwrap();
        }
        docs
    }

    fn read_job(dir: &Path) -> Job {
        let job: Job = serde_json::from_slice(&std::fs::read(dir.join(JOB_FILE)).unwrap()).unwrap();
        job.validate().unwrap();
        job
    }

    /// The fixture page has 68 LoC characters besides white space: 4 seconds at 22 a second.
    fn secs() -> Params {
        Params {
            min_unit_secs: 7,
            ..Params::default()
        }
    }

    #[test]
    fn stages_a_small_page_for_packing_and_holds_it_alone() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 1);
        let opts = options(tmp.path(), "qwen", secs());
        let s = run(&opts, None).unwrap();
        assert_eq!(
            (s.staged, s.single, s.held, s.bundles, s.regions),
            (1, 0, 1, 0, 1)
        );
        let job = read_job(&opts.root.join("pages").join(&docs[0]));
        let pm = job.page_mode.as_ref().unwrap();
        assert_eq!(
            (job.group.as_str(), job.prompt_version, job.model.as_deref()),
            ("x08-test", 5, Some("xiaomi-ocr-0-q8"))
        );
        assert_eq!(
            (pm.regions[0].id.as_str(), pm.regions[0].block.as_str()),
            ("r0001", "B0")
        );
        assert_eq!(pm.regions[0].lines, strs(&["B0L0", "B0L1"]));
        assert!(pm.slots.is_none());
        let prov: PageProvenance = serde_json::from_slice(
            &std::fs::read(opts.root.join("pages").join(&docs[0]).join(PROVENANCE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(prov.campaign, "x08-test");
        assert_eq!(prov.doc_id, docs[0]);
        assert!(prov.est_fast_secs == 4 && prov.deadline_secs >= 300);
        // Nothing published yet: a lone page that cannot fill a unit waits.
        assert!(std::fs::read_dir(units(&opts.root))
            .unwrap()
            .next()
            .is_none());
        // Flushing it publishes a short bundle of one, with a sidecar before its job.
        let flush = Options {
            flush_short: true,
            ..opts.clone()
        };
        let s = run(&flush, None).unwrap();
        assert_eq!(
            (s.already, s.bundles, s.bundled, s.short_bundles, s.held),
            (1, 1, 1, 1, 0)
        );
        let bundle = units(&opts.root).join(format!("bundle-00001-{}", docs[0]));
        let b = read_job(&bundle);
        assert_eq!(b.crops[0].id, format!("{}:r0001", docs[0]));
        assert_eq!(
            b.page_mode.as_ref().unwrap().regions[0].id,
            format!("{}:r0001", docs[0])
        );
        let bp: BundleProvenance =
            serde_json::from_slice(&std::fs::read(bundle.join(PROVENANCE_FILE)).unwrap()).unwrap();
        assert_eq!((bp.campaign.as_str(), bp.pages.len()), ("x08-test", 1));
    }

    #[test]
    fn packs_small_pages_until_a_unit_fills_and_is_idempotent() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 3);
        let opts = options(tmp.path(), "qwen", secs());
        let s = run(&opts, None).unwrap();
        // 4 s a page and a 7 s floor: two pages fill a unit, the third waits.
        assert_eq!(
            (s.staged, s.bundles, s.bundled, s.short_bundles, s.held),
            (3, 1, 2, 0, 1)
        );
        let b = read_job(&units(&opts.root).join(format!("bundle-00001-{}", docs[0])));
        assert_eq!(
            b.pages.iter().map(|p| p.doc_id.clone()).collect::<Vec<_>>(),
            docs[..2].to_vec()
        );
        let each = read_job(&opts.root.join("pages").join(&docs[0])).gpu_timeout_secs;
        assert_eq!(b.gpu_timeout_secs, 2 * each, "the pages' deadlines, summed");
        assert_eq!(b.page_mode.as_ref().unwrap().regions.len(), 2);
        // Again: nothing new, nothing duplicated, the third still waits.
        let s = run(&opts, None).unwrap();
        assert_eq!((s.staged, s.already, s.bundles, s.held), (0, 3, 0, 1));
        // Another page arrives and fills the unit with the one that waited.
        let more = quality_pages(tmp.path(), 4);
        assert_eq!(more[3], "sn83045462_1890-05-03_ed-1_seq-4");
        let s = run(&opts, None).unwrap();
        assert_eq!((s.staged, s.bundles, s.held), (1, 1, 0));
        let b2 = read_job(&units(&opts.root).join(format!("bundle-00002-{}", docs[2])));
        assert_eq!(b2.pages.len(), 2);
    }

    #[test]
    fn a_page_that_fills_a_unit_is_one_unit_and_the_pack_size_is_a_cap() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 3);
        let alone = options(
            tmp.path(),
            "alone",
            Params {
                min_unit_secs: 3,
                ..Params::default()
            },
        );
        let s = run(&alone, None).unwrap();
        assert_eq!((s.staged, s.single, s.bundles, s.held), (3, 3, 0, 0));
        let job = read_job(&units(&alone.root).join(&docs[0]));
        assert!(job.pages.is_empty() && job.page_mode.is_some());
        // A floor nothing reaches: bundles stop at the pack size.
        let capped = options(
            tmp.path(),
            "capped",
            Params {
                min_unit_secs: 10_000,
                pack_pages: 2,
                ..Params::default()
            },
        );
        let s = run(&capped, None).unwrap();
        assert_eq!(
            (s.bundles, s.bundled, s.short_bundles, s.held),
            (1, 2, 1, 1)
        );
    }

    #[test]
    fn pages_with_nothing_to_read_or_too_slow_are_marked_and_a_root_keeps_its_campaign() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 2);
        // A page whose deadline would pass four hours at a crawl.
        let slow = options(
            tmp.path(),
            "slow",
            Params {
                fastest_cps: 0.01,
                slowest_cps: 0.005,
                ..Params::default()
            },
        );
        let s = run(&slow, None).unwrap();
        assert_eq!((s.over_deadline, s.staged), (2, 0));
        assert!(slow
            .root
            .join("pages")
            .join(&docs[0])
            .join(OVER_DEADLINE)
            .exists());
        assert_eq!(run(&slow, None).unwrap().already, 2);
        // No text block over the minimum: no regions.
        let none = options(
            tmp.path(),
            "none",
            Params {
                min_chars: 1000,
                ..Params::default()
            },
        );
        let s = run(&none, None).unwrap();
        assert_eq!((s.no_regions, s.staged), (2, 0));
        // Another campaign into the same root is refused.
        let mut other = options(tmp.path(), "campaigns", secs());
        run(&other, None).unwrap();
        other.campaign = "x08-other".into();
        let err = run(&other, None).unwrap_err();
        assert!(format!("{err:#}").contains("its own --root"), "{err:#}");
    }

    #[test]
    fn members_pick_the_pages_and_strata_reach_the_provenance() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 3);
        let members = tmp.path().join("members.json");
        std::fs::write(&members, serde_json::to_vec(&[&docs[2], &docs[0]]).unwrap()).unwrap();
        let strata = tmp.path().join("strata.json");
        std::fs::write(
            &strata,
            serde_json::to_vec(&serde_json::json!({
                docs[0].clone(): {"decade": 1890, "band": "worst", "tess_read": false, "wf1415": true}
            }))
            .unwrap(),
        )
        .unwrap();
        let mut opts = options(tmp.path(), "qwen", secs());
        opts.strata = Some(strata);
        let s = run(&opts, Some(&members)).unwrap();
        assert_eq!(s.staged, 2);
        assert!(!opts.root.join("pages").join(&docs[1]).exists());
        let prov = |doc: &str| -> PageProvenance {
            serde_json::from_slice(
                &std::fs::read(opts.root.join("pages").join(doc).join(PROVENANCE_FILE)).unwrap(),
            )
            .unwrap()
        };
        assert_eq!(
            prov(&docs[0]).stratum,
            Some(Stratum {
                decade: 1890,
                band: "worst".into(),
                tess_read: false,
                wf1415: true
            })
        );
        assert_eq!(prov(&docs[2]).stratum, None);
    }

    #[test]
    fn a_unit_is_sized_for_its_readings_not_for_sample_crops() {
        // 1,024 regions of the longest LoC text a crop may carry would pass MAX_READINGS if
        // sized like sample crops; a page-mode job is sized by its own text.
        let tmp = tempfile::tempdir().unwrap();
        quality_pages(tmp.path(), 1);
        let opts = options(tmp.path(), "qwen", secs());
        run(&opts, None).unwrap();
        let job = read_job(
            &opts
                .root
                .join("pages")
                .join("sn83045462_1890-05-03_ed-1_seq-1"),
        );
        let len = longest_readings_len(std::slice::from_ref(&job)).unwrap();
        assert!(len < 16 * 1024 + 4096, "{len}");
        assert!(len <= MAX_READINGS);
    }

    /// A staged quality page like `quality_pages`' but with `alto` for LoC's.
    fn quality_page_with_alto(tmp: &Path, doc_id: &str, alto: &[u8]) {
        let u = tmp.join("quality").join("pages").join(doc_id);
        let mut qjob = crate::quality::work::tests::unit(&u);
        std::fs::write(u.join(crate::quality::ALTO_FILE), alto).unwrap();
        qjob.inputs.alto = usnm_ocr_contract::quality::Input::of(alto, "test:loc.xml");
        qjob.doc_id = doc_id.into();
        qjob.id = qjob.identity().unwrap();
        std::fs::write(
            u.join(crate::quality::JOB_FILE),
            serde_json::to_vec_pretty(&qjob).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn refuses_a_line_id_used_twice() {
        let body = format!(
            "{}{}",
            block("TB1", &[("L1", 100, 100, 100, "word")]),
            block("TB2", &[("L1", 100, 200, 100, "more")])
        );
        let err = plan(&alto(&body), DIMS, &Params::default()).unwrap_err();
        assert!(
            format!("{err:#}").contains("TextLine ID L1 is used twice"),
            "{err:#}"
        );
    }

    #[test]
    fn a_root_belongs_to_the_campaign_that_first_staged_into_it() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 2);
        let members = |doc: &str| {
            let p = tmp.path().join(format!("{doc}.json"));
            std::fs::write(&p, serde_json::to_vec(&[doc]).unwrap()).unwrap();
            p
        };
        let mut opts = options(tmp.path(), "qwen", secs());
        run(&opts, Some(&members(&docs[0]))).unwrap();
        // Other pages, so nothing the first campaign staged is revisited: still refused.
        opts.campaign = "x08-other".into();
        let err = run(&opts, Some(&members(&docs[1]))).unwrap_err();
        assert!(
            format!("{err:#}").contains("this root is for campaign"),
            "{err:#}"
        );
        assert!(!opts.root.join("pages").join(&docs[1]).exists());
    }

    #[test]
    fn bundles_stop_at_the_deadline_a_unit_may_have() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 3);
        // 68 characters at 0.0132 a second with the margin: about 7,800 s a page, so two
        // pages' deadlines, summed, would pass the 14,400 s a unit may have.
        let opts = options(
            tmp.path(),
            "qwen",
            Params {
                min_unit_secs: 10_000,
                slowest_cps: 0.0132,
                ..Params::default()
            },
        );
        let each = read_job_deadline(&opts, &docs[0]);
        assert!(each > 7000 && 2 * each > MAX_DEADLINE_SECS, "{each}");
        let s = run(&opts, None).unwrap();
        assert_eq!((s.bundles, s.bundled, s.held), (2, 2, 1));
        for entry in std::fs::read_dir(units(&opts.root)).unwrap() {
            let job = read_job(&entry.unwrap().path());
            assert_eq!(job.pages.len(), 1);
            assert!(job.gpu_timeout_secs <= MAX_DEADLINE_SECS);
        }
    }

    fn read_job_deadline(opts: &Options, doc: &str) -> u32 {
        // Staged by the run; read it from a first run on a copy of the root.
        let probe = Options {
            root: opts.root.with_file_name("probe"),
            ..opts.clone()
        };
        run(&probe, None).unwrap();
        read_job(&probe.root.join("pages").join(doc)).gpu_timeout_secs
    }

    #[test]
    fn a_region_of_text_over_the_limit_keeps_its_page_out_without_stopping_the_run() {
        let tmp = tempfile::tempdir().unwrap();
        let big = "abcdefghi ".repeat(900);
        let alto = alto(&block("TB1", &[("L1", 100, 100, 800, big.trim_end())]));
        quality_page_with_alto(tmp.path(), "sn83045462_1890-05-03_ed-1_seq-1", &alto);
        quality_page(tmp.path(), 2);
        let opts = options(tmp.path(), "qwen", secs());
        let s = run(&opts, None).unwrap();
        assert_eq!((s.too_large, s.staged), (1, 1));
        let marker = opts
            .root
            .join("pages")
            .join("sn83045462_1890-05-03_ed-1_seq-1")
            .join(TOO_LARGE);
        assert!(std::fs::read_to_string(marker)
            .unwrap()
            .contains("over the 8192-byte limit"));
        // A rerun skips it and has nothing more to do.
        let again = run(&opts, None).unwrap();
        assert_eq!((again.already, again.too_large, again.staged), (2, 0, 0));
    }

    /// Page `i` of the fixture's newspaper, as `quality_pages` stages it.
    fn quality_page(tmp: &Path, i: usize) {
        let doc_id = format!("sn83045462_1890-05-03_ed-1_seq-{i}");
        let u = tmp.join("quality").join("pages").join(&doc_id);
        let mut qjob = crate::quality::work::tests::unit(&u);
        qjob.doc_id = doc_id;
        qjob.id = qjob.identity().unwrap();
        std::fs::write(
            u.join(crate::quality::JOB_FILE),
            serde_json::to_vec_pretty(&qjob).unwrap(),
        )
        .unwrap();
    }

    #[test]
    fn a_page_whose_one_page_bundle_is_too_large_is_marked_and_the_rest_packed() {
        let tmp = tempfile::tempdir().unwrap();
        // 60 regions of 7,000 characters: about 430 KB as a page's job, twice that in a bundle,
        // which lists the crops twice.
        let line = "abcdefghi ".repeat(700);
        let line = line.trim_end();
        let blocks: String = (0..60u32)
            .map(|i| {
                block(
                    &format!("TB{i}"),
                    &[(&format!("L{i}"), 100, 20 + 22 * i, 800, line)],
                )
            })
            .collect();
        quality_page_with_alto(
            tmp.path(),
            "sn83045462_1890-05-03_ed-1_seq-1",
            &alto(&blocks),
        );
        quality_page(tmp.path(), 2);
        let opts = options(
            tmp.path(),
            "qwen",
            Params {
                fastest_cps: 1e6,
                slowest_cps: 1e6,
                min_unit_secs: 5,
                ..Params::default()
            },
        );
        let s = run(&opts, None).unwrap();
        assert_eq!((s.staged, s.too_large), (2, 1), "{s:?}");
        let big = opts.root.join("pages/sn83045462_1890-05-03_ed-1_seq-1");
        assert!(big.join(JOB_FILE).exists() && big.join(TOO_LARGE).exists());
        assert!(std::fs::read(big.join(JOB_FILE)).unwrap().len() <= MAX_JOB_JSON);
        // The small page is still packed (held: it alone cannot fill a unit at a 5 s floor).
        assert_eq!(s.held, 1);
        // Marked pages are skipped on the next run, not met again.
        let again = run(&opts, None).unwrap();
        assert_eq!((again.too_large, again.held), (0, 1));
    }

    #[test]
    fn ids_the_contract_would_refuse_fail_the_page_alone() {
        let params = Params::default();
        let long = "T".repeat(65);
        for id in ["", "a b", long.as_str(), "caf\u{e9}"] {
            let body = block("TB1", &[("L1", 100, 100, 100, "word")])
                .replace("ID=\"TB1\"", &format!("ID=\"{id}\""));
            let err = plan(&alto(&body), DIMS, &params).unwrap_err();
            assert!(
                format!("{err:#}").contains("TextBlock ID"),
                "{id:?}: {err:#}"
            );
        }
        let body = block("TB1", &[("L1", 100, 100, 100, "word")]).replace("ID=\"L1\"", "ID=\"\"");
        let err = plan(&alto(&body), DIMS, &params).unwrap_err();
        assert!(format!("{err:#}").contains("TextLine ID"), "{err:#}");
        // LoC's own ids pass.
        let body = block("P1_TB00005.2", &[("P1_TL00005", 100, 100, 100, "word")]);
        assert_eq!(plan(&alto(&body), DIMS, &params).unwrap().regions.len(), 1);
    }

    #[tokio::test]
    async fn a_root_staged_with_sample_crops_is_not_reused() {
        let tmp = tempfile::tempdir().unwrap();
        quality_pages(tmp.path(), 1);
        let opts = options(tmp.path(), "qwen", secs());
        super::super::stage::run(&super::super::stage::Options {
            quality_root: opts.quality_root.clone(),
            root: opts.root.clone(),
            engine: opts.engine.clone(),
            gpu_timeout_secs: 1800,
            cpu_timeout_secs: 7200,
            max_tokens_per_1000px2: 4,
            prompt_version: PROMPT_VERSION_XIAOMI,
            model: Some(super::super::XIAOMI_MODEL.into()),
            max_pages: 20,
            bundle_size: 1,
            page_records: None,
            max_hold_secs: 0,
            crop_rule: Default::default(),
        })
        .await
        .unwrap();
        let err = run(&opts, None).unwrap_err();
        assert!(
            format!("{err:#}").contains("staged without --page-mode"),
            "{err:#}"
        );
        // Not counted as staged, and nothing of page mode written.
        assert!(!opts.root.join(CAMPAIGN_FILE).exists());
    }

    #[test]
    fn pages_staged_with_other_settings_are_not_packed_together() {
        let tmp = tempfile::tempdir().unwrap();
        let docs = quality_pages(tmp.path(), 2);
        let members = |doc: &str| {
            let p = tmp.path().join(format!("{doc}.json"));
            std::fs::write(&p, serde_json::to_vec(&[doc]).unwrap()).unwrap();
            p
        };
        let mut opts = options(
            tmp.path(),
            "qwen",
            Params {
                min_unit_secs: 10_000,
                ..Params::default()
            },
        );
        let s = run(&opts, Some(&members(&docs[0]))).unwrap();
        assert_eq!((s.staged, s.held, s.bundles), (1, 1, 0));
        // A later run with another token allowance: its page cannot join the held one, which
        // is then as full as it gets. Packing goes on instead of failing on every rerun.
        opts.max_tokens_per_1000px2 = 8;
        let s = run(&opts, Some(&members(&docs[1]))).unwrap();
        assert_eq!((s.staged, s.bundles, s.bundled, s.held), (1, 1, 1, 1));
        let again = run(&opts, None).unwrap();
        assert_eq!((again.bundles, again.held), (0, 1));
    }

    #[test]
    fn a_region_never_lists_more_lines_than_the_contract_allows() {
        // 1,025 one-letter lines in a block fit the character budget, so only the line limit
        // closes the first region.
        let lines: Vec<L<'_>> = (0..=1024u32)
            .map(|i| {
                (
                    Box::leak(format!("L{i}").into_boxed_str()) as &str,
                    100,
                    20,
                    20,
                    "a",
                )
            })
            .collect();
        let params = Params {
            region_chars: 4000,
            max_side_px: 60_000,
            ..Params::default()
        };
        let plan = plan(&alto(&block("TB1", &lines)), DIMS, &params).unwrap();
        let sizes: Vec<usize> = plan.regions.iter().map(|p| p.region.lines.len()).collect();
        assert_eq!(sizes, vec![MAX_REGION_LINES, 1]);
        let mut job_regions = plan
            .regions
            .iter()
            .map(|p| p.region.clone())
            .collect::<Vec<_>>();
        assert!(job_regions
            .drain(..)
            .all(|r| r.lines.len() <= MAX_REGION_LINES));
    }

    #[test]
    fn only_elements_in_the_altos_own_namespace_are_blocks() {
        // A foreign TextBlock under Tags/XmlData is not one to the parser, nor here.
        let foreign = r#"<Tags><OtherTag><XmlData xmlns:x="urn:other"><x:TextBlock ID="X1"><x:TextLine ID="XL"/></x:TextBlock></XmlData></OtherTag></Tags>"#;
        let text =
            String::from_utf8(alto(&block("TB1", &[("L1", 100, 100, 300, "first line")]))).unwrap();
        let text = text.replace("<Layout>", &format!("{foreign}<Layout>"));
        let plan = plan(text.as_bytes(), DIMS, &Params::default()).unwrap();
        assert_eq!(ids(&plan)[0].1, "TB1");
        assert_eq!(plan.regions.len(), 1);
    }
}
