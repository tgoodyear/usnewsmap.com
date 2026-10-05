//! Brute-force in-memory backend. It evaluates the AST exactly against
//! tokenized text, so it doubles as the oracle for engine count checks
//! (05 §5.8) and as the backend for local development on the fixture corpus.

use std::collections::{BTreeMap, HashMap};

use async_trait::async_trait;
use usnm_core::params::Filters;
use usnm_core::query::{Node, Term};
use usnm_core::text::tokenize;
use usnm_core::time::BucketSpec;

use crate::snippet::text_snippets;
use crate::{
    ja_snippets, rank, Capabilities, CubeCell, Hit, HitSort, HitsPage, HitsQuery, IndexSet,
    KeyCount, PageDoc, PlaceSummary, SearchBackend, SearchError, Summary,
};

struct Indexed {
    doc: PageDoc,
    tokens: Vec<String>,
}

/// Documents grouped by index id, so explicit index sets behave like the real engine.
#[derive(Default)]
pub struct MemoryBackend {
    indexes: HashMap<String, Vec<Indexed>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add_index(&mut self, index_id: &str, docs: impl IntoIterator<Item = PageDoc>) {
        let entry = self.indexes.entry(index_id.to_owned()).or_default();
        entry.extend(docs.into_iter().map(|doc| Indexed {
            // A Japanese page's text is already its tokens (Quickwit's
            // `whitespace` tokenizer, pages-ja-index.yaml); folding them again
            // would strip dakuten (が → か).
            tokens: if doc.printed.is_some() {
                doc.text.split_whitespace().map(str::to_owned).collect()
            } else {
                tokenize(&doc.text)
            },
            doc,
        }));
    }

    fn matching<'a>(
        &'a self,
        indexes: &'a IndexSet,
        query: &'a Node,
        filters: &'a Filters,
    ) -> Result<impl Iterator<Item = &'a Indexed> + 'a, SearchError> {
        let mut selected = Vec::new();
        for id in indexes.ids() {
            let docs = self
                .indexes
                .get(id)
                .ok_or_else(|| SearchError::Backend(format!("index `{id}` not found")))?;
            selected.push(docs);
        }
        let (from, to) = (filters.from_day(), filters.to_day());
        Ok(selected.into_iter().flatten().filter(move |d| {
            let doc = &d.doc;
            doc.day >= from
                && doc.day <= to
                && (filters.states.is_empty() || filters.states.contains(&doc.state))
                && (filters.lccns.is_empty() || filters.lccns.contains(&doc.lccn))
                && (filters.langs.is_empty()
                    || doc.language.iter().any(|l| filters.langs.contains(l)))
                && (!filters.front_only || doc.front_page)
                && !indexes.hides(&doc.doc_id, &doc.batch)
                && eval(query, &d.tokens)
        }))
    }
}

#[async_trait]
impl SearchBackend for MemoryBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            fuzzy: true,
            max_slop: usnm_core::query::MAX_SLOP,
            nested_aggregations: true,
        }
    }

    async fn summary(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
    ) -> Result<Summary, SearchError> {
        let mut series = vec![0u64; spec.len()];
        let mut places: BTreeMap<&str, (u64, u32, u32)> = BTreeMap::new();
        let mut total = 0;
        let mut span: Option<(u32, u32)> = None;
        let mut days = std::collections::BTreeSet::new();
        let mut papers: BTreeMap<&str, u64> = BTreeMap::new();
        let mut languages: BTreeMap<&str, u64> = BTreeMap::new();
        for d in self.matching(indexes, query, filters)? {
            let day = d.doc.day;
            total += 1;
            days.insert(day);
            *papers.entry(&d.doc.lccn).or_default() += 1;
            for l in &d.doc.language {
                *languages.entry(l).or_default() += 1;
            }
            series[spec.index_of_day(day)] += 1;
            let e = places.entry(&d.doc.place_id).or_insert((0, u32::MAX, 0));
            e.0 += 1;
            e.1 = e.1.min(day);
            e.2 = e.2.max(day);
            span = Some(span.map_or((day, day), |(lo, hi)| (lo.min(day), hi.max(day))));
        }
        Ok(Summary {
            total_hits: total,
            first_day: span.map(|s| s.0),
            last_day: span.map(|s| s.1),
            series,
            places: places
                .into_iter()
                .map(|(id, (hits, first_day, last_day))| PlaceSummary {
                    place_id: id.to_owned(),
                    hits,
                    first_day,
                    last_day,
                })
                .collect(),
            papers: counts(papers),
            languages: counts(languages),
            days: days.len() as u64,
        })
    }

    async fn cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        let mut cells: BTreeMap<(&str, u32), u32> = BTreeMap::new();
        for d in self
            .matching(indexes, query, filters)?
            .filter(|d| shards.contains(&d.doc.place_shard))
        {
            *cells
                .entry((&d.doc.place_id, spec.index_of_day(d.doc.day) as u32))
                .or_default() += 1;
        }
        Ok(cells
            .into_iter()
            .map(|((place, bucket), hits)| CubeCell {
                place_id: place.to_owned(),
                bucket,
                hits,
            })
            .collect())
    }

    async fn hits(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        page: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        let highlight = positive_terms(query);
        let mut docs: Vec<(usize, &PageDoc)> = self
            .matching(indexes, query, filters)?
            .filter(|d| page.place_id.as_ref().is_none_or(|p| &d.doc.place_id == p))
            .filter(|d| page.lccn.as_ref().is_none_or(|l| &d.doc.lccn == l))
            .map(|d| (mentions(&d.tokens, &highlight), &d.doc))
            .collect();
        docs.sort_by(|(ma, a), (mb, b)| {
            let order = (a.day, a.sort_key, &a.doc_id).cmp(&(b.day, b.sort_key, &b.doc_id));
            match page.sort {
                HitSort::Oldest => order,
                HitSort::Newest => order.reverse(),
                HitSort::Relevant => mb.cmp(ma).then(order),
            }
        });
        let days = page.days.then(|| {
            docs.iter()
                .map(|(_, d)| d.day)
                .collect::<std::collections::BTreeSet<_>>()
                .len() as u64
        });
        Ok(HitsPage {
            total: docs.len() as u64,
            days,
            hits: docs
                .into_iter()
                .skip(page.offset)
                .take(page.limit)
                .map(|(_, d)| Hit {
                    doc_id: d.doc_id.clone(),
                    day: d.day,
                    lccn: d.lccn.clone(),
                    place_id: d.place_id.clone(),
                    edition: d.edition,
                    seq: d.seq,
                    front_page: d.front_page,
                    snippets: match &d.printed {
                        Some(printed) => ja_snippets(printed, query),
                        None => text_snippets(&d.text, query),
                    },
                    ocr_source: d.ocr_source.clone(),
                    ocr_engine: d.ocr_engine.clone(),
                })
                .collect(),
        })
    }

    async fn health(&self) -> Result<(), SearchError> {
        Ok(())
    }
}

/// Evaluate the AST against a document's token stream.
pub fn eval(node: &Node, tokens: &[String]) -> bool {
    match node {
        Node::Term(t) => tokens.iter().any(|tok| term_matches(t, tok)),
        Node::Phrase { terms, slop } => phrase_matches(terms, *slop, tokens),
        Node::And(c) => c.iter().all(|n| eval(n, tokens)),
        Node::Or(c) => c.iter().any(|n| eval(n, tokens)),
        Node::Not(n) => !eval(n, tokens),
    }
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

/// Ordered phrase match where the total number of extra words between
/// consecutive terms is at most `slop`.
fn phrase_matches(terms: &[String], slop: u8, tokens: &[String]) -> bool {
    fn from(terms: &[String], tokens: &[String], pos: usize, budget: usize) -> bool {
        let Some((first, rest)) = terms.split_first() else {
            return true;
        };
        let end = (pos + budget + 1).min(tokens.len());
        (pos..end).any(|i| tokens[i] == *first && from(rest, tokens, i + 1, budget - (i - pos)))
    }
    let Some((first, rest)) = terms.split_first() else {
        return false;
    };
    tokens
        .iter()
        .enumerate()
        .any(|(i, t)| t == first && from(rest, tokens, i + 1, usize::from(slop)))
}

pub(crate) fn levenshtein_within(a: &str, b: &str, max: usize) -> bool {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.len().abs_diff(b.len()) > max {
        return false;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1; b.len() + 1];
        for (j, cb) in b.iter().enumerate() {
            cur[j + 1] = (prev[j] + usize::from(ca != cb))
                .min(prev[j + 1] + 1)
                .min(cur[j] + 1);
        }
        prev = cur;
    }
    prev[b.len()] <= max
}

/// Positive terms of the query, with their prefix/fuzzy semantics; phrase
/// words become exact terms. Used to highlight exactly what matched.
fn positive_terms(node: &Node) -> Vec<Term> {
    fn walk(node: &Node, out: &mut Vec<Term>) {
        match node {
            Node::Term(t) => out.push(t.clone()),
            Node::Phrase { terms, .. } => out.extend(terms.iter().map(|t| Term {
                text: t.clone(),
                fuzzy: 0,
                prefix: false,
            })),
            Node::And(c) | Node::Or(c) => c.iter().for_each(|n| walk(n, out)),
            Node::Not(_) => {}
        }
    }
    let mut out = Vec::new();
    walk(node, &mut out);
    out
}

/// A ~25-word window around the first highlighted word, HTML-escaped with `<mark>`.
fn counts(m: BTreeMap<&str, u64>) -> Vec<KeyCount> {
    rank(
        m.into_iter()
            .map(|(k, hits)| KeyCount {
                key: k.to_owned(),
                hits,
            })
            .collect(),
    )
}

/// How many of the page's words match one of the query's positive words:
/// the reference for "most mentions first" (`HitSort::Relevant`).
fn mentions(tokens: &[String], highlight: &[Term]) -> usize {
    tokens
        .iter()
        .filter(|tok| highlight.iter().any(|t| term_matches(t, tok)))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use usnm_core::query::parse;

    fn toks(s: &str) -> Vec<String> {
        tokenize(s)
    }

    #[test]
    fn evaluates_terms_phrases_and_operators() {
        let t = toks("You shall not crucify mankind upon a cross of gold said Bryan");
        assert!(eval(&parse(r#""cross of gold""#).unwrap(), &t));
        assert!(!eval(&parse(r#""gold of cross""#).unwrap(), &t));
        assert!(eval(&parse(r#""crucify gold"~5"#).unwrap(), &t));
        assert!(!eval(&parse(r#""crucify gold"~4"#).unwrap(), &t));
        assert!(eval(&parse("bryan -silver").unwrap(), &t));
        assert!(!eval(&parse("bryan -gold").unwrap(), &t));
        assert!(eval(&parse("silver OR mankind").unwrap(), &t));
        assert!(eval(&parse("cruc*").unwrap(), &t));
        assert!(eval(&parse("mankimd~1").unwrap(), &t));
        assert!(!eval(&parse("mankxxd~1").unwrap(), &t));
    }

    #[test]
    fn mentions_count_every_matching_word() {
        let hl = positive_terms(&parse("gold cruc* -silver").unwrap());
        assert_eq!(
            mentions(&toks("gold, gold and crucify; silver gold"), &hl),
            4
        );
        assert_eq!(mentions(&toks("silver"), &hl), 0);
    }
}

#[cfg(test)]
mod shard_tests {
    use super::*;
    use chrono::NaiveDate;
    use usnm_core::params::Filters;
    use usnm_core::time::{BucketSpec, BucketUnit};

    fn doc(i: u32, place: u8) -> PageDoc {
        PageDoc {
            printed: None,
            ocr_source: None,
            ocr_engine: None,
            doc_id: format!("sn99{i:06}_1896-07-10_ed-1_seq-1"),
            day: 71_000 + i,
            ym: 1896 * 12 + 6,
            year: 1896,
            place_id: format!("P{place:05}"),
            place_shard: place % 8,
            lccn: format!("sn99{place:06}"),
            state: "GA".into(),
            language: vec!["eng".into()],
            front_page: true,
            edition: 1,
            seq: 1,
            sort_key: u64::from(place) << 32 | 1 << 16 | 1,
            batch: "b".into(),
            text: "a cross of gold".into(),
        }
    }

    #[tokio::test]
    async fn sharded_cube_equals_unsharded_cube() {
        let mut b = MemoryBackend::new();
        b.add_index("i", (0..200).map(|i| doc(i, (i % 13) as u8)));
        let idx = IndexSet::new(vec!["i".into()]);
        let q = usnm_core::query::parse("gold").unwrap();
        let from = usnm_core::time::date_from_day(71_000);
        let to = usnm_core::time::date_from_day(71_300);
        let f = Filters {
            from,
            to,
            states: vec![],
            lccns: vec![],
            langs: vec![],
            front_only: false,
        };
        let spec = BucketSpec::new(BucketUnit::Week, from, to);
        let all = b
            .cube(&idx, &q, &f, &spec, &(0..8).collect::<Vec<_>>())
            .await
            .unwrap();
        for n in 1..=8u8 {
            let mut merged = Vec::new();
            for k in 0..n {
                merged.extend(
                    b.cube(&idx, &q, &f, &spec, &usnm_core::cube::shards_for(k, n))
                        .await
                        .unwrap(),
                );
            }
            merged.sort_by(|a, b| (&a.place_id, a.bucket).cmp(&(&b.place_id, b.bucket)));
            assert_eq!(merged, all, "n = {n}");
        }
        let _ = NaiveDate::MIN;
    }

    #[tokio::test]
    async fn hidden_copies_are_not_counted_or_listed() {
        let mut copy = doc(1, 1);
        copy.batch = "other".into();
        let mut b = MemoryBackend::new();
        b.add_index("base", [doc(1, 1), doc(2, 1)]);
        b.add_index("delta", [copy]);
        let both = IndexSet::new(vec!["base".into(), "delta".into()]);
        let hidden = both.clone().hiding([(doc(1, 1).doc_id, "b".to_owned())]);
        let q = usnm_core::query::parse("gold").unwrap();
        let (from, to) = (
            usnm_core::time::date_from_day(71_000),
            usnm_core::time::date_from_day(71_300),
        );
        let f = Filters {
            from,
            to,
            states: vec![],
            lccns: vec![],
            langs: vec![],
            front_only: false,
        };
        let spec = BucketSpec::new(BucketUnit::Year, from, to);
        let total = |set: IndexSet| {
            let (b, q, f, spec) = (&b, &q, &f, &spec);
            async move { b.summary(&set, q, f, spec).await.unwrap().total_hits }
        };
        assert_eq!(total(both.clone()).await, 3);
        assert_eq!(total(hidden.clone()).await, 2);
        let page = HitsQuery {
            limit: 10,
            ..HitsQuery::default()
        };
        let hits = b.hits(&hidden, &q, &f, &page).await.unwrap();
        assert_eq!(hits.total, 2);
        assert_eq!(
            hits.hits
                .iter()
                .filter(|h| h.doc_id == doc(1, 1).doc_id)
                .count(),
            1
        );
    }
}
