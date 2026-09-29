//! Quickwit backend (ADR-0001): translates the AST into Quickwit's query
//! language and aggregation requests, and queries an explicit set of sealed
//! indexes (08 §8.4.1).
//!
//! Behaviour confirmed against Quickwit 0.9.1 in Spike S-2 is marked `S-2`
//! (05 §5.5.1); `tests/quickwit_parity.rs` checks it against the reference
//! backend.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use usnm_core::params::Filters;
use usnm_core::query::Node;
use usnm_core::time::BucketSpec;

use crate::{
    mark_html, Capabilities, CubeCell, Hit, HitsPage, HitsQuery, IndexSet, PlaceSummary,
    SearchBackend, SearchError, Summary,
};

/// Upper bound on places in one terms aggregation.
const MAX_PLACES: u32 = 5_000;

pub struct QuickwitBackend {
    base_url: String,
    client: reqwest::Client,
    /// Limit on a search request, if different from the client's.
    search_timeout: Option<Duration>,
}

impl QuickwitBackend {
    /// `timeout` limits every request: searches, index lookups and health checks.
    pub fn new(base_url: &str, timeout: Duration) -> Result<Self, SearchError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| SearchError::Backend(e.to_string()))?;
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            client,
            search_timeout: None,
        })
    }

    /// Give searches their own limit, leaving index lookups and health
    /// checks on the client's.
    pub fn with_search_timeout(mut self, timeout: Duration) -> Self {
        self.search_timeout = Some(timeout);
        self
    }

    async fn search(
        &self,
        indexes: &IndexSet,
        body: &Value,
    ) -> Result<SearchResponse, SearchError> {
        if indexes.ids().is_empty() {
            return Err(SearchError::Backend(
                "no indexes in the published version".into(),
            ));
        }
        let url = format!(
            "{}/api/v1/{}/search",
            self.base_url,
            indexes.ids().join(",")
        );
        let mut req = self.client.post(url).json(body);
        if let Some(t) = self.search_timeout {
            req = req.timeout(t);
        }
        let resp = req.send().await.map_err(map_err)?;
        let status = resp.status();
        if !status.is_success() {
            // Never include the response body: Quickwit may echo the query, and
            // query text must not reach logs (09 §9.4.2).
            return Err(SearchError::Backend(format!("quickwit returned {status}")));
        }
        let parsed: SearchResponse = resp.json().await.map_err(map_err)?;
        check_complete(&parsed)?;
        Ok(parsed)
    }
}

fn map_err(e: reqwest::Error) -> SearchError {
    if e.is_timeout() {
        SearchError::Timeout
    } else {
        SearchError::Backend(e.to_string())
    }
}

// ------------------------------------------------------------ translation

/// Quickwit query-language string for the AST. Terms are folded alphanumerics
/// from our own parser, so they never need escaping.
pub fn query_string(node: &Node) -> Result<String, SearchError> {
    Ok(match node {
        Node::Term(t) if t.fuzzy > 0 => {
            // S-2: Quickwit 0.9's query language has no fuzzy terms (`~n` on a
            // term silently matches nothing), so refuse rather than miscount.
            return Err(SearchError::Unsupported(
                "fuzzy (OCR-tolerant) matching".into(),
            ));
        }
        Node::Term(t) if t.prefix => format!("text:{}*", t.text),
        Node::Term(t) => format!("text:{}", t.text),
        Node::Phrase { terms, slop } if *slop > 0 => format!("text:\"{}\"~{slop}", terms.join(" ")),
        Node::Phrase { terms, .. } => format!("text:\"{}\"", terms.join(" ")),
        Node::And(c) => format!("({})", join(c, " AND ")?),
        Node::Or(c) => format!("({})", join(c, " OR ")?),
        Node::Not(n) => format!("NOT {}", query_string(n)?),
    })
}

fn join(children: &[Node], sep: &str) -> Result<String, SearchError> {
    Ok(children
        .iter()
        .map(query_string)
        .collect::<Result<Vec<_>, _>>()?
        .join(sep))
}

/// Filter clauses (values are validated by `usnm_core::params`).
pub fn filter_clauses(filters: &Filters) -> Vec<String> {
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
    if !filters.langs.is_empty() {
        out.push(set("language", &filters.langs));
    }
    if filters.front_only {
        out.push("front_page:true".into());
    }
    out
}

fn full_query(node: &Node, filters: &Filters, extra: Vec<String>) -> Result<String, SearchError> {
    let mut parts = vec![query_string(node)?];
    parts.extend(filter_clauses(filters));
    parts.extend(extra);
    Ok(parts.join(" AND "))
}

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
    spec: &BucketSpec,
) -> Result<Value, SearchError> {
    Ok(json!({
        "query": full_query(node, filters, Vec::new())?,
        "max_hits": 0,
        "aggs": {
            "series": histogram(spec, 1),
            "places": {
                "terms": { "field": "place_id", "size": MAX_PLACES },
                "aggs": { "first": { "min": { "field": "day" } } }
            }
        }
    }))
}

pub fn cube_request(
    node: &Node,
    filters: &Filters,
    spec: &BucketSpec,
    shards: &[u8],
) -> Result<Value, SearchError> {
    let mut extra = Vec::new();
    if shards.len() < usize::from(usnm_core::cube::PLACE_SHARDS) {
        let list: Vec<String> = shards.iter().map(u8::to_string).collect();
        extra.push(format!("place_shard:IN [{}]", list.join(" ")));
    }
    Ok(json!({
        "query": full_query(node, filters, extra)?,
        "max_hits": 0,
        "aggs": {
            "places": {
                "terms": { "field": "place_id", "size": MAX_PLACES },
                "aggs": { "t": histogram(spec, 1) }
            }
        }
    }))
}

pub fn hits_request(
    node: &Node,
    filters: &Filters,
    page: &HitsQuery,
) -> Result<Value, SearchError> {
    let mut extra = Vec::new();
    if let Some(p) = &page.place_id {
        extra.push(format!("place_id:{p}"));
    }
    if let Some(l) = &page.lccn {
        extra.push(format!("lccn:{l}"));
    }
    Ok(json!({
        "query": full_query(node, filters, extra)?,
        "max_hits": page.limit,
        "start_offset": page.offset,
        // Oldest first, then title, edition and page (`sort_key`). Quickwit 0.9
        // can't sort on text fields, and a leading `-` means ascending (S-2).
        "sort_by": "-day,-sort_key",
        // A comma-separated string, not an array, in Quickwit 0.9 (S-2).
        "snippet_fields": "text"
    }))
}

// ------------------------------------------------------------ responses

#[derive(Debug, Deserialize)]
pub struct SearchResponse {
    pub num_hits: u64,
    #[serde(default)]
    pub hits: Vec<StoredHit>,
    #[serde(default)]
    pub snippets: Option<Vec<Value>>,
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
}

impl StoredHit {
    fn day(&self) -> Option<u32> {
        let ymd = self.date.get(..10)?;
        let date = chrono::NaiveDate::parse_from_str(ymd, "%Y-%m-%d").ok()?;
        Some(usnm_core::time::day_number(date))
    }
}

/// Reject responses that report partial failures, without echoing their content.
pub fn check_complete(resp: &SearchResponse) -> Result<(), SearchError> {
    if resp.errors.is_empty() {
        Ok(())
    } else {
        Err(SearchError::Backend(format!(
            "quickwit reported {} partial failure(s)",
            resp.errors.len()
        )))
    }
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

#[derive(Debug, Deserialize)]
struct MinValue {
    value: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PlaceBucket {
    key: String,
    doc_count: u64,
    first: Option<MinValue>,
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
    let places = agg::<Buckets<PlaceBucket>>(resp, "places")?
        .buckets
        .into_iter()
        .map(|b| PlaceSummary {
            first_day: b.first.and_then(|f| f.value).map_or(0, |v| v as u32),
            place_id: b.key,
            hits: b.doc_count,
        })
        .collect();
    Ok(Summary {
        total_hits: resp.num_hits,
        series,
        places,
    })
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

pub fn parse_hits(resp: SearchResponse) -> Result<HitsPage, SearchError> {
    let snippets = resp.snippets.unwrap_or_default();
    let mut hits = Vec::with_capacity(resp.hits.len());
    for (i, d) in resp.hits.into_iter().enumerate() {
        let day = d
            .day()
            .ok_or_else(|| SearchError::Backend("hit has an unparseable date".into()))?;
        hits.push(Hit {
            snippets: snippets
                .get(i)
                .and_then(|s| s.get("text"))
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(convert_snippet)
                        .collect()
                })
                .unwrap_or_default(),
            front_page: d.seq == 1,
            day,
            doc_id: d.doc_id,
            lccn: d.lccn,
            place_id: d.place_id,
            edition: d.edition,
            seq: d.seq,
        });
    }
    Ok(HitsPage {
        total: resp.num_hits,
        hits,
    })
}

/// Quickwit highlights with `<b>…</b>`. Rebuild the snippet so that the only
/// markup that can reach the browser is `<mark>`. Quickwit returns snippet
/// text HTML-escaped (S-2), so it is unescaped here and re-escaped once.
pub fn convert_snippet(s: &str) -> String {
    let mut segments: Vec<(bool, String)> = Vec::new();
    let mut rest = s;
    while let Some(start) = rest.find("<b>") {
        segments.push((false, unescape(&rest[..start])));
        let after = &rest[start + 3..];
        let end = after.find("</b>").unwrap_or(after.len());
        segments.push((true, unescape(&after[..end])));
        rest = after.get(end + 4..).unwrap_or("");
    }
    segments.push((false, unescape(rest)));
    let borrowed: Vec<(bool, &str)> = segments.iter().map(|(m, t)| (*m, t.as_str())).collect();
    mark_html(&borrowed)
}

fn unescape(s: &str) -> String {
    s.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&#x27;", "'")
        .replace("&amp;", "&")
}

#[async_trait]
impl SearchBackend for QuickwitBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            fuzzy: false,
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
        let resp = self
            .search(indexes, &summary_request(query, filters, spec)?)
            .await?;
        parse_summary(&resp, spec)
    }

    async fn cube(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        spec: &BucketSpec,
        shards: &[u8],
    ) -> Result<Vec<CubeCell>, SearchError> {
        let resp = self
            .search(indexes, &cube_request(query, filters, spec, shards)?)
            .await?;
        parse_cube(&resp, spec)
    }

    async fn hits(
        &self,
        indexes: &IndexSet,
        query: &Node,
        filters: &Filters,
        page: &HitsQuery,
    ) -> Result<HitsPage, SearchError> {
        let resp = self
            .search(indexes, &hits_request(query, filters, page)?)
            .await?;
        parse_hits(resp)
    }

    /// A read-only searcher reads the metastore manifest once at start, so it
    /// can't find indexes created later by name in a search. Looking an index
    /// up by id loads it from storage (and then it's polled like the rest), so
    /// each index is looked up before its version is served (S-2, 08 §8.4.1).
    async fn prepare(&self, indexes: &IndexSet) -> Result<(), SearchError> {
        for id in indexes.ids() {
            let url = format!("{}/api/v1/indexes/{id}", self.base_url);
            let resp = self.client.get(url).send().await.map_err(map_err)?;
            if !resp.status().is_success() {
                return Err(SearchError::Backend(format!(
                    "index `{id}` is not available: quickwit returned {}",
                    resp.status()
                )));
            }
        }
        Ok(())
    }

    async fn health(&self) -> Result<(), SearchError> {
        let url = format!("{}/health/readyz", self.base_url);
        let resp = self.client.get(url).send().await.map_err(map_err)?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(SearchError::Backend(format!(
                "quickwit readyz returned {}",
                resp.status()
            )))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use usnm_core::query::parse;
    use usnm_core::time::BucketUnit;

    fn d(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    fn filters() -> Filters {
        Filters {
            from: d("1896-06-01"),
            to: d("1896-12-31"),
            states: vec!["GA".into(), "SC".into()],
            lccns: vec![],
            langs: vec![],
            front_only: true,
        }
    }

    #[test]
    fn translates_ast_to_query_language() {
        let q = parse(r#""cross of gold" -bryan (silver OR free*) "gold silver"~3"#).unwrap();
        assert_eq!(
            query_string(&q).unwrap(),
            r#"(text:"cross of gold" AND text:"gold silver"~3 AND NOT text:bryan AND (text:free* OR text:silver))"#
        );
        assert!(matches!(
            query_string(&parse("gold~1").unwrap()),
            Err(SearchError::Unsupported(_))
        ));
    }

    #[test]
    fn builds_summary_and_sharded_cube_requests() {
        let q = parse("gold").unwrap();
        let spec = BucketSpec::new(BucketUnit::Week, d("1896-06-01"), d("1896-12-31"));
        let origin = spec.histogram_field().origin;
        let s = summary_request(&q, &filters(), &spec).unwrap();
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
        let c = cube_request(&q, &filters(), &spec, &[0, 5]).unwrap();
        assert!(c["query"]
            .as_str()
            .unwrap()
            .ends_with("AND place_shard:IN [0 5]"));
        let all = cube_request(&q, &filters(), &spec, &[0, 1, 2, 3, 4, 5, 6, 7]).unwrap();
        assert!(!all["query"].as_str().unwrap().contains("place_shard"));
    }

    #[test]
    fn hits_sort_by_day_then_sort_key() {
        let page = HitsQuery {
            place_id: Some("P00001".into()),
            lccn: None,
            offset: 50,
            limit: 25,
        };
        let r = hits_request(&parse("gold").unwrap(), &filters(), &page).unwrap();
        assert_eq!(r["sort_by"], "-day,-sort_key");
        assert_eq!(r["snippet_fields"], "text");
        assert_eq!(r["start_offset"], 50);
        assert!(r["query"]
            .as_str()
            .unwrap()
            .ends_with("AND place_id:P00001"));
    }

    #[test]
    fn decodes_hits_from_stored_fields_only() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 2,
            "hits": [
                {"doc_id": "sn99000001_1896-07-10_ed-1_seq-1", "date": "1896-07-10T00:00:00Z",
                 "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 1, "text": "…"},
                {"doc_id": "sn99000001_1896-07-10_ed-1_seq-3", "date": "1896-07-10T00:00:00Z",
                 "place_id": "P00001", "lccn": "sn99000001", "edition": 1, "seq": 3}
            ],
            "snippets": [{"text": ["a <b>gold</b> b"]}, {"text": []}]
        }))
        .unwrap();
        let page = parse_hits(resp).unwrap();
        assert_eq!(
            page.hits[0].day,
            usnm_core::time::day_number(d("1896-07-10"))
        );
        assert!(page.hits[0].front_page);
        assert!(!page.hits[1].front_page);
        assert_eq!(page.hits[0].snippets, vec!["a <mark>gold</mark> b"]);
    }

    #[test]
    fn rejects_partial_failures_without_echoing_them() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3,
            "errors": [{"split_id": "x", "error": "failed on query text:secret"}]
        }))
        .unwrap();
        let err = check_complete(&resp).unwrap_err().to_string();
        assert!(err.contains("1 partial failure"));
        assert!(!err.contains("secret"));
    }

    #[test]
    fn parses_aggregation_responses() {
        let spec = BucketSpec::new(BucketUnit::Year, d("1895-01-01"), d("1897-12-31"));
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 7,
            "aggregations": {
                "series": { "buckets": [ {"key": 1895.0, "doc_count": 2}, {"key": 1896.0, "doc_count": 5} ] },
                "places": { "buckets": [
                    {"key": "P00001", "doc_count": 4, "first": {"value": 71000.0},
                     "t": {"buckets": [{"key": 1895.0, "doc_count": 1}, {"key": 1896.0, "doc_count": 3}]}},
                    {"key": "P00002", "doc_count": 3, "first": {"value": 71100.0},
                     "t": {"buckets": [{"key": 1896.0, "doc_count": 3}]}}
                ]}
            }
        }))
        .unwrap();
        let s = parse_summary(&resp, &spec).unwrap();
        assert_eq!(s.series, vec![2, 5, 0]);
        assert_eq!(s.places[0].first_day, 71000);
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
    fn snippets_only_ever_contain_mark_tags() {
        assert_eq!(
            convert_snippet("a &lt;script&gt; <b>gold</b> &amp; x"),
            "a &lt;script&gt; <mark>gold</mark> &amp; x"
        );
        assert_eq!(
            convert_snippet("<b>x<img></b>"),
            "<mark>x&lt;img&gt;</mark>"
        );
    }
}
