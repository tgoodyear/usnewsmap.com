//! Snippets of a page's stored text (#126), the same for every backend:
//! up to [`MAX_FRAGMENTS`] fragments around the query's matches, HTML-escaped
//! with only `<mark>` as markup. Built in the API from the text Quickwit
//! already returns with each hit, so its single short fragment per field
//! (which its REST API can't lengthen) no longer decides what readers see.

use usnm_core::query::{Node, Term};
use usnm_core::text::{fold, index_tokens, MAX_TOKEN_CHARS, REMOVE_LONG_BYTES, USNM_TEXT};
use usnm_core::text_layout::TextLayout;

use crate::memory::{as_analyzed, eval, term_matches, word_matches};
use crate::{
    mark_html, MATCHED_IN_LOC, SNIPPETS_FROM_AMERICAN_STORIES, SNIPPETS_FROM_LOC_ORIGINAL,
};

/// Fragments per page.
pub const MAX_FRAGMENTS: usize = 3;
/// Characters of context on each side of a match, about a line of print.
pub const CONTEXT: usize = 80;

/// One word as the analyzer counts positions: its folded token, or `None`
/// for an over-long OCR run the index drops but still counts as a position
/// ([`indexed`]), and its character range in the text.
struct Word {
    token: Option<String>,
    start: usize,
    end: usize,
}

/// A word as the searched field of `layout` holds it (05 §5.5.7): on
/// `text` and `text_as` (layout 1) `None` for 255 bytes or more as split
/// ([`index_tokens`], #286), on `text_all` (layouts 2 and 3) also for a
/// folded word over [`MAX_TOKEN_CHARS`].
fn indexed(raw: &str, layout: TextLayout) -> Option<String> {
    (raw.len() < REMOVE_LONG_BYTES)
        .then(|| fold(raw, USNM_TEXT))
        .and_then(|t| single_keeps(t, layout))
}

/// `None` for a word the one searched field of `layout` drops (over
/// [`MAX_TOKEN_CHARS`]).
fn single_keeps(word: String, layout: TextLayout) -> Option<String> {
    (!layout.searches_one_field() || word.chars().count() <= MAX_TOKEN_CHARS).then_some(word)
}

/// The text's words as the searched field of `layout` splits and folds
/// them, with positions.
fn words(chars: &[char], layout: TextLayout) -> Vec<Word> {
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
        // A dropped run keeps its position, so a phrase can't match across it.
        out.push(Word {
            token: indexed(&raw, layout),
            start,
            end: i,
        });
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
                    wildcard: false,
                    ocr: false,
                })
            })),
            Node::And(c) | Node::Or(c) | Node::AtLeast { of: c, .. } => stack.extend(c),
            Node::Not(_) => {}
        }
    }
    out
}

/// Character ranges of the matches, sorted and merged.
fn matches(words: &[Word], patterns: &[Pattern]) -> Vec<(usize, usize)> {
    let mut hits = Vec::new();
    for p in patterns {
        match p {
            Pattern::Term(t) => hits.extend(
                words
                    .iter()
                    .filter(|w| w.token.as_deref().is_some_and(|tok| term_matches(t, tok)))
                    .map(|w| (w.start, w.end)),
            ),
            Pattern::Phrase(terms) if !terms.is_empty() => {
                for run in words.windows(terms.len()) {
                    if run
                        .iter()
                        .zip(terms)
                        .all(|(w, t)| w.token.as_deref().is_some_and(|tok| word_matches(t, tok)))
                    {
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
/// NEAR phrase's words one by one, prefix, wildcard and fuzzy terms on each
/// word they match. Matches closer together than the context share a
/// fragment, and fragments never overlap. Line breaks (columns) become spaces; a fragment
/// that doesn't reach the text's start or end is marked with `…`. The words
/// are matched as the engine matches them: the text's as `usnm_text` reads
/// them, the query's analyzed again ([`as_analyzed`]), so with any analyzer
/// version the marks land on the words the engine found (#168), and with
/// either text layout on the words its searched field holds.
pub fn text_snippets(text: &str, query: &Node, layout: TextLayout) -> Vec<String> {
    let chars: Vec<char> = text
        .chars()
        .map(|c| if c.is_whitespace() { ' ' } else { c })
        .collect();
    let hits = matches(&words(&chars, layout), &patterns(&as_analyzed(query)));
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

/// The texts of a page that `layout` keeps beside its own: LoC's original
/// text only where the layout keeps it (#925), so a caller can pass what
/// the index stores whatever the layout.
fn kept_loc_original(text_loc: Option<&str>, layout: TextLayout) -> Option<&str> {
    text_loc.filter(|_| layout.keeps_loc_original())
}

/// A page's snippets and, when they aren't from the page's own text, where
/// they come from (05 §5.5.4): its `text` first (LoC's, or our OCR's where it
/// replaced LoC's); when the query marks nothing there and the search covers
/// American Stories' text (`american_stories`), that text's, from
/// [`SNIPPETS_FROM_AMERICAN_STORIES`]; when it marks nothing in either and
/// the layout keeps LoC's original text of the page (`text_loc`, #925),
/// that text's, from [`SNIPPETS_FROM_LOC_ORIGINAL`]. Both backends use it.
pub fn page_snippets(
    text: &str,
    text_as: Option<&str>,
    text_loc: Option<&str>,
    query: &Node,
    american_stories: bool,
    layout: TextLayout,
) -> (Vec<String>, Option<&'static str>) {
    let own = text_snippets(text, query, layout);
    if !own.is_empty() {
        return (own, None);
    }
    let others = [
        (
            text_as.filter(|_| american_stories),
            SNIPPETS_FROM_AMERICAN_STORIES,
        ),
        (
            kept_loc_original(text_loc, layout),
            SNIPPETS_FROM_LOC_ORIGINAL,
        ),
    ];
    for (other, source) in others {
        if let Some(other) = other {
            let snippets = text_snippets(other, query, layout);
            if !snippets.is_empty() {
                return (snippets, Some(source));
            }
        }
    }
    (own, None)
}

/// Which of a page's texts the query matches (05 §5.5.4), from the texts
/// the hit already carries for its snippets: [`MATCHED_IN_LOC`] when the
/// page's own `text` matches the whole query on its own,
/// [`SNIPPETS_FROM_AMERICAN_STORIES`] when American Stories' text does, and
/// [`SNIPPETS_FROM_LOC_ORIGINAL`] when LoC's original text of a page our OCR
/// replaced it on does (#925). A page found only by taking a word from each
/// of several texts (`a b` with `a` in one and `b` in the other) lists them
/// all. Evaluated as the memory backend evaluates a query, so a page listed
/// without `loc` is one a search of the page's own text alone doesn't find:
/// the pages `/v1/aggregate` counts in `total.american_stories_only`, on a
/// layout where that can be searched. Both backends use it. The texts are
/// read as the searched field of `layout` holds them ([`indexed`]).
pub fn matched_in(
    text: &str,
    text_as: Option<&str>,
    text_loc: Option<&str>,
    query: &Node,
    layout: TextLayout,
) -> Vec<&'static str> {
    let others = [
        (text_as, SNIPPETS_FROM_AMERICAN_STORIES),
        (
            kept_loc_original(text_loc, layout),
            SNIPPETS_FROM_LOC_ORIGINAL,
        ),
    ];
    let texts: Vec<(&'static str, &str)> = std::iter::once((MATCHED_IN_LOC, text))
        .chain(
            others
                .into_iter()
                .filter_map(|(t, source)| t.map(|t| (source, t))),
        )
        .collect();
    if texts.len() == 1 {
        return vec![MATCHED_IN_LOC];
    }
    let query = as_analyzed(query);
    let read = |t: &str| -> Vec<Option<String>> {
        index_tokens(t, USNM_TEXT)
            .into_iter()
            .map(|w| w.and_then(|w| single_keeps(w, layout)))
            .collect()
    };
    let matching: Vec<&'static str> = texts
        .iter()
        .filter(|(_, t)| eval(&query, &read(t)))
        .map(|(source, _)| *source)
        .collect();
    if matching.is_empty() {
        texts.into_iter().map(|(source, _)| source).collect()
    } else {
        matching
    }
}

/// Characters of a page's opening excerpt when it is listed without a
/// query (#274): two lines of print, as a fragment around a match is.
pub const OPENING: usize = 2 * CONTEXT;

/// A page listed without a query (`/v1/hits?lccn=…` with no `q`, #274):
/// at most the first [`OPENING`] characters of its text, cut before the
/// word the limit falls in (or at the limit when that word is the first, as
/// an over-long OCR run can be), with columns' line breaks and runs of spaces as one space, `…` when the
/// text goes on, and HTML-escaped with no `<mark>`. No fragment for a page
/// with no text. Both backends use it, on the text the hit already carries.
pub fn opening_snippet(text: &str) -> Vec<String> {
    // Whitespace is made one space before the cut is chosen, so a long run
    // of spaces or line breaks in the OCR neither spends the budget nor
    // leaves `…` after nothing but trailing whitespace.
    let chars: Vec<char> = text
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .collect();
    if chars.is_empty() {
        return Vec::new();
    }
    // Back to the start of the word the limit falls in, never forward: an
    // OCR run with no break can go on for most of a page, and the excerpt
    // must stay within the limit whatever the text.
    let limit = OPENING.min(chars.len());
    let to = match word_start(&chars, limit) {
        0 => limit,
        start => start,
    };
    let mut excerpt = chars[..to].iter().collect::<String>().trim_end().to_owned();
    if to < chars.len() {
        excerpt.push_str(" …");
    }
    vec![mark_html(&[(false, excerpt.as_str())])]
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
        text_snippets(text, &parse(q).unwrap(), TextLayout::Separate)
    }

    #[test]
    fn an_opening_excerpt_is_the_first_words_escaped_and_unmarked() {
        assert_eq!(
            opening_snippet("  The <Bee>\n\nOmaha  & Co."),
            ["The &lt;Bee&gt; Omaha &amp; Co."]
        );
        assert!(opening_snippet("").is_empty());
        assert!(opening_snippet(" \n ").is_empty());
        // A long text is cut between words, within the limit.
        let long = format!("{} tail", "word ".repeat(40));
        let cut = &opening_snippet(&long)[0];
        assert!(cut.ends_with("word …"), "{cut}");
        assert!(cut.chars().count() <= OPENING + 2, "{cut}");
        // The word the limit falls in is left out, not finished.
        let lead = "a".repeat(OPENING - 5);
        assert_eq!(
            opening_snippet(&format!("{lead} straddling words")),
            [format!("{lead} …")]
        );
        // An over-long OCR run after the first word is left out whole, and
        // one that starts the text is cut at the limit.
        let run = "x".repeat(100_000);
        assert_eq!(opening_snippet(&format!("The {run} y")), ["The …"]);
        assert_eq!(
            opening_snippet(&format!("{run} y")),
            [format!("{} …", "x".repeat(OPENING))]
        );
    }

    #[test]
    fn an_opening_excerpt_counts_a_run_of_whitespace_as_one_space() {
        // A long run of spaces and line breaks takes one character of the
        // budget, so the words after it are still in the excerpt.
        let gap = format!("{}{}", " ".repeat(OPENING * 3), "\n".repeat(OPENING));
        assert_eq!(
            opening_snippet(&format!("The{gap}Omaha\t\t\tBee")),
            ["The Omaha Bee"]
        );
        let words = "word ".repeat(OPENING / 5 - 2);
        let spaced = format!("lead{gap}{words}end");
        let want = format!("lead {words}end");
        assert!(want.chars().count() <= OPENING, "{want}");
        assert_eq!(opening_snippet(&spaced), [want]);
        // Trailing whitespace, however long, is not more text: no `…`.
        assert_eq!(
            opening_snippet(&format!("The Omaha Bee{}", " \n".repeat(OPENING * 2))),
            ["The Omaha Bee"]
        );
        let full = "x".repeat(OPENING);
        assert_eq!(
            opening_snippet(&format!("{full}{}", "\n ".repeat(OPENING))),
            [full.as_str()]
        );
        // Only the text after the cut makes `…`.
        assert_eq!(
            opening_snippet(&format!("{full}{} y", "\n".repeat(50))),
            [format!("{full} …")]
        );
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
                "\"bryan mankind\"~5 cruci* mankimd~1 -silver"
            ),
            ["<mark>Bryan</mark> spoke; <mark>crucify</mark> <mark>mankind</mark> upon silver"]
        );
    }

    #[test]
    fn marks_each_word_a_wildcard_matches() {
        assert_eq!(
            snip(
                "The President and the Presidency, the Presidents; presi",
                "presi?ent*"
            ),
            ["The <mark>President</mark> and the Presidency, the <mark>Presidents</mark>; presi"]
        );
        assert_eq!(
            snip("Washington, Washinton, Washi. ton", "washi*ton"),
            ["<mark>Washington</mark>, <mark>Washinton</mark>, Washi. ton"]
        );
    }

    #[test]
    fn marks_each_ocr_spelling_and_each_item_of_at_least() {
        assert_eq!(
            snip(
                "The Presldent, the prefident and the presidents",
                "president~ocr"
            ),
            ["The <mark>Presldent</mark>, the <mark>prefident</mark> and the presidents"]
        );
        assert_eq!(
            snip(
                "gold and silver, not bryan",
                "2 OF (gold silver bryan) -orator"
            ),
            ["<mark>gold</mark> and <mark>silver</mark>, not <mark>bryan</mark>"]
        );
    }

    #[test]
    fn marks_what_the_engine_finds_for_a_version_1_query() {
        // With analyzer version 1, `½` is `1⁄2`, which the engine reads as
        // the phrase "1 2" (#168): the mark lands on `1/2`.
        let q = usnm_core::query::parse_with("½", usnm_core::text::Analyzer::V1).unwrap();
        assert_eq!(
            text_snippets("Oats closed 1/2 lower", &q, TextLayout::Separate),
            ["Oats closed <mark>1/2</mark> lower"]
        );
        assert!(text_snippets("Wheat closed ½ higher", &q, TextLayout::Separate).is_empty());
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
    fn a_dropped_ocr_run_breaks_a_phrase() {
        // A run of 41 to 254 bytes is a word of `text`, and one of 255 or
        // more is dropped; both keep their positions (#286).
        for run in ["x".repeat(41), "x".repeat(254), "x".repeat(255)] {
            assert!(snip(&format!("cross {run} of gold"), "\"cross of gold\"").is_empty());
            assert_eq!(
                snip(&format!("{run} gold"), "gold"),
                [format!("{run} <mark>gold</mark>")]
            );
        }
        // A prefix reaches the first, as on `text`.
        let (kept, dropped) = ("y".repeat(254), "z".repeat(255));
        assert_eq!(
            snip(&format!("{kept} {dropped}"), "yyyyy* zzzzz*"),
            [format!("<mark>{kept}</mark> {dropped}")]
        );
        // On layout 2, `text_all` drops words over 40 characters, so a
        // prefix marks none of them, and an OR's other branch found the page.
        let q = parse("gold OR yyyyy*").unwrap();
        let long = "y".repeat(41);
        let text = format!("{long} gold");
        assert_eq!(
            text_snippets(&text, &q, TextLayout::Single),
            [format!("{long} <mark>gold</mark>")]
        );
        assert_eq!(
            text_snippets(&text, &q, TextLayout::Separate),
            [format!("<mark>{long}</mark> <mark>gold</mark>")]
        );
        let q = parse("yyyyy*").unwrap();
        assert!(text_snippets(&long, &q, TextLayout::Single).is_empty());
        assert_eq!(
            matched_in("gold", Some(&long), None, &q, TextLayout::Separate),
            [SNIPPETS_FROM_AMERICAN_STORIES]
        );
        assert_eq!(
            matched_in("gold", Some(&long), None, &q, TextLayout::Single),
            [MATCHED_IN_LOC, SNIPPETS_FROM_AMERICAN_STORIES]
        );
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

    #[test]
    fn american_stories_text_only_when_loc_text_marks_nothing() {
        let q = parse("bryan").unwrap();
        let loc = "the boy orator spoke";
        let american = "BRYAN AT CHICAGO the boy orator spoke";
        // Off: LoC's text only.
        assert_eq!(
            page_snippets(loc, Some(american), None, &q, false, TextLayout::Separate),
            (Vec::new(), None)
        );
        assert_eq!(
            page_snippets(loc, Some(american), None, &q, true, TextLayout::Separate),
            (
                vec!["<mark>BRYAN</mark> AT CHICAGO the boy orator spoke".to_owned()],
                Some(SNIPPETS_FROM_AMERICAN_STORIES)
            )
        );
        // A match in LoC's text keeps its snippets.
        let q = parse("orator").unwrap();
        assert_eq!(
            page_snippets(loc, Some(american), None, &q, true, TextLayout::Separate),
            (vec!["the boy <mark>orator</mark> spoke".to_owned()], None)
        );
        // Nothing in either text, or no second text.
        let q = parse("silver").unwrap();
        assert_eq!(
            page_snippets(loc, Some(american), None, &q, true, TextLayout::Separate),
            (Vec::new(), None)
        );
        assert_eq!(
            page_snippets(loc, None, None, &q, true, TextLayout::Separate),
            (Vec::new(), None)
        );
    }

    #[test]
    fn matched_in_names_the_texts_that_match_the_whole_query() {
        let m = |q: &str, loc: &str, american: Option<&str>| {
            matched_in(
                loc,
                american,
                None,
                &parse(q).unwrap(),
                TextLayout::Separate,
            )
        };
        let loc = "a cross of goid, said the boy orator";
        let american = "BRYAN AT CHICAGO a cross of gold, said the boy oratar";
        assert_eq!(m("cross", loc, Some(american)), ["loc", "american_stories"]);
        assert_eq!(m("orator", loc, Some(american)), ["loc"]);
        assert_eq!(m("gold", loc, Some(american)), ["american_stories"]);
        assert_eq!(
            m(r#""cross of gold""#, loc, Some(american)),
            ["american_stories"]
        );
        // Every word of the query in one text: `boy` is in both, `gold` in one.
        assert_eq!(m("boy gold", loc, Some(american)), ["american_stories"]);
        // A word from each text: neither matches alone, so both are listed.
        assert_eq!(
            m("bryan orator", loc, Some(american)),
            ["loc", "american_stories"]
        );
        // An empty LoC text (a page only American Stories has text for).
        assert_eq!(m("gold", "", Some(american)), ["american_stories"]);
        // No American Stories text: LoC's.
        assert_eq!(m("orator", loc, None), ["loc"]);
    }

    // Our OCR's text with a line of the next column inside the sentence, and
    // LoC's original text of the same page (#925).
    const OURS: &str =
        "waren auf dem Wege Dubnaue, Jowa. ben ist ersichtlich nach Mailand begriffen";
    const LOC_ORIGINAL: &str = "waren auf dem Wege nach Mailand begriffen";

    #[test]
    fn loc_original_snippets_only_when_nothing_else_marks() {
        let layout = TextLayout::SingleWithLoc;
        let american = Some("BRYAN AT CHICAGO the boy orator spoke");
        // Ours marks the word: its snippets, whatever else matches.
        let q = parse("Mailand").unwrap();
        let (snippets, source) = page_snippets(OURS, None, Some(LOC_ORIGINAL), &q, true, layout);
        assert_eq!(source, None);
        assert!(snippets[0].contains("<mark>Mailand</mark>"), "{snippets:?}");
        assert!(snippets[0].contains("Dubnaue"), "{snippets:?}");
        // The phrase is only in LoC's original text.
        let q = parse(r#""Wege nach Mailand""#).unwrap();
        assert_eq!(
            page_snippets(OURS, None, Some(LOC_ORIGINAL), &q, true, layout),
            (
                vec!["waren auf dem <mark>Wege nach Mailand</mark> begriffen".to_owned()],
                Some(SNIPPETS_FROM_LOC_ORIGINAL)
            )
        );
        // American Stories' text comes before LoC's original.
        let q = parse("bryan").unwrap();
        let both = page_snippets(OURS, american, Some("bryan was here"), &q, true, layout);
        assert_eq!(both.1, Some(SNIPPETS_FROM_AMERICAN_STORIES));
        let q = parse("here").unwrap();
        let both = page_snippets(OURS, american, Some("bryan was here"), &q, true, layout);
        assert_eq!(both.1, Some(SNIPPETS_FROM_LOC_ORIGINAL));
        // With American Stories' text left out of the search it isn't marked.
        let q = parse("bryan").unwrap();
        assert_eq!(
            page_snippets(OURS, american, Some("bryan was here"), &q, false, layout),
            (
                vec!["<mark>bryan</mark> was here".to_owned()],
                Some(SNIPPETS_FROM_LOC_ORIGINAL)
            )
        );
        // Nothing marks in any text.
        let q = parse("silver").unwrap();
        assert_eq!(
            page_snippets(OURS, american, Some(LOC_ORIGINAL), &q, true, layout),
            (Vec::new(), None)
        );
    }

    #[test]
    fn a_layout_without_the_original_text_ignores_it() {
        // Layouts 1 and 2 never index the text, so a stray `text_loc` on a
        // document changes neither snippets nor `matched_in`.
        let q = parse(r#""Wege nach Mailand""#).unwrap();
        for layout in [TextLayout::Separate, TextLayout::Single] {
            assert_eq!(
                page_snippets(OURS, None, Some(LOC_ORIGINAL), &q, true, layout),
                (Vec::new(), None)
            );
            assert_eq!(
                matched_in(OURS, None, Some(LOC_ORIGINAL), &q, layout),
                [MATCHED_IN_LOC]
            );
        }
    }

    #[test]
    fn matched_in_names_the_original_text_beside_ours_and_american_stories() {
        let m = |q: &str, american: Option<&str>, original: Option<&str>| {
            matched_in(
                OURS,
                american,
                original,
                &parse(q).unwrap(),
                TextLayout::SingleWithLoc,
            )
        };
        let american = Some("BRYAN AT CHICAGO the boy orator spoke");
        // Words in ours and in LoC's original, or in the original alone.
        assert_eq!(
            m("Mailand", None, Some(LOC_ORIGINAL)),
            ["loc", "loc_original"]
        );
        assert_eq!(
            m(r#""Wege nach Mailand""#, None, Some(LOC_ORIGINAL)),
            ["loc_original"]
        );
        assert_eq!(m("Dubnaue", None, Some(LOC_ORIGINAL)), ["loc"]);
        // Three texts, each found alone.
        assert_eq!(
            m("bryan", american, Some(LOC_ORIGINAL)),
            ["american_stories"]
        );
        assert_eq!(
            m("waren", american, Some(LOC_ORIGINAL)),
            ["loc", "loc_original"]
        );
        assert_eq!(
            m(r#""auf dem Wege""#, american, Some(LOC_ORIGINAL)),
            ["loc", "loc_original"]
        );
        // A word from each of two texts: neither matches alone, all are listed.
        assert_eq!(
            m("Dubnaue bryan", american, Some(LOC_ORIGINAL)),
            ["loc", "american_stories", "loc_original"]
        );
        // No original text: ours, as before.
        assert_eq!(m("Mailand", None, None), ["loc"]);
        assert_eq!(m("bryan", american, None), ["american_stories"]);
        // An original text with no word in common with the query.
        assert_eq!(m("Dubnaue", None, Some("")), ["loc"]);
    }
}
