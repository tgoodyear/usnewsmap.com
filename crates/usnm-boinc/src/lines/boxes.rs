//! Line jobs for pages with no LoC text (#1622): the Georgia Historic Newspapers (GHN) pages of
//! the _Cherokee Phoenix_, whose line boxes come from the line finder at staging instead of from
//! LoC's ALTO.
//!
//! - The staging step (`scripts/ocr-survey/chr_ghn_layout/ghn_boxes.py`) finds each page's lines
//!   with finder S1 of #2128, calls each line Cherokee or Latin, and writes one `boxes.jsonl` a
//!   page at `<dir>/ghn/<local id>/boxes.jsonl` ([`path`]). Its first row is the page
//!   ([`PageRow`]): the `ghn:` id, the scan's sha256 and size, the finder's and the classifier's
//!   version strings, the merged-lines share and whether the page is held back. Each other row is
//!   a line ([`LineRow`]) in the finder's reading order.
//! - Two classifier formats are read. The first (#1830's shape classifier) calls a line Cherokee
//!   at `p_chr >= 0.5`. The second (#2128's v2 ensemble) writes `chr_threshold` on the page row
//!   and `p_line` on each line, calls a line at `p_line >= chr_threshold`, and gives the page's
//!   `held_reasons` (`merged_lines`, `faded`) and `contrast`.
//! - `export --line-read chr --line-boxes <dir>` reads it ([`parse`]), puts the Cherokee lines'
//!   boxes in the job ([`Boxes::cherokee`]) and keeps the file beside the job
//!   ([`super::BOXES_FILE`]).
//! - `import` assembles the page from the job's readings in the boxes' order ([`assemble`]) and
//!   judges it with rule `line-assemble-v1`. The page has no LoC text to keep beside it.
use anyhow::{ensure, Context};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use usnm_alto::{Block, BlockKind, Granularity, Line, Page, Rect};
use usnm_core::page_id::{PageId, Source};
use usnm_ocr_contract::boinc::{self, Job, LineBox};

use super::{Admission, LineReadRecord, MergeStats};

/// A page's line boxes, under the directory `--line-boxes` names: `ghn/<local id>/boxes.jsonl`.
pub const BOXES_NAME: &str = "boxes.jsonl";
/// The largest `boxes.jsonl`: a few thousand lines of about 100 bytes.
pub const MAX_BOXES: usize = 4 * 1024 * 1024;
/// The longest version string the file may carry.
const MAX_VERSION: usize = 512;
/// The reasons the staging step may hold a page back.
const HELD_REASONS: [&str; 2] = ["merged_lines", "faded"];

/// Where the boxes of page `id` are under `dir`: `<dir>/<source>/<local id>/boxes.jsonl`, the
/// path ADR-0026 gives a page's files where sources mix.
pub fn path(dir: &Path, id: &PageId) -> PathBuf {
    dir.join(id.path()).join(BOXES_NAME)
}

/// The first row of `boxes.jsonl`. Fields in alphabetical order, as the staging step writes them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PageRow {
    /// The v2 classifier's threshold: a line is Cherokee when `p_line` is at least this. None in
    /// the first format, where the threshold is 0.5 on `p_chr`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chr_threshold: Option<f64>,
    /// The line classifier, its features and its training data, as one version string.
    pub classifier: String,
    /// The page's contrast (#2128 amendment 2), which sets the faded hold. v2 format only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contrast: Option<f64>,
    /// The page's `ghn:` id (ADR-0026).
    pub doc_id: String,
    /// The line finder, its frozen inputs and its settings, as one version string.
    pub finder: String,
    pub height: u32,
    /// The page waits for a second layout attempt or a look by hand, and `export` passes it
    /// over: its merged-lines share was over the staging step's threshold, or (v2 format) it is
    /// faded.
    pub held: bool,
    /// Why the page is held (v2 format): `held` is true exactly when this isn't empty.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub held_reasons: Option<Vec<String>>,
    pub kind: String,
    pub scan_sha256: String,
    /// The share of boxes over 1.8 times the page's median line height (`chr_layout_qa.py`).
    pub tall_share: f64,
    pub width: u32,
}

/// One line row of `boxes.jsonl`, in the finder's reading order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LineRow {
    /// Box in scan pixels, without padding.
    #[serde(rename = "box")]
    pub rect: [u32; 4],
    /// The classifier calls the line Cherokee: `p_line` at least the page's `chr_threshold` in
    /// the v2 format, `p_chr` at least 0.5 in the first.
    pub chr: bool,
    /// `r<region>b<block>p<paragraph>l<line>`: the finder's column region and Tesseract's own
    /// numbering inside it.
    pub id: String,
    pub kind: String,
    /// The shape classifier's probability that the line is Cherokee; none for a crop with no ink.
    pub p_chr: Option<f64>,
    /// v2 format: the CNN's score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_cnn: Option<f64>,
    /// v2 format: the ensemble's score, which makes the call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_line: Option<f64>,
    /// v2 format: the reading-based score.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p_v2: Option<f64>,
}

fn probability_ok(p: Option<f64>) -> bool {
    p.is_none_or(|p| p.is_finite() && (0.0..=1.0).contains(&p))
}

/// A page's `boxes.jsonl`, checked.
#[derive(Debug, Clone, PartialEq)]
pub struct Boxes {
    pub page: PageRow,
    pub id: PageId,
    pub lines: Vec<LineRow>,
}

impl Boxes {
    /// The boxes a line job reads: the lines called Cherokee, in the file's order.
    pub fn cherokee(&self) -> Vec<LineBox> {
        self.lines
            .iter()
            .filter(|l| l.chr)
            .map(|l| LineBox {
                id: l.id.clone(),
                rect: l.rect,
            })
            .collect()
    }
}

fn version_ok(s: &str) -> bool {
    !s.trim().is_empty() && s.len() <= MAX_VERSION && !s.chars().any(char::is_control)
}

/// Reads and checks a `boxes.jsonl`: a page row, then line rows with unique ids and boxes on
/// the page.
pub fn parse(bytes: &[u8]) -> anyhow::Result<Boxes> {
    ensure!(
        bytes.len() <= MAX_BOXES,
        "boxes.jsonl is over its size limit"
    );
    let text = std::str::from_utf8(bytes).context("boxes.jsonl is not UTF-8")?;
    let mut rows = text.lines().filter(|l| !l.trim().is_empty());
    let first = rows.next().context("boxes.jsonl is empty")?;
    let page: PageRow = serde_json::from_str(first).context("boxes.jsonl's page row")?;
    ensure!(page.kind == "page", "boxes.jsonl starts with its page row");
    let id: PageId = page.doc_id.parse()?;
    ensure!(
        id.source() == Source::Ghn,
        "boxes.jsonl is for pages without LoC text, a ghn: page, not {}",
        page.doc_id
    );
    ensure!(
        boinc::is_sha256(&page.scan_sha256),
        "invalid scan sha256 in boxes.jsonl"
    );
    ensure!(
        (1..=boinc::MAX_LINE_COORD).contains(&page.width)
            && (1..=boinc::MAX_LINE_COORD).contains(&page.height),
        "invalid page size in boxes.jsonl"
    );
    ensure!(
        version_ok(&page.finder) && version_ok(&page.classifier),
        "boxes.jsonl names its finder and classifier"
    );
    ensure!(
        page.tall_share.is_finite() && (0.0..=1.0).contains(&page.tall_share),
        "invalid merged-lines share in boxes.jsonl"
    );
    if let Some(t) = page.chr_threshold {
        ensure!(
            t.is_finite() && t > 0.0 && t < 1.0,
            "invalid classifier threshold in boxes.jsonl"
        );
        let reasons = page
            .held_reasons
            .as_ref()
            .context("a v2 boxes.jsonl gives its held reasons")?;
        ensure!(
            reasons.iter().all(|r| HELD_REASONS.contains(&r.as_str()))
                && reasons.iter().collect::<BTreeSet<_>>().len() == reasons.len(),
            "unknown or repeated held reason in boxes.jsonl"
        );
        ensure!(
            page.held == !reasons.is_empty(),
            "boxes.jsonl's page is held exactly when it has a reason"
        );
        ensure!(
            page.contrast.is_some_and(|c| c.is_finite() && c >= 0.0),
            "a v2 boxes.jsonl gives the page's contrast"
        );
    } else {
        ensure!(
            page.held_reasons.is_none() && page.contrast.is_none(),
            "held reasons and contrast come with the v2 classifier's threshold"
        );
    }
    let mut lines = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        let line: LineRow = serde_json::from_str(row).context("a line row of boxes.jsonl")?;
        ensure!(line.kind == "line", "one page row, then line rows");
        ensure!(
            !line.id.is_empty()
                && line.id.len() <= boinc::MAX_LINE_ID
                && line.id.bytes().all(|b| b.is_ascii_alphanumeric()),
            "invalid line id {:?}",
            line.id
        );
        ensure!(
            seen.insert(line.id.clone()),
            "duplicate line id {}",
            line.id
        );
        let [x0, y0, x1, y1] = line.rect;
        ensure!(
            x0 < x1 && y0 < y1 && x1 <= page.width && y1 <= page.height,
            "line {}'s box is not on the {}x{} page",
            line.id,
            page.width,
            page.height
        );
        ensure!(
            [line.p_chr, line.p_cnn, line.p_line, line.p_v2]
                .into_iter()
                .all(probability_ok),
            "invalid score on line {}",
            line.id
        );
        match page.chr_threshold {
            Some(t) => ensure!(
                line.chr == line.p_line.is_some_and(|p| p >= t),
                "line {} is called Cherokee exactly when p_line is at least {t}",
                line.id
            ),
            None => {
                ensure!(
                    line.p_cnn.is_none() && line.p_line.is_none() && line.p_v2.is_none(),
                    "line {} has v2 scores but the page has no v2 threshold",
                    line.id
                );
                ensure!(
                    line.chr == line.p_chr.is_some_and(|p| p >= 0.5),
                    "line {} is called Cherokee exactly when p_chr is at least 0.5",
                    line.id
                );
            }
        }
        lines.push(line);
    }
    Ok(Boxes { page, id, lines })
}

/// A line job's page with no LoC text, assembled.
pub struct Assembled {
    /// The page's text, as `text.txt` holds a page's, before normalization.
    pub text: String,
    /// The page's ALTO, in the scan's pixels.
    pub alto: String,
    pub stats: MergeStats,
    /// Rule `line-assemble-v1`'s verdict.
    pub admission: Admission,
    /// The page's `ghn:` id.
    pub doc_id: String,
}

/// The page of a GHN line job from its `boxes.jsonl` and its result text: the Cherokee lines'
/// readings in the boxes' order, one ALTO line each at its box, judged with
/// `line-assemble-v1`. `completed_at` is the result's: the ALTO is the same bytes each time.
pub fn assemble(
    job: &Job,
    boxes: &[u8],
    record: &LineReadRecord,
    result_text: &str,
    completed_at: Option<&str>,
) -> anyhow::Result<Assembled> {
    use usnm_ocr_local::rule::line_assemble;
    ensure!(
        record.boxes_sha256.as_deref() == Some(boinc::sha256(boxes).as_str()),
        "the page's boxes changed since export"
    );
    let parsed = parse(boxes)?;
    let lines = job.lines.as_ref().context("not a line job")?;
    ensure!(
        super::is_ghn(job)
            && parsed.id == usnm_core::page_id::PageId::from_wire_key(&job.message.key()?)?,
        "the boxes are not this job's page"
    );
    ensure!(
        lines.lines == parsed.cherokee(),
        "the job's lines are not this page's Cherokee lines"
    );
    let readings = job.line_texts(result_text)?.context("not a line job")?;
    let texts: Vec<&str> = readings.iter().map(|(_, t)| t.trim()).collect();
    let (counts, verdict) = line_assemble::verdict(&texts);
    let mut out_lines = Vec::with_capacity(texts.len());
    for (line, text) in lines.lines.iter().zip(&texts) {
        let [x0, y0, x1, y1] = line.rect.map(f64::from);
        let rect = Rect::new(x0, y0, x1, y1);
        out_lines.push(Line {
            rect,
            words: super::line_words(rect, text, None),
            hyphen: None,
        });
    }
    let (w, h) = (f64::from(record.scan_width), f64::from(record.scan_height));
    let page = Page {
        width: w,
        height: h,
        granularity: Granularity::Line,
        blocks: vec![Block {
            kind: BlockKind::Text,
            rect: Rect::new(0.0, 0.0, w, h),
            lang: Some(lines.language.clone()),
            lines: out_lines,
        }],
    };
    let stats = MergeStats {
        lines: texts.len(),
        model: counts.model,
        latin_words: texts.iter().map(|t| super::script::latin_words(t)).sum(),
        ..MergeStats::default()
    };
    let mut desc = super::description(
        job,
        "line finder boxes; Cherokee lines read one crop each",
        completed_at,
    )?;
    desc.ocr
        .settings
        .push(("granularity".into(), page.granularity.as_str().into()));
    desc.post = Some(usnm_alto::Step {
        date_time: None,
        settings: vec![
            ("page".into(), parsed.page.doc_id.clone()),
            ("finder".into(), parsed.page.finder.clone()),
            ("classifier".into(), parsed.page.classifier.clone()),
            ("boxes_sha256".into(), boinc::sha256(boxes)),
            ("lines_read".into(), stats.lines.to_string()),
            ("lines_model".into(), stats.model.to_string()),
        ],
        software: usnm_alto::Software {
            creator: "usnewsmap.com".into(),
            name: "usnm-boinc line assemble".into(),
            version: env!("CARGO_PKG_VERSION").into(),
        },
    });
    Ok(Assembled {
        text: usnm_alto::text(&page),
        alto: usnm_alto::write(&page, &desc, &lines.language),
        stats,
        admission: Admission {
            rule: line_assemble::VERSION,
            verdict: match verdict {
                Ok(()) => "admit",
                Err(keep) => keep.as_str(),
            },
            cherokee: counts.cherokee,
            model: counts.model,
        },
        doc_id: parsed.page.doc_id,
    })
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub(crate) const DOC: &str = "ghn:sn83020866_1828-02-21_ed-1_seq-1";
    pub(crate) const WIRE: &str = "ghnsn83020866/1828-02-21/ed-1/seq-1";

    /// A `boxes.jsonl` for a 100 by 100 page: two Cherokee lines around a Latin one.
    pub(crate) fn boxes_file(scan_sha256: &str, held: bool) -> String {
        let page = PageRow {
            chr_threshold: None,
            classifier: "chr_line_script-1830".into(),
            contrast: None,
            doc_id: DOC.into(),
            finder: "s1-2128".into(),
            height: 100,
            held,
            held_reasons: None,
            kind: "page".into(),
            scan_sha256: scan_sha256.into(),
            tall_share: 0.01,
            width: 100,
        };
        let line = |id: &str, y: u32, p: Option<f64>| LineRow {
            rect: [10, y, 90, y + 10],
            chr: p.is_some_and(|p| p >= 0.5),
            id: id.into(),
            kind: "line".into(),
            p_chr: p,
            p_cnn: None,
            p_line: None,
            p_v2: None,
        };
        let mut s = serde_json::to_string(&page).unwrap() + "\n";
        for l in [
            line("r0b1p1l1", 10, Some(0.97)),
            line("r0b1p1l2", 30, Some(0.02)),
            line("r1b1p1l1", 50, Some(0.81)),
            line("r1b1p1l2", 70, None),
        ] {
            s += &(serde_json::to_string(&l).unwrap() + "\n");
        }
        s
    }

    #[test]
    fn parses_a_page_row_then_lines_and_picks_the_cherokee_ones() {
        let file = boxes_file(&"a".repeat(64), false);
        let b = parse(file.as_bytes()).unwrap();
        assert_eq!(b.id.to_string(), DOC);
        assert_eq!(
            b.cherokee(),
            [
                LineBox {
                    id: "r0b1p1l1".into(),
                    rect: [10, 10, 90, 20]
                },
                LineBox {
                    id: "r1b1p1l1".into(),
                    rect: [10, 50, 90, 60]
                },
            ]
        );
        assert_eq!(
            path(Path::new("/s/line-boxes"), &b.id),
            Path::new("/s/line-boxes/ghn/sn83020866_1828-02-21_ed-1_seq-1/boxes.jsonl")
        );
        // The rows' wire form is exactly what the staging step writes: keys sorted.
        assert!(file.starts_with("{\"classifier\":"));
        assert!(file.lines().nth(1).unwrap().starts_with("{\"box\":"));
    }

    #[test]
    fn refuses_a_file_that_is_not_one_ghn_pages_boxes() {
        let good = boxes_file(&"a".repeat(64), false);
        let edit = |from: &str, to: &str| good.replacen(from, to, 1);
        for bad in [
            String::new(),
            good.lines().skip(1).collect::<Vec<_>>().join("\n"),
            edit(DOC, "sn83020866_1828-02-21_ed-1_seq-1"),
            edit(&"a".repeat(64), "abc"),
            edit("\"finder\":\"s1-2128\"", "\"finder\":\" \""),
            edit("[10,10,90,20]", "[10,10,190,20]"),
            edit("\"r0b1p1l2\"", "\"r0b1p1l1\""),
            edit("\"r0b1p1l2\"", "\"r0.1.1.2\""),
            edit(
                "\"chr\":false,\"id\":\"r0b1p1l2\"",
                "\"chr\":true,\"id\":\"r0b1p1l2\"",
            ),
            edit("\"tall_share\":0.01", "\"tall_share\":1.5"),
            edit("\"width\":100}", "\"width\":100,\"extra\":1}"),
            edit("\"p_chr\":0.02}", "\"p_chr\":0.02,\"p_line\":0.9}"),
            edit("\"held\":false", "\"held\":false,\"held_reasons\":[]"),
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
    }

    /// A v2 `boxes.jsonl`, as `ghn_boxes.py write` writes it since #2128's amendment 3.3: the
    /// calls follow `p_line`, not `p_chr`.
    fn v2_file(held_reasons: &str) -> String {
        let held = held_reasons != "[]";
        let mut s = format!(
            "{{\"chr_threshold\":0.46,\"classifier\":\"chr line classifier v2\",\"contrast\":181.0,\
             \"doc_id\":\"{DOC}\",\"finder\":\"s1-2128\",\"height\":100,\"held\":{held},\
             \"held_reasons\":{held_reasons},\"kind\":\"page\",\"scan_sha256\":\"{}\",\
             \"tall_share\":0.01,\"width\":100}}\n",
            "a".repeat(64)
        );
        for (id, y, chr, p_chr, p_line) in [
            ("r0b1p1l1", 10, true, "0.2", "0.97"),
            ("r0b1p1l2", 30, false, "0.9", "0.4599"),
            ("r1b1p1l1", 50, true, "0.4", "0.46"),
            ("r1b1p1l2", 70, false, "null", "null"),
        ] {
            let scores = if p_line == "null" {
                "\"p_chr\":null,\"p_cnn\":null,\"p_line\":null,\"p_v2\":null".to_string()
            } else {
                format!("\"p_chr\":{p_chr},\"p_cnn\":0.5,\"p_line\":{p_line},\"p_v2\":0.5")
            };
            s += &format!(
                "{{\"box\":[10,{y},90,{}],\"chr\":{chr},\"id\":\"{id}\",\"kind\":\"line\",{scores}}}\n",
                y + 10
            );
        }
        s
    }

    #[test]
    fn reads_the_v2_format_with_calls_on_the_ensemble_score() {
        let b = parse(v2_file("[]").as_bytes()).unwrap();
        let ids: Vec<String> = b.cherokee().into_iter().map(|l| l.id).collect();
        assert_eq!(ids, ["r0b1p1l1", "r1b1p1l1"]);
        assert_eq!(b.page.chr_threshold, Some(0.46));
        assert!(!b.page.held);
        let held = parse(v2_file("[\"merged_lines\",\"faded\"]").as_bytes()).unwrap();
        assert!(held.page.held);
    }

    #[test]
    fn refuses_a_v2_file_whose_calls_or_holds_disagree() {
        let good = v2_file("[]");
        let edit = |from: &str, to: &str| good.replacen(from, to, 1);
        for bad in [
            // The shape score's call instead of the ensemble's.
            edit(
                "\"chr\":false,\"id\":\"r0b1p1l2\"",
                "\"chr\":true,\"id\":\"r0b1p1l2\"",
            ),
            edit(
                "\"chr\":true,\"id\":\"r0b1p1l1\"",
                "\"chr\":false,\"id\":\"r0b1p1l1\"",
            ),
            edit("\"held\":false", "\"held\":true"),
            edit("\"held_reasons\":[]", "\"held_reasons\":[\"tilted\"]"),
            edit(",\"held_reasons\":[]", ""),
            edit(",\"contrast\":181.0", ""),
            edit("\"chr_threshold\":0.46", "\"chr_threshold\":1.5"),
            edit("\"p_line\":0.97", "\"p_line\":1.97"),
        ] {
            assert!(parse(bad.as_bytes()).is_err(), "{bad}");
        }
        assert!(parse(v2_file("[\"faded\",\"faded\"]").as_bytes()).is_err());
    }
}
