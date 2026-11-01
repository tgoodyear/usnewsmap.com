//! Quickwit's search responses, decoded into the backend's results.

use serde::Deserialize;
use serde_json::Value;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use super::error::{aggregation_limit, failure_cause, splits_named};
use crate::snippet::{matched_in, opening_snippet, page_snippets};
use crate::{
    ja_snippets, rank, CubeCell, Hit, HitsPage, IndexSet, KeyCount, PlaceSummary, SearchError,
    Summary,
};

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub num_hits: u64,
    #[serde(default)]
    pub hits: Vec<StoredHit>,
    #[serde(default)]
    pub aggregations: Option<Value>,
    /// Partial failures (e.g. splits that could not be searched). A 200 with
    /// any errors is incomplete and must not be served or cached.
    #[serde(default)]
    pub errors: Vec<Value>,
}

/// The fields a hit carries: only those stored in the index (05 §5.5).
/// `day` and `front_page` are not stored; they are derived from `date` and `seq`.
#[derive(Debug, Deserialize)]
pub struct StoredHit {
    pub doc_id: String,
    /// Quickwit renders datetimes as RFC 3339 (`1896-07-10T00:00:00Z`) by default.
    pub date: String,
    pub place_id: String,
    pub lccn: String,
    pub edition: u16,
    pub seq: u16,
    /// The same-day order (stored, as fields are by default), which a
    /// browse list resumes after (#274).
    #[serde(default)]
    pub sort_key: Option<u64>,
    /// The page's stored text, for its snippets (#126).
    #[serde(default)]
    pub text: Option<String>,
    /// A Japanese page's printed text (pages-ja-index.yaml), for its snippets.
    #[serde(default)]
    pub printed: Option<String>,
    /// American Stories' text (05 §5.5.4), for snippets when the query
    /// matched only there.
    #[serde(default)]
    pub text_as: Option<String>,
    /// LoC's original text of a page our OCR replaced it on (#925), for
    /// snippets when the query matched only there.
    #[serde(default)]
    pub text_loc: Option<String>,
    #[serde(default)]
    pub ocr_source: Option<String>,
    #[serde(default)]
    pub ocr_engine: Option<String>,
    /// A Congress page's stored fields (congress-index.yaml, #229).
    #[serde(flatten)]
    pub congress: crate::CongressFields,
}

impl StoredHit {
    fn day(&self) -> Option<u32> {
        let ymd = self.date.get(..10)?;
        let date = chrono::NaiveDate::parse_from_str(ymd, "%Y-%m-%d").ok()?;
        Some(usnm_core::time::day_number(date))
    }
}

/// Reject responses that report partial failures, without echoing their
/// content: the error names how many, what they were about
/// ([`failure_cause`]) and
/// up to 5 of the failed splits, whose ids are opaque.
pub fn check_complete(resp: &SearchResponse, query: &str) -> Result<(), SearchError> {
    if resp.errors.is_empty() {
        return Ok(());
    }
    let mut causes = std::collections::BTreeMap::<&str, usize>::new();
    let mut texts = Vec::new();
    for e in &resp.errors {
        // Quickwit 0.9's REST response gives each error as a string.
        let text = e.as_str().map_or_else(|| e.to_string(), str::to_owned);
        *causes.entry(failure_cause(&text, query)).or_default() += 1;
        texts.push(text);
    }
    let causes: Vec<String> = causes.iter().map(|(c, n)| format!("{c}×{n}")).collect();
    let msg = format!(
        "quickwit reported {} partial failure(s): {}{}",
        resp.errors.len(),
        causes.join(", "),
        splits_named(&texts.join(" "), query)
    );
    // With any other failure among them, the search failed for that.
    if texts.iter().all(|t| aggregation_limit(t).is_some()) {
        return Err(SearchError::OverAggregationLimit(msg));
    }
    Err(SearchError::Backend(msg))
}

#[derive(Debug, Deserialize)]
struct Buckets<B> {
    buckets: Vec<B>,
}

#[derive(Debug, Deserialize)]
struct NumBucket {
    key: f64,
    doc_count: u64,
}

/// A `min` or `max` metric; `value` is null when no document matched.
#[derive(Debug, Deserialize)]
struct MetricValue {
    value: Option<f64>,
}

impl MetricValue {
    fn day(m: Option<Self>) -> Option<u32> {
        m.and_then(|m| m.value).map(|v| v as u32)
    }
}

#[derive(Debug, Deserialize)]
struct PlaceBucket {
    key: String,
    doc_count: u64,
    first: Option<MetricValue>,
    last: Option<MetricValue>,
    t: Option<Buckets<NumBucket>>,
}

fn agg<T: for<'de> Deserialize<'de>>(resp: &SearchResponse, name: &str) -> Result<T, SearchError> {
    let v = resp
        .aggregations
        .as_ref()
        .and_then(|a| a.get(name))
        .ok_or_else(|| SearchError::Backend(format!("missing aggregation `{name}`")))?;
    serde_json::from_value(v.clone())
        .map_err(|e| SearchError::Backend(format!("aggregation `{name}`: {e}")))
}

pub fn parse_summary(resp: &SearchResponse, spec: &BucketSpec) -> Result<Summary, SearchError> {
    let mut series = vec![0u64; spec.len()];
    for b in agg::<Buckets<NumBucket>>(resp, "series")?.buckets {
        if let Some(i) = spec.index_of_key(b.key as u32) {
            series[i] += b.doc_count;
        }
    }
    // Every place bucket has at least one page, so both days must be there.
    let mut places: Vec<PlaceSummary> = agg::<Buckets<PlaceBucket>>(resp, "places")?
        .buckets
        .into_iter()
        .map(|b| {
            let missing =
                || SearchError::Backend("place bucket without its first or last day".into());
            Ok(PlaceSummary {
                first_day: MetricValue::day(b.first).ok_or_else(missing)?,
                last_day: MetricValue::day(b.last).ok_or_else(missing)?,
                place_id: b.key,
                hits: b.doc_count,
            })
        })
        .collect::<Result<_, SearchError>>()?;
    // By id, as the memory backend gives them: Quickwit orders tied buckets
    // differently from one split layout to another (#293).
    places.sort_by(|a, b| a.place_id.cmp(&b.place_id));
    // Null when nothing matched; otherwise both days must be there.
    let day = |name| -> Result<Option<u32>, SearchError> {
        if resp.num_hits == 0 {
            return Ok(None);
        }
        MetricValue::day(Some(agg::<MetricValue>(resp, name)?))
            .map(Some)
            .ok_or_else(|| SearchError::Backend(format!("aggregation `{name}` has no day")))
    };
    Ok(Summary {
        total_hits: resp.num_hits,
        first_day: day("first")?,
        last_day: day("last")?,
        series,
        places,
        papers: key_counts(resp, "papers")?,
        languages: key_counts(resp, "languages")?,
        // Always requested: missing with matches is an incomplete response.
        days: match distinct(resp)? {
            Some(d) => d,
            None if resp.num_hits == 0 => 0,
            None => return Err(SearchError::Backend("missing aggregation `days`".into())),
        },
    })
}

#[derive(Debug, Deserialize)]
struct KeyBucket {
    key: String,
    doc_count: u64,
}

/// A terms aggregation's buckets, ranked as the memory backend ranks them.
fn key_counts(resp: &SearchResponse, name: &str) -> Result<Vec<KeyCount>, SearchError> {
    Ok(rank(
        agg::<Buckets<KeyBucket>>(resp, name)?
            .buckets
            .into_iter()
            .map(|b| KeyCount {
                key: b.key,
                hits: b.doc_count,
            })
            .collect(),
    ))
}

/// The `days` cardinality, when the request asked for it. Quickwit's
/// HyperLogLog gives a float; nothing matched is 0.
fn distinct(resp: &SearchResponse) -> Result<Option<u64>, SearchError> {
    if resp
        .aggregations
        .as_ref()
        .and_then(|a| a.get("days"))
        .is_none()
    {
        return Ok(None);
    }
    if resp.num_hits == 0 {
        return Ok(Some(0));
    }
    let m = agg::<MetricValue>(resp, "days")?;
    m.value
        .map(|v| Some(v.round() as u64))
        .ok_or_else(|| SearchError::Backend("aggregation `days` has no value".into()))
}

pub fn parse_cube(resp: &SearchResponse, spec: &BucketSpec) -> Result<Vec<CubeCell>, SearchError> {
    let mut cells = Vec::new();
    for place in agg::<Buckets<PlaceBucket>>(resp, "places")?.buckets {
        for b in place.t.map(|t| t.buckets).unwrap_or_default() {
            if let Some(i) = spec.index_of_key(b.key as u32) {
                cells.push(CubeCell {
                    place_id: place.key.clone(),
                    bucket: i as u32,
                    hits: b.doc_count as u32,
                });
            }
        }
    }
    Ok(cells)
}

/// `indexes`: whether the search covered American Stories' text, whose
/// snippets stand in when LoC's text has no match (05 §5.5.4), and the
/// analyzer that folded a Japanese page's text, for its snippets (#168).
pub fn parse_hits(
    resp: SearchResponse,
    query: &Node,
    indexes: &IndexSet,
) -> Result<HitsPage, SearchError> {
    hits_page(resp, Some(query), indexes)
}

/// The pages of a [`browse_request`] (#274): each with the opening of its
/// text as its snippet ([`opening_snippet`]), and no `snippet_source` or
/// `matched_in`.
///
/// [`browse_request`]: super::browse_request
pub fn parse_browse(resp: SearchResponse, indexes: &IndexSet) -> Result<HitsPage, SearchError> {
    hits_page(resp, None, indexes)
}

/// The hits of `resp`, with snippets of `query`, or without one the
/// opening of each page's text.
fn hits_page(
    resp: SearchResponse,
    query: Option<&Node>,
    indexes: &IndexSet,
) -> Result<HitsPage, SearchError> {
    let american_stories = indexes.american_stories();
    let reports_matched_in = indexes.reports_matched_in();
    let analyzer = indexes.analyzer();
    let layout = indexes.text_layout();
    let days = distinct(&resp)?;
    let mut hits = Vec::with_capacity(resp.hits.len());
    for d in resp.hits {
        let day = d
            .day()
            .ok_or_else(|| SearchError::Backend("hit has an unparseable date".into()))?;
        // Built here from the stored text, as the memory backend does (#126).
        // A Japanese page's come from its printed text: its indexed text is
        // folded tokens with spaces between them.
        // A browse list resumes after its last page's key, so it needs
        // each page's; a search's hits don't.
        let sort_key = match (d.sort_key, query) {
            (Some(k), _) => k,
            (None, Some(_)) => 0,
            (None, None) => {
                return Err(SearchError::Backend("hit has no sort_key".into()));
            }
        };
        let text = d.text.as_deref().unwrap_or_default();
        let (snippets, snippet_source, matched_in) = match query {
            None => (
                opening_snippet(d.printed.as_deref().unwrap_or(text)),
                None,
                None,
            ),
            Some(query) => {
                let (snippets, snippet_source) = match &d.printed {
                    Some(printed) => (ja_snippets(printed, query, analyzer), None),
                    None => page_snippets(
                        text,
                        d.text_as.as_deref(),
                        d.text_loc.as_deref(),
                        query,
                        american_stories,
                        layout,
                    ),
                };
                // Which texts match, from the same stored texts: no extra query.
                let matched_in = (reports_matched_in && d.printed.is_none()).then(|| {
                    matched_in(
                        text,
                        d.text_as.as_deref(),
                        d.text_loc.as_deref(),
                        query,
                        layout,
                    )
                });
                (snippets, snippet_source, matched_in)
            }
        };
        // A Congress unit's first page is no front page (#229), as its
        // document says and the memory backend reads.
        let front_page = d.seq == 1 && !d.congress.is_congress();
        hits.push(Hit {
            snippets,
            snippet_source,
            matched_in,
            ocr_source: d.ocr_source,
            ocr_engine: d.ocr_engine,
            congress: d.congress,
            front_page,
            day,
            sort_key,
            doc_id: d.doc_id,
            lccn: d.lccn,
            place_id: d.place_id,
            edition: d.edition,
            seq: d.seq,
        });
    }
    Ok(HitsPage {
        total: resp.num_hits,
        days,
        hits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quickwit::browse_request;
    use crate::quickwit::test_support::{d, filters, none, stories};
    use crate::{BrowseKey, HitSort, HitsQuery};
    use serde_json::json;
    use usnm_core::text_layout::TextLayout;
    use usnm_core::time::BucketUnit;

    /// Partial failures count as a refusal for size only when each of them
    /// is the engine's refusal.
    #[test]
    fn partial_failures_are_a_size_refusal_only_when_all_of_them_are() {
        let refusal = "Aborting aggregation because bucket limit was exceeded.";
        let check = |errors: Vec<&str>| {
            let resp: SearchResponse =
                serde_json::from_value(json!({ "num_hits": 1, "errors": errors })).unwrap();
            check_complete(&resp, "text:gold").unwrap_err()
        };
        assert!(matches!(
            check(vec![refusal, refusal]),
            SearchError::OverAggregationLimit(_)
        ));
        for errors in [
            vec![refusal, "storage error"],
            vec!["could not read the bucket from storage"],
        ] {
            assert!(matches!(check(errors), SearchError::Backend(_)));
        }
    }

    #[test]
    fn decodes_hits_from_stored_fields_only() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 2,
            "hits": [
                {"doc_id": "sn99000001_1896-07-10_ed-1_seq-1", "date": "1896-07-10T00:00:00Z",
                 "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 1,
                 "text": "a <gold> b"},
                {"doc_id": "sn99000001_1896-07-10_ed-1_seq-3", "date": "1896-07-10T00:00:00Z",
                 "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 3}
            ]
        }))
        .unwrap();
        let page = parse_hits(
            resp,
            &usnm_core::query::parse("gold").unwrap(),
            &stories(false),
        )
        .unwrap();
        assert_eq!(
            page.hits[0].day,
            usnm_core::time::day_number(d("1896-07-10"))
        );
        assert!(page.hits[0].front_page);
        assert!(!page.hits[1].front_page);
        assert_eq!(page.hits[0].snippets, vec!["a &lt;<mark>gold</mark>&gt; b"]);
        assert!(page.hits[1].snippets.is_empty());
        // Without the `days` aggregation (a later page), no count.
        assert_eq!(page.days, None);
        let first: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3, "hits": [], "aggregations": { "days": { "value": 2.0 } }
        }))
        .unwrap();
        let q = usnm_core::query::parse("gold").unwrap();
        assert_eq!(
            parse_hits(first, &q, &stories(false)).unwrap().days,
            Some(2)
        );
    }

    /// A title's pages without a query (#274): the filters alone, in date
    /// order, never by relevance, and each page's opening as its snippet.
    #[test]
    fn browsing_sends_the_filters_alone_and_decodes_openings() {
        let page = HitsQuery {
            place_id: None,
            lccn: Some("sn99000001".into()),
            sort: HitSort::Newest,
            offset: 0,
            limit: 20,
            days: true,
        };
        let mut f = filters();
        f.lccns = vec!["sn99000001".into()];
        let r = browse_request(&f, &stories(true), &page, None).unwrap();
        assert_eq!(
            r["query"],
            format!(
                "day:[{} TO {}] AND state:IN [GA SC] AND lccn:IN [sn99000001] \
                 AND front_page:true AND lccn:sn99000001",
                f.from_day(),
                f.to_day()
            )
        );
        assert_eq!(r["sort_by"], "day,sort_key");
        assert_eq!(r["max_hits"], 20);
        assert_eq!(r["aggs"]["days"]["cardinality"]["field"], "day");
        // After a key: the pages before it newest first, later ones oldest
        // first, by day and then `sort_key` (a fast field: a range on it
        // needs no index).
        let key = BrowseKey {
            day: 71_995,
            sort_key: 7 << 32 | 1 << 16 | 2,
        };
        let r = browse_request(&f, &stories(true), &page, Some(key)).unwrap();
        let q = r["query"].as_str().unwrap();
        assert!(
            q.ends_with(
                " AND lccn:sn99000001 AND (day:<71995 OR (day:71995 AND sort_key:<30064836610))"
            ),
            "{q}"
        );
        let oldest = HitsQuery {
            sort: HitSort::Oldest,
            ..page.clone()
        };
        let r = browse_request(&f, &stories(true), &oldest, Some(key)).unwrap();
        assert!(r["query"]
            .as_str()
            .unwrap()
            .ends_with(" AND (day:>71995 OR (day:71995 AND sort_key:>30064836610))"));
        let relevant = HitsQuery {
            sort: HitSort::Relevant,
            ..page.clone()
        };
        assert!(matches!(
            browse_request(&f, &none(), &relevant, None),
            Err(SearchError::Unsupported(_))
        ));
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 2,
            "hits": [
                {"doc_id": "sn99000001_1896-07-10_ed-1_seq-1", "date": "1896-07-10T00:00:00Z",
                 "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 1,
                 "sort_key": 4_295_032_833_u64, "text": "THE <BEE>\nOmaha", "text_as": "other"},
                {"doc_id": "sn99000001_1896-07-10_ed-1_seq-2", "date": "1896-07-10T00:00:00Z",
                 "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 2,
                 "sort_key": 4_295_032_834_u64}
            ]
        }))
        .unwrap();
        let page = parse_browse(resp, &stories(true)).unwrap();
        assert_eq!(page.total, 2);
        assert_eq!(
            BrowseKey::of(&page.hits[1]),
            BrowseKey {
                day: 71_778,
                sort_key: 4_295_032_834
            }
        );
        assert_eq!(page.hits[0].snippets, vec!["THE &lt;BEE&gt; Omaha"]);
        assert!(page.hits[1].snippets.is_empty());
        assert!(page
            .hits
            .iter()
            .all(|h| h.matched_in.is_none() && h.snippet_source.is_none()));
    }

    /// A Congress page's stored fields come back with it, and the first page
    /// of its unit is no front page (#229).
    #[test]
    fn decodes_congress_hits() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 1,
            "hits": [
                {"doc_id": "fixture-globe-0001_p1", "date": "1850-01-07T00:00:00Z",
                 "place_id": "PC001", "lccn": "", "edition": 1, "seq": 1,
                 "text": "the gold of california", "series": "globe",
                 "title": "Fixture Globe", "citation": "Cong. Globe p. 1", "congress": 31,
                 "link": "https://example.org/p1", "ocr_source": "unt"}
            ]
        }))
        .unwrap();
        let q = usnm_core::query::parse("gold").unwrap();
        let hit = &parse_hits(resp, &q, &stories(false)).unwrap().hits[0];
        assert!(!hit.front_page);
        assert_eq!((hit.seq, hit.place_id.as_str()), (1, "PC001"));
        assert_eq!(hit.congress.series.as_deref(), Some("globe"));
        assert_eq!(hit.congress.congress, Some(31));
        assert_eq!(hit.congress.link.as_deref(), Some("https://example.org/p1"));
        assert_eq!(hit.ocr_source.as_deref(), Some("unt"));
        assert_eq!(hit.snippets, vec!["the <mark>gold</mark> of california"]);
    }

    /// A hit says which texts the query matches only when the search covers
    /// American Stories' text (05 §5.5.4), from the texts it already has.
    #[test]
    fn hits_say_which_texts_match_when_both_are_searched() {
        let resp = || -> SearchResponse {
            serde_json::from_value(json!({
                "num_hits": 3,
                "hits": [
                    {"doc_id": "sn99000001_1896-07-10_ed-1_seq-1", "date": "1896-07-10T00:00:00Z",
                     "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 1,
                     "text": "a cross of goid", "text_as": "a cross of gold"},
                    {"doc_id": "sn99000001_1896-07-10_ed-1_seq-2", "date": "1896-07-10T00:00:00Z",
                     "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 2,
                     "text": "gold", "text_as": "gold"},
                    {"doc_id": "sn99000001_1896-07-10_ed-1_seq-3", "date": "1896-07-10T00:00:00Z",
                     "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 3,
                     "text": "gold"}
                ]
            }))
            .unwrap()
        };
        let q = usnm_core::query::parse("gold").unwrap();
        let on = parse_hits(resp(), &q, &stories(true)).unwrap();
        let matched: Vec<_> = on.hits.iter().map(|h| h.matched_in.clone()).collect();
        assert_eq!(
            matched,
            [
                Some(vec!["american_stories"]),
                Some(vec!["loc", "american_stories"]),
                Some(vec!["loc"]),
            ]
        );
        let off = parse_hits(resp(), &q, &stories(false)).unwrap();
        assert!(off.hits.iter().all(|h| h.matched_in.is_none()));
    }

    /// LoC's original text of a page our OCR replaced it on is stored with
    /// the hit (#925): the layout that keeps it reads it for the snippets and
    /// `matched_in`, as the memory backend does; the layouts before it
    /// never do, whatever a document holds.
    #[test]
    fn hits_read_the_stored_original_text_on_the_layout_that_keeps_it() {
        let resp = || -> SearchResponse {
            serde_json::from_value(json!({
                "num_hits": 2,
                "hits": [
                    {"doc_id": "sn99000001_1896-07-10_ed-1_seq-1", "date": "1896-07-10T00:00:00Z",
                     "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 1,
                     "text": "waren auf dem Wege Dubnaue nach Mailand",
                     "text_loc": "Die Frauen waren auf dem Wege nach Mailand begriffen"},
                    {"doc_id": "sn99000001_1896-07-10_ed-1_seq-2", "date": "1896-07-10T00:00:00Z",
                     "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 2,
                     "text": "waren auf dem Wege nach Mailand"}
                ]
            }))
            .unwrap()
        };
        let kept = stories(false).with_text_layout(TextLayout::SingleWithLoc);
        let q = usnm_core::query::parse(r#""Wege nach Mailand""#).unwrap();
        let page = parse_hits(resp(), &q, &kept).unwrap();
        assert_eq!(
            page.hits[0].snippets,
            ["Die Frauen waren auf dem <mark>Wege nach Mailand</mark> begriffen"]
        );
        assert_eq!(page.hits[0].snippet_source, Some("loc_original"));
        assert_eq!(page.hits[0].matched_in, Some(vec!["loc_original"]));
        assert_eq!(page.hits[1].snippet_source, None);
        assert_eq!(page.hits[1].matched_in, Some(vec!["loc"]));
        // Layout 2 has no such field; a document that has it is read as before.
        let single = stories(false).with_text_layout(TextLayout::Single);
        let page = parse_hits(resp(), &q, &single).unwrap();
        assert!(page.hits[0].snippets.is_empty());
        assert!(page.hits.iter().all(|h| h.matched_in.is_none()));
    }

    #[test]
    fn parses_aggregation_responses() {
        let spec = BucketSpec::new(BucketUnit::Year, d("1895-01-01"), d("1897-12-31"));
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 7,
            "aggregations": {
                "series": { "buckets": [ {"key": 1895.0, "doc_count": 2}, {"key": 1896.0, "doc_count": 5} ] },
                "first": {"value": 71000.0},
                "last": {"value": 71250.0},
                "places": { "buckets": [
                    {"key": "P00001", "doc_count": 4, "first": {"value": 71000.0}, "last": {"value": 71200.0},
                     "t": {"buckets": [{"key": 1895.0, "doc_count": 1}, {"key": 1896.0, "doc_count": 3}]}},
                    {"key": "P00002", "doc_count": 3, "first": {"value": 71100.0}, "last": {"value": 71250.0},
                     "t": {"buckets": [{"key": 1896.0, "doc_count": 3}]}}
                ]},
                "papers": { "buckets": [ {"key": "sn2", "doc_count": 3}, {"key": "sn1", "doc_count": 4},
                                         {"key": "sn3", "doc_count": 3} ] },
                "languages": { "buckets": [ {"key": "eng", "doc_count": 7}, {"key": "ger", "doc_count": 2} ] },
                "days": { "value": 5.0 }
            }
        }))
        .unwrap();
        let s = parse_summary(&resp, &spec).unwrap();
        assert_eq!(s.series, vec![2, 5, 0]);
        assert_eq!(s.places[0].first_day, 71000);
        assert_eq!(s.places[0].last_day, 71200);
        assert_eq!((s.first_day, s.last_day), (Some(71000), Some(71250)));
        // Most first, ties by key, whatever order Quickwit gives them in.
        let keys = |c: &[KeyCount]| {
            c.iter()
                .map(|k| (k.key.clone(), k.hits))
                .collect::<Vec<_>>()
        };
        assert_eq!(
            keys(&s.papers),
            [("sn1".into(), 4), ("sn2".into(), 3), ("sn3".into(), 3)]
        );
        assert_eq!(keys(&s.languages), [("eng".into(), 7), ("ger".into(), 2)]);
        assert_eq!(s.days, 5);
        // Without `days`, a summary with matches is incomplete, not 0 days.
        let mut partial = resp.aggregations.clone().unwrap();
        partial.as_object_mut().unwrap().remove("days");
        let incomplete: SearchResponse =
            serde_json::from_value(json!({ "num_hits": 7, "aggregations": partial })).unwrap();
        assert!(matches!(
            parse_summary(&incomplete, &spec),
            Err(SearchError::Backend(_))
        ));
        let cells = parse_cube(&resp, &spec).unwrap();
        assert_eq!(cells.len(), 3);
        assert_eq!(
            cells[1],
            CubeCell {
                place_id: "P00001".into(),
                bucket: 1,
                hits: 3
            }
        );
    }

    #[test]
    fn places_come_by_id_whatever_order_quickwit_gives_them_in() {
        // Two layouts of one index gave these tied buckets in opposite orders (#293).
        let spec = BucketSpec::new(BucketUnit::Year, d("1895-01-01"), d("1897-12-31"));
        let place = |key: &str, n: u64| {
            let (first, last) = (json!({"value": 71000.0}), json!({"value": 71200.0}));
            json!({"key": key, "doc_count": n, "first": first, "last": last})
        };
        let summary = |places: Vec<Value>| {
            let resp: SearchResponse = serde_json::from_value(json!({
                "num_hits": 128,
                "aggregations": {
                    "series": { "buckets": [] },
                    "first": {"value": 71000.0},
                    "last": {"value": 71200.0},
                    "places": { "buckets": places },
                    "papers": { "buckets": [] },
                    "languages": { "buckets": [] },
                    "days": { "value": 3.0 }
                }
            }))
            .unwrap();
            parse_summary(&resp, &spec).unwrap()
        };
        let a = summary(vec![
            place("P00009", 16),
            place("P00003", 56),
            place("P00004", 56),
        ]);
        let b = summary(vec![
            place("P00004", 56),
            place("P00003", 56),
            place("P00009", 16),
        ]);
        assert_eq!(a, b);
        let ids: Vec<&str> = a.places.iter().map(|p| p.place_id.as_str()).collect();
        assert_eq!(ids, ["P00003", "P00004", "P00009"]);
    }

    #[test]
    fn a_summary_with_no_matches_has_no_first_or_last_day() {
        let spec = BucketSpec::new(BucketUnit::Year, d("1895-01-01"), d("1897-12-31"));
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 0,
            "aggregations": {
                "series": { "buckets": [] },
                "first": {"value": null},
                "last": {"value": null},
                "places": { "buckets": [] },
                "papers": { "buckets": [] },
                "languages": { "buckets": [] }
            }
        }))
        .unwrap();
        let s = parse_summary(&resp, &spec).unwrap();
        assert_eq!((s.first_day, s.last_day), (None, None));
    }

    #[test]
    fn a_summary_with_matches_needs_its_first_and_last_day() {
        let spec = BucketSpec::new(BucketUnit::Year, d("1895-01-01"), d("1897-12-31"));
        for (first, last) in [
            (
                Some(json!({"value": null})),
                Some(json!({"value": 71200.0})),
            ),
            (None, Some(json!({"value": 71200.0}))),
            (
                Some(json!({"value": 71000.0})),
                Some(json!({"value": "late"})),
            ),
        ] {
            let mut aggs = json!({
                "series": { "buckets": [] }, "places": { "buckets": [] },
                "papers": { "buckets": [] }, "languages": { "buckets": [] }
            });
            if let Some(f) = first {
                aggs["first"] = f;
            }
            if let Some(l) = last {
                aggs["last"] = l;
            }
            let resp: SearchResponse =
                serde_json::from_value(json!({ "num_hits": 4, "aggregations": aggs })).unwrap();
            assert!(matches!(
                parse_summary(&resp, &spec),
                Err(SearchError::Backend(_))
            ));
        }
    }

    #[test]
    fn a_place_without_its_days_is_an_error_not_day_zero() {
        let spec = BucketSpec::new(BucketUnit::Year, d("1895-01-01"), d("1897-12-31"));
        for place in [
            json!({"key": "P00001", "doc_count": 4, "first": {"value": 71000.0}}),
            json!({"key": "P00001", "doc_count": 4, "first": {"value": null}, "last": {"value": 71200.0}}),
        ] {
            let resp: SearchResponse = serde_json::from_value(json!({
                "num_hits": 4,
                "aggregations": {
                    "series": { "buckets": [] },
                    "first": {"value": 71000.0},
                    "last": {"value": 71200.0},
                    "places": { "buckets": [place] },
                    "papers": { "buckets": [] },
                    "languages": { "buckets": [] }
                }
            }))
            .unwrap();
            assert!(matches!(
                parse_summary(&resp, &spec),
                Err(SearchError::Backend(_))
            ));
        }
    }
}
