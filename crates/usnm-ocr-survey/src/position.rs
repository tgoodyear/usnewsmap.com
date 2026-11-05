//! OCR engines scored by where each word sits on the page (#1438), so that reading errors and
//! layout errors are counted apart. docs/ocr-gold-set.md#word-position-against-alto has the
//! measures and what they can tell.
//!
//! - [`compare`]: two word-level ALTO readings of one page image, matched word by word on their
//!   boxes. A matched pair with different text is a substitution, a reference word with no match a
//!   deletion and a hypothesis word with no match an insertion. Reading order is a separate number
//!   over the matched words ([`Order`]).
//! - [`crop`]: a word-level ALTO clipped to a crop's box ([`clip`]) and compared with a crop-level
//!   text, such as a vision-language engine's reading of the crop or a transcription: text-order
//!   CER and WER of the clipped words in the ALTO's reading order, and the word-bag overlap.
//!
//! **Matching.** Two words are candidates when their boxes' intersection over union is at least
//! [`MIN_IOU`]. Candidates are taken greedily, the highest IoU first, ties by the reference word's
//! reading-order index and then the hypothesis word's, and a pair is kept when neither word is
//! matched yet: one-to-one, and a function of the two pages alone. Above an IoU of one half a box
//! meets at most one of a set of disjoint boxes, so when neither page's word boxes overlap each
//! other and no pair sits at exactly one half, the candidates are already one-to-one and the
//! greedy order changes nothing.
//!
//! **Text.** Each word is normalized as the gold set normalizes an engine's text
//! ([`crate::text::normalize`]: NFC, whitespace squashed; case, punctuation and diacritics count).
//! A line's hyphen (ALTO's `HYP`) is kept on the line's last word, so `Clare` with a `HYP` and
//! `Clare-` are the same word. Character and word edits come from [`crate::score::counts`], as
//! the gold set's CER and WER.

use crate::score::{self, Counts};
use crate::text;
use anyhow::{anyhow, ensure};
use serde::Serialize;
use usnm_alto::{Granularity, Page, ParseOptions, Rect};
use usnm_ocr_contract::quality::round6;
use usnm_ocr_quality::{agreement, slot};

/// Two words are the same word on the page when their boxes' intersection over union is at least
/// this. One half is the usual threshold for matching detections: it allows a few pixels of
/// shift or padding on a word's box, and a box covering two words of about the same width (two
/// words read as one) matches neither.
pub const MIN_IOU: f64 = 0.5;

/// Cells on each side of the grid that finds the boxes a box meets.
const GRID: usize = 64;

/// Reads an ALTO file into a page, in `frame` (width and height) or, without one, in the ALTO's
/// own units: any ALTO `usnm_alto` reads, NDNP's early CCS ALTO included, through
/// [`slot::parse_alto`]. Matching compares boxes by proportion, so LoC's 1/1200 inch needs no dpi.
/// The page must have a box for every word.
pub fn load(xml: &[u8], frame: Option<(f64, f64)>) -> anyhow::Result<Page> {
    let xml = slot::normalize_alto_ns(xml);
    let (width, height) = match frame {
        Some(f) => f,
        None => {
            let p = usnm_alto::check(&xml, &ParseOptions::default().limits)
                .map_err(|e| anyhow!("ALTO refused: {e}"))?;
            (p.width, p.height)
        }
    };
    let page = slot::parse_alto(&xml, width, height)?;
    ensure!(
        page.granularity == Granularity::Word,
        "the ALTO has no box for every word (granularity {})",
        page.granularity.as_str()
    );
    Ok(page)
}

/// A word of a page, in the page's reading order.
#[derive(Debug, Clone, PartialEq)]
pub struct PageWord {
    /// The word's place among the page's words in reading order, from 0.
    pub index: usize,
    pub rect: Rect,
    /// Normalized; a line's last word carries the line's hyphen.
    pub text: String,
}

/// The words of a page's text blocks in reading order. Words that normalize to nothing are left
/// out, as the stored ALTO and the page text leave them out.
pub fn words(page: &Page) -> Vec<PageWord> {
    let mut out: Vec<PageWord> = Vec::new();
    for b in page.blocks.iter().filter(|b| b.kind.is_text()) {
        for l in &b.lines {
            let start = out.len();
            for w in &l.words {
                let t = text::normalize(&w.text, false);
                if !t.is_empty() {
                    out.push(PageWord {
                        index: out.len(),
                        rect: w.rect,
                        text: t,
                    });
                }
            }
            if let (Some(h), true) = (&l.hyphen, out.len() > start) {
                if let Some(last) = out.last_mut() {
                    last.text = text::normalize(&format!("{}{h}", last.text), false);
                }
            }
        }
    }
    out
}

/// The intersection over union of two boxes; 0 when neither has any area.
pub fn iou(a: &Rect, b: &Rect) -> f64 {
    let w = (a.x1.min(b.x1) - a.x0.max(b.x0)).max(0.0);
    let h = (a.y1.min(b.y1) - a.y0.max(b.y0)).max(0.0);
    let inter = w * h;
    let union = a.width() * a.height() + b.width() * b.height() - inter;
    if union > 0.0 {
        inter / union
    } else {
        0.0
    }
}

/// A uniform grid over the boxes' extent, each cell listing the boxes that touch it, so a box is
/// compared only with the boxes near it.
struct Grid {
    origin: (f64, f64),
    cell: (f64, f64),
    cells: Vec<Vec<usize>>,
}

impl Grid {
    fn new(rects: &[Rect], extent: Rect) -> Self {
        let size = |len: f64| {
            let s = len / GRID as f64;
            if s > 0.0 {
                s
            } else {
                1.0
            }
        };
        let mut g = Grid {
            origin: (extent.x0, extent.y0),
            cell: (size(extent.width()), size(extent.height())),
            cells: vec![Vec::new(); GRID * GRID],
        };
        for (i, r) in rects.iter().enumerate() {
            for c in g.span(r) {
                g.cells[c].push(i);
            }
        }
        g
    }

    /// The cells `r` touches.
    fn span(&self, r: &Rect) -> impl Iterator<Item = usize> {
        let at = |v: f64, o: f64, s: f64| (((v - o) / s).floor().max(0.0) as usize).min(GRID - 1);
        let (cx0, cx1) = (
            at(r.x0, self.origin.0, self.cell.0),
            at(r.x1, self.origin.0, self.cell.0),
        );
        let (cy0, cy1) = (
            at(r.y0, self.origin.1, self.cell.1),
            at(r.y1, self.origin.1, self.cell.1),
        );
        (cy0..=cy1).flat_map(move |y| (cx0..=cx1).map(move |x| y * GRID + x))
    }
}

/// The one-to-one matching of reference words to hypothesis words described in the module's
/// documentation, as (reference index, hypothesis index) pairs sorted by the reference index.
pub fn match_words(reference: &[PageWord], hypothesis: &[PageWord]) -> Vec<(usize, usize)> {
    let hyp: Vec<Rect> = hypothesis.iter().map(|w| w.rect).collect();
    let Some(extent) = Rect::union_all(hyp.iter().chain(reference.iter().map(|w| &w.rect))) else {
        return Vec::new();
    };
    let grid = Grid::new(&hyp, extent);
    let mut candidates: Vec<(f64, usize, usize)> = Vec::new();
    // The last reference word (plus one) each hypothesis word was compared with, so a word in
    // several cells is compared once.
    let mut seen = vec![0; hypothesis.len()];
    for (i, r) in reference.iter().enumerate() {
        for c in grid.span(&r.rect) {
            for &j in &grid.cells[c] {
                if seen[j] == i + 1 {
                    continue;
                }
                seen[j] = i + 1;
                let v = iou(&r.rect, &hyp[j]);
                if v >= MIN_IOU {
                    candidates.push((v, i, j));
                }
            }
        }
    }
    candidates.sort_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
    let mut ref_used = vec![false; reference.len()];
    let mut hyp_used = vec![false; hypothesis.len()];
    let mut pairs = Vec::new();
    for (_, i, j) in candidates {
        if !ref_used[i] && !hyp_used[j] {
            ref_used[i] = true;
            hyp_used[j] = true;
            pairs.push((i, j));
        }
    }
    pairs.sort_unstable();
    pairs
}

/// How far the hypothesis reads the matched words in the reference's order.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Order {
    /// Matched words.
    pub words: usize,
    /// Kendall's tau between the two reading orders of the matched words: 1 when the hypothesis
    /// reads them in the reference's order, -1 when it reads them backwards. Global: two columns
    /// read in the other order pull it down by the share of word pairs that cross them. None for
    /// fewer than two words.
    pub kendall_tau: Option<f64>,
    /// The share of consecutive matched words in the reference's order that the hypothesis also
    /// reads in that order. Local: two columns read in the other order break one seam. None for
    /// fewer than two words.
    pub adjacent_in_order: Option<f64>,
}

/// Pairs of `v` out of order, by merge sort.
fn inversions(v: &mut [usize]) -> u64 {
    let n = v.len();
    if n < 2 {
        return 0;
    }
    let (left, right) = v.split_at_mut(n / 2);
    let mut count = inversions(left) + inversions(right);
    let mut merged = Vec::with_capacity(n);
    let (mut i, mut j) = (0, 0);
    while i < left.len() && j < right.len() {
        if left[i] <= right[j] {
            merged.push(left[i]);
            i += 1;
        } else {
            merged.push(right[j]);
            count += (left.len() - i) as u64;
            j += 1;
        }
    }
    merged.extend_from_slice(&left[i..]);
    merged.extend_from_slice(&right[j..]);
    v.copy_from_slice(&merged);
    count
}

/// Reading-order agreement over `pairs` sorted by the reference index, as [`match_words`] gives.
pub fn order(pairs: &[(usize, usize)]) -> Order {
    let k = pairs.len();
    if k < 2 {
        return Order {
            words: k,
            kendall_tau: None,
            adjacent_in_order: None,
        };
    }
    let in_order = pairs.windows(2).filter(|p| p[0].1 < p[1].1).count();
    let mut hyp: Vec<usize> = pairs.iter().map(|p| p.1).collect();
    let all = (k as u64) * (k as u64 - 1) / 2;
    let inv = inversions(&mut hyp);
    Order {
        words: k,
        kendall_tau: Some(round6(1.0 - 2.0 * inv as f64 / all as f64)),
        adjacent_in_order: Some(round6(in_order as f64 / (k - 1) as f64)),
    }
}

fn ratio(n: usize, d: usize) -> Option<f64> {
    (d > 0).then(|| round6(n as f64 / d as f64))
}

/// Character and word edits of one text against another, with their rates (none for an empty
/// reference).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct TextScore {
    #[serde(flatten)]
    pub counts: Counts,
    pub cer: Option<f64>,
    pub wer: Option<f64>,
}

impl From<Counts> for TextScore {
    fn from(counts: Counts) -> Self {
        TextScore {
            cer: ratio(counts.char_edits, counts.ref_chars),
            wer: ratio(counts.word_edits, counts.ref_words),
            counts,
        }
    }
}

/// A word with no match.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Unmatched {
    pub index: usize,
    pub text: String,
    /// `[x0, y0, x1, y1]` in the reference's frame; a hypothesis word's box is mapped onto it.
    pub rect: [f64; 4],
}

impl From<&PageWord> for Unmatched {
    fn from(w: &PageWord) -> Self {
        Unmatched {
            index: w.index,
            text: w.text.clone(),
            rect: [w.rect.x0, w.rect.y0, w.rect.x1, w.rect.y1].map(round6),
        }
    }
}

/// Two readings of a page compared by word position.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Comparison {
    pub min_iou: f64,
    pub reference_words: usize,
    pub hypothesis_words: usize,
    /// Pairs matched by position.
    pub matched: usize,
    /// Matched pairs with the same text.
    pub correct: usize,
    /// Matched pairs with different text.
    pub substitutions: usize,
    /// Reference words with no match.
    pub deletions: usize,
    /// Hypothesis words with no match.
    pub insertions: usize,
    /// `(substitutions + deletions + insertions) / reference_words`.
    pub wer: Option<f64>,
    /// Character edits within the matched pairs, over the matched reference words' characters:
    /// how well the hypothesis read the words both readers found.
    pub matched_chars: TextScore,
    pub order: Order,
    /// Each page's whole text in its own reading order against the other's, as a text-order score
    /// sees them, for comparison.
    pub text_order: TextScore,
    /// The words with no match, with `list_unmatched`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmatched_reference: Option<Vec<Unmatched>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unmatched_hypothesis: Option<Vec<Unmatched>>,
}

/// Compares `hypothesis` with `reference` by word position. Both are readings of one image; the
/// hypothesis is mapped onto the reference's frame by proportion.
pub fn compare(reference: &Page, hypothesis: &Page, list_unmatched: bool) -> Comparison {
    let mut hyp_page = hypothesis.clone();
    if (hyp_page.width, hyp_page.height) != (reference.width, reference.height) {
        hyp_page.rescale(reference.width, reference.height);
    }
    let (r, h) = (words(reference), words(&hyp_page));
    let pairs = match_words(&r, &h);
    let (mut ref_matched, mut hyp_matched) = (vec![false; r.len()], vec![false; h.len()]);
    let (mut correct, mut ref_chars, mut char_edits) = (0, 0, 0);
    for &(i, j) in &pairs {
        ref_matched[i] = true;
        hyp_matched[j] = true;
        let c = score::counts(&r[i].text, &h[j].text);
        correct += usize::from(c.char_edits == 0);
        ref_chars += c.ref_chars;
        char_edits += c.char_edits;
    }
    let substitutions = pairs.len() - correct;
    let deletions = r.len() - pairs.len();
    let insertions = h.len() - pairs.len();
    let unmatched = |ws: &[PageWord], used: &[bool]| {
        list_unmatched.then(|| {
            ws.iter()
                .zip(used)
                .filter(|(_, u)| !**u)
                .map(|(w, _)| Unmatched::from(w))
                .collect()
        })
    };
    let text_order = score::counts(
        &text::normalize(&usnm_alto::text(reference), false),
        &text::normalize(&usnm_alto::text(&hyp_page), false),
    );
    Comparison {
        min_iou: MIN_IOU,
        reference_words: r.len(),
        hypothesis_words: h.len(),
        matched: pairs.len(),
        correct,
        substitutions,
        deletions,
        insertions,
        wer: ratio(substitutions + deletions + insertions, r.len()),
        matched_chars: Counts {
            ref_chars,
            char_edits,
            ref_words: pairs.len(),
            word_edits: substitutions,
        }
        .into(),
        order: order(&pairs),
        text_order: text_order.into(),
        unmatched_reference: unmatched(&r, &ref_matched),
        unmatched_hypothesis: unmatched(&h, &hyp_matched),
    }
}

/// Whether `r`'s centre is in `crop`: `x0 <= cx < x1` and `y0 <= cy < y1`, the rule of the gold
/// set's `crops.py` and of `usnm_ocr_quality::crops::text_in_box`.
fn centre_in(r: &Rect, crop: &Rect) -> bool {
    let (cx, cy) = r.centre();
    crop.x0 <= cx && cx < crop.x1 && crop.y0 <= cy && cy < crop.y1
}

/// The page with only the words whose box's centre is in `crop`, in the page's reading order.
/// Lines and blocks left with no word are dropped, and a line keeps its hyphen only when it keeps
/// its last word. A kept hyphen joins the next kept line's first word even when the box cut off
/// the word that followed it on the page: the crop's image shows the same two lines, so its own
/// text joins the same pair when normalized.
pub fn clip(page: &Page, crop: &Rect) -> Page {
    let mut blocks = Vec::new();
    for b in page.blocks.iter().filter(|b| b.kind.is_text()) {
        let mut lines = Vec::new();
        for l in &b.lines {
            let words: Vec<_> = l
                .words
                .iter()
                .filter(|w| centre_in(&w.rect, crop))
                .cloned()
                .collect();
            if words.is_empty() {
                continue;
            }
            let last_kept = l.words.last().is_some_and(|w| centre_in(&w.rect, crop));
            lines.push(usnm_alto::Line {
                rect: l.rect,
                words,
                hyphen: l.hyphen.clone().filter(|_| last_kept),
            });
        }
        if !lines.is_empty() {
            blocks.push(usnm_alto::Block {
                kind: b.kind,
                rect: b.rect,
                lang: b.lang.clone(),
                lines,
            });
        }
    }
    Page {
        width: page.width,
        height: page.height,
        granularity: page.granularity,
        blocks,
    }
}

/// The word bags of the clipped ALTO and the crop's text: lower-case runs of letters and digits,
/// as the text agreement counts them ([`agreement::compare`]).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Bag {
    pub alto_words: usize,
    pub text_words: usize,
    /// Words the two bags share, the smaller count of each.
    pub shared_words: usize,
    /// `shared_words / alto_words`: how much of the ALTO's text the crop's text has.
    pub cover_alto: Option<f64>,
    /// `shared_words / text_words`.
    pub cover_text: Option<f64>,
    pub dice: f64,
}

/// A clipped ALTO against a crop's text.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct CropScore {
    /// Words of the ALTO in the box.
    pub clipped_words: usize,
    /// `alto` or `text`: which side the rates are over.
    pub reference: &'static str,
    /// The clipped words in the ALTO's reading order against the crop's text.
    pub text_order: TextScore,
    pub bag: Bag,
}

/// Clips `page` to `rect` and compares its words in reading order with `crop_text`. By default
/// the clipped ALTO is the reference and both are normalized as engine text; with
/// `text_is_reference` the text is a transcription, normalized as one (`[?x]` read as `x`, `[?]`
/// dropped), and the rates are over it.
pub fn crop(page: &Page, rect: &Rect, crop_text: &str, text_is_reference: bool) -> CropScore {
    let clipped = clip(page, rect);
    let alto = text::normalize(&usnm_alto::text(&clipped), false);
    let other = text::normalize(crop_text, text_is_reference);
    let counts = if text_is_reference {
        score::counts(&other, &alto)
    } else {
        score::counts(&alto, &other)
    };
    let a = agreement::compare(&alto, &other);
    CropScore {
        clipped_words: words(&clipped).len(),
        reference: if text_is_reference { "text" } else { "alto" },
        text_order: counts.into(),
        bag: Bag {
            alto_words: a.bag_words_a,
            text_words: a.bag_words_b,
            shared_words: a.shared_words,
            cover_alto: a.cover_a,
            cover_text: a.cover_b,
            dice: a.pair.dice,
        },
    }
}

/// `X0,Y0,X1,Y1` as a box with `x0 < x1` and `y0 < y1`.
pub fn parse_box(s: &str) -> anyhow::Result<Rect> {
    let v: Vec<f64> = s
        .split(',')
        .map(|p| p.trim().parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| anyhow!("--box wants X0,Y0,X1,Y1, got {s:?}"))?;
    let &[x0, y0, x1, y1] = v.as_slice() else {
        return Err(anyhow!("--box wants X0,Y0,X1,Y1, got {s:?}"));
    };
    ensure!(
        [x0, y0, x1, y1].iter().all(|x| x.is_finite()) && x0 < x1 && y0 < y1,
        "--box wants X0 < X1 and Y0 < Y1, got {s:?}"
    );
    Ok(Rect::new(x0, y0, x1, y1))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A word: its text and box `[x0, y0, x1, y1]` in pixels.
    type W<'a> = (&'a str, [u32; 4]);

    /// Synthetic ALTO 3.1 of a 1000 by 1000 pixel page: blocks of lines of words, in reading
    /// order. A line's hyphen follows its last word.
    fn alto(blocks: &[&[(&[W<'_>], bool)]]) -> String {
        let mut s = String::from(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<alto xmlns="http://www.loc.gov/standards/alto/ns-v3#"><Description><MeasurementUnit>pixel</MeasurementUnit></Description>
<Layout><Page WIDTH="1000" HEIGHT="1000" PHYSICAL_IMG_NR="1"><PrintSpace>"#,
        );
        for b in blocks {
            s.push_str("<TextBlock>");
            for (line, hyphen) in *b {
                s.push_str("<TextLine>");
                for (t, [x0, y0, x1, y1]) in *line {
                    s.push_str(&format!(
                        r#"<String CONTENT="{t}" HPOS="{x0}" VPOS="{y0}" WIDTH="{}" HEIGHT="{}"/>"#,
                        x1 - x0,
                        y1 - y0
                    ));
                }
                if *hyphen {
                    s.push_str(r#"<HYP CONTENT="-"/>"#);
                }
                s.push_str("</TextLine>");
            }
            s.push_str("</TextBlock>");
        }
        s.push_str("</PrintSpace></Page></Layout></alto>\n");
        s
    }

    fn page(blocks: &[&[(&[W<'_>], bool)]]) -> Page {
        load(alto(blocks).as_bytes(), None).unwrap()
    }

    /// Two blocks side by side, two lines each.
    const LEFT: [&[W<'static>]; 2] = [
        &[
            ("The", [100, 100, 160, 130]),
            ("quick", [170, 100, 260, 130]),
        ],
        &[
            ("brown", [100, 140, 190, 170]),
            ("fox", [200, 140, 250, 170]),
        ],
    ];
    const RIGHT: [&[W<'static>]; 2] = [
        &[
            ("jumps", [600, 100, 690, 130]),
            ("over", [700, 100, 770, 130]),
        ],
        &[
            ("lazy", [600, 140, 670, 170]),
            ("dogs", [680, 140, 750, 170]),
        ],
    ];

    fn lines(ls: &[&'static [W<'static>]]) -> Vec<(&'static [W<'static>], bool)> {
        ls.iter().map(|l| (*l, false)).collect()
    }

    fn owned_lines<'a>(v: &'a [Vec<W<'static>>]) -> Vec<(&'a [W<'static>], bool)> {
        v.iter().map(|l| (&l[..], false)).collect()
    }

    fn two_columns() -> Page {
        page(&[&lines(&LEFT), &lines(&RIGHT)])
    }

    #[test]
    fn identical_pages_agree_everywhere() {
        let c = compare(&two_columns(), &two_columns(), true);
        assert_eq!((c.reference_words, c.hypothesis_words), (8, 8));
        assert_eq!((c.matched, c.correct), (8, 8));
        assert_eq!((c.substitutions, c.deletions, c.insertions), (0, 0, 0));
        assert_eq!(c.wer, Some(0.0));
        assert_eq!(c.matched_chars.cer, Some(0.0));
        assert_eq!(c.order.kendall_tau, Some(1.0));
        assert_eq!(c.order.adjacent_in_order, Some(1.0));
        assert_eq!(c.text_order.cer, Some(0.0));
        assert_eq!(c.unmatched_reference, Some(vec![]));
        assert_eq!(c.unmatched_hypothesis, Some(vec![]));
    }

    #[test]
    fn a_misread_word_is_a_substitution() {
        let left: [&[W<'_>]; 2] = [
            LEFT[0],
            &[
                ("brown", [100, 140, 190, 170]),
                ("f0x", [200, 140, 250, 170]),
            ],
        ];
        let c = compare(
            &two_columns(),
            &page(&[&lines(&left), &lines(&RIGHT)]),
            false,
        );
        assert_eq!((c.matched, c.correct, c.substitutions), (8, 7, 1));
        assert_eq!((c.deletions, c.insertions), (0, 0));
        assert_eq!(c.wer, Some(0.125));
        assert_eq!(c.matched_chars.counts.char_edits, 1);
        assert_eq!(c.order.kendall_tau, Some(1.0));
        assert_eq!(c.unmatched_reference, None);
    }

    #[test]
    fn a_missing_and_an_extra_word_are_a_deletion_and_an_insertion() {
        // "fox" is not read; "Extra" is read where the reference has nothing.
        let left: [&[W<'_>]; 3] = [
            LEFT[0],
            &[("brown", [100, 140, 190, 170])],
            &[("Extra", [100, 400, 200, 430])],
        ];
        let c = compare(
            &two_columns(),
            &page(&[&lines(&left), &lines(&RIGHT)]),
            true,
        );
        assert_eq!((c.matched, c.correct, c.substitutions), (7, 7, 0));
        assert_eq!((c.deletions, c.insertions), (1, 1));
        assert_eq!(c.wer, Some(0.25));
        let r = c.unmatched_reference.unwrap();
        assert_eq!((r[0].index, r[0].text.as_str()), (3, "fox"));
        assert_eq!(r[0].rect, [200.0, 140.0, 250.0, 170.0]);
        let h = c.unmatched_hypothesis.unwrap();
        assert_eq!((h[0].index, h[0].text.as_str()), (3, "Extra"));
        assert_eq!(c.order.kendall_tau, Some(1.0));
    }

    #[test]
    fn a_box_shifted_below_the_threshold_matches_nothing() {
        // 50 by 30 at x 200; moved 17 px right the IoU is 33 * 30 / (2 * 1500 - 990) = 0.4925.
        // Moved 16 px it is 34 / 66 = 0.515 and still matches.
        let moved = |dx: u32| {
            let line: Vec<W<'_>> = vec![
                ("brown", [100, 140, 190, 170]),
                ("fox", [200 + dx, 140, 250 + dx, 170]),
            ];
            let left: Vec<(&[W<'_>], bool)> = vec![(LEFT[0], false), (&line[..], false)];
            compare(&two_columns(), &page(&[&left, &lines(&RIGHT)]), true)
        };
        let c = moved(16);
        assert_eq!((c.matched, c.deletions, c.insertions), (8, 0, 0));
        let c = moved(17);
        assert!(
            iou(
                &Rect::new(200.0, 140.0, 250.0, 170.0),
                &Rect::new(217.0, 140.0, 267.0, 170.0)
            ) < MIN_IOU
        );
        assert_eq!((c.matched, c.correct, c.substitutions), (7, 7, 0));
        assert_eq!((c.deletions, c.insertions), (1, 1));
        assert_eq!(c.unmatched_hypothesis.unwrap()[0].text, "fox");
        // The text is the same in the same order: a text-order score sees no error.
        assert_eq!(c.text_order.cer, Some(0.0));
    }

    #[test]
    fn swapped_reading_order_is_an_order_error_not_a_text_error() {
        // The same words in the same places, the columns read right first.
        let c = compare(
            &two_columns(),
            &page(&[&lines(&RIGHT), &lines(&LEFT)]),
            false,
        );
        assert_eq!((c.matched, c.correct, c.wer), (8, 8, Some(0.0)));
        // 16 of the 28 word pairs cross the columns, out of order: 1 - 2 * 16 / 28.
        assert_eq!(c.order.kendall_tau, Some(round6(-4.0 / 28.0)));
        // One seam of seven: the left column's last word, then the right's first.
        assert_eq!(c.order.adjacent_in_order, Some(round6(6.0 / 7.0)));
        // A text-order score sees mostly errors: 36 edits over 40 characters.
        assert_eq!(c.text_order.cer, Some(0.9));

        // Every column, line and word read backwards: no pair in order.
        let rev = |ls: [&[W<'static>]; 2]| -> Vec<Vec<W<'static>>> {
            ls.iter()
                .rev()
                .map(|l| l.iter().rev().copied().collect())
                .collect()
        };
        let (l, r) = (rev(LEFT), rev(RIGHT));
        let c = compare(
            &two_columns(),
            &page(&[&owned_lines(&r), &owned_lines(&l)]),
            false,
        );
        assert_eq!((c.matched, c.correct), (8, 8));
        assert_eq!(c.order.kendall_tau, Some(-1.0));
        assert_eq!(c.order.adjacent_in_order, Some(0.0));
    }

    #[test]
    fn hyphen_and_frame_do_not_count_as_errors() {
        // The reference has `qui` with a HYP, the hypothesis `qui-` and the page at half scale.
        let r = page(&[&[(
            &[("The", [100, 100, 160, 130]), ("qui", [170, 100, 260, 130])],
            true,
        )]]);
        let half = alto(&[&[(
            &[("The", [50, 50, 80, 65]), ("qui-", [85, 50, 130, 65])],
            false,
        )]])
        .replace(
            r#"WIDTH="1000" HEIGHT="1000""#,
            r#"WIDTH="500" HEIGHT="500""#,
        );
        let h = load(half.as_bytes(), None).unwrap();
        assert_eq!(h.width, 500.0);
        let c = compare(&r, &h, false);
        assert_eq!((c.matched, c.correct), (2, 2));
    }

    #[test]
    fn matching_is_greedy_by_iou_with_stable_ties() {
        let w = |i: usize, x0: f64, t: &str| PageWord {
            index: i,
            rect: Rect::new(x0, 0.0, x0 + 10.0, 10.0),
            text: t.into(),
        };
        // Two identical hypothesis boxes over one reference word: the first in reading order wins.
        let r = [w(0, 0.0, "a")];
        assert_eq!(
            match_words(&r, &[w(0, 0.0, "b"), w(1, 0.0, "a")]),
            vec![(0, 0)]
        );
        // The better overlap wins over the earlier word.
        let h = [w(0, 2.0, "a"), w(1, 1.0, "a")];
        assert_eq!(match_words(&r, &h), vec![(0, 1)]);
        assert!(match_words(&[], &h).is_empty());
    }

    #[test]
    fn order_counts_inversions() {
        assert_eq!(order(&[]).kendall_tau, None);
        assert_eq!(order(&[(0, 0)]).adjacent_in_order, None);
        let o = order(&[(0, 1), (1, 0), (2, 2), (3, 3)]);
        assert_eq!(o.kendall_tau, Some(round6(1.0 - 2.0 / 6.0)));
        assert_eq!(o.adjacent_in_order, Some(round6(2.0 / 3.0)));
        let mut v = vec![4, 3, 2, 1, 0];
        assert_eq!(inversions(&mut v), 10);
        assert_eq!(v, vec![0, 1, 2, 3, 4]);
    }

    #[test]
    fn crop_clips_at_the_box_by_word_centre() {
        let p = two_columns();
        // "fox" spans x 200 to 250, centre 225: in a box to 226, out of one to 225 (half-open).
        let text = "The quick\nbrown fox";
        let s = crop(&p, &Rect::new(90.0, 90.0, 226.0, 180.0), text, false);
        assert_eq!(s.clipped_words, 4);
        assert_eq!(s.text_order.counts.word_edits, 0);
        let s = crop(&p, &Rect::new(90.0, 90.0, 225.0, 180.0), text, false);
        assert_eq!(s.clipped_words, 3);
        assert_eq!(s.reference, "alto");
        // ALTO "The quick brown" against the text: "fox" inserted.
        assert_eq!(s.text_order.counts.ref_words, 3);
        assert_eq!(s.text_order.counts.word_edits, 1);
        assert_eq!(
            (s.bag.alto_words, s.bag.text_words, s.bag.shared_words),
            (3, 4, 3)
        );
        assert_eq!(
            (s.bag.cover_alto, s.bag.cover_text),
            (Some(1.0), Some(0.75))
        );
        // The whole left column, read in the ALTO's order, against a transcription with a
        // guessed character: the transcription is the reference.
        let s = crop(
            &p,
            &Rect::new(90.0, 90.0, 300.0, 180.0),
            "The qu[?i]ck\nbrown fox",
            true,
        );
        assert_eq!(s.clipped_words, 4);
        assert_eq!(s.reference, "text");
        assert_eq!(s.text_order.cer, Some(0.0));
        assert_eq!(s.bag.dice, 1.0);
        let s = crop(&p, &Rect::new(0.0, 0.0, 10.0, 10.0), "x", false);
        assert_eq!((s.clipped_words, s.text_order.cer), (0, None));
    }

    #[test]
    fn clip_drops_a_hyphen_whose_word_is_cut_off() {
        let p = page(&[&[(
            &[("The", [100, 100, 160, 130]), ("qui", [170, 100, 260, 130])],
            true,
        )]]);
        let cut = clip(&p, &Rect::new(0.0, 0.0, 200.0, 200.0));
        assert_eq!(usnm_alto::text(&cut), "The");
        assert_eq!(
            usnm_alto::text(&clip(&p, &Rect::new(0.0, 0.0, 300.0, 200.0))),
            "The qui-"
        );
    }

    #[test]
    fn a_kept_hyphen_joins_the_next_kept_line_as_the_crops_text_does() {
        let lines: [(&[W<'_>], bool); 2] = [
            (&[("inter", [100, 100, 200, 130])], true),
            (
                &[
                    ("national", [100, 140, 180, 170]),
                    ("today", [300, 140, 400, 170]),
                ],
                false,
            ),
        ];
        // "national" (centre x 140) is cut off, so the crop shows "inter-" above "today".
        let s = crop(
            &page(&[&lines[..]]),
            &Rect::new(145.0, 90.0, 500.0, 180.0),
            "inter-\ntoday",
            false,
        );
        assert_eq!(s.clipped_words, 2);
        assert_eq!(s.text_order.cer, Some(0.0));
        assert_eq!(s.bag.dice, 1.0);
    }

    #[test]
    fn refuses_pages_without_word_boxes_and_bad_boxes() {
        let xml = alto(&[&[(&[("The", [100, 100, 160, 130])], false)]])
            .replace(r#" HPOS="100" VPOS="100" WIDTH="60" HEIGHT="30""#, "");
        let e = load(xml.as_bytes(), None).unwrap_err().to_string();
        assert!(e.contains("no box for every word"), "{e}");
        assert_eq!(parse_box("1,2,3,4").unwrap(), Rect::new(1.0, 2.0, 3.0, 4.0));
        assert!(parse_box("1,2,3").is_err());
        assert!(parse_box("3,2,1,4").is_err());
        assert!(parse_box("a,2,3,4").is_err());
    }
}
