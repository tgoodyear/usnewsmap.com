//! A fraction joined to a number, spelled the way OCR often reads it (06 §6.4,
//! #1067).
//!
//! From [`Analyzer::V2`](crate::text::Analyzer) a fraction glyph is one word
//! (`½`, `61½`), as the `usnm_text` analyzer has it. Newspaper print is often
//! read with the fraction as digits and a slash: `61½` comes out as `61 1/2`
//! or `611/2`, which the analyzer splits at the slash into the words "61 1 2"
//! and "611 2". So a search for `61½` also looks for those two phrases. This
//! is done when a search is run, not in the index: the engine is sent the
//! glyph and the phrases `OR`ed (`(text:61½ OR text:"61 1 2" OR text:"611
//! 2")`), and the memory backend and the snippets read the query the same
//! way, so a version needs no rebuild to search this way.
//!
//! Only a number and a vulgar fraction character are read this way. A bare
//! `½` keeps its meaning, the glyph (the decision on #1067). Nor is a word
//! that analyzer version 1 folded (`611⁄2` holds U+2044): the engine already
//! reads it as the phrase "611 2".

use unicode_normalization::UnicodeNormalization;

use crate::query::Node;

/// The most fraction words a phrase may have to be searched this way: each
/// has three spellings, so a phrase of two is nine phrases, and one of
/// three would be twenty-seven. A phrase with more is searched with its
/// glyphs only.
pub const MAX_PHRASE_FRACTIONS: usize = 2;

/// The numerator and denominator of a vulgar fraction character, as the
/// digits of its compatibility form: `½` is `1⁄2`, so ("1", "2"). `None` for
/// any other character, and for `⅟` (U+215F), which has a numerator only.
fn parts(c: char) -> Option<(String, String)> {
    if c.is_ascii() {
        return None;
    }
    let decomposed: String = c.nfkd().collect();
    let (numerator, denominator) = decomposed.split_once('\u{2044}')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(numerator) && digits(denominator))
        .then(|| (numerator.to_owned(), denominator.to_owned()))
}

/// The two ways OCR reads a number and a fraction character as digits and a
/// slash, as the words the analyzer makes of them: `61½` is `61 1/2`, the
/// words `["61", "1", "2"]`, and `611/2`, `["611", "2"]`. `None` unless
/// `word` is ASCII digits followed by one vulgar fraction character.
pub fn slash_spellings(word: &str) -> Option<[Vec<String>; 2]> {
    let fraction = word.chars().next_back()?;
    let number = &word[..word.len() - fraction.len_utf8()];
    if number.is_empty() || !number.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (numerator, denominator) = parts(fraction)?;
    Some([
        vec![number.to_owned(), numerator.clone(), denominator.clone()],
        vec![format!("{number}{numerator}"), denominator],
    ])
}

/// The phrases, besides the leaf itself, that a search for it also looks for
/// (#1067): for an exact term that is a number and a fraction character
/// (`61½`), its [`slash_spellings`]; for an exact phrase (no slop) with up
/// to [`MAX_PHRASE_FRACTIONS`] such words, the phrase with each of them
/// spelled each way, except as it is (`wheat 61½` also looks for "wheat 61 1
/// 2" and "wheat 611 2"). Empty for anything else: a prefix, wildcard,
/// `~ocr` or fuzzy term, a bare fraction, a NEAR phrase (a slash spelling is
/// two or three words where the glyph is one, so the slop would mean
/// something else), and any operator.
pub fn slash_phrases(node: &Node) -> Vec<Vec<String>> {
    match node {
        Node::Term(t) if t.is_exact() => slash_spellings(&t.text).map_or_else(Vec::new, Vec::from),
        Node::Phrase { terms, slop: 0 } => phrase_spellings(terms),
        _ => Vec::new(),
    }
}

fn phrase_spellings(terms: &[String]) -> Vec<Vec<String>> {
    let spellings: Vec<Option<[Vec<String>; 2]>> =
        terms.iter().map(|t| slash_spellings(t)).collect();
    let fractions = spellings.iter().flatten().count();
    if fractions == 0 || fractions > MAX_PHRASE_FRACTIONS {
        return Vec::new();
    }
    // Every way to spell each word, the first being the phrase as it is.
    let mut phrases: Vec<Vec<String>> = vec![Vec::new()];
    for (term, spelled) in terms.iter().zip(spellings) {
        let mut ways = vec![vec![term.clone()]];
        ways.extend(spelled.into_iter().flatten());
        phrases = phrases
            .iter()
            .flat_map(|so_far| {
                ways.iter().map(move |way| {
                    let mut phrase = so_far.clone();
                    phrase.extend(way.iter().cloned());
                    phrase
                })
            })
            .collect();
    }
    // The first is the phrase as it is, which the engine is sent already.
    debug_assert_eq!(phrases[0], terms);
    phrases.into_iter().skip(1).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::common_grams;
    use crate::query::{parse, Term};
    use crate::text::{tokenize, vulgar_fractions, Analyzer};

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    fn phrases(q: &str) -> Vec<Vec<String>> {
        slash_phrases(&parse(q).unwrap())
    }

    #[test]
    fn a_number_and_a_fraction_have_two_slash_spellings() {
        assert_eq!(
            slash_spellings("61½"),
            Some([words("61 1 2"), words("611 2")])
        );
        assert_eq!(slash_spellings("3¾"), Some([words("3 3 4"), words("33 4")]));
        assert_eq!(
            slash_spellings("1⅒"),
            Some([words("1 1 10"), words("11 10")])
        );
        assert_eq!(
            slash_spellings("20↉"),
            Some([words("20 0 3"), words("200 3")])
        );
    }

    #[test]
    fn other_words_have_none() {
        for w in [
            // A bare fraction keeps its meaning.
            "½", "⅟", "1⅟", // Not a number, not one fraction, not the fraction last.
            "a½", "x61½", "61½½", "61½2", "½61", "6.1½", "-61½", "",
            // Another character that has digits in its compatibility form.
            "61²", "61⑴", "61㊀", "61٣", "６１½",
            // Plain words and numbers, and what version 1 folds a fraction to.
            "gold", "61", "611⁄2", "61/2",
        ] {
            assert_eq!(slash_spellings(w), None, "{w:?}");
        }
    }

    /// Every vulgar fraction reads as the words the analyzer makes of the
    /// number and the fraction typed with a slash, with a space after the
    /// number or without (`61 1/2`, `611/2`).
    #[test]
    fn the_spellings_are_the_words_the_analyzer_makes_of_the_slash_forms() {
        let mut read = 0;
        for c in vulgar_fractions() {
            let Some([spaced, joined]) = slash_spellings(&format!("61{c}")) else {
                assert!(c == '\u{215F}', "{c:?} has no slash spellings");
                continue;
            };
            read += 1;
            let (n, d) = parts(c).unwrap();
            for a in Analyzer::ALL {
                assert_eq!(tokenize(&format!("61 {n}/{d}"), a), spaced, "{c:?} {a:?}");
                assert_eq!(tokenize(&format!("61{n}/{d}"), a), joined, "{c:?} {a:?}");
            }
        }
        // U+00BC–U+00BE, U+2150–U+215E and U+2189.
        assert_eq!(read, 3 + 15 + 1);
    }

    #[test]
    fn only_an_exact_term_has_them() {
        assert_eq!(phrases("61½"), [words("61 1 2"), words("611 2")]);
        // The canonical form is the glyph's alone.
        assert_eq!(parse("61½").unwrap().to_string(), "61½");
        let exact = Term {
            text: "61½".into(),
            fuzzy: 0,
            prefix: false,
            wildcard: false,
            ocr: false,
        };
        for t in [
            Term {
                fuzzy: 1,
                ..exact.clone()
            },
            Term {
                prefix: true,
                ..exact.clone()
            },
            Term {
                wildcard: true,
                ..exact.clone()
            },
            Term {
                ocr: true,
                ..exact.clone()
            },
        ] {
            assert!(slash_phrases(&Node::Term(t)).is_empty());
        }
        assert_eq!(slash_phrases(&Node::Term(exact)).len(), 2);
        // Operators hold leaves, which have their own.
        for q in [
            "61½ gold",
            "61½ OR gold",
            "gold -61½",
            "2 OF (61½ gold silver)",
        ] {
            assert!(phrases(q).is_empty(), "{q}");
        }
        // Version 1 reads the glyph as the phrase "611 2" already.
        let v1 = crate::query::parse_with("61½", Analyzer::V1).unwrap();
        assert!(slash_phrases(&v1).is_empty(), "{v1}");
        for q in ["½", "61/2", "gold", "\"61 1 2\""] {
            assert!(phrases(q).is_empty(), "{q}");
        }
    }

    #[test]
    fn an_exact_phrase_has_each_of_its_fraction_words_spelled_each_way() {
        assert_eq!(
            phrases(r#""wheat 61½""#),
            [words("wheat 61 1 2"), words("wheat 611 2")]
        );
        assert_eq!(
            phrases(r#""61½ wheat""#),
            [words("61 1 2 wheat"), words("611 2 wheat")]
        );
        // A bare fraction stays the glyph.
        assert_eq!(
            phrases(r#""closed ½ higher at 61¼""#),
            [
                words("closed ½ higher at 61 1 4"),
                words("closed ½ higher at 611 4")
            ]
        );
        // Two fraction words: nine phrases less the one as typed.
        let two = phrases(r#""wheat 61½ oats 20¾""#);
        assert_eq!(two.len(), 8);
        assert!(!two.contains(&words("wheat 61½ oats 20¾")));
        for want in [
            "wheat 61 1 2 oats 20¾",
            "wheat 611 2 oats 20¾",
            "wheat 61½ oats 20 3 4",
            "wheat 61½ oats 203 4",
            "wheat 61 1 2 oats 20 3 4",
            "wheat 61 1 2 oats 203 4",
            "wheat 611 2 oats 20 3 4",
            "wheat 611 2 oats 203 4",
        ] {
            assert!(two.contains(&words(want)), "{want}");
        }
        let mut sorted = two.clone();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), two.len());
        // More than the cap: the glyphs only.
        assert!(phrases(r#""61½ 62¼ 63¾""#).is_empty());
        // The same fraction twice is two fraction words.
        assert_eq!(phrases(r#""61½ 61½""#).len(), 8);
        // A NEAR phrase isn't read this way.
        assert!(phrases(r#""wheat 61½"~3"#).is_empty());
        assert!(slash_phrases(&Node::Phrase {
            terms: words("wheat 61½"),
            slop: 2
        })
        .is_empty());
        // Nor is one without a fraction word.
        assert!(phrases(r#""wheat closed""#).is_empty());
    }

    /// The spellings are digits only, so the common-word pairs (`text_cg`)
    /// never take them: a phrase of them alone stays in `text`, and one with
    /// a common word before them reads the pair as the index writes it.
    #[test]
    fn spellings_are_digits_and_leave_the_common_word_pairs_alone() {
        for w in slash_spellings("61½").unwrap() {
            assert!(w.iter().all(|t| t.bytes().all(|b| b.is_ascii_digit())));
            assert!(w.iter().all(|t| !common_grams::is_common(t)));
            assert_eq!(common_grams::query_terms(&w), None);
        }
        let at = phrases(r#""higher at 61½""#);
        assert_eq!(
            common_grams::query_terms(&at[0]),
            Some(words("higher at_61 61 1 2"))
        );
    }
}
