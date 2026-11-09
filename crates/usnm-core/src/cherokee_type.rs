//! Two misreadings of the first Cherokee type, undone on a word (#1620).
//!
//! The *Cherokee Phoenix* (1828–1834) was set in one font, cast in Boston
//! in 1827. Every OCR engine we have tried misreads two of its letters the
//! same way:
//!
//! - Ꮩ (do) is printed as Λ, an upside-down V, and read as Ꭺ (go). The
//!   1834 recut of the font made it a V; the recut type reached the press in
//!   1835 (Giasson 2004, report section 4 on #1618).
//! - Ꮎ (na) at the start of a word is printed as a block G and read as one
//!   of the G-shaped letters Ꮹ (wa), Ꮳ (tsa) or Ᏻ (yu), or as Ꮐ (nah).
//!   Inside a word it is the θ-like Ꮎ (test set v2, #1620).
//!
//! [`fix_word`] undoes them on a word that isn't in the vocabulary of the
//! 1860 Cherokee New Testament, when exactly one reading is in it. On the
//! test set's 70 hand-checked lines it took LoC's character error from
//! 8.39% to 7.86% (#1620). On check.usnewsmap.com (8 November 2026) no
//! answer on a word the Ꮩ rule changes picked LoC's reading. The Ꮎ rule
//! rests on alignment with the New Testament instead (ᎾᏍᎩ: 103 G-shaped
//! readings, 3 Ꮎ): checkers matching shapes picked LoC's G-shaped letter
//! 5 times and never the Ꮎ reading. [`misreadings`] is its inverse: the words a
//! fix would turn into a given word, which a search for that word also
//! looks for ([`query_phrases`]).
//!
//! The release applies the fix only to pages printed before
//! [`RECUT_ARRIVED`] ([`printed_with_1828_type`]). A search can't tell a
//! page's date from a word, so it looks for the misreadings on every page;
//! they are words outside the vocabulary that a fix would give this word,
//! so on later print they are rare.

use std::collections::{BTreeSet, HashSet};
use std::sync::OnceLock;

use chrono::NaiveDate;

use crate::query::Node;

/// The first day of the year the recut type reached the press (1835): a
/// page printed before it was set in the 1827 type.
pub const RECUT_ARRIVED: NaiveDate = match NaiveDate::from_ymd_opt(1835, 1, 1) {
    Some(d) => d,
    None => panic!("a valid date"),
};

/// The most Ꭺ letters of one word that [`fix_word`] tries as Ꮩ, in every
/// combination (2⁶ − 1 readings). A word with more is left as it is.
pub const MAX_GO_LETTERS: usize = 6;

/// The most Ꮩ letters of one word that [`misreadings`] reads back as Ꭺ.
/// A word with more has only its Ꮎ misreadings.
pub const MAX_DO_LETTERS: usize = 4;

const DO: char = 'Ꮩ';
const GO: char = 'Ꭺ';
const NA: char = 'Ꮎ';
/// The letters OCR reads a word-initial Ꮎ as.
const INITIAL_NA_READ_AS: [char; 4] = ['Ꮹ', 'Ꮳ', 'Ᏻ', 'Ꮐ'];

/// Whether a page printed on `date` was set in the 1827 type.
pub fn printed_with_1828_type(date: NaiveDate) -> bool {
    date < RECUT_ARRIVED
}

/// The forms found in both editions of the 1860 New Testament
/// (`data/damage-words/chr-attested.txt.zst`, CC0, #1581), in capitals.
fn vocabulary() -> &'static HashSet<&'static str> {
    static WORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| {
        include_str!(concat!(
            env!("OUT_DIR"),
            "/data/damage-words/chr-attested.txt"
        ))
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect()
    })
}

/// Whether `c` is a letter of the syllabary, capital or small.
pub fn is_syllabary(c: char) -> bool {
    matches!(c, '\u{13A0}'..='\u{13F5}' | '\u{13F8}'..='\u{13FD}' | '\u{AB70}'..='\u{ABBF}')
}

fn capital(c: char) -> char {
    c.to_uppercase().next().unwrap_or(c)
}

/// `word` in capitals, and whether it had a small letter (the index's
/// folded form). `None` unless every character is a syllabary letter.
fn capitals(word: &str) -> Option<(Vec<char>, bool)> {
    if word.is_empty() || !word.chars().all(is_syllabary) {
        return None;
    }
    let small = word.chars().any(|c| capital(c) != c);
    Some((word.chars().map(capital).collect(), small))
}

fn spell(letters: &[char], small: bool) -> String {
    let s: String = letters.iter().collect();
    if small {
        s.to_lowercase()
    } else {
        s
    }
}

fn known(letters: &[char]) -> bool {
    vocabulary().contains(letters.iter().collect::<String>().as_str())
}

/// Every reading of `letters` with one or more of its Ꭺ as Ꮩ, or none
/// when it has more than [`MAX_GO_LETTERS`].
fn go_as_do(letters: &[char]) -> Vec<Vec<char>> {
    let at: Vec<usize> = (0..letters.len()).filter(|&i| letters[i] == GO).collect();
    if at.is_empty() || at.len() > MAX_GO_LETTERS {
        return Vec::new();
    }
    (1u32..1 << at.len())
        .map(|mask| {
            let mut out = letters.to_vec();
            for (bit, &i) in at.iter().enumerate() {
                if mask & 1 << bit != 0 {
                    out[i] = DO;
                }
            }
            out
        })
        .collect()
}

/// The word OCR misread as `word`, by the two rules in the module
/// documentation, or `None` to leave it. Only a word wholly of syllabary
/// letters and not in the vocabulary is read again. Ꭺ as Ꮩ is tried first;
/// if no reading of that kind is in the vocabulary, a word-initial Ꮹ, Ꮳ,
/// Ᏻ or Ꮐ as Ꮎ. A rule is taken only when exactly one of its readings is in
/// the vocabulary. The result keeps the word's case: capitals as LoC's text
/// has them, or small letters as the index folds them.
pub fn fix_word(word: &str) -> Option<String> {
    let (letters, small) = capitals(word)?;
    if known(&letters) {
        return None;
    }
    let mut hits: Vec<Vec<char>> = go_as_do(&letters)
        .into_iter()
        .filter(|r| known(r))
        .collect();
    if hits.is_empty() && INITIAL_NA_READ_AS.contains(&letters[0]) {
        let mut r = letters.clone();
        r[0] = NA;
        if known(&r) {
            hits.push(r);
        }
    }
    match hits.as_slice() {
        [one] => Some(spell(one, small)),
        _ => None,
    }
}

/// `text` with [`fix_word`] applied to each run of syllabary letters, or
/// `None` when no word changes. Everything else, Latin text included, is
/// left exactly as it is.
pub fn fix_text(text: &str) -> Option<String> {
    let mut out = String::with_capacity(text.len());
    let mut changed = false;
    let mut run_start: Option<usize> = None;
    let mut flush = |out: &mut String, run: &str| match fix_word(run) {
        Some(fixed) => {
            out.push_str(&fixed);
            changed = true;
        }
        None => out.push_str(run),
    };
    for (i, c) in text.char_indices() {
        if is_syllabary(c) {
            run_start.get_or_insert(i);
        } else {
            if let Some(s) = run_start.take() {
                flush(&mut out, &text[s..i]);
            }
            out.push(c);
        }
    }
    if let Some(s) = run_start {
        flush(&mut out, &text[s..]);
    }
    changed.then_some(out)
}

/// The words [`fix_word`] turns into `word`, in `word`'s case, sorted:
/// `word` with some of its Ꮩ as Ꭺ (up to [`MAX_DO_LETTERS`] of them), or
/// with a word-initial Ꮎ as Ꮹ, Ꮳ, Ᏻ or Ꮐ, each kept only if the fix reads
/// it back as `word`. Empty for a word not wholly of syllabary letters.
pub fn misreadings(word: &str) -> Vec<String> {
    let Some((letters, small)) = capitals(word) else {
        return Vec::new();
    };
    let mut candidates: Vec<Vec<char>> = Vec::new();
    let at: Vec<usize> = (0..letters.len()).filter(|&i| letters[i] == DO).collect();
    if !at.is_empty() && at.len() <= MAX_DO_LETTERS {
        for mask in 1u32..1 << at.len() {
            let mut r = letters.clone();
            for (bit, &i) in at.iter().enumerate() {
                if mask & 1 << bit != 0 {
                    r[i] = GO;
                }
            }
            candidates.push(r);
        }
    }
    if letters[0] == NA {
        for c in INITIAL_NA_READ_AS {
            let mut r = letters.clone();
            r[0] = c;
            candidates.push(r);
        }
    }
    let target = spell(&letters, false);
    candidates
        .into_iter()
        .filter(|r| fix_word(&spell(r, false)).as_deref() == Some(target.as_str()))
        .map(|r| spell(&r, small))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

/// The one-word phrases a search for `node` also looks for: an exact
/// term's [`misreadings`]. Empty for anything else: a prefix, wildcard,
/// `~ocr` or fuzzy term (`~ocr` has its own Cherokee table), a phrase and
/// any operator.
pub fn query_phrases(node: &Node) -> Vec<Vec<String>> {
    match node {
        Node::Term(t) if t.is_exact() => {
            misreadings(&t.text).into_iter().map(|m| vec![m]).collect()
        }
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Term;

    #[test]
    fn go_read_for_do_is_undone() {
        // ᎦᏙᎯ ("earth"): its Ꮩ is always read as Ꭺ (#1619).
        assert_eq!(fix_word("ᎦᎪᎯ").as_deref(), Some("ᎦᏙᎯ"));
        // The index's folded form stays folded.
        assert_eq!(fix_word("ꭶꭺꭿ").as_deref(), Some("ꭶꮩꭿ"));
    }

    #[test]
    fn word_initial_g_is_na() {
        for misread in ["ᏩᏍᎩ", "ᏣᏍᎩ", "ᏳᏍᎩ", "ᏀᏍᎩ"] {
            assert_eq!(fix_word(misread).as_deref(), Some("ᎾᏍᎩ"), "{misread}");
        }
    }

    #[test]
    fn known_words_and_other_text_are_left() {
        assert_eq!(fix_word("ᎾᏍᎩ"), None);
        assert_eq!(fix_word("ᎠᎴ"), None);
        // A Ꭺ that is right stays: ᎪᎱᏍᏗ ("something") is a word.
        assert_eq!(fix_word("ᎪᎱᏍᏗ"), None);
        assert_eq!(fix_word("gold"), None);
        assert_eq!(fix_word("ᎾᏍᎩ,"), None, "not wholly syllabary");
        assert_eq!(fix_word(""), None);
    }

    #[test]
    fn text_keeps_everything_but_the_fixed_words() {
        assert_eq!(
            fix_text("ᏩᏍᎩ ᎠᎴ ᎦᎪᎯ, THE EARTH.").as_deref(),
            Some("ᎾᏍᎩ ᎠᎴ ᎦᏙᎯ, THE EARTH.")
        );
        assert_eq!(fix_text("ᎾᏍᎩ ᎠᎴ the gold"), None);
        assert_eq!(fix_text(""), None);
    }

    #[test]
    fn misreadings_are_the_fix_inverted() {
        assert_eq!(misreadings("ᎾᏍᎩ"), ["ᏀᏍᎩ", "ᏣᏍᎩ", "ᏩᏍᎩ", "ᏳᏍᎩ"]);
        assert_eq!(misreadings("ꭶꮩꭿ"), ["ꭶꭺꭿ"]);
        for word in ["ᎾᏍᎩ", "ᎦᏙᎯ", "ꮎꮝꭹ"] {
            for m in misreadings(word) {
                assert_eq!(fix_word(&m).as_deref(), Some(word), "{m}");
            }
        }
        assert!(misreadings("gold").is_empty());
        assert!(misreadings("ᎠᎴ").is_empty());
    }

    #[test]
    fn only_an_exact_term_has_phrases() {
        let exact = Term {
            text: "ꮎꮝꭹ".into(),
            fuzzy: 0,
            prefix: false,
            wildcard: false,
            ocr: false,
        };
        assert_eq!(query_phrases(&Node::Term(exact.clone())).len(), 4);
        let ocr = Term {
            ocr: true,
            ..exact.clone()
        };
        assert!(query_phrases(&Node::Term(ocr)).is_empty());
        assert!(query_phrases(&Node::Phrase {
            terms: vec!["ꮎꮝꭹ".into(), "ꭰꮄ".into()],
            slop: 0
        })
        .is_empty());
    }

    #[test]
    fn the_recut_date() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert!(printed_with_1828_type(d(1828, 2, 21)));
        assert!(printed_with_1828_type(d(1834, 5, 31)));
        assert!(!printed_with_1828_type(d(1835, 1, 1)));
        assert!(!printed_with_1828_type(d(1844, 9, 26)));
    }
}
