//! Per-word script detection for a Cherokee line read (#1666): which words of a line the
//! syllabary model wrote as syllabary that are really Latin, so the English engine reads them.
//!
//! The _Cherokee Phoenix_ prints English names and transliterations inside syllabary lines.
//! The syllabary model writes them as syllabary, and Tesseract's own two-language mode
//! (`chr+eng`) gets the other half of the job wrong: it re-reads low-confidence words in
//! English and writes Latin letters over words that are syllabary, because 16 of the type's
//! letters are Latin small capitals (R D W G P M B A Z E T J K S H L).
//!
//! The classifier is rules, in two steps, and decides on evidence, never on LoC's text:
//!
//! 1. [`candidates`] picks the words worth a second reading from what the syllabary read left
//!    behind: a word whose reading isn't a New Testament form and has low confidence, or one
//!    that stands taller than its line (Cherokee is cast at small-capital height, with almost
//!    no ascenders or descenders, and English is not).
//! 2. [`verdicts`] reads the English engine's reading of each candidate, with the syllabary
//!    reading's confidence, the word's shape and its neighbours', and calls it Latin when the
//!    evidence agrees. A reading of only the 16 shared capitals is never enough: lowercase
//!    letters, a hyphenated run of lowercase syllables (`a-ska-ya`) or brackets around
//!    lowercase letters are.
//!
//! Confidences are Tesseract's, 0 to 100. The thresholds are measured on test set v2 and on
//! the words LoC's pages label as English, in the Cherokee report.
use image::GrayImage;
use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;
use usnm_core::cherokee_type::{is_known, is_syllabary};
use usnm_core::query_language::ENGLISH_WORDS;

/// A candidate's syllabary reading is under this confidence.
pub const CANDIDATE_CONF: f64 = 80.0;
/// A word is as tall as this many times the line's other words, or more, to be a candidate on
/// its shape alone.
pub const CANDIDATE_HEIGHT: f64 = 1.3;
/// The share of a word's ink in its top third at most, to be a candidate on its shape alone.
pub const CANDIDATE_TOP: f64 = 0.15;
/// The syllabary model's confidence is under this for a word to be Latin.
pub const MAX_CHR_CONF: f64 = 70.0;
/// A hyphenated run of lowercase syllables needs a less confident syllabary reading to be Latin.
pub const MAX_CHR_CONF_HYPHEN: f64 = 80.0;
/// The English reading's confidence for a word of three or more lowercase letters.
pub const STRONG_CONF: f64 = 70.0;
/// The English reading's confidence for a hyphenated run of lowercase syllables.
pub const HYPHEN_CONF: f64 = 0.0;
/// The English reading's confidence when the word's shape agrees: no ink in its top third and
/// taller than the line.
pub const SHAPE_CONF: f64 = 55.0;
/// The most ink in the top third of a word the shape test allows.
pub const SHAPE_TOP: f64 = 0.2;
/// The least height, against the line's other words, the shape test needs.
pub const SHAPE_HEIGHT: f64 = 1.1;
/// A word the English engine read is as plausible as English when this share of its letter
/// pairs occur [`PAIR_MIN`] times in the word list, unless the word is in the list.
pub const PLAUSIBLE: f64 = 0.7;
/// A letter pair is common in English when it begins this many words of the word list or more.
pub const PAIR_MIN: u32 = 300;
/// The English reading's confidence for a word next to one already Latin.
pub const NEIGHBOUR_CONF: f64 = 30.0;
/// Pixels around a candidate's box in the crop the English engine reads.
pub const WORD_PAD: u32 = 10;

/// One word of a line as the syllabary model read it, with its box in the line crop.
#[derive(Debug, Clone, PartialEq)]
pub struct Word {
    pub text: String,
    /// Tesseract's confidence, 0 to 100; none when it gave none.
    pub conf: Option<f64>,
    /// `[left, top, right, bottom]` in the crop.
    pub rect: [u32; 4],
}

/// What a word's ink looks like.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Shape {
    /// Rows from the first to the last with ink.
    pub height: u32,
    /// The mean share of its width that is ink, over the top third of those rows.
    pub top: f64,
}

/// A line crop thresholded once, for measuring words in it.
pub struct Ink {
    width: u32,
    height: u32,
    dark: Vec<bool>,
}

impl Ink {
    /// `img` with Otsu's threshold: pixels under it are ink.
    pub fn new(img: &GrayImage) -> Self {
        let mut hist = [0u64; 256];
        for p in img.pixels() {
            hist[usize::from(p.0[0])] += 1;
        }
        let threshold = otsu(&hist);
        Self {
            width: img.width(),
            height: img.height(),
            dark: img.pixels().map(|p| p.0[0] < threshold).collect(),
        }
    }

    /// The ink inside `rect` (clamped to the crop), or none when there is almost none.
    pub fn shape(&self, rect: [u32; 4]) -> Option<Shape> {
        let [x0, y0, x1, y1] = rect;
        let (x1, y1) = (x1.min(self.width), y1.min(self.height));
        if x0 >= x1 || y0 >= y1 {
            return None;
        }
        let mut rows = Vec::new();
        let (mut left, mut right) = (u32::MAX, 0u32);
        let mut total = 0u32;
        for y in y0..y1 {
            let base = (y * self.width) as usize;
            let mut count = 0u32;
            for x in x0..x1 {
                if self.dark[base + x as usize] {
                    count += 1;
                    left = left.min(x);
                    right = right.max(x);
                }
            }
            total += count;
            rows.push(count);
        }
        if total < 8 {
            return None;
        }
        let first = rows.iter().position(|c| *c > 0)?;
        let last = rows.iter().rposition(|c| *c > 0)?;
        let rows = &rows[first..=last];
        let width = f64::from(right - left + 1);
        let third = (rows.len() / 3).max(1);
        let top = rows[..third]
            .iter()
            .map(|c| f64::from(*c) / width)
            .sum::<f64>()
            / third as f64;
        Some(Shape {
            height: rows.len() as u32,
            top,
        })
    }
}

fn otsu(hist: &[u64; 256]) -> u8 {
    let total: u64 = hist.iter().sum();
    if total == 0 {
        return 128;
    }
    let sum_all: f64 = hist
        .iter()
        .enumerate()
        .map(|(i, n)| i as f64 * *n as f64)
        .sum();
    let (mut w0, mut sum0) = (0f64, 0f64);
    let (mut best, mut best_var) = (128u8, -1f64);
    for (t, n) in hist.iter().enumerate() {
        w0 += *n as f64;
        if w0 == 0.0 {
            continue;
        }
        let w1 = total as f64 - w0;
        if w1 == 0.0 {
            break;
        }
        sum0 += t as f64 * *n as f64;
        let (m0, m1) = (sum0 / w0, (sum_all - sum0) / w1);
        let var = w0 * w1 * (m0 - m1) * (m0 - m1);
        if var > best_var {
            best_var = var;
            // Pixels strictly under the next grey level are ink.
            best = (t + 1).min(255) as u8;
        }
    }
    best
}

/// Whether the syllabary reading `text` is a New Testament form of two or more letters, with
/// the punctuation around it set aside. One-letter forms prove nothing: the model writes one
/// letter for many Latin words.
pub fn is_attested(text: &str) -> bool {
    let core = text.trim_matches(|c: char| !c.is_alphabetic());
    core.chars().count() >= 2 && is_known(core)
}

/// Each word's shape and its height against the line's other words (the median of theirs; 1
/// when it is the line's only word).
fn shapes(ink: &Ink, words: &[Word]) -> Vec<Option<(Shape, f64)>> {
    let raw: Vec<Option<Shape>> = words.iter().map(|w| ink.shape(w.rect)).collect();
    raw.iter()
        .enumerate()
        .map(|(i, s)| {
            let s = (*s)?;
            let mut others: Vec<u32> = raw
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != i)
                .filter_map(|(_, o)| o.map(|o| o.height))
                .collect();
            others.sort_unstable();
            let rel = match others.len() {
                0 => 1.0,
                n if n % 2 == 1 => f64::from(others[n / 2]),
                n => f64::from(others[n / 2 - 1] + others[n / 2]) / 2.0,
            };
            Some((s, f64::from(s.height) / rel.max(1.0)))
        })
        .collect()
}

/// The words of a line worth reading in English: the syllabary reading is no New Testament
/// form and is unsure of itself (or the word stands tall), the first step of the classifier.
pub fn candidates(ink: &Ink, words: &[Word]) -> Vec<bool> {
    let shapes = shapes(ink, words);
    words
        .iter()
        .zip(&shapes)
        .map(|(w, s)| {
            if is_attested(&w.text) {
                return false;
            }
            let unsure = w.conf.is_none_or(|c| c < CANDIDATE_CONF);
            let tall = s.is_some_and(|(s, rel)| rel >= CANDIDATE_HEIGHT && s.top <= CANDIDATE_TOP);
            unsure || tall
        })
        .collect()
}

/// What the English engine read at one candidate.
#[derive(Debug, Clone, PartialEq)]
pub struct English {
    pub text: String,
    /// The lowest confidence of its words, 0 to 100.
    pub conf: f64,
}

type EnglishWords = (HashSet<&'static str>, HashMap<[u8; 2], u32>);

/// The English word list as a set, and how many of its words each letter pair occurs in (with
/// `^` and `$` for a word's ends).
fn english() -> &'static EnglishWords {
    static ENGLISH: OnceLock<EnglishWords> = OnceLock::new();
    ENGLISH.get_or_init(|| {
        let words: HashSet<&str> = ENGLISH_WORDS
            .lines()
            .filter(|w| !w.is_empty() && w.bytes().all(|b| b.is_ascii_lowercase()))
            .collect();
        let mut pairs: HashMap<[u8; 2], u32> = HashMap::new();
        for w in &words {
            let b: Vec<u8> = std::iter::once(b'^')
                .chain(w.bytes())
                .chain(std::iter::once(b'$'))
                .collect();
            for p in b.windows(2) {
                *pairs.entry([p[0], p[1]]).or_default() += 1;
            }
        }
        (words, pairs)
    })
}

/// Whether one word of the English engine's reading could be English: in the word list, or made
/// of letter pairs English uses (a name, a form the list lacks).
fn plausible_word(token: &str) -> bool {
    let (words, pairs) = english();
    let lower: Vec<u8> = token
        .bytes()
        .filter(u8::is_ascii_alphabetic)
        .map(|b| b.to_ascii_lowercase())
        .collect();
    if lower.len() < 3 {
        return false;
    }
    if words.contains(std::str::from_utf8(&lower).unwrap_or_default()) {
        return true;
    }
    let b: Vec<u8> = std::iter::once(b'^')
        .chain(lower)
        .chain(std::iter::once(b'$'))
        .collect();
    let common = b
        .windows(2)
        .filter(|p| pairs.get(&[p[0], p[1]]).is_some_and(|n| *n >= PAIR_MIN))
        .count();
    common as f64 >= PLAUSIBLE * (b.len() - 1) as f64
}

/// Whether the English reading looks like text: only the characters printed English has, and
/// every word of three or more letters plausible (shorter ones, as in "a pair", are let through
/// but are not evidence). Noise glyphs (`|`, `@`, `=`) never are.
fn looks_english(text: &str) -> bool {
    let ok = text.chars().all(|c| {
        c.is_ascii_alphanumeric()
            || c.is_whitespace()
            || ".,;:!?'\"()[]-\u{2018}\u{2019}\u{201c}\u{201d}*".contains(c)
    });
    let tokens: Vec<&str> = text
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|t| t.len() >= 3)
        .collect();
    ok && !tokens.is_empty() && tokens.into_iter().all(plausible_word)
}

fn lowercase(s: &str) -> usize {
    s.chars().filter(char::is_ascii_lowercase).count()
}

/// `(a-ska-ya)`: lowercase syllables of one to six letters joined by hyphens, at least two,
/// with brackets or a quote before and punctuation after. Several runs side by side
/// (`ga-lv-la-ti e-hi`) count, since the English engine reads a transliterated phrase as one box.
fn is_hyphenated_run(s: &str) -> bool {
    let mut runs = 0;
    for piece in s.split_whitespace() {
        let piece = piece.trim_start_matches(['(', '[', '\'', '*', '\u{2018}', '\u{201c}', '"']);
        let piece = piece.trim_end_matches(['.', ',', ';', ':', ')', ']']);
        let parts: Vec<&str> = piece.split('-').collect();
        let syllables = parts
            .iter()
            .all(|p| (1..=6).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_lowercase()));
        if !syllables {
            return false;
        }
        runs += usize::from(parts.len() >= 2);
    }
    runs >= 1
}

fn bracketed(s: &str) -> bool {
    s.starts_with(['(', '['])
        || s.trim_end_matches(['.', ',', ';', ':'])
            .ends_with([')', ']'])
}

/// Whether the evidence on one word alone says Latin, before its neighbours are looked at.
fn alone(word: &Word, shape: Option<(Shape, f64)>, eng: &English) -> bool {
    if is_attested(&word.text) {
        return false;
    }
    let chr = word.conf.unwrap_or(0.0);
    let lower = lowercase(&eng.text);
    let text = eng.text.trim();
    // Tall, and empty at the top: ink the way English has it and the syllabary does not.
    let shaped = shape.is_some_and(|(s, rel)| s.top <= SHAPE_TOP && rel >= SHAPE_HEIGHT);
    // Four lowercase letters the English engine is sure of (three when the shape agrees; two
    // when brackets enclose them). Three-letter readings of a syllabary word are common.
    if chr < MAX_CHR_CONF
        && eng.conf >= STRONG_CONF
        && (lower >= 4 || (lower >= 3 && shaped) || (lower >= 2 && bracketed(text)))
        && looks_english(text)
    {
        return true;
    }
    // A transliteration: `a-ska-ya`.
    if chr < MAX_CHR_CONF_HYPHEN && eng.conf >= HYPHEN_CONF && is_hyphenated_run(text) {
        return true;
    }
    // A less sure reading, backed by the shape of the ink.
    chr < MAX_CHR_CONF && lower >= 4 && eng.conf >= SHAPE_CONF && shaped && looks_english(text)
}

/// Which words of a line are Latin: for each, its English reading if it was a candidate. A word
/// next to a Latin one is Latin too when its own English reading has three lowercase letters
/// and the syllabary model doubts it (two passes, so a run of English words is caught).
pub fn verdicts(ink: &Ink, words: &[Word], english: &[Option<English>]) -> Vec<bool> {
    assert_eq!(words.len(), english.len());
    let shapes = shapes(ink, words);
    let mut latin: Vec<bool> = words
        .iter()
        .zip(&shapes)
        .zip(english)
        .map(|((w, s), e)| e.as_ref().is_some_and(|e| alone(w, *s, e)))
        .collect();
    for _ in 0..2 {
        let before = latin.clone();
        for i in 0..words.len() {
            if before[i] {
                continue;
            }
            let near = (i > 0 && before[i - 1]) || (i + 1 < words.len() && before[i + 1]);
            let Some(e) = english[i].as_ref().filter(|_| near) else {
                continue;
            };
            if !is_attested(&words[i].text)
                && words[i].conf.unwrap_or(0.0) < MAX_CHR_CONF
                && lowercase(&e.text) >= 3
                && e.conf >= NEIGHBOUR_CONF
                && (looks_english(&e.text) || is_hyphenated_run(e.text.trim()))
            {
                latin[i] = true;
            }
        }
    }
    latin
}

/// The line as text: each word as the syllabary model read it, except the Latin ones, which
/// are the English engine's reading. Words with an empty English reading stay as they were.
pub fn assemble(words: &[Word], english: &[Option<English>], latin: &[bool]) -> String {
    words
        .iter()
        .zip(english)
        .zip(latin)
        .map(|((w, e), l)| match (l, e) {
            (true, Some(e)) if !e.text.trim().is_empty() => {
                e.text.split_whitespace().collect::<Vec<_>>().join(" ")
            }
            _ => w.text.clone(),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Whether `c` is a Latin letter or digit: what the English engine's words are made of.
pub fn is_latin_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
}

/// How many words of `text` have a Latin letter in them and no syllabary letter: the Latin words
/// a reading holds.
pub fn latin_words(text: &str) -> usize {
    text.split_whitespace()
        .filter(|w| w.chars().any(|c| c.is_ascii_alphabetic()) && !w.chars().any(is_syllabary))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word(text: &str, conf: f64, rect: [u32; 4]) -> Word {
        Word {
            text: text.into(),
            conf: Some(conf),
            rect,
        }
    }

    fn eng(text: &str, conf: f64) -> Option<English> {
        Some(English {
            text: text.into(),
            conf,
        })
    }

    /// A line crop with a block of ink per word: `(x0, x1, y0, y1, top_fraction_filled)`.
    fn crop(blocks: &[(u32, u32, u32, u32)]) -> GrayImage {
        let mut img = GrayImage::from_pixel(400, 60, image::Luma([255]));
        for (x0, x1, y0, y1) in blocks {
            for y in *y0..*y1 {
                for x in *x0..*x1 {
                    img.put_pixel(x, y, image::Luma([0]));
                }
            }
        }
        img
    }

    #[test]
    fn ink_shape_measures_height_and_top_third() {
        // Full block: ink all the way up. Hollow top: ink only in the lower two thirds.
        let img = crop(&[(10, 60, 10, 40), (100, 150, 25, 40)]);
        let ink = Ink::new(&img);
        let full = ink.shape([5, 0, 70, 60]).unwrap();
        assert_eq!(full.height, 30);
        assert!(full.top > 0.95);
        let low = ink.shape([95, 0, 160, 60]).unwrap();
        assert_eq!(low.height, 15);
        assert!(ink.shape([200, 0, 300, 60]).is_none());
        // A word made of an ascender and a body: little ink on the top rows.
        let mut img = crop(&[(10, 80, 30, 50)]);
        for y in 10..30 {
            img.put_pixel(20, y, image::Luma([0]));
        }
        let ink = Ink::new(&img);
        let s = ink.shape([5, 0, 90, 60]).unwrap();
        assert_eq!(s.height, 40);
        assert!(s.top < 0.1, "{s:?}");
    }

    #[test]
    fn attested_forms_need_two_letters() {
        assert!(is_attested("ᎠᎴ,"));
        assert!(is_attested("(ᎠᎴ"));
        // One letter is a form, but proves nothing about the word.
        assert!(!is_attested("Ꭰ"));
        assert!(!is_attested("Cherokee"));
    }

    #[test]
    fn a_hyphenated_run_is_a_transliteration() {
        for s in ["a-ska-ya", "(u-wo-du", "u-na-tse-li.", "v-gi-do"] {
            assert!(is_hyphenated_run(s), "{s}");
        }
        for s in ["well-known-Case", "ska", "a--b", "abcdefg-h", "A-b", ""] {
            assert!(!is_hyphenated_run(s), "{s}");
        }
    }

    fn line_with(words: &[Word]) -> Ink {
        // One block of ink per word, all the same height, filling the box.
        let blocks: Vec<(u32, u32, u32, u32)> = words
            .iter()
            .map(|w| (w.rect[0], w.rect[2], w.rect[1], w.rect[3]))
            .collect();
        Ink::new(&crop(&blocks))
    }

    #[test]
    fn candidates_are_unsure_unattested_words() {
        let words = [
            word("ᎠᎴ", 95.0, [10, 20, 60, 40]),
            word("ᎤᏙᏓ", 40.0, [80, 20, 130, 40]),
            word("ᎤᏙᏓ", 95.0, [150, 20, 200, 40]),
            word("ᏔᎵ", 95.0, [220, 20, 270, 40]),
        ];
        let ink = line_with(&words);
        // ᎠᎴ is attested; a low-confidence form that is attested is still not a candidate.
        let c = candidates(&ink, &words);
        assert!(!c[0]);
        assert!(!c[1], "{c:?}");
        let words = [
            word("ᎠᎴ", 95.0, [10, 20, 60, 40]),
            word("ᎤᏪᏟᏌᏁᏯ", 40.0, [80, 20, 130, 40]),
            word("ᎤᏪᏟᏌᏁᏯ", 90.0, [150, 20, 200, 40]),
        ];
        let c = candidates(&line_with(&words), &words);
        assert_eq!(c, [false, true, false]);
    }

    #[test]
    fn a_tall_word_is_a_candidate_even_when_the_model_is_sure() {
        // Two short blocks, one tall word whose top third is empty (an ascender at the left
        // only).
        let mut img = crop(&[(10, 60, 30, 50), (80, 130, 30, 50), (150, 260, 30, 50)]);
        for y in 5..30 {
            img.put_pixel(155, y, image::Luma([0]));
        }
        let words = [
            word("ᎤᏪᏟᏌᏁᏯ", 95.0, [5, 0, 65, 60]),
            word("ᎤᏪᏟᏌᏁᏯ", 95.0, [75, 0, 135, 60]),
            word("ᎤᏪᏟᏌᏁᏯ", 95.0, [145, 0, 265, 60]),
        ];
        let c = candidates(&Ink::new(&img), &words);
        assert_eq!(c, [false, false, true]);
    }

    #[test]
    fn english_words_the_engine_is_sure_of_are_latin() {
        let words = [
            word("ᎤᏪᏟᏌᏁᏯ", 90.0, [10, 20, 60, 40]),
            word("ᏣᎳᎩ", 5.0, [80, 20, 130, 40]),
            word("ᏣᏍᏆᎻᏯ)", 30.0, [150, 20, 200, 40]),
        ];
        let ink = line_with(&words);
        let english = [None, eng("(Cherokee", 96.0), eng("Academy)", 95.0)];
        assert_eq!(verdicts(&ink, &words, &english), [false, true, true]);
        assert_eq!(
            assemble(&words, &english, &[false, true, true]),
            "ᎤᏪᏟᏌᏁᏯ (Cherokee Academy)"
        );
    }

    #[test]
    fn capitals_shared_with_the_syllabary_are_not_evidence() {
        let words = [word("ᏣᎳᎻ", 20.0, [10, 20, 60, 40])];
        let ink = line_with(&words);
        // `eng` reads Ꭰ-like shapes as DE, DS, RAD: no lowercase letter at all.
        for t in ["DE", "DS", "RAD", "TYRGR.", "ORGAN"] {
            assert_eq!(verdicts(&ink, &words, &[eng(t, 90.0)]), [false], "{t}");
        }
        // The same letters in lowercase are.
        assert_eq!(verdicts(&ink, &words, &[eng("done", 90.0)]), [true]);
    }

    #[test]
    fn the_syllabary_models_confidence_and_attested_forms_veto_a_reading() {
        let sure = [word("ᎤᏪᏟᏌᏁᏯ", 90.0, [10, 20, 60, 40])];
        let ink = line_with(&sure);
        assert_eq!(verdicts(&ink, &sure, &[eng("which", 95.0)]), [false]);
        let known = [word("ᎠᎴ", 10.0, [10, 20, 60, 40])];
        assert_eq!(verdicts(&ink, &known, &[eng("which", 95.0)]), [false]);
        let unsure = [word("ᏣᎳ", 10.0, [10, 20, 60, 40])];
        assert_eq!(verdicts(&ink, &unsure, &[eng("which", 95.0)]), [true]);
        // A shaky English reading is not enough on its own.
        assert_eq!(verdicts(&ink, &unsure, &[eng("which", 40.0)]), [false]);
        assert_eq!(verdicts(&ink, &unsure, &[eng("whi", 20.0)]), [false]);
    }

    #[test]
    fn transliterations_and_neighbours_follow_their_context() {
        let words = [
            word("ᎤᏪᏟ", 90.0, [10, 20, 60, 40]),
            word("ᏣᎳ", 30.0, [80, 20, 130, 40]),
            word("ᏔᎵᎢ", 40.0, [150, 20, 200, 40]),
            word("ᏔᎵᎢ", 40.0, [220, 20, 270, 40]),
        ];
        let ink = line_with(&words);
        let english = [
            None,
            eng("Judge", 90.0),
            eng("Clayton", 35.0),
            eng("xy", 35.0),
        ];
        // Judge is sure; Clayton follows it; the two-letter reading doesn't.
        assert_eq!(verdicts(&ink, &words, &english), [false, true, true, false]);
        let tr = [word("ᏣᎳ", 30.0, [10, 20, 60, 40])];
        assert_eq!(
            verdicts(&line_with(&tr), &tr, &[eng("a-ska-ya", 28.0)]),
            [true]
        );
        assert_eq!(
            verdicts(&line_with(&tr), &tr, &[eng("A-SKA-YA", 80.0)]),
            [false]
        );
    }

    #[test]
    fn latin_words_are_counted_without_syllabary() {
        assert_eq!(latin_words("ᎠᎴ (Cherokee Academy) 12 ᎤᏙᏓ"), 2);
        assert_eq!(latin_words("ᎠᎴ ᎤᏙᏓ, 13."), 0);
        assert_eq!(latin_words("aᎠ"), 0);
    }
}
