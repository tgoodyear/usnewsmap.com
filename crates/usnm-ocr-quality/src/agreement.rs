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
//! Agreement is not accuracy: two readers that make the same mistake agree
//! on it. The hat's own caveat ([`crate::report::LOW_BOUND_NOTE`]) applies.
//!
//! Everything is a pure function of the two texts, single-threaded, with
//! floats rounded to six decimals, so the same inputs give the same bytes.
use crate::{align, report};
use serde::{Deserialize, Serialize};
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
    Agreement {
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
