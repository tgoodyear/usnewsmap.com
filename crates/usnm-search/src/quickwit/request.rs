//! The request bodies of the searches: summary, cube, hits, browse and the American Stories count.

use serde_json::{json, Value};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use super::query::{american_stories_only_string, full_query, scope, scoped};
use super::{MAX_LANGUAGES, MAX_PAPERS, MAX_PLACES};
use crate::{BrowseKey, HitSort, HitsQuery, IndexSet, SearchError};

fn histogram(spec: &BucketSpec, min_doc_count: u64) -> Value {
    let h = spec.histogram_field();
    json!({
        "histogram": {
            "field": h.field,
            "interval": h.interval,
            "offset": h.origin % h.interval,
            "min_doc_count": min_doc_count
        }
    })
}

pub fn summary_request(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    spec: &BucketSpec,
) -> Result<Value, SearchError> {
    Ok(json!({
        "query": full_query(node, filters, indexes, Vec::new())?,
        "max_hits": 0,
        "aggs": {
            "series": histogram(spec, 1),
            "first": { "min": { "field": "day" } },
            "last": { "max": { "field": "day" } },
            "places": {
                "terms": { "field": "place_id", "size": MAX_PLACES },
                "aggs": {
                    "first": { "min": { "field": "day" } },
                    "last": { "max": { "field": "day" } }
                }
            },
            "days": { "cardinality": { "field": "day" } },
            "papers": { "terms": { "field": "lccn", "size": MAX_PAPERS } },
            "languages": { "terms": { "field": indexes.language_field(), "size": MAX_LANGUAGES } }
        }
    }))
}

pub fn cube_request(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    spec: &BucketSpec,
    shards: &[u8],
) -> Result<Value, SearchError> {
    let mut extra = Vec::new();
    if shards.len() < usize::from(usnm_core::cube::PLACE_SHARDS) {
        let list: Vec<String> = shards.iter().map(u8::to_string).collect();
        extra.push(format!("place_shard:IN [{}]", list.join(" ")));
    }
    cube_body(node, filters, indexes, spec, extra, MAX_PLACES)
}

/// The cube for the places in `places` only (`/v1/days`). Place ids are
/// our own (letters, digits, `_` and `-`), so they need no escaping; any
/// other id is refused rather than put in the query.
pub fn place_cube_request(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    spec: &BucketSpec,
    places: &[String],
) -> Result<Value, SearchError> {
    let plain = |p: &String| {
        !p.is_empty()
            && p.bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
    };
    if places.is_empty() || !places.iter().all(plain) {
        return Err(SearchError::Rejected(
            "place ids must be letters, digits, `_` or `-`".into(),
        ));
    }
    let extra = vec![format!("place_id:IN [{}]", places.join(" "))];
    let size = u32::try_from(places.len()).unwrap_or(MAX_PLACES);
    cube_body(node, filters, indexes, spec, extra, size)
}

fn cube_body(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    spec: &BucketSpec,
    extra: Vec<String>,
    places: u32,
) -> Result<Value, SearchError> {
    Ok(json!({
        "query": full_query(node, filters, indexes, extra)?,
        "max_hits": 0,
        "aggs": {
            "places": {
                "terms": { "field": "place_id", "size": places },
                "aggs": { "t": histogram(spec, 1) }
            }
        }
    }))
}

pub fn hits_request(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    page: &HitsQuery,
) -> Result<Value, SearchError> {
    let query = full_query(node, filters, indexes, selection(page))?;
    Ok(page_request(query, page))
}

/// The place or title a page list is limited to.
fn selection(page: &HitsQuery) -> Vec<String> {
    let mut extra = Vec::new();
    if let Some(p) = &page.place_id {
        extra.push(format!("place_id:{p}"));
    }
    if let Some(l) = &page.lccn {
        extra.push(format!("lccn:{l}"));
    }
    extra
}

/// Pages without a query (#274): the filters' clauses alone, with no text
/// term, in date order. Quickwit then has no posting list of a word to
/// skip splits by: it reads the `lccn` (or `place_id`) and `day` postings
/// of every split of the version, and skips splits only as any search
/// limited to some decades does (05 §5.5.5).
/// `Relevant` is refused, as by every backend.
///
/// With `after`, a clause keeps only the pages after that key in the
/// list's order (`day`, then `sort_key`): keyset pagination, as the native
/// search API of Quickwit 0.9 takes no `search_after` (its body refuses
/// the field). Both fields are fast fields, and a range on a fast field
/// needs no index (`sort_key` has `indexed: false`), so no reindex.
pub fn browse_request(
    filters: &Filters,
    indexes: &IndexSet,
    page: &HitsQuery,
    after: Option<BrowseKey>,
) -> Result<Value, SearchError> {
    crate::browse_order(page.sort)?;
    let mut extra = selection(page);
    extra.extend(after.map(|a| after_clause(a, page.sort)));
    let query = scope(filters, indexes, extra).join(" AND ");
    Ok(page_request(query, page))
}

/// The pages after `a` in `sort` order (`Oldest` or `Newest`), as
/// [`BrowseKey::precedes`] says.
fn after_clause(a: BrowseKey, sort: HitSort) -> String {
    let op = match sort {
        HitSort::Newest => '<',
        HitSort::Oldest | HitSort::Relevant => '>',
    };
    format!(
        "(day:{op}{day} OR (day:{day} AND sort_key:{op}{key}))",
        day = a.day,
        key = a.sort_key
    )
}

/// A page of a hit list for `query`, with the days counted when asked.
fn page_request(query: String, page: &HitsQuery) -> Value {
    let mut req = json!({
        "query": query,
        "max_hits": page.limit,
        "start_offset": page.offset,
        "sort_by": sort_by(page.sort)
    });
    // On how many days the pages appeared (#127), when asked: the first page
    // of a visitor's list, not the aggregate's first and last page lookups.
    if page.days {
        req["aggs"] = json!({ "days": { "cardinality": { "field": "day" } } });
    }
    req
}

/// How many pages only American Stories' text finds
/// ([`american_stories_only_string`]): no hits, no aggregations, no sort.
/// Quickwit still counts every match (`num_hits`). Refused with one field
/// for all the texts (05 §5.5.7): LoC's alone isn't indexed there.
pub fn american_stories_only_request(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
) -> Result<Value, SearchError> {
    if indexes.text_layout().searches_one_field() {
        return Err(SearchError::Unsupported(
            "counting the pages only American Stories' text matches, on these indexes".into(),
        ));
    }
    let query = american_stories_only_string(node, indexes.common_grams())?;
    Ok(json!({
        "query": scoped(query, filters, indexes, Vec::new()),
        "max_hits": 0
    }))
}

/// By day, then title, edition and page (`sort_key`). Quickwit 0.9 can't sort
/// on text fields, and a leading `-` means ascending (S-2). `_score` sorts
/// highest first; with `fieldnorms: false` on `text` it follows how often a
/// page mentions the words (#126), and ties go oldest first. A third field
/// (`-sort_key`) is refused: "sort by field must be up to 2 fields".
fn sort_by(sort: HitSort) -> &'static str {
    match sort {
        HitSort::Oldest => "-day,-sort_key",
        HitSort::Newest => "day,sort_key",
        HitSort::Relevant => "_score,-day",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quickwit::test_support::{d, filters, none};
    use crate::quickwit::{parse_cube, SearchResponse};
    use usnm_core::query::parse;
    use usnm_core::time::BucketUnit;

    #[test]
    fn builds_summary_and_sharded_cube_requests() {
        let q = parse("gold").unwrap();
        let spec = BucketSpec::new(BucketUnit::Week, d("1896-06-01"), d("1896-12-31"));
        let origin = spec.histogram_field().origin;
        let s = summary_request(&q, &filters(), &none(), &spec).unwrap();
        assert_eq!(
            s["query"],
            format!(
                "text:gold AND day:[{} TO {}] AND state:IN [GA SC] AND front_page:true",
                filters().from_day(),
                filters().to_day()
            )
        );
        assert_eq!(s["aggs"]["series"]["histogram"]["interval"], 7);
        assert_eq!(s["aggs"]["series"]["histogram"]["offset"], origin % 7);
        assert_eq!(s["aggs"]["first"]["min"]["field"], "day");
        assert_eq!(s["aggs"]["last"]["max"]["field"], "day");
        assert_eq!(s["aggs"]["places"]["aggs"]["first"]["min"]["field"], "day");
        assert_eq!(s["aggs"]["places"]["aggs"]["last"]["max"]["field"], "day");
        let c = cube_request(&q, &filters(), &none(), &spec, &[0, 5]).unwrap();
        assert!(c["query"]
            .as_str()
            .unwrap()
            .ends_with("AND place_shard:IN [0 5]"));
        let all = cube_request(&q, &filters(), &none(), &spec, &[0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        assert!(!all["query"].as_str().unwrap().contains("place_shard"));
    }

    #[test]
    fn a_version_laid_out_by_decade_names_the_searchs_decades() {
        let q = parse("gold").unwrap();
        let spec = BucketSpec::new(BucketUnit::Week, d("1896-06-01"), d("1896-12-31"));
        let set = IndexSet::new(vec!["i".into()]).with_decades(Some(1820..=1960));
        // After the exact `day` filter, the decade for the splits to open,
        // in every request: summaries, cubes, hits and the count.
        let want = format!(
            "text:gold AND day:[{} TO {}] AND state:IN [GA SC] AND front_page:true \
             AND decade:IN [1890]",
            filters().from_day(),
            filters().to_day()
        );
        let s = summary_request(&q, &filters(), &set, &spec).unwrap();
        assert_eq!(s["query"], want);
        let c = cube_request(&q, &filters(), &set, &spec, &[0, 5]).unwrap();
        assert!(c["query"]
            .as_str()
            .unwrap()
            .contains("AND decade:IN [1890] AND place_shard"));
        let h = hits_request(&q, &filters(), &set, &HitsQuery::default()).unwrap();
        assert!(h["query"]
            .as_str()
            .unwrap()
            .ends_with("AND decade:IN [1890]"));
        let both = set.clone().with_american_stories(true);
        let n = american_stories_only_request(&q, &filters(), &both).unwrap();
        assert!(n["query"]
            .as_str()
            .unwrap()
            .ends_with("AND decade:IN [1890]"));
        // A search over every decade of the version skips nothing: no clause.
        let mut all = filters();
        all.from = d("1700-01-01");
        all.to = d("1963-12-31");
        let s = summary_request(&q, &all, &set, &spec).unwrap();
        assert!(!s["query"].as_str().unwrap().contains("decade"));
        // A version without the field never gets the clause, and no search
        // ever filters on `date` or sends timestamps (#158).
        let s = summary_request(&q, &filters(), &none(), &spec).unwrap();
        assert!(!s["query"].as_str().unwrap().contains("decade"));
        for r in [
            summary_request(&q, &filters(), &set, &spec).unwrap(),
            hits_request(&q, &filters(), &set, &HitsQuery::default()).unwrap(),
        ] {
            assert!(r.get("start_timestamp").is_none() && r.get("end_timestamp").is_none());
            assert!(!r["query"].as_str().unwrap().contains("date:"));
        }
    }

    #[test]
    fn builds_place_cube_requests_by_day() {
        let q = parse("gold").unwrap();
        let spec = BucketSpec::new(BucketUnit::Day, d("1896-06-01"), d("1896-12-31"));
        let places = vec!["P00003".to_owned(), "P00001".to_owned()];
        let r = place_cube_request(&q, &filters(), &none(), &spec, &places).unwrap();
        assert_eq!(
            r["query"],
            format!(
                "text:gold AND day:[{} TO {}] AND state:IN [GA SC] AND front_page:true \
                 AND place_id:IN [P00003 P00001]",
                filters().from_day(),
                filters().to_day()
            )
        );
        assert_eq!(r["max_hits"], 0);
        let places_agg = &r["aggs"]["places"];
        assert_eq!(places_agg["terms"]["field"], "place_id");
        assert_eq!(places_agg["terms"]["size"], 2);
        let h = &places_agg["aggs"]["t"]["histogram"];
        assert_eq!(h["field"], "day");
        assert_eq!(h["interval"], 1);
        assert_eq!(h["min_doc_count"], 1);
        assert_eq!(h["offset"], 0);
        // Keys come back as day numbers: bucket = day - from.
        let from = usnm_core::time::day_number(d("1896-06-01"));
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3,
            "aggregations": { "places": { "buckets": [
                {"key": "P00003", "doc_count": 3,
                 "t": {"buckets": [{"key": f64::from(from + 2), "doc_count": 1},
                                   {"key": f64::from(from + 9), "doc_count": 2}]}}
            ]}}
        }))
        .unwrap();
        let cells = parse_cube(&resp, &spec).unwrap();
        assert_eq!(
            cells.iter().map(|c| (c.bucket, c.hits)).collect::<Vec<_>>(),
            [(2, 1), (9, 2)]
        );
        // Anything but a plain id never reaches the query.
        for bad in [vec![], vec!["P1 OR x".to_owned()], vec![String::new()]] {
            assert!(matches!(
                place_cube_request(&q, &filters(), &none(), &spec, &bad),
                Err(SearchError::Rejected(_))
            ));
        }
    }

    #[test]
    fn hits_sort_by_day_then_sort_key() {
        let page = HitsQuery {
            place_id: Some("P00001".into()),
            lccn: None,
            sort: HitSort::Oldest,
            offset: 50,
            limit: 25,
            days: false,
        };
        let r = hits_request(&parse("gold").unwrap(), &filters(), &none(), &page).unwrap();
        // A leading `-` is ascending in Quickwit 0.9 (S-2): oldest first.
        assert_eq!(r["sort_by"], "-day,-sort_key");
        let newest = HitsQuery {
            sort: HitSort::Newest,
            ..page.clone()
        };
        let r2 = hits_request(&parse("gold").unwrap(), &filters(), &none(), &newest).unwrap();
        assert_eq!(r2["sort_by"], "day,sort_key");
        let relevant = HitsQuery {
            sort: HitSort::Relevant,
            ..page.clone()
        };
        let r3 = hits_request(&parse("gold").unwrap(), &filters(), &none(), &relevant).unwrap();
        assert_eq!(r3["sort_by"], "_score,-day");
        // Snippets are built from the stored text, not asked of Quickwit (#126).
        assert!(r.get("snippet_fields").is_none());
        // Distinct days (#127) only when asked, even on a first page.
        assert!(r.get("aggs").is_none());
        let first = HitsQuery {
            offset: 0,
            ..page.clone()
        };
        let r4 = hits_request(&parse("gold").unwrap(), &filters(), &none(), &first).unwrap();
        assert!(r4.get("aggs").is_none());
        let counted = HitsQuery {
            days: true,
            ..first.clone()
        };
        let r5 = hits_request(&parse("gold").unwrap(), &filters(), &none(), &counted).unwrap();
        assert_eq!(r5["aggs"]["days"]["cardinality"]["field"], "day");
        assert_eq!(r["start_offset"], 50);
        assert!(r["query"]
            .as_str()
            .unwrap()
            .ends_with("AND place_id:P00001"));
    }

    #[test]
    fn american_stories_only_asks_for_a_count_of_one_text_but_not_the_other() {
        let q = parse("gold -bryan").unwrap();
        let both = none().with_american_stories(true);
        let r = american_stories_only_request(&q, &filters(), &both).unwrap();
        assert_eq!(r["max_hits"], 0);
        assert!(r.get("aggs").is_none() && r.get("sort_by").is_none());
        assert!(r["query"].as_str().unwrap().starts_with(
            "((NOT (text:bryan OR text_as:bryan) AND (text:gold OR text_as:gold))) \
             AND NOT ((NOT text:bryan AND text:gold)) \
             AND ((NOT text_as:bryan AND text_as:gold)) AND day:["
        ));
        // Exact phrases through the pairs, one text at a time.
        let phrase = parse(r#""cross of gold""#).unwrap();
        assert_eq!(
            american_stories_only_string(&phrase, true).unwrap(),
            "(((text_cg:\"cross of_gold gold\" AND text:cross AND text:gold) \
             OR (text_as_cg:\"cross of_gold gold\" AND text_as:cross AND text_as:gold))) \
             AND NOT ((text_cg:\"cross of_gold gold\" AND text:cross AND text:gold)) \
             AND ((text_as_cg:\"cross of_gold gold\" AND text_as:cross AND text_as:gold))"
        );
    }
}
