//! Common-word pairs (05 §5.5.3): a second copy of a page's words, in the
//! index field `text_cg`, where each common word is joined to the word after
//! it (`of` before `gold` is `of_gold`). A phrase holding a common word is
//! searched there instead of in `text`, so the engine never reads the
//! positions of the common word itself, which is on almost every page: over
//! 1896, the phrase "cross of gold" took 41 s in `text` and `cross gold`
//! within 2 words 9 s (06 §6.5).
//!
//! The field has the same positions as `text`: one token per word. A word
//! the analyzer drops (longer than [`MAX_TOKEN_CHARS`]) keeps its position
//! as [`GAP`]. A common word's token holds the next word's, so a phrase whose
//! words map through [`query_terms`] matches at a position in `text_cg`
//! exactly when the words are at that position in `text`: a non-common word
//! is its own token, and a common one checks the word after it as well.
//! Only the last word of the phrase has no word after it to check, so a
//! phrase ending in a common word stays on `text`.
//!
//! The list is part of the index: changing it changes [`VERSION`], and a
//! release then rebuilds every index (`usnm-ingest`), since the API only
//! searches `text_cg` when the published version was built with its own
//! [`VERSION`] (`current.json`'s `common_grams`).

use crate::text::{fold, MAX_TOKEN_CHARS};

/// Bumped whenever [`WORDS`] or the token rules change.
pub const VERSION: u32 = 1;

/// The position of a word the analyzer drops. Not a word: words are
/// alphanumeric.
pub const GAP: &str = "_";

/// The common words, folded: function words on about half of all pages or
/// more, measured over a week of 1896 (06 §6.5), and `mr`. Content words that
/// are as frequent (`new`, `one`, `time`, `great`) are left out: they are
/// searched for themselves. A longer list costs little: `text_cg` has one
/// token per word whatever the list, and only phrases whose last word is on
/// it can't use the pairs.
pub const WORDS: &[&str] = &[
    "a", "about", "after", "all", "also", "an", "and", "any", "are", "as", "at", "be", "been",
    "before", "being", "but", "by", "can", "could", "do", "for", "from", "had", "has", "have",
    "he", "her", "here", "him", "his", "i", "if", "in", "into", "is", "it", "its", "may", "more",
    "most", "mr", "much", "must", "no", "not", "now", "of", "on", "only", "or", "other", "our",
    "out", "over", "said", "same", "shall", "she", "should", "so", "some", "such", "than", "that",
    "the", "their", "them", "then", "there", "these", "they", "this", "those", "to", "under", "up",
    "upon", "very", "was", "we", "were", "what", "when", "where", "which", "who", "will", "with",
    "would",
];

pub fn is_common(word: &str) -> bool {
    // Every word of every page asks this during a release: WORDS is sorted.
    WORDS.binary_search(&word).is_ok()
}

/// The page's words, one per position as the `usnm_text` analyzer counts
/// them; `None` where it drops one.
fn positions(text: &str) -> Vec<Option<String>> {
    text.split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .map(|t| {
            let w = fold(t);
            (!w.is_empty() && w.chars().count() <= MAX_TOKEN_CHARS).then_some(w)
        })
        .collect()
}

/// The `text_cg` field for a page's text: its tokens joined by spaces (the
/// field's tokenizer is `whitespace`).
pub fn index_text(text: &str) -> String {
    let words = positions(text);
    let mut out = String::with_capacity(text.len() + text.len() / 8);
    for (i, w) in words.iter().enumerate() {
        if i > 0 {
            out.push(' ');
        }
        match w {
            None => out.push_str(GAP),
            Some(w) if is_common(w) => {
                out.push_str(w);
                out.push('_');
                if let Some(Some(next)) = words.get(i + 1) {
                    out.push_str(next);
                }
            }
            Some(w) => out.push_str(w),
        }
    }
    out
}

/// The `text_cg` tokens for an exact phrase of folded words, when searching
/// there gives the same pages as searching `text`: two or more words, at
/// least one of them common, the last one not. `None` otherwise.
pub fn query_terms(words: &[String]) -> Option<Vec<String>> {
    let (last, rest) = words.split_last()?;
    if rest.is_empty()
        || is_common(last)
        || !rest.iter().any(|w| is_common(w))
        || words
            .iter()
            .any(|w| w.is_empty() || w.chars().count() > MAX_TOKEN_CHARS)
    {
        return None;
    }
    Some(
        words
            .iter()
            .enumerate()
            .map(|(i, w)| match words.get(i + 1) {
                Some(next) if is_common(w) => format!("{w}_{next}"),
                _ => w.clone(),
            })
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::tokenize;

    fn words(s: &str) -> Vec<String> {
        s.split(' ').map(str::to_owned).collect()
    }

    #[test]
    fn the_list_is_sorted_for_binary_search() {
        assert!(WORDS.windows(2).all(|w| w[0] < w[1]));
        assert!(WORDS.iter().all(|w| is_common(w)));
        assert!(!is_common("gold") && !is_common(""));
    }

    #[test]
    fn joins_each_common_word_to_the_next() {
        assert_eq!(
            index_text("Upon a Cross of Gold, the end."),
            "upon_a a_cross cross of_gold gold the_end end"
        );
        // The last word, and a common word before a dropped one.
        assert_eq!(
            index_text(&format!("gold of {} the", "x".repeat(41))),
            "gold of_ _ the_"
        );
    }

    #[test]
    fn keeps_the_analyzers_positions() {
        let text = format!("Café—ſo {} of Æsop's ﷺ", "y".repeat(50));
        let cg = index_text(&text);
        // One token per word the analyzer saw, dropped ones included.
        let seen = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|t| !t.is_empty())
            .count();
        assert_eq!(cg.split(' ').count(), seen);
        // The words it keeps are the same.
        let kept: Vec<String> = cg
            .split(' ')
            .filter(|t| *t != GAP)
            .map(|t| t.split('_').next().unwrap().to_owned())
            .collect();
        assert_eq!(kept, tokenize(&text));
    }

    #[test]
    fn maps_phrases_with_common_words() {
        assert_eq!(
            query_terms(&words("cross of gold")),
            Some(words("cross of_gold gold"))
        );
        assert_eq!(
            query_terms(&words("war of the worlds")),
            Some(words("war of_the the_worlds worlds"))
        );
        assert_eq!(
            query_terms(&words("the yellow kid")),
            Some(words("the_yellow yellow kid"))
        );
        // Left in `text`: no common word, one word, or a common word last.
        assert_eq!(query_terms(&words("yellow fever")), None);
        assert_eq!(query_terms(&words("the")), None);
        assert_eq!(query_terms(&words("remember the")), None);
        assert_eq!(query_terms(&[]), None);
    }

    /// Where `phrase` starts in `tokens`, matching word for word.
    fn starts(tokens: &[String], phrase: &[String]) -> Vec<usize> {
        if phrase.is_empty() || tokens.len() < phrase.len() {
            return Vec::new();
        }
        (0..=tokens.len() - phrase.len())
            .filter(|&p| phrase.iter().enumerate().all(|(i, w)| &tokens[p + i] == w))
            .collect()
    }

    #[test]
    fn a_mapped_phrase_matches_exactly_where_the_phrase_does() {
        // Every phrase of 2 to 4 words over a small vocabulary, against pages
        // made of the same words: the phrase in the words and the mapped
        // phrase in `text_cg` start at the same positions.
        let vocab = ["of", "the", "a", "gold", "cross", "silver", "free"];
        let pages = [
            "cross of gold free silver of the free",
            "the cross of the gold of gold cross",
            "a free silver a gold of a cross of",
            "of of the the gold gold cross cross",
        ];
        let mut phrases: Vec<Vec<String>> = vocab.iter().map(|w| vec![(*w).to_owned()]).collect();
        for _ in 0..3 {
            let longer: Vec<Vec<String>> = phrases
                .iter()
                .filter(|p| p.len() < 4)
                .flat_map(|p| {
                    vocab.iter().map(move |w| {
                        let mut q = p.clone();
                        q.push((*w).to_owned());
                        q
                    })
                })
                .collect();
            phrases.extend(longer);
        }
        let mut mapped = 0;
        for page in pages {
            let page_words = tokenize(page);
            let cg: Vec<String> = index_text(page).split(' ').map(str::to_owned).collect();
            for phrase in &phrases {
                if let Some(terms) = query_terms(phrase) {
                    mapped += 1;
                    assert_eq!(
                        starts(&cg, &terms),
                        starts(&page_words, phrase),
                        "{phrase:?} in {page:?}"
                    );
                }
            }
        }
        assert!(mapped > 500, "only {mapped} phrases were mapped");
    }
}
