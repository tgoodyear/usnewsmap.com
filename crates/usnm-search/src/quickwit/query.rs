//! The AST as Quickwit's query language, and the filter clauses that scope a search (05 §5.5).

use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::text_layout::TextLayout;

use crate::{IndexSet, SearchError};

/// Quickwit query-language string for the AST. Terms are folded alphanumerics
/// from our own parser, with at most the wildcards `?` and `*`, so they never
/// need escaping. With `grams` (the indexes have `text_cg`, 05 §5.5.3), an
/// exact phrase holding a common word searches `text_cg`, which never reads
/// the common word's positions. Each of its other words is also required in
/// `text`: that changes no match (a page with the phrase has the words) and
/// gives the snippets on `text` their highlights. A word that folds to
/// several (`ﷺ`) is left out of those: unquoted it would be several terms,
/// and the pairs already require it.
///
/// With `american_stories` (the indexes have `text_as` and `text_as_cg`,
/// 05 §5.5.4), each term or phrase matches in either text: `(text:… OR
/// text_as:…)`. A phrase stays within one text, and `NOT x` excludes a page
/// with `x` in either. Without it, the string is exactly LoC's text alone.
/// This is [`TextLayout::Separate`]'s rendering; [`query_for`] picks the
/// one of a set's layout.
pub fn query_string(
    node: &Node,
    grams: bool,
    american_stories: bool,
) -> Result<String, SearchError> {
    if american_stories {
        render(node, grams, &[&LOC, &AMERICAN_STORIES])
    } else {
        render(node, grams, &[&LOC])
    }
}

/// The query string for `indexes`, as their text layout has the texts
/// (05 §5.5.7): [`query_string`] on a field per text, and on one field for
/// both, each term or phrase once in `text_all` (and `text_all_cg`). The
/// release puts a gap between the texts there, so a phrase still can't
/// span them, and `NOT x` still excludes a page with `x` in either.
pub fn query_for(node: &Node, indexes: &IndexSet) -> Result<String, SearchError> {
    match indexes.text_layout() {
        TextLayout::Separate => {
            query_string(node, indexes.common_grams(), indexes.american_stories())
        }
        TextLayout::Single => render(node, indexes.common_grams(), &[&BOTH]),
    }
}

/// The query with each term or phrase in any of `texts`.
fn render(node: &Node, grams: bool, texts: &[&TextFields]) -> Result<String, SearchError> {
    Ok(match node {
        Node::And(c) => format!("({})", join(c, " AND ", grams, texts)?),
        Node::Or(c) => format!("({})", join(c, " OR ", grams, texts)?),
        Node::AtLeast { min, of } => at_least(usize::from(*min), of, grams, texts)?,
        Node::Not(n) => format!("NOT {}", render(n, grams, texts)?),
        leaf => {
            let leaves = texts
                .iter()
                .map(|f| leaf_string(leaf, grams, f))
                .collect::<Result<Vec<_>, _>>()?;
            match leaves.as_slice() {
                [one] => one.clone(),
                _ => format!("({})", leaves.join(" OR ")),
            }
        }
    })
}

/// `N OF (...)` (#124): an `OR` of each `min` of the items `AND`ed. Quickwit
/// 0.9.1's query language has no "at least N"; its Elasticsearch-compatible
/// API has (`bool` with `minimum_should_match`) and counts the same pages on
/// the fixtures, but this way hits, aggregations and every text layout keep
/// one request shape. At most `C(6, 3) = 20` groups
/// ([`usnm_core::query::MAX_AT_LEAST_ITEMS`]).
fn at_least(
    min: usize,
    of: &[Node],
    grams: bool,
    texts: &[&TextFields],
) -> Result<String, SearchError> {
    let items = of
        .iter()
        .map(|n| render(n, grams, texts))
        .collect::<Result<Vec<_>, _>>()?;
    let mut groups = Vec::new();
    let mut pick: Vec<usize> = (0..min).collect();
    loop {
        let parts: Vec<&str> = pick.iter().map(|&i| items[i].as_str()).collect();
        groups.push(format!("({})", parts.join(" AND ")));
        // The next `min` indexes in lexicographic order, if any.
        let Some(i) = (0..min).rev().find(|&i| pick[i] < items.len() - min + i) else {
            break;
        };
        pick[i] += 1;
        for j in i + 1..min {
            pick[j] = pick[j - 1] + 1;
        }
    }
    Ok(format!("({})", groups.join(" OR ")))
}

/// The pages a search of both texts finds that match the query in American
/// Stories' text but not in LoC's (05 §5.5.4): the search in both texts,
/// `AND NOT` the same in LoC's text alone, `AND` the same in American
/// Stories' text alone. All three, because with `NOT` in the query a page
/// can match in one text and still be left out by a word in the other.
/// The hits these are list `matched_in: ["american_stories"]`.
pub fn american_stories_only_string(node: &Node, grams: bool) -> Result<String, SearchError> {
    let both = render(node, grams, &[&LOC, &AMERICAN_STORIES])?;
    let loc = render(node, grams, &[&LOC])?;
    let american = render(node, grams, &[&AMERICAN_STORIES])?;
    Ok(format!("({both}) AND NOT ({loc}) AND ({american})"))
}

/// A page's text field and its common-word pairs.
struct TextFields {
    text: &'static str,
    pairs: &'static str,
}

/// LoC's OCR (05 §5.5).
const LOC: TextFields = TextFields {
    text: "text",
    pairs: "text_cg",
};

/// American Stories' (05 §5.5.4).
const AMERICAN_STORIES: TextFields = TextFields {
    text: "text_as",
    pairs: "text_as_cg",
};

/// Both texts in one field, with a gap between them (05 §5.5.7).
const BOTH: TextFields = TextFields {
    text: usnm_core::text_layout::FIELD,
    pairs: usnm_core::text_layout::PAIRS_FIELD,
};

/// A term or phrase in one text's fields. One with a fraction joined to a
/// number (`61½`) is also looked for as OCR reads it with a slash (#1067): in
/// an `OR` with the phrases of its digits, `(text:61½ OR text:"61 1 2" OR
/// text:"611 2")`, each in the text's fields like any phrase, and never as a
/// term set (`IN [...]`, see `~ocr` below). The glyph's own clause comes
/// first, as it is the search as typed. These are in the string only: the
/// AST, its canonical form and the limits on what is typed are the query's
/// as the visitor wrote it.
fn leaf_string(node: &Node, grams: bool, f: &TextFields) -> Result<String, SearchError> {
    let slash = usnm_core::fractions::slash_phrases(node);
    if slash.is_empty() {
        return plain_leaf_string(node, grams, f);
    }
    let mut parts = vec![plain_leaf_string(node, grams, f)?];
    for terms in slash {
        parts.push(plain_leaf_string(
            &Node::Phrase { terms, slop: 0 },
            grams,
            f,
        )?);
    }
    Ok(format!("({})", parts.join(" OR ")))
}

/// A term or phrase in one text's fields, as it is.
fn plain_leaf_string(node: &Node, grams: bool, f: &TextFields) -> Result<String, SearchError> {
    let text = f.text;
    Ok(match node {
        Node::Term(t) if t.fuzzy > 0 => {
            // S-2: Quickwit 0.9's query language has no fuzzy terms (`~n` on a
            // term silently matches nothing), so refuse rather than miscount.
            return Err(SearchError::Unsupported(
                "fuzzy (OCR-tolerant) matching".into(),
            ));
        }
        Node::Term(t) if t.prefix => format!("{text}:{}*", t.text),
        // `word~ocr` (#124): the word and its OCR spellings, `OR`ed. Never a
        // term set (`text:IN [...]`): for a field in one, Quickwit 0.9.1
        // reads the whole term dictionary and every posting list of each
        // split before searching it, hundreds of MB a split on OCR text,
        // which ran the prod searchers out of memory. An `OR` reads only
        // the listed words. Each spelling must be one index token as it
        // stands, or the engine would analyze it into several. The parser
        // refuses a word that isn't (`ﷺ` folded by `Analyzer::V1`); this
        // never sends one.
        Node::Term(t) if t.ocr => {
            let variants = usnm_core::ocr_variants::variants(&t.text);
            if !variants
                .iter()
                .all(|v| usnm_core::ocr_variants::is_one_token(v))
            {
                return Err(SearchError::Unsupported(
                    "~ocr on a word the index holds as several words".into(),
                ));
            }
            let terms: Vec<String> = variants.iter().map(|v| format!("{text}:{v}")).collect();
            match terms.as_slice() {
                [one] => one.clone(),
                _ => format!("({})", terms.join(" OR ")),
            }
        }
        // With `?` or `*` in it (#124), an unquoted word is a wildcard query
        // in Quickwit 0.9, matched against the index's words.
        Node::Term(t) => format!("{text}:{}", t.text),
        Node::Phrase { terms, slop } if *slop > 0 => {
            format!("{text}:\"{}\"~{slop}", terms.join(" "))
        }
        Node::Phrase { terms, .. } => match grams
            .then(|| usnm_core::common_grams::query_terms(terms))
            .flatten()
        {
            Some(pairs) => {
                let mut parts = vec![format!("{}:\"{}\"", f.pairs, pairs.join(" "))];
                let mut seen = std::collections::BTreeSet::new();
                parts.extend(
                    terms
                        .iter()
                        .filter(|t| !usnm_core::common_grams::is_common(t))
                        // A word `Analyzer::V1` folds to several (`ﷺ`) is
                        // only required through the pairs.
                        .filter(|t| !t.contains(char::is_whitespace) && seen.insert(*t))
                        .map(|w| format!("{text}:{w}")),
                );
                format!("({})", parts.join(" AND "))
            }
            None => format!("{text}:\"{}\"", terms.join(" ")),
        },
        Node::And(_) | Node::Or(_) | Node::AtLeast { .. } | Node::Not(_) => {
            unreachable!("query_string renders operators")
        }
    })
}

fn join(
    children: &[Node],
    sep: &str,
    grams: bool,
    texts: &[&TextFields],
) -> Result<String, SearchError> {
    Ok(children
        .iter()
        .map(|c| render(c, grams, texts))
        .collect::<Result<Vec<_>, _>>()?
        .join(sep))
}

/// Filter clauses (values are validated by `usnm_core::params`), `lang=` as
/// `indexes` match it (title or page languages, 05 §5.5.8).
pub fn filter_clauses(filters: &Filters, indexes: &IndexSet) -> Vec<String> {
    let mut out = vec![format!(
        "day:[{} TO {}]",
        filters.from_day(),
        filters.to_day()
    )];
    let set = |field: &str, values: &[String]| format!("{field}:IN [{}]", values.join(" "));
    if !filters.states.is_empty() {
        out.push(set("state", &filters.states));
    }
    if !filters.lccns.is_empty() {
        out.push(set("lccn", &filters.lccns));
    }
    if let Some(m) = indexes.language_match(&filters.langs) {
        out.push(m.clause());
    }
    if filters.front_only {
        out.push("front_page:true".into());
    }
    // Only Congress pages have a series (#229); usnm_core::params refuses
    // `series` on a newspaper search.
    if !filters.series.is_empty() {
        out.push(set("series", &filters.series));
    }
    out
}

/// The decades a date-limited search needs, on a version laid out by
/// decade (05 §5.5.5): Quickwit then skips the other decades' splits before
/// opening them (tag pruning). The `day` filter stays the exact one. Never a
/// timestamp or `date:` range: Quickwit 0.9.1 matches nothing before 1972
/// with those (#158).
pub fn decade_clause(filters: &Filters, indexes: &IndexSet) -> Option<String> {
    usnm_core::decade::clause(filters.from, filters.to, indexes.decades()?)
}

/// Clauses that hide the copies of duplicated pages `indexes` names, one per
/// batch: that batch's documents with those ids (04 §4.7). Doc ids and batch
/// names are our own (letters, digits, `_` and `-`), so they need no escaping.
pub fn hidden_clauses(indexes: &IndexSet) -> Vec<String> {
    indexes
        .hidden()
        .iter()
        .map(|(batch, ids)| {
            let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
            format!("NOT (batch:{batch} AND doc_id:IN [{}])", ids.join(" "))
        })
        .collect()
}

pub(super) fn full_query(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    extra: Vec<String>,
) -> Result<String, SearchError> {
    let query = query_for(node, indexes)?;
    Ok(scoped(query, filters, indexes, extra))
}

/// `query` with the filters and the hidden copies' clauses.
pub(super) fn scoped(
    query: String,
    filters: &Filters,
    indexes: &IndexSet,
    extra: Vec<String>,
) -> String {
    let mut parts = vec![query];
    parts.extend(scope(filters, indexes, extra));
    parts.join(" AND ")
}

/// The filters' clauses, the decades', the hidden copies' and `extra`:
/// never empty, as the filters always name the days.
pub(super) fn scope(filters: &Filters, indexes: &IndexSet, extra: Vec<String>) -> Vec<String> {
    let mut parts = filter_clauses(filters, indexes);
    parts.extend(decade_clause(filters, indexes));
    parts.extend(hidden_clauses(indexes));
    parts.extend(extra);
    parts
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quickwit::test_support::{d, filters, none, stories};
    use crate::quickwit::{
        american_stories_only_request, cube_request, hits_request, place_cube_request,
        summary_request,
    };
    use crate::HitsQuery;
    use usnm_core::query::parse;
    use usnm_core::text::Analyzer;
    use usnm_core::time::{BucketSpec, BucketUnit};

    #[test]
    fn series_filter_is_a_term_set_and_absent_by_default() {
        let mut f = filters();
        let set = IndexSet::new(vec!["congress".into()]);
        assert!(!filter_clauses(&f, &set)
            .iter()
            .any(|c| c.starts_with("series")));
        f.front_only = false;
        f.series = vec!["globe".into(), "serialset".into()];
        assert_eq!(
            filter_clauses(&f, &set).last().map(String::as_str),
            Some("series:IN [globe serialset]")
        );
    }

    #[test]
    fn translates_ast_to_query_language() {
        let q = parse(r#""cross of gold" -bryan (silver OR freed*) "gold silver"~3"#).unwrap();
        assert_eq!(
            query_string(&q, false, false).unwrap(),
            r#"(text:"cross of gold" AND text:"gold silver"~3 AND NOT text:bryan AND (text:freed* OR text:silver))"#
        );
        assert!(matches!(
            query_string(&parse("gold~1").unwrap(), false, false),
            Err(SearchError::Unsupported(_))
        ));
        // Wildcards inside a word go as typed, folded (#124).
        assert_eq!(
            query_string(&parse("Presi?ent washi*TON").unwrap(), false, false).unwrap(),
            "(text:presi?ent AND text:washi*ton)"
        );
        // ...in either text when the version has American Stories' (#218).
        assert_eq!(
            query_string(&parse("presi?ent").unwrap(), false, true).unwrap(),
            "(text:presi?ent OR text_as:presi?ent)"
        );
        // A word's OCR spellings are `OR`ed (#124), never a term set.
        assert_eq!(
            query_string(&parse("Gold~ocr").unwrap(), false, false).unwrap(),
            "(text:goid OR text:golcl OR text:gold)"
        );
        assert_eq!(
            query_string(&parse("gold~ocr").unwrap(), false, true).unwrap(),
            "((text:goid OR text:golcl OR text:gold) OR (text_as:goid OR text_as:golcl OR text_as:gold))"
        );
        assert_eq!(
            query_string(&parse("bryan -gold~ocr").unwrap(), false, false).unwrap(),
            "(NOT (text:goid OR text:golcl OR text:gold) AND text:bryan)"
        );
        // A word with no other spelling is the word alone.
        assert_eq!(
            query_string(&parse("xyz~ocr").unwrap(), false, true).unwrap(),
            "(text:xyz OR text_as:xyz)"
        );
        // A word that isn't one index token (`ﷺ` or `½` folded by
        // `Analyzer::V1`) is never sent split into several members; the
        // parser refuses it, and a term built without the parser is refused
        // here.
        for text in [
            usnm_core::text::fold("ﷺ", Analyzer::V1),
            usnm_core::text::fold("½", Analyzer::V1),
            "gold]".into(),
        ] {
            let node = Node::Term(usnm_core::query::Term {
                text: text.clone(),
                fuzzy: 0,
                prefix: false,
                wildcard: false,
                ocr: true,
            });
            for american_stories in [false, true] {
                assert!(
                    matches!(
                        query_string(&node, false, american_stories),
                        Err(SearchError::Unsupported(_))
                    ),
                    "{text}"
                );
            }
        }
        // At least N: each N of them, ANDed, ORed.
        assert_eq!(
            query_string(&parse("2 OF (gold silver bryan)").unwrap(), false, false).unwrap(),
            "((text:bryan AND text:gold) OR (text:bryan AND text:silver) \
             OR (text:gold AND text:silver))"
        );
        let q = parse("3 OF (a1 b1 c1 d1 e1 f1)").unwrap();
        let s = query_string(&q, false, false).unwrap();
        assert_eq!(s.matches(" OR (").count() + 1, 20);
        assert!(s.starts_with("((text:a1 AND text:b1 AND text:c1) OR "));
        assert!(s.ends_with(" OR (text:d1 AND text:e1 AND text:f1))"));
        assert_eq!(
            query_string(&parse("-2 OF (a1 b1 c1) gold").unwrap(), false, true).unwrap(),
            "(NOT (((text:a1 OR text_as:a1) AND (text:b1 OR text_as:b1)) \
             OR ((text:a1 OR text_as:a1) AND (text:c1 OR text_as:c1)) \
             OR ((text:b1 OR text_as:b1) AND (text:c1 OR text_as:c1))) \
             AND (text:gold OR text_as:gold))"
        );
    }

    /// No request puts a text field in a term set (`text:IN [...]`): for
    /// one, Quickwit 0.9.1 reads the field's whole term dictionary and
    /// every posting list of each split before searching it. Term sets
    /// stay on keyword fields (states, LCCNs, languages, decades, places,
    /// doc ids), whose dictionaries are small.
    #[test]
    fn no_request_has_a_term_set_on_a_text_field() {
        let q = parse(
            r#"gold~ocr -silver~ocr 2 OF (fever~ocr bryan "cross of gold") (wheat~ocr OR corn)"#,
        )
        .unwrap();
        let mut f = filters();
        f.lccns = vec!["sn99000001".into()];
        let spec = BucketSpec::new(BucketUnit::Year, d("1896-01-01"), d("1896-12-31"));
        for grams in [false, true] {
            let sets = [
                stories(false).with_common_grams(grams),
                stories(true).with_common_grams(grams),
                stories(true)
                    .with_common_grams(grams)
                    .with_text_layout(TextLayout::Single),
            ];
            for set in &sets {
                let mut requests = vec![
                    summary_request(&q, &f, set, &spec).unwrap(),
                    cube_request(&q, &f, set, &spec, &[0, 1]).unwrap(),
                    place_cube_request(&q, &f, set, &spec, &["P1".into()]).unwrap(),
                    hits_request(&q, &f, set, &HitsQuery::default()).unwrap(),
                ];
                if set.text_layout() == TextLayout::Separate && set.american_stories() {
                    requests.push(american_stories_only_request(&q, &f, set).unwrap());
                }
                for r in requests {
                    let s = r["query"].as_str().unwrap();
                    assert!(
                        s.contains("text:goid") || s.contains("text_all:goid"),
                        "{s}"
                    );
                    for field in [
                        "text",
                        "text_as",
                        "text_all",
                        "text_cg",
                        "text_as_cg",
                        "text_all_cg",
                    ] {
                        assert!(!s.contains(&format!("{field}:IN [")), "{s}");
                    }
                    assert!(s.contains("lccn:IN [sn99000001]"), "{s}");
                }
            }
        }
    }

    #[test]
    fn phrases_with_common_words_search_the_pairs_field() {
        let q = parse(r#""cross of gold" -bryan "gold silver"~3 "yellow fever""#).unwrap();
        assert_eq!(
            query_string(&q, true, false).unwrap(),
            "((text_cg:\"cross of_gold gold\" AND text:cross AND text:gold) \
             AND text:\"gold silver\"~3 AND text:\"yellow fever\" AND NOT text:bryan)"
        );
        // A repeated word is required once.
        assert_eq!(
            query_string(&parse(r#""the gold of the cross""#).unwrap(), true, false).unwrap(),
            "(text_cg:\"the_gold gold of_the the_cross cross\" AND text:gold AND text:cross)"
        );
        // A character that decomposes into several words (`ﷺ`) or with a
        // separator (`½`) is one word, as the analyzer has it (#168).
        assert_eq!(
            query_string(&parse("\"of \u{fdfa} gold\"").unwrap(), true, false).unwrap(),
            "(text_cg:\"of_\u{fdfa} \u{fdfa} gold\" AND text:\u{fdfa} AND text:gold)"
        );
        // `61¼` is also read as OCR prints it with a slash (#1067), each
        // spelling its own phrase through the pairs; `½` stays the glyph.
        assert_eq!(
            query_string(&parse(r#""½ higher at 61¼""#).unwrap(), true, false).unwrap(),
            "((text_cg:\"½ higher at_61¼ 61¼\" AND text:½ AND text:higher AND text:61¼) \
             OR (text_cg:\"½ higher at_61 61 1 4\" AND text:½ AND text:higher \
             AND text:61 AND text:1 AND text:4) \
             OR (text_cg:\"½ higher at_611 611 4\" AND text:½ AND text:higher \
             AND text:611 AND text:4))"
        );
        // On an index built with version 1 the query folds them as its pairs
        // have them: `ﷺ` as its words run together, required only through
        // the pairs, and `½` as `1⁄2`.
        let v1 = |q: &str| usnm_core::query::parse_with(q, Analyzer::V1).unwrap();
        let s = query_string(&v1("\"of \u{fdfa} gold\""), true, false).unwrap();
        assert!(s.starts_with("(text_cg:\"of_"), "{s}");
        assert!(s.ends_with(" AND text:gold)"), "{s}");
        assert_eq!(s.matches(" AND ").count(), 1, "{s}");
        assert_eq!(
            query_string(&v1(r#""½ higher at 61¼""#), true, false).unwrap(),
            "(text_cg:\"1\u{2044}2 higher at_611\u{2044}4 611\u{2044}4\" AND text:1\u{2044}2 \
             AND text:higher AND text:611\u{2044}4)"
        );
        // Without the field, or for a phrase ending in a common word: `text`.
        assert_eq!(
            query_string(&parse(r#""cross of gold""#).unwrap(), false, false).unwrap(),
            r#"text:"cross of gold""#
        );
        assert_eq!(
            query_string(&parse(r#""remember the""#).unwrap(), true, false).unwrap(),
            r#"text:"remember the""#
        );
        let set = IndexSet::new(vec!["i".into()]).with_common_grams(true);
        let r = summary_request(
            &parse(r#""cross of gold""#).unwrap(),
            &filters(),
            &set,
            &BucketSpec::new(BucketUnit::Year, d("1896-01-01"), d("1896-12-31")),
        )
        .unwrap();
        assert!(r["query"].as_str().unwrap().starts_with("(text_cg:"), "{r}");
    }

    /// A fraction joined to a number is also looked for as OCR reads it with
    /// a slash (#1067): the glyph and the phrases of its digits, `OR`ed
    /// within each text's fields, and never a term set.
    #[test]
    fn a_fraction_in_a_number_is_also_searched_with_a_slash() {
        let qs =
            |q: &str, grams, stories| query_string(&parse(q).unwrap(), grams, stories).unwrap();
        let both = r#"(text:61½ OR text:"61 1 2" OR text:"611 2")"#;
        for grams in [false, true] {
            assert_eq!(qs("61½", grams, false), both);
            assert_eq!(
                qs("61½", grams, true),
                r#"((text:61½ OR text:"61 1 2" OR text:"611 2") OR (text_as:61½ OR text_as:"61 1 2" OR text_as:"611 2"))"#
            );
            // The exclusion holds for either spelling.
            assert_eq!(
                qs("gold -61½", grams, false),
                format!("(NOT {both} AND text:gold)")
            );
        }
        // One field for both texts: once, in `text_all`.
        let single = |grams: bool| {
            none()
                .with_american_stories(true)
                .with_common_grams(grams)
                .with_text_layout(TextLayout::Single)
        };
        assert_eq!(
            query_for(&parse("61½").unwrap(), &single(true)).unwrap(),
            r#"(text_all:61½ OR text_all:"61 1 2" OR text_all:"611 2")"#
        );
        // The count of pages only American Stories' text matches.
        assert_eq!(
            american_stories_only_string(&parse("61½").unwrap(), true).unwrap(),
            format!(
                "(({both} OR {as_text})) AND NOT ({both}) AND ({as_text})",
                as_text = r#"(text_as:61½ OR text_as:"61 1 2" OR text_as:"611 2")"#
            )
        );
        // At least N of them: the group is one item.
        assert_eq!(
            qs("2 OF (61½ gold silver)", false, false),
            format!(
                "(({both} AND text:gold) OR ({both} AND text:silver) \
                 OR (text:gold AND text:silver))"
            )
        );
        // A number and its fraction in a phrase: the phrase as typed, then
        // each other way to spell its fraction. The spellings are digits,
        // so a common word before them reads its pair as the index has it
        // and they never need the pairs themselves.
        assert_eq!(
            qs(r#""wheat 61½""#, false, false),
            r#"(text:"wheat 61½" OR text:"wheat 61 1 2" OR text:"wheat 611 2")"#
        );
        assert_eq!(
            qs(r#""wheat 61½""#, true, false),
            r#"(text:"wheat 61½" OR text:"wheat 61 1 2" OR text:"wheat 611 2")"#
        );
        assert_eq!(qs(r#""61 1 2""#, true, false), r#"text:"61 1 2""#);
        // Not a bare fraction, a word that isn't a number and a fraction,
        // a term that isn't exact, or a NEAR search.
        for (q, plain) in [
            ("½", "text:½"),
            ("a½", "text:a½"),
            ("12345½*", "text:12345½*"),
            ("61½~ocr", "text:61½"),
            (r#""wheat 61½"~3"#, r#"text:"wheat 61½"~3"#),
            (r#""61½ 62¼ 63¾""#, r#"text:"61½ 62¼ 63¾""#),
        ] {
            assert_eq!(qs(q, false, false), plain, "{q}");
        }
        // Version 1 folded `61½` to `611⁄2`, which the engine reads as the
        // phrase "611 2" already.
        let v1 = usnm_core::query::parse_with("61½", Analyzer::V1).unwrap();
        assert_eq!(
            query_string(&v1, false, false).unwrap(),
            "text:611\u{2044}2"
        );
        // Never a term set, on any field.
        for set in [&none(), &stories(true), &single(true)] {
            let s = query_for(&parse(r#"61½ "wheat 20¾" gold~ocr"#).unwrap(), set).unwrap();
            assert!(!s.contains(":IN ["), "{s}");
        }
    }

    #[test]
    fn american_stories_text_is_searched_leaf_by_leaf() {
        let qs = |q: &str, grams| query_string(&parse(q).unwrap(), grams, true).unwrap();
        assert_eq!(qs("gold", false), "(text:gold OR text_as:gold)");
        assert_eq!(qs("silve*", false), "(text:silve* OR text_as:silve*)");
        assert_eq!(
            qs(r#""gold silver"~3"#, true),
            r#"(text:"gold silver"~3 OR text_as:"gold silver"~3)"#
        );
        assert_eq!(
            qs(r#""cross of gold""#, false),
            r#"(text:"cross of gold" OR text_as:"cross of gold")"#
        );
        assert_eq!(
            qs(r#""yellow fever""#, true),
            r#"(text:"yellow fever" OR text_as:"yellow fever")"#
        );
        // An exact phrase through the pairs stays within one text.
        assert_eq!(
            qs(r#""cross of gold""#, true),
            "((text_cg:\"cross of_gold gold\" AND text:cross AND text:gold) \
             OR (text_as_cg:\"cross of_gold gold\" AND text_as:cross AND text_as:gold))"
        );
        // Operators are unchanged; NOT excludes a page with the word in either text.
        assert_eq!(
            qs("bryan -silver (free OR orator)", false),
            "(NOT (text:silver OR text_as:silver) AND (text:bryan OR text_as:bryan) \
             AND ((text:free OR text_as:free) OR (text:orator OR text_as:orator)))"
        );
        assert!(matches!(
            query_string(&parse("gold~1").unwrap(), false, true),
            Err(SearchError::Unsupported(_))
        ));
        // Every request takes the flag from the index set.
        let set = IndexSet::new(vec!["i".into()]).with_american_stories(true);
        let r = summary_request(
            &parse("gold").unwrap(),
            &filters(),
            &set,
            &BucketSpec::new(BucketUnit::Year, d("1896-01-01"), d("1896-12-31")),
        )
        .unwrap();
        assert!(
            r["query"]
                .as_str()
                .unwrap()
                .starts_with("(text:gold OR text_as:gold) AND "),
            "{r}"
        );
    }

    /// One field for both texts (05 §5.5.7): each term or phrase once, in
    /// `text_all` and `text_all_cg`, never in a field of one text.
    #[test]
    fn one_field_for_both_texts_is_searched_once() {
        let single = |grams: bool| {
            none()
                .with_american_stories(true)
                .with_common_grams(grams)
                .with_text_layout(TextLayout::Single)
        };
        let qs = |q: &str, grams| query_for(&parse(q).unwrap(), &single(grams)).unwrap();
        assert_eq!(qs("gold", true), "text_all:gold");
        assert_eq!(qs("silve*", true), "text_all:silve*");
        assert_eq!(qs("presi?ent", true), "text_all:presi?ent");
        assert_eq!(
            qs(r#""gold silver"~3"#, true),
            r#"text_all:"gold silver"~3"#
        );
        assert_eq!(qs(r#""yellow fever""#, true), r#"text_all:"yellow fever""#);
        assert_eq!(
            qs(r#""cross of gold""#, true),
            "(text_all_cg:\"cross of_gold gold\" AND text_all:cross AND text_all:gold)"
        );
        // Without pairs it knows (a newer `common_grams`): the phrase itself.
        assert_eq!(
            qs(r#""cross of gold""#, false),
            r#"text_all:"cross of gold""#
        );
        assert_eq!(
            qs("bryan -silver (free OR orator)", true),
            "(NOT text_all:silver AND text_all:bryan AND (text_all:free OR text_all:orator))"
        );
        assert!(matches!(
            query_for(&parse("gold~1").unwrap(), &single(true)),
            Err(SearchError::Unsupported(_))
        ));
        // OCR spellings `OR`ed in the one field, never a term set: Quickwit
        // 0.9.1 reads a term set's field whole on each split (#124).
        assert_eq!(
            qs("gold~ocr", true),
            "(text_all:goid OR text_all:golcl OR text_all:gold)"
        );
        assert_eq!(
            qs("2 OF (gold~ocr bryan silver)", true),
            "((text_all:bryan AND (text_all:goid OR text_all:golcl OR text_all:gold)) \
             OR (text_all:bryan AND text_all:silver) \
             OR ((text_all:goid OR text_all:golcl OR text_all:gold) AND text_all:silver))"
        );
        // Whether the set says American Stories' text is there or not.
        assert_eq!(
            query_for(
                &parse("gold").unwrap(),
                &none().with_text_layout(TextLayout::Single)
            )
            .unwrap(),
            "text_all:gold"
        );
        // Every request takes it from the set, and none names a field of
        // one text.
        let q = parse(r#""cross of gold" bryan"#).unwrap();
        let spec = BucketSpec::new(BucketUnit::Year, d("1896-01-01"), d("1896-12-31"));
        for r in [
            summary_request(&q, &filters(), &single(true), &spec).unwrap(),
            cube_request(&q, &filters(), &single(true), &spec, &[0, 1]).unwrap(),
            place_cube_request(&q, &filters(), &single(true), &spec, &["P1".into()]).unwrap(),
            hits_request(&q, &filters(), &single(true), &HitsQuery::default()).unwrap(),
        ] {
            let s = r["query"].as_str().unwrap();
            assert!(s.starts_with("((text_all_cg:"), "{s}");
            for field in ["text:", "text_cg:", "text_as:", "text_as_cg:"] {
                assert!(!s.contains(&format!(" {field}")), "{s}");
                assert!(!s.contains(&format!("({field}")), "{s}");
            }
        }
        // A field per text is as before.
        assert_eq!(
            query_for(&parse("gold").unwrap(), &stories(true)).unwrap(),
            "(text:gold OR text_as:gold)"
        );
        assert_eq!(
            query_for(&parse("gold").unwrap(), &stories(false)).unwrap(),
            "text:gold"
        );
        // The count of pages only American Stories' text matches needs
        // LoC's text on its own: refused.
        assert!(matches!(
            american_stories_only_request(&q, &filters(), &single(true)),
            Err(SearchError::Unsupported(_))
        ));
    }
}
