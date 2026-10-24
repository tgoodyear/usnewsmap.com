//! Text agreement between two OCR readings of one page (#962): LoC's text
//! against American Stories', with no image and no third reader.
//!
//! The core is [`align::pair`], the pairwise metrics the `usnm_quality`
//! app computes between every two readers of a page, rounded with
//! [`report::round_pair`] as the app's record rounds them. So
//! `pair.cer_sym` here is the same number the three-cornered hat
//! ([`crate::hat`]) is solved on for that pair of readers: a page that
//! later gets a third reader can take it as `d_ij` without recomputing it.
//!
//! On top of the pair, [`compare`] adds what an order-free view of the two
//! texts says. American Stories' text is in article reading order (a layout
//! model's), and LoC's is in ALTO block order, so a page whose columns or
//! articles come out in a different order has a high `cer_sym` even when
//! both readers read every word right. The word bags don't see order:
//! - `cover_a`, `cover_b`: the share of each reader's words (lower case,
//!   runs of letters and digits, counted with multiplicity, as
//!   [`align::bag`] counts them) that the other reader also has;
//! - `len_ratio`: the second reader's characters over the first's, after
//!   normalisation, which separates "one reader dropped half the page" from
//!   "both read it, differently".
//!
//! And a local alignment that doesn't depend on order either ([`local`]):
//! each reader's text is cut into chunks of [`CHUNK_WORDS`] words, each
//! chunk is placed in the other text by the rare words they share, and the
//! chunk's character edits are counted against the best-matching stretch
//! there (an infix edit distance). `local_cer_*` is the character
//! disagreement where both readers have the text, and `matched_*` the share
//! of a reader's characters that found a place in the other's. On real
//! pages the whole-page `cer_sym` is mostly order and coverage, so
//! `local_cer_sym` is the better disagreement for the two readers' errors.
//!
//! Agreement is not accuracy: two readers that make the same mistake agree
//! on it. The hat's own caveat ([`crate::report::LOW_BOUND_NOTE`]) applies.
//!
//! Everything is a pure function of the two texts, single-threaded, with
//! floats rounded to six decimals, so the same inputs give the same bytes.
use crate::{align, report};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use usnm_ocr_contract::quality::{round6, PairMetrics};

/// The metric's version: bumped whenever what [`compare`] returns for the
/// same two texts changes, including the normalisation in
/// [`crate::text::normalize`] and the rounding.
pub const VERSION: u32 = 1;

/// How two readings of a page agree. Reader `a` is the first argument of
/// [`compare`], `b` the second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Agreement {
    /// The pairwise metrics, rounded as the `usnm_quality` record rounds
    /// them. `cer_a` is `a`'s CER with `b` as the reference, `cer_b` the
    /// other way; `cer_sym` is the hat's `d_ab`.
    pub pair: PairMetrics,
    /// Words of `a`'s bag and of `b`'s (with multiplicity).
    pub bag_words_a: usize,
    pub bag_words_b: usize,
    /// Words the two bags share (the smaller count of each word).
    pub shared_words: usize,
    /// `shared_words / bag_words_a`: how much of `a` the other reader has;
    /// none for an empty bag.
    pub cover_a: Option<f64>,
    /// `shared_words / bag_words_b`.
    pub cover_b: Option<f64>,
    /// `chars_b / chars_a` after normalisation; none when `a` is empty.
    pub len_ratio: Option<f64>,
    /// `a`'s chunks placed in `b`, and `b`'s in `a`.
    pub local_a: Local,
    pub local_b: Local,
    /// Edits over the matched characters of `a`'s chunks: `a`'s disagreement
    /// with `b` where `b` has the text; none when nothing matched.
    pub local_cer_a: Option<f64>,
    pub local_cer_b: Option<f64>,
    /// Both directions' edits over both directions' matched characters.
    pub local_cer_sym: Option<f64>,
    /// `local_a.matched_chars / local_a.chars`: the share of `a` that found a
    /// place in `b`; none for an empty `a`.
    pub matched_a: Option<f64>,
    pub matched_b: Option<f64>,
}

/// Words per chunk of the local alignment.
pub const CHUNK_WORDS: usize = 40;
/// Words of the other text on each side of a chunk's anchored place.
pub const SLACK_WORDS: usize = 12;
/// An anchor is a word of at least this many characters
pub const ANCHOR_MIN_CHARS: usize = 4;
/// that occurs at most this often in the other text.
pub const ANCHOR_MAX_OCC: usize = 2;
/// A chunk is placed where at least this many anchors agree,
pub const MIN_ANCHORS: usize = 2;
/// to within this many words of offset.
pub const OFFSET_BAND: i64 = 6;
/// A placed chunk with more edits than this share of its characters is a
/// wrong place (or text the other reader doesn't have): not matched.
pub const MAX_LOCAL_CER: f64 = 0.5;

/// One direction of the local alignment.
#[derive(
    Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema,
)]
#[serde(deny_unknown_fields)]
pub struct Local {
    pub chunks: usize,
    pub matched_chunks: usize,
    /// Characters of every chunk (words joined by one space).
    pub chars: usize,
    pub matched_chars: usize,
    /// Edits of the matched chunks against their places.
    pub edits: usize,
}

/// The smallest edit distance between `p` and any substring of `t`.
fn infix_distance(p: &[char], t: &[char]) -> usize {
    let mut prev = vec![0u32; t.len() + 1];
    let mut cur = vec![0u32; t.len() + 1];
    for (i, &pc) in p.iter().enumerate() {
        cur[0] = i as u32 + 1;
        for j in 1..=t.len() {
            let sub = prev[j - 1] + u32::from(pc != t[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev.into_iter().min().unwrap_or(0) as usize
}

fn chars_of(words: &[&str]) -> Vec<char> {
    let mut out = Vec::new();
    for (k, w) in words.iter().enumerate() {
        if k > 0 {
            out.push(' ');
        }
        out.extend(w.chars());
    }
    out
}

/// `a`'s chunks placed in `b`. Both are whitespace words of normalised
/// text. Deterministic: anchors are taken in order, and ties between
/// places go to the smallest offset.
pub fn local(a: &[&str], b: &[&str]) -> Local {
    let mut index: HashMap<&str, Vec<u32>> = HashMap::new();
    for (p, w) in b.iter().enumerate() {
        if w.chars().count() >= ANCHOR_MIN_CHARS {
            index.entry(w).or_default().push(p as u32);
        }
    }
    let mut placer = Placer {
        b,
        index,
        votes: Vec::new(),
    };
    let mut out = Local::default();
    for (c, chunk) in a.chunks(CHUNK_WORDS).enumerate() {
        out.chunks += 1;
        out.chars += chars_of(chunk).len();
        let (chars, edits) = placer.place(chunk, c * CHUNK_WORDS);
        if chars > 0 {
            out.matched_chunks += 1;
            out.matched_chars += chars;
            out.edits += edits;
        }
    }
    out
}

/// A chunk placed with more edits than this share of its characters is
/// split in two and each half placed on its own: a chunk that straddles a
/// seam between two columns the other reader has in another order.
pub const SPLIT_CER: f64 = 0.2;
/// Halving stops at pieces of this many words.
pub const MIN_PIECE_WORDS: usize = 10;

struct Placer<'a> {
    b: &'a [&'a str],
    index: HashMap<&'a str, Vec<u32>>,
    votes: Vec<i64>,
}

impl Placer<'_> {
    /// Where `piece` (starting at word `start` of its text) sits in `b`:
    /// the offset most anchors agree on, the first on ties.
    fn offset(&mut self, piece: &[&str], start: usize) -> Option<i64> {
        self.votes.clear();
        for (i, w) in piece.iter().enumerate() {
            if let Some(ps) = self.index.get(w) {
                if ps.len() <= ANCHOR_MAX_OCC {
                    self.votes
                        .extend(ps.iter().map(|&p| i64::from(p) - (start + i) as i64));
                }
            }
        }
        self.votes.sort_unstable();
        let (mut best, mut lo) = ((0usize, 0usize), 0usize);
        for hi in 0..self.votes.len() {
            while self.votes[hi] - self.votes[lo] > OFFSET_BAND {
                lo += 1;
            }
            if hi + 1 - lo > best.0 {
                best = (hi + 1 - lo, lo);
            }
        }
        (best.0 >= MIN_ANCHORS).then(|| self.votes[best.1 + best.0 / 2])
    }

    /// The matched characters and their edits for `piece`; (0, 0) when it
    /// found no place.
    fn place(&mut self, piece: &[&str], start: usize) -> (usize, usize) {
        let pattern = chars_of(piece);
        let placed = self.offset(piece, start).and_then(|offset| {
            let from = (start as i64 + offset - SLACK_WORDS as i64).max(0) as usize;
            let to = ((start + piece.len()) as i64 + offset + SLACK_WORDS as i64)
                .clamp(0, self.b.len() as i64) as usize;
            (from < to).then(|| infix_distance(&pattern, &chars_of(&self.b[from..to])))
        });
        let cer = placed.map(|e| e as f64 / pattern.len().max(1) as f64);
        if cer.is_none_or(|c| c > SPLIT_CER) && piece.len() >= 2 * MIN_PIECE_WORDS {
            let mid = piece.len() / 2;
            let (c1, e1) = self.place(&piece[..mid], start);
            let (c2, e2) = self.place(&piece[mid..], start + mid);
            // The halves replace the whole when they place better.
            // The space between the halves counts when both placed.
            let halves = (c1 + c2 + usize::from(c1 > 0 && c2 > 0), e1 + e2);
            let whole_ok = cer.is_some_and(|c| c <= MAX_LOCAL_CER);
            if !whole_ok
                || (halves.0 > 0 && (halves.1 as f64 / halves.0 as f64) < cer.unwrap_or(1.0))
            {
                return halves;
            }
        }
        match (placed, cer) {
            (Some(e), Some(c)) if c <= MAX_LOCAL_CER => (pattern.len(), e),
            _ => (0, 0),
        }
    }
}

fn ratio(n: usize, d: usize) -> Option<f64> {
    (d > 0).then(|| round6(n as f64 / d as f64))
}

/// Compares two readings of one page.
pub fn compare(a: &str, b: &str) -> Agreement {
    let pair = report::round_pair(align::pair(a, b));
    let (na, nb) = (
        crate::text::normalize(a, false),
        crate::text::normalize(b, false),
    );
    let (wa, wb) = (align::bag(&na), align::bag(&nb));
    let bag_words_a: usize = wa.values().sum();
    let bag_words_b: usize = wb.values().sum();
    let shared_words: usize = wa
        .iter()
        .map(|(w, n)| (*n).min(*wb.get(w).unwrap_or(&0)))
        .sum();
    let share = |n: usize| (n > 0).then(|| round6(shared_words as f64 / n as f64));
    let (ta, tb) = (crate::text::py_split(&na), crate::text::py_split(&nb));
    let (local_a, local_b) = (local(&ta, &tb), local(&tb, &ta));
    Agreement {
        local_cer_a: ratio(local_a.edits, local_a.matched_chars),
        local_cer_b: ratio(local_b.edits, local_b.matched_chars),
        local_cer_sym: ratio(
            local_a.edits + local_b.edits,
            local_a.matched_chars + local_b.matched_chars,
        ),
        matched_a: ratio(local_a.matched_chars, local_a.chars),
        matched_b: ratio(local_b.matched_chars, local_b.chars),
        local_a,
        local_b,
        bag_words_a,
        bag_words_b,
        shared_words,
        cover_a: share(bag_words_a),
        cover_b: share(bag_words_b),
        len_ratio: (pair.chars_a > 0).then(|| round6(pair.chars_b as f64 / pair.chars_a as f64)),
        pair,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::hat;

    struct Lcg(u64);
    impl Lcg {
        fn next(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0 >> 33
        }
    }

    /// `n` random words of 2 to 9 letters.
    fn words(n: usize, r: &mut Lcg) -> Vec<String> {
        (0..n)
            .map(|_| {
                let len = 2 + r.next() % 8;
                (0..len)
                    .map(|_| (b'a' + (r.next() % 26) as u8) as char)
                    .collect()
            })
            .collect()
    }

    /// Every `every`-th letter replaced.
    fn misread(text: &str, every: usize) -> String {
        text.chars()
            .enumerate()
            .map(|(i, c)| if c != ' ' && i % every == 0 { 'q' } else { c })
            .collect()
    }

    #[test]
    fn infix_distance_known_values() {
        let c = |s: &str| s.chars().collect::<Vec<_>>();
        assert_eq!(infix_distance(&c("abc"), &c("xxabcxx")), 0);
        assert_eq!(infix_distance(&c("abc"), &c("xxabxx")), 1);
        assert_eq!(infix_distance(&c("abc"), &c("")), 3);
        assert_eq!(infix_distance(&c(""), &c("abc")), 0);
    }

    #[test]
    fn local_ignores_order_and_finds_the_errors() {
        let mut r = Lcg(962);
        let cols: Vec<String> = (0..4).map(|_| words(300, &mut r).join(" ")).collect();
        let loc = cols.join(" ");
        // The other reader has the columns in another order and misreads
        // about one letter in 25.
        let other = misread(
            &[&cols[2], &cols[0], &cols[3], &cols[1]]
                .map(|s| s.as_str())
                .join(" "),
            25,
        );
        let g = compare(&loc, &other);
        assert!(
            g.pair.cer_sym > 0.3,
            "whole-page cer_sym {}",
            g.pair.cer_sym
        );
        // Chunks that straddle a seam between columns can fail to place.
        assert!(g.matched_a.unwrap() > 0.9, "{:?}", g.matched_a);
        assert!(g.matched_b.unwrap() > 0.9, "{:?}", g.matched_b);
        // The true rate within the columns, against the local estimate.
        let truth = align::pair(&cols[0], &misread(&cols[0], 25)).cer_sym;
        let local = g.local_cer_sym.unwrap();
        assert!(
            (local - truth).abs() < 0.015,
            "local {local}, in-column {truth}"
        );
        // Identical columns in another order: nothing to find.
        let same = compare(
            &loc,
            &[&cols[3], &cols[1], &cols[0], &cols[2]]
                .map(|s| s.as_str())
                .join(" "),
        );
        assert_eq!(same.local_cer_sym, Some(0.0));
        assert_eq!(same.matched_a, Some(1.0));
    }

    #[test]
    fn local_separates_missing_text_from_misreading() {
        let mut r = Lcg(5);
        let page = words(800, &mut r);
        let half = page[..400].join(" ");
        let g = compare(&page.join(" "), &half);
        let m = g.matched_a.unwrap();
        assert!((m - 0.5).abs() < 0.03, "matched_a {m}");
        assert_eq!(g.matched_b, Some(1.0));
        assert_eq!(g.local_cer_sym, Some(0.0));
        // Unrelated texts place nothing.
        let other = words(800, &mut r).join(" ");
        let g = compare(&page.join(" "), &other);
        assert_eq!(g.local_a.matched_chunks, 0);
        assert_eq!(g.local_cer_sym, None);
        assert_eq!(g.matched_a, Some(0.0));
    }

    #[test]
    fn identical_texts_agree_fully() {
        let t = "The Senate met at noon. Mr. Bryan spoke on silver.";
        let g = compare(t, t);
        assert_eq!(g.pair.cer_sym, 0.0);
        assert_eq!(g.pair.dice, 1.0);
        assert_eq!(g.cover_a, Some(1.0));
        assert_eq!(g.cover_b, Some(1.0));
        assert_eq!(g.len_ratio, Some(1.0));
        assert_eq!(g.bag_words_a, 10);
        assert_eq!(g.shared_words, 10);
    }

    #[test]
    fn pair_is_the_quality_records_pair() {
        // The same numbers the usnm_quality record carries for these two
        // readers, so the hat can take `cer_sym` as is.
        let (a, b) = ("Clare-\nmont is a town", "Claremont is a tovvn");
        assert_eq!(compare(a, b).pair, report::round_pair(align::pair(a, b)));
        assert_eq!(
            compare(a, b).pair.cer_sym,
            compare(b, a).pair.cer_sym,
            "cer_sym is symmetric"
        );
    }

    #[test]
    fn reordered_columns_keep_the_bags() {
        // The same two articles in the other order: the character distance
        // is large, the bags are the same.
        let one = "the mayor of the city spoke at the hall on monday evening";
        let two = "wheat closed higher in chicago as buyers came back to the pit";
        let g = compare(&format!("{one} {two}"), &format!("{two} {one}"));
        assert!(g.pair.cer_sym > 0.3, "{}", g.pair.cer_sym);
        assert_eq!(g.pair.dice, 1.0);
        assert_eq!(g.cover_a, Some(1.0));
        assert_eq!(g.len_ratio, Some(1.0));
    }

    #[test]
    fn a_missing_half_shows_in_cover_and_length() {
        let a = "one two three four five six seven eight";
        let b = "one two three four";
        let g = compare(a, b);
        assert_eq!(g.cover_a, Some(0.5));
        assert_eq!(g.cover_b, Some(1.0));
        assert_eq!(g.len_ratio, Some(round6(18.0 / 39.0)));
    }

    #[test]
    fn empty_readers() {
        let g = compare("", "");
        assert_eq!(g.pair.cer_sym, 0.0);
        assert_eq!(g.cover_a, None);
        assert_eq!(g.cover_b, None);
        assert_eq!(g.len_ratio, None);
        let g = compare("", "some text");
        assert_eq!(g.pair.cer_a, 1.0);
        assert_eq!(g.cover_a, None);
        assert_eq!(g.cover_b, Some(0.0));
        assert_eq!(g.len_ratio, None);
        let g = compare("some text", "");
        assert_eq!(g.len_ratio, Some(0.0));
    }

    #[test]
    fn deterministic_and_rounded() {
        let a = "Tbe qnick brown f0x jumped ovcr the lazy dog's back.";
        let b = "The quick brown fox jumped over the lazy dogs back";
        let g = compare(a, b);
        assert_eq!(g, compare(a, b));
        for x in [g.pair.cer_a, g.pair.cer_b, g.pair.cer_sym, g.pair.dice] {
            assert_eq!(x, round6(x));
        }
        assert_eq!(
            serde_json::to_string(&g).unwrap(),
            serde_json::to_string(&compare(a, b)).unwrap()
        );
    }

    #[test]
    fn feeds_the_hat_with_a_third_reader() {
        // With a third reader, the pair's cer_sym is one of the hat's three
        // equations, unchanged.
        let truth = "the steamer arrived from havre with forty passengers aboard";
        let loc = "tbe steamer arrlved from havre with forty passeugers aboard";
        let as_ = "the steamer arrived frorn havre with forty passengers aboard";
        let ours = "the steamer arived from havre wtih forty passengers aboard";
        let d = |x: &str, y: &str| compare(x, y).pair.cer_sym;
        let h = hat::solve(
            3,
            &[
                (0, 1, d(loc, as_)),
                (0, 2, d(loc, ours)),
                (1, 2, d(as_, ours)),
            ],
        );
        assert!(h.determined);
        let true_cer = |x: &str| align::pair(x, truth).cer_a;
        // LoC has the most errors, and the hat ranks it worst.
        assert!(true_cer(loc) > true_cer(as_));
        assert!(h.estimates[0] > h.estimates[1]);
    }
}
