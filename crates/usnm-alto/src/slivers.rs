//! Sliver words: a column rule, a speck or the edge of a crease that the
//! engine read as a letter.
//!
//! OCR reads a rule as `|`, `l` or `I`, and in the Cherokee syllabary as Ꭵ,
//! Ꮄ or Ꭹ. Its box is a few pixels wide, far narrower than any printed
//! letter of the line's type, so a word is a sliver when its box is
//! narrower than [`MIN_LETTER`] times the line's height for each letter it
//! holds. The line's height is the median height of its word boxes
//! ([`line_height`]), so the rule works at any scan size.
//! [`drop_slivers`] removes those words from the page before the worker
//! writes the stored ALTO and derives the text, so the two still hold the
//! same words.
//!
//! Only letters, digits and rule-like marks (`|`, `¦`, `‖`) count: a word of
//! punctuation alone (`.`, `,`, `'`) is narrow in print and is kept. A
//! smudge read as a whole word with a plausible width is not a sliver and
//! stays.

use crate::model::{Granularity, Line, Page};

/// The narrowest a word may be, in line heights for each letter it holds.
///
/// Measured on LoC's ALTO for the 324 _Cherokee Phoenix_ pages (#1621): a
/// printed `I` or `1` is 0.25 to 0.6 line heights wide, and boxes on rules
/// and specks read as `|`, `l`, `Ꭵ` and the like gather at 0.02 to 0.08,
/// with few words between 0.1 and 0.2.
pub const MIN_LETTER: f64 = 0.15;

/// What [`drop_slivers`] removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Slivers {
    /// Words dropped.
    pub words: usize,
    /// Lines left with no word, and dropped with their last word.
    pub lines: usize,
}

/// Rule-like marks an engine prints for a rule, counted as letters.
fn rule_mark(c: char) -> bool {
    matches!(
        c,
        '|' | '¦' | '‖' | '│' | '┃' | '∣' | 'ǀ' | 'ǁ' | '丨' | '｜'
    )
}

/// The letters `text` holds, for the width test: letters, digits and
/// rule-like marks.
pub fn letters(text: &str) -> usize {
    text.chars()
        .filter(|&c| c.is_alphanumeric() || rule_mark(c))
        .count()
}

/// Whether a word with box width `width` and `text` is a sliver in a line
/// `height` tall.
pub fn is_sliver(width: f64, text: &str, height: f64) -> bool {
    is_sliver_below(width, text, height, MIN_LETTER)
}

/// [`is_sliver`] with another threshold, in line heights a letter: for
/// measuring the threshold.
pub fn is_sliver_below(width: f64, text: &str, height: f64, min_letter: f64) -> bool {
    let n = letters(text);
    n > 0 && height.is_finite() && height > 0.0 && width < min_letter * height * n as f64
}

/// The height a line's letters are measured against: the median height of
/// its word boxes. An engine's line box can run to twice the type's height
/// (LoC's often do, over a crooked line), and the median isn't moved by a
/// tall rule or a speck in the line. A line without word boxes of positive
/// height falls back to its own box.
pub fn line_height(line: &Line) -> f64 {
    let mut hs: Vec<f64> = line
        .words
        .iter()
        .map(|w| w.rect.height())
        .filter(|h| h.is_finite() && *h > 0.0)
        .collect();
    if hs.is_empty() {
        return line.rect.height();
    }
    hs.sort_by(f64::total_cmp);
    let n = hs.len();
    if n % 2 == 1 {
        hs[n / 2]
    } else {
        (hs[n / 2 - 1] + hs[n / 2]) / 2.0
    }
}

fn drop_in(line: &mut Line) -> usize {
    let h = line_height(line);
    let before = line.words.len();
    line.words
        .retain(|w| !is_sliver(w.rect.width(), w.text.trim(), h));
    before - line.words.len()
}

/// Removes sliver words from `page`, and lines left with no word.
///
/// Only a page with word boxes is changed: a page read by line, block or
/// page has no word widths to measure. Blocks keep their boxes, and lines
/// that keep a word keep theirs and their hyphen.
pub fn drop_slivers(page: &mut Page) -> Slivers {
    let mut out = Slivers::default();
    if page.granularity != Granularity::Word {
        return out;
    }
    for b in page.blocks.iter_mut().filter(|b| b.kind.is_text()) {
        b.lines.retain_mut(|l| {
            let had = !l.words.is_empty();
            let n = drop_in(l);
            out.words += n;
            if had && n > 0 && l.words.is_empty() {
                out.lines += 1;
                return false;
            }
            true
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Block, BlockKind, Rect, Word};

    /// A line 40 pixels tall, its words given by `(x0, x1, text)`.
    fn line(words: &[(f64, f64, &str)]) -> Line {
        let words: Vec<Word> = words
            .iter()
            .map(|&(x0, x1, t)| Word {
                rect: Rect::new(x0, 100.0, x1, 140.0),
                text: t.into(),
                conf: Some(0.9),
            })
            .collect();
        let rect = Rect::union_all(words.iter().map(|w| &w.rect)).unwrap();
        Line {
            rect,
            words,
            hyphen: None,
        }
    }

    fn page(lines: Vec<Line>, granularity: Granularity) -> Page {
        Page {
            width: 2000.0,
            height: 3000.0,
            granularity,
            blocks: vec![Block {
                kind: BlockKind::Text,
                rect: Rect::new(0.0, 0.0, 2000.0, 3000.0),
                lang: None,
                lines,
            }],
        }
    }

    fn texts(p: &Page) -> Vec<Vec<String>> {
        p.blocks[0]
            .lines
            .iter()
            .map(|l| l.words.iter().map(|w| w.text.clone()).collect())
            .collect()
    }

    /// A column rule two pixels wide, read as `Ꭵ` beside a Cherokee word
    /// and as `|` beside English ones, is dropped.
    #[test]
    fn a_rule_read_as_a_letter_is_dropped() {
        let mut p = page(
            vec![
                line(&[(100.0, 102.0, "Ꭵ"), (120.0, 230.0, "ᎠᏆᏤᎵ")]),
                line(&[(100.0, 180.0, "the"), (190.0, 192.0, "|"), (200.0, 300.0, "news")]),
            ],
            Granularity::Word,
        );
        let s = drop_slivers(&mut p);
        assert_eq!(s, Slivers { words: 2, lines: 0 });
        assert_eq!(texts(&p), [vec!["ᎠᏆᏤᎵ"], vec!["the", "news"]]);
        let text = crate::text(&p);
        assert_eq!(text, "ᎠᏆᏤᎵ\nthe news");
        let xml = crate::write(&p, &crate::Description::default(), "chr");
        assert!(!xml.contains("CONTENT=\"Ꭵ\"") && !xml.contains("CONTENT=\"|\""));
    }

    /// A printed `I`, `l` or `i` is narrow, but not that narrow: over a
    /// quarter of the line's height survives. So does punctuation, which has
    /// no letter to measure.
    #[test]
    fn real_narrow_letters_and_punctuation_survive() {
        let mut p = page(
            vec![
                line(&[(100.0, 111.0, "I"), (130.0, 220.0, "said")]),
                line(&[(100.0, 112.0, "l"), (118.0, 129.0, "i"), (140.0, 152.0, "1.")]),
                line(&[(100.0, 104.0, "."), (110.0, 114.0, ","), (120.0, 123.0, "'")]),
            ],
            Granularity::Word,
        );
        let before = p.clone();
        assert_eq!(drop_slivers(&mut p), Slivers::default());
        assert_eq!(p, before);
    }

    /// A word of several letters whose box was squashed to a sliver is
    /// dropped; the same box holding one narrow letter would be kept, and a
    /// line left empty goes with it.
    #[test]
    fn a_multi_letter_word_in_a_squashed_box_is_dropped() {
        // 15 pixels: a fair `Ꭵ` (0.375 line heights), too narrow for three
        // letters (0.125 a letter).
        let mut p = page(
            vec![
                line(&[(100.0, 115.0, "Ꭵ"), (130.0, 220.0, "ᎠᎴ")]),
                line(&[(100.0, 115.0, "ill"), (130.0, 220.0, "news")]),
                line(&[(500.0, 515.0, "ᏥᏌᎢ")]),
            ],
            Granularity::Word,
        );
        let s = drop_slivers(&mut p);
        assert_eq!(s, Slivers { words: 2, lines: 1 });
        assert_eq!(texts(&p), [vec!["Ꭵ", "ᎠᎴ"], vec!["news"]]);
    }

    /// A line box twice the type's height (LoC's, over a crooked line)
    /// doesn't make real words narrow: the words' own median height is the
    /// measure, and a rule taller than the type doesn't move it.
    #[test]
    fn the_line_is_measured_by_its_words_not_its_box() {
        let mut l = line(&[
            (100.0, 125.0, "it"),
            (140.0, 141.5, "|"),
            (160.0, 240.0, "was"),
        ]);
        l.rect = Rect::new(100.0, 80.0, 240.0, 180.0);
        l.words[1].rect = Rect::new(140.0, 60.0, 141.5, 200.0);
        assert_eq!(line_height(&l), 40.0);
        let mut p = page(vec![l], Granularity::Word);
        assert_eq!(drop_slivers(&mut p), Slivers { words: 1, lines: 0 });
        assert_eq!(texts(&p), [vec!["it", "was"]]);
    }

    /// Only word boxes are measured: a page read by line has words whose
    /// boxes are the line's, and is left alone.
    #[test]
    fn only_pages_with_word_boxes_are_changed() {
        let mut p = page(vec![line(&[(100.0, 102.0, "|")])], Granularity::Line);
        let before = p.clone();
        assert_eq!(drop_slivers(&mut p), Slivers::default());
        assert_eq!(p, before);
    }

    #[test]
    fn letters_count_letters_digits_and_rules() {
        assert_eq!(letters("ᎠᏆᏤᎵ,"), 4);
        assert_eq!(letters("\"I.\""), 1);
        assert_eq!(letters("24."), 2);
        assert_eq!(letters("|"), 1);
        assert_eq!(letters("..."), 0);
        assert!(!is_sliver(1.0, ".", 40.0));
        assert!(!is_sliver(1.0, "|", 0.0));
    }
}
