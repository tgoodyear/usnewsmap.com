//! Text similarity for the fuzzy validator.
//!
//! The measure is a Dice coefficient over the multiset of words. It is linear in text length.
//! Edit distance is quadratic and a newspaper page has about 30 KB of text, so it is too slow
//! for a validator that checks many results.

use std::collections::HashMap;

/// Normalise OCR text into comparable words: undo end-of-line hyphenation, collapse all
/// whitespace, and trim ASCII punctuation from the ends of each word. Case is kept.
pub fn words(text: &str) -> Vec<String> {
    let joined = text.replace("-\r\n", "").replace("-\n", "");
    joined
        .split_whitespace()
        .map(|w| w.trim_matches(|c: char| c.is_ascii_punctuation()))
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

fn counts(ws: &[String]) -> HashMap<&str, usize> {
    let mut m = HashMap::new();
    for w in ws {
        *m.entry(w.as_str()).or_insert(0) += 1;
    }
    m
}

/// Dice coefficient over word multisets, from 0.0 (nothing shared) to 1.0 (same words).
/// Two empty texts score 1.0; one empty text against a non-empty one scores 0.0.
pub fn dice(a: &str, b: &str) -> f64 {
    let wa = words(a);
    let wb = words(b);
    if wa.is_empty() && wb.is_empty() {
        return 1.0;
    }
    if wa.is_empty() || wb.is_empty() {
        return 0.0;
    }
    let ca = counts(&wa);
    let cb = counts(&wb);
    let shared: usize = ca
        .iter()
        .map(|(w, n)| cb.get(w).map_or(0, |m| (*n).min(*m)))
        .sum();
    2.0 * shared as f64 / (wa.len() + wb.len()) as f64
}

/// Ratio of the shorter word count to the longer one, in [0, 1]. Equal counts, including
/// both zero, give 1.0.
pub fn length_ratio(a: &str, b: &str) -> f64 {
    let (la, lb) = (words(a).len(), words(b).len());
    if la == lb {
        return 1.0;
    }
    la.min(lb) as f64 / la.max(lb) as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identical_text_scores_one() {
        let t = "Der Kaiser ist in Berlin angekommen.";
        assert_eq!(dice(t, t), 1.0);
    }

    #[test]
    fn whitespace_and_hyphenation_do_not_matter() {
        let a = "Die Zei-\ntung berichtet\n\n  heute.";
        let b = "Die Zeitung berichtet heute.";
        assert_eq!(dice(a, b), 1.0);
    }

    #[test]
    fn edge_punctuation_is_ignored_but_case_is_not() {
        assert_eq!(dice("Haus, Hof.", "Haus Hof"), 1.0);
        assert!(dice("Haus", "haus") < 1.0);
    }

    #[test]
    fn small_ocr_difference_scores_high_but_not_one() {
        let a = "one two three four five six seven eight nine ten";
        let b = "one two three four five six seven eight nine tcn";
        let d = dice(a, b);
        assert!(d > 0.85 && d < 1.0, "{d}");
    }

    #[test]
    fn unrelated_text_scores_low() {
        assert!(dice("alpha beta gamma delta", "one two three four") < 0.01);
    }

    #[test]
    fn empty_cases() {
        assert_eq!(dice("", " \n"), 1.0);
        assert_eq!(dice("", "word"), 0.0);
        assert_eq!(length_ratio("", ""), 1.0);
        assert_eq!(length_ratio("a b", ""), 0.0);
    }

    #[test]
    fn repeated_words_count_as_a_multiset() {
        // "a a a" against "a" shares one word out of four total.
        assert!((dice("a a a", "a") - 0.5).abs() < 1e-9);
    }

    #[test]
    fn length_ratio_is_symmetric() {
        let a = "a b c d";
        let b = "a b";
        assert_eq!(length_ratio(a, b), length_ratio(b, a));
        assert!((length_ratio(a, b) - 0.5).abs() < 1e-9);
    }
}
