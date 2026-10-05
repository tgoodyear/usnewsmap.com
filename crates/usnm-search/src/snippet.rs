//! Snippets of a page's stored text (#126), the same for every backend:
//! up to [`MAX_FRAGMENTS`] fragments around the query's matches, HTML-escaped
//! with only `<mark>` as markup. Built in the API from the text Quickwit
//! already returns with each hit, so its single short fragment per field
//! (which its REST API can't lengthen) no longer decides what readers see.

use usnm_core::query::{Node, Term};
use usnm_core::text::{fold, MAX_TOKEN_CHARS};

use crate::mark_html;
use crate::memory::levenshtein_within;

/// Fragments per page.
pub const MAX_FRAGMENTS: usize = 3;
/// Characters of context on each side of a match, about a line of print.
pub const CONTEXT: usize = 80;

/// One indexed word: its folded token and its character range in the text.
struct Word {
    token: String,
    start: usize,
    end: usize,
}

/// The text's words as the `usnm_text` analyzer splits them, with positions.
fn words(chars: &[char]) -> Vec<Word> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_alphanumeric() {
            i += 1;
            continue;
        }
        let start = i;
        while i < chars.len() && chars[i].is_alphanumeric() {
            i += 1;
        }
        let raw: String = chars[start..i].iter().collect();
        let token = fold(&raw);
        // The analyzer drops over-long OCR runs, so they hold no position.
        if token.chars().count() <= MAX_TOKEN_CHARS {
            out.push(Word {
                token,
                start,
                end: i,
            });
        }
    }
    out
}

/// What to mark: a word matching a term, or an exact phrase as a whole.
enum Pattern {
    Term(Term),
    Phrase(Vec<String>),
}

fn patterns(query: &Node) -> Vec<Pattern> {
    let mut out = Vec::new();
    let mut stack = vec![query];
    while let Some(node) = stack.pop() {
        match node {
            Node::Term(t) => out.push(Pattern::Term(t.clone())),
            Node::Phrase { terms, slop: 0 } => out.push(Pattern::Phrase(terms.clone())),
            // A NEAR phrase's words can be far apart: mark them one by one.
            Node::Phrase { terms, .. } => out.extend(terms.iter().map(|t| {
                Pattern::Term(Term {
                    text: t.clone(),
                    fuzzy: 0,
                    prefix: false,
                })
            })),
            Node::And(c) | Node::Or(c) => stack.extend(c),
            Node::Not(_) => {}
        }
    }
    out
}

fn term_matches(t: &Term, token: &str) -> bool {
    if t.prefix {
        token.starts_with(&t.text)
    } else if t.fuzzy > 0 {
        levenshtein_within(&t.text, token, usize::from(t.fuzzy))
    } else {
        token == t.text
    }
}

/// Character ranges of the matches, sorted and merged.
fn matches(words: &[Word], patterns: &[Pattern]) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for p in patterns {
        match p {
            Pattern::Term(t) => hits.extend(
                words
                    .iter()
                    .filter(|w| term_matches(t, &w.token))
                    .map(|w| (w.start, w.end)),
            ),
            Pattern::Phrase(terms) if !terms.is_empty() => {
                for run in words.windows(terms.len()) {
                    if run.iter().zip(terms).all(|(w, t)| &w.token == t) {
                        hits.push((run[0].start, run[terms.len() - 1].end));
                    }
                }
            }
            Pattern::Phrase(_) => {}
        }
    }
    hits.sort_unstable();
    let mut merged: Vec<(usize, usize)> = Vec::new();
    for (s, e) in hits {
        match merged.last_mut() {
            Some(last) if s <= last.1 => last.1 = last.1.max(e),
            _ => merged.push((s, e)),
        }
    }
    merged
}

/// Move `at` back to the start of the word it falls in.
fn word_start(chars: &[char], mut at: usize) -> usize {
    while at > 0
        && at < chars.len()
        && chars[at - 1].is_alphanumeric()
        && chars[at].is_alphanumeric()
    {
        at -= 1;
    }
    at
}

/// Move `at` forward to the end of the word it falls in.
fn word_end(chars: &[char], mut at: usize) -> usize {
    while at > 0
        && at < chars.len()
        && chars[at - 1].is_alphanumeric()
        && chars[at].is_alphanumeric()
    {
        at += 1;
    }
    at
}

/// Up to [`MAX_FRAGMENTS`] fragments of `text` around the matches of the
/// query's positive words and phrases: exact phrases marked as a whole, a
/// NEAR phrase's words one by one, prefix and fuzzy terms on each word they
/// match. Matches closer together than the context share a fragment, and
/// fragments never overlap. Line breaks (columns) become spaces; a fragment
/// that doesn't reach the text's start or end is marked with `…`.
pub fn text_snippets(text: &str, query: &Node) -> Vec<String> {
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let hits = matches(&words(&chars), &patterns(query));
    let mut out = Vec::new();
    let mut covered = 0;
    let mut k = 0;
    while k < hits.len() && out.len() < MAX_FRAGMENTS {
        let (s, _) = hits[k];
        let from = word_start(&chars, s.saturating_sub(CONTEXT).max(covered)).max(covered);
        let mut pieces: Vec<(bool, String)> = Vec::new();
        if from > 0 {
            pieces.push((false, "… ".to_owned()));
        }
        let mut at = from;
        // Take every match that starts within the context of the last one.
        while k < hits.len() && hits[k].0 <= at.max(s) + CONTEXT {
            let (hs, he) = hits[k];
            if hs > at {
                pieces.push((false, chars[at..hs].iter().collect()));
            }
            pieces.push((true, chars[hs..he].iter().collect()));
            at = he;
            k += 1;
        }
        let to = word_end(&chars, (at + CONTEXT).min(chars.len()));
        if to > at {
            pieces.push((false, chars[at..to].iter().collect()));
        }
        if to < chars.len() {
            pieces.push((false, " …".to_owned()));
        }
        covered = to;
        // Collapse the spaces OCR leaves between columns.
        let pieces: Vec<(bool, String)> = pieces
            .into_iter()
            .map(|(m, s)| (m, collapse_spaces(&s)))
            .collect();
        let borrowed: Vec<(bool, &str)> = pieces.iter().map(|(m, s)| (*m, s.as_str())).collect();
        out.push(mark_html(&borrowed));
    }
    out
}

fn collapse_spaces(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if c == ' ' {
            if !space {
                out.push(' ');
            }
            space = true;
        } else {
            out.push(c);
            space = false;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_core::query::parse;

    fn snip(text: &str, q: &str) -> Vec<String> {
        text_snippets(text, &parse(q).unwrap())
    }

    #[test]
    fn marks_words_and_escapes_everything_else() {
        assert_eq!(
            snip("a <b> cross of Gold, & more", "gold"),
            ["a &lt;b&gt; cross of <mark>Gold</mark>, &amp; more"]
        );
    }

    #[test]
    fn marks_an_exact_phrase_as_a_whole_and_not_its_words_alone() {
        assert_eq!(
            snip(
                "gold here, and a cross\nof  gold there",
                "\"cross of gold\""
            ),
            ["gold here, and a <mark>cross of gold</mark> there"]
        );
    }

    #[test]
    fn marks_near_words_prefixes_and_fuzzy_matches_but_not_excluded_words() {
        assert_eq!(
            snip(
                "Bryan spoke; crucify mankind upon silver",
                "\"bryan mankind\"~5 cruc* mankimd~1 -silver"
            ),
            ["<mark>Bryan</mark> spoke; <mark>crucify</mark> <mark>mankind</mark> upon silver"]
        );
    }

    #[test]
    fn folds_like_the_index() {
        assert_eq!(
            snip("the Ñoño café", "nono cafe"),
            ["the <mark>Ñoño</mark> <mark>café</mark>"]
        );
    }

    #[test]
    fn gives_up_to_three_fragments_with_ellipses_on_word_boundaries() {
        let filler = |n: usize| vec!["word"; n].join(" ");
        let text = [
            filler(40),
            "gold".into(),
            filler(60),
            "gold".into(),
            filler(60),
            "gold".into(),
            filler(60),
            "gold".into(),
            filler(10),
        ]
        .join(" ");
        let s = snip(&text, "gold");
        assert_eq!(s.len(), MAX_FRAGMENTS);
        for f in &s {
            assert_eq!(f.matches("<mark>gold</mark>").count(), 1, "{f}");
            assert!(f.starts_with("… word") && f.ends_with("word …"), "{f}");
        }
    }

    #[test]
    fn nearby_matches_share_a_fragment() {
        let s = snip("gold and silver and gold again", "gold silver");
        assert_eq!(
            s,
            ["<mark>gold</mark> and <mark>silver</mark> and <mark>gold</mark> again"]
        );
    }

    #[test]
    fn no_match_no_fragment() {
        assert!(snip("nothing here", "gold").is_empty());
        assert!(snip("gold", "silver -gold").is_empty());
    }
}
