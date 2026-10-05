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

use crate::snippet::text_snippets;
use crate::{
    ja_snippets, rank, Capabilities, CubeCell, Hit, HitSort, HitsPage, HitsQuery, IndexSet,
    KeyCount, PlaceSummary, SearchBackend, SearchError, Summary,
};

/// Upper bound on places in one terms aggregation.
const MAX_PLACES: u32 = 5_000;
/// Upper bound on titles: every title (4,693 on loc.gov in October 2026)
/// fits, so the newspaper count is exact (#121).
pub const MAX_PAPERS: u32 = 10_000;
/// Upper bound on title languages (about 50 in the catalog).
const MAX_LANGUAGES: u32 = 200;

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
        // The query string sent, whose words a failure's cause leaves out.
        let sent = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned();
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
            // query text must not reach logs (09 §9.4.2). Its cause, classified
            // from the body, is safe to log.
            // 408 is Quickwit cancelling a search at its own
            // `searcher.request_timeout_secs`: a timeout, not a refusal.
            if status == reqwest::StatusCode::REQUEST_TIMEOUT {
                return Err(SearchError::Timeout);
            }
            // A failure reading it is the real cause, with its own chain.
            let body = resp.text().await.map_err(map_err)?;
            // With failed splits not allowed (the default), Quickwit answers a
            // split's failure this way, naming the split in the body.
            let msg = format!(
                "quickwit returned {status} ({}){}",
                cause(&body, &sent),
                splits_named(&body, &sent)
            );
            return Err(if status.is_client_error() {
                SearchError::Rejected(msg)
            } else {
                SearchError::Backend(msg)
            });
        }
        let parsed: SearchResponse = resp.json().await.map_err(map_err)?;
        check_complete(&parsed, &sent)?;
        Ok(parsed)
    }
}

fn map_err(e: reqwest::Error) -> SearchError {
    if e.is_timeout() {
        return SearchError::Timeout;
    }
    // The whole chain (e.g. "connection refused" or "connection closed before
    // message completed"), which says whether the sidecar went away. It holds
    // the URL (index names) and transport errors, never the query, which is
    // in the request body.
    let mut msg = e.to_string();
    let mut source = std::error::Error::source(&e);
    while let Some(s) = source {
        msg.push_str(": ");
        msg.push_str(&s.to_string());
        source = s.source();
    }
    let kind = if e.is_connect() {
        "connect"
    } else if e.is_decode() {
        "decode"
    } else if e.is_body() {
        "body"
    } else {
        "request"
    };
    SearchError::Backend(format!("quickwit {kind} error: {msg}"))
}

/// What a Quickwit error message is about, by keyword, so its cause can be
/// logged without its text (which may echo the query, 09 §9.4.2). The
/// message is compared word by word, leaving out the words of `query` (the
/// Quickwit query string sent), so an echoed query can't decide the
/// category: a search for "memory" that fails on storage is `storage`. A
/// failure whose only telling word is also in the query is `other`.
pub fn cause(message: &str, query: &str) -> &'static str {
    let echoed: std::collections::HashSet<String> = words(query).collect();
    let w: Vec<String> = words(message).filter(|w| !echoed.contains(w)).collect();
    let any = |of: &[&str]| w.iter().any(|x| of.contains(&x.as_str()));
    let pair = |a: &str, b: &str| w.windows(2).any(|p| p[0] == a && p[1] == b);
    if any(&["memory"]) {
        "memory_limit"
    } else if any(&["bucket", "buckets"]) {
        "bucket_limit"
    } else if any(&["timeout", "deadline", "elapsed"]) || pair("timed", "out") {
        "timeout"
    } else if pair("too", "many")
        || pair("rate", "limit")
        || any(&[
            "concurrent",
            "concurrency",
            "throttled",
            "throttling",
            "503",
            "429",
        ])
    {
        "overloaded"
    } else if any(&[
        "storage",
        "azure",
        "blob",
        "io",
        "connection",
        "http",
        "network",
    ]) {
        "storage"
    } else if any(&[
        "split",
        "splits",
        "footer",
        "hotcache",
        "corrupt",
        "corrupted",
    ]) {
        "split"
    } else if w.is_empty() {
        "empty"
    } else {
        "other"
    }
}

/// `; splits A, B` for up to 5 split ids named in a Quickwit message, or
/// nothing. Split ids are ULIDs: 26 characters of Crockford base32 in upper
/// case, starting 0-7, with digits and letters. A word of `query` (the
/// query string sent, which the message may echo) is never one, in any
/// case, so no search text is logged.
fn splits_named(message: &str, query: &str) -> String {
    let echoed: std::collections::HashSet<String> = words(query).collect();
    let crockford = |c: char| c.is_ascii_digit() || (c.is_ascii_uppercase() && !"ILOU".contains(c));
    let mut ids: Vec<&str> = Vec::new();
    for w in message.split(|c: char| !c.is_ascii_alphanumeric()) {
        if w.len() == 26
            && w.starts_with(|c: char| ('0'..='7').contains(&c))
            && w.chars().all(crockford)
            && !echoed.contains(&w.to_ascii_lowercase())
            && w.chars().any(|c| c.is_ascii_digit())
            && w.chars().any(|c| c.is_ascii_uppercase())
            && !ids.contains(&w)
        {
            ids.push(w);
        }
    }
    if ids.is_empty() {
        String::new()
    } else {
        format!("; splits {}", ids[..ids.len().min(5)].join(", "))
    }
}

/// Lowercase ASCII alphanumeric words.
fn words(text: &str) -> impl Iterator<Item = String> + '_ {
    text.split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_ascii_lowercase)
}

// ------------------------------------------------------------ translation

/// Quickwit query-language string for the AST. Terms are folded alphanumerics
/// from our own parser, so they never need escaping. With `grams` (the
/// indexes have `text_cg`, 05 §5.5.3), an exact phrase holding a common word
/// searches `text_cg`, which never reads the common word's positions. Each
/// of its other words is also required in `text`: that changes no match (a
/// page with the phrase has the words) and gives the snippets on `text`
/// their highlights. A word that folds to several (`ﷺ`) is left out of those:
/// unquoted it would be several terms, and the pairs already require it.
pub fn query_string(node: &Node, grams: bool) -> Result<String, SearchError> {
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
        Node::Phrase { terms, .. } => match grams
            .then(|| usnm_core::common_grams::query_terms(terms))
            .flatten()
        {
            Some(pairs) => {
                let mut parts = vec![format!("text_cg:\"{}\"", pairs.join(" "))];
                let mut seen = std::collections::BTreeSet::new();
                parts.extend(
                    terms
                        .iter()
                        .filter(|t| !usnm_core::common_grams::is_common(t))
                        .filter(|t| !t.contains(char::is_whitespace) && seen.insert(*t))
                        .map(|w| format!("text:{w}")),
                );
                format!("({})", parts.join(" AND "))
            }
            None => format!("text:\"{}\"", terms.join(" ")),
        },
        Node::And(c) => format!("({})", join(c, " AND ", grams)?),
        Node::Or(c) => format!("({})", join(c, " OR ", grams)?),
        Node::Not(n) => format!("NOT {}", query_string(n, grams)?),
    })
}

fn join(children: &[Node], sep: &str, grams: bool) -> Result<String, SearchError> {
    Ok(children
        .iter()
        .map(|c| query_string(c, grams))
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

fn full_query(
    node: &Node,
    filters: &Filters,
    indexes: &IndexSet,
    extra: Vec<String>,
) -> Result<String, SearchError> {
    let mut parts = vec![query_string(node, indexes.common_grams())?];
    parts.extend(filter_clauses(filters));
    parts.extend(hidden_clauses(indexes));
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
            "languages": { "terms": { "field": "language", "size": MAX_LANGUAGES } }
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
    Ok(json!({
        "query": full_query(node, filters, indexes, extra)?,
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
    indexes: &IndexSet,
    page: &HitsQuery,
) -> Result<Value, SearchError> {
    let mut extra = Vec::new();
    if let Some(p) = &page.place_id {
        extra.push(format!("place_id:{p}"));
    }
    if let Some(l) = &page.lccn {
        extra.push(format!("lccn:{l}"));
    }
    let mut req = json!({
        "query": full_query(node, filters, indexes, extra)?,
        "max_hits": page.limit,
        "start_offset": page.offset,
        "sort_by": sort_by(page.sort)
    });
    // On how many days the pages appeared (#127), when asked: the first page
    // of a visitor's list, not the aggregate's first and last page lookups.
    if page.days {
        req["aggs"] = json!({ "days": { "cardinality": { "field": "day" } } });
    }
    Ok(req)
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

// ------------------------------------------------------------ responses

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
    /// The page's stored text, for its snippets (#126).
    #[serde(default)]
    pub text: Option<String>,
    /// A Japanese page's printed text (pages-ja-index.yaml), for its snippets.
    #[serde(default)]
    pub printed: Option<String>,
    #[serde(default)]
    pub ocr_source: Option<String>,
    #[serde(default)]
    pub ocr_engine: Option<String>,
}

impl StoredHit {
    fn day(&self) -> Option<u32> {
        let ymd = self.date.get(..10)?;
        let date = chrono::NaiveDate::parse_from_str(ymd, "%Y-%m-%d").ok()?;
        Some(usnm_core::time::day_number(date))
    }
}

/// Reject responses that report partial failures, without echoing their
/// content: the error names how many, what they were about ([`cause`]) and
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
        *causes.entry(cause(&text, query)).or_default() += 1;
        texts.push(text);
    }
    let causes: Vec<String> = causes.iter().map(|(c, n)| format!("{c}×{n}")).collect();
    let msg = format!(
        "quickwit reported {} partial failure(s): {}{}",
        resp.errors.len(),
        causes.join(", "),
        splits_named(&texts.join(" "), query)
    );
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
    let places = agg::<Buckets<PlaceBucket>>(resp, "places")?
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
        days: distinct(resp)?.unwrap_or(0),
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

pub fn parse_hits(resp: SearchResponse, query: &Node) -> Result<HitsPage, SearchError> {
    let days = distinct(&resp)?;
    let mut hits = Vec::with_capacity(resp.hits.len());
    for d in resp.hits {
        let day = d
            .day()
            .ok_or_else(|| SearchError::Backend("hit has an unparseable date".into()))?;
        // Built here from the stored text, as the memory backend does (#126).
        // A Japanese page's come from its printed text: its indexed text is
        // folded tokens with spaces between them.
        let snippets = match (&d.printed, &d.text) {
            (Some(printed), _) => ja_snippets(printed, query),
            (None, Some(text)) => text_snippets(text, query),
            (None, None) => Vec::new(),
        };
        hits.push(Hit {
            snippets,
            ocr_source: d.ocr_source,
            ocr_engine: d.ocr_engine,
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
        days,
        hits,
    })
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
            .search(indexes, &summary_request(query, filters, indexes, spec)?)
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
            .search(
                indexes,
                &cube_request(query, filters, indexes, spec, shards)?,
            )
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
            .search(indexes, &hits_request(query, filters, indexes, page)?)
            .await?;
        parse_hits(resp, query)
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

    fn none() -> IndexSet {
        IndexSet::new(vec!["i".into()])
    }

    /// Answers every request with `status` and an empty JSON object.
    async fn stub(status: u16) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut sock, _)) = listener.accept().await {
                let mut buf = vec![0; 16 * 1024];
                let _ = sock.read(&mut buf).await;
                let reply = format!(
                    "HTTP/1.1 {status} Stub\r\ncontent-type: application/json\r\n\
                     content-length: 2\r\nconnection: close\r\n\r\n{{}}"
                );
                let _ = sock.write_all(reply.as_bytes()).await;
            }
        });
        url
    }

    #[tokio::test]
    async fn quickwit_cancelling_a_search_at_its_timeout_is_a_timeout() {
        let qw = QuickwitBackend::new(&stub(408).await, Duration::from_secs(5)).unwrap();
        let err = qw
            .search(&IndexSet::new(vec!["i".into()]), &serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(matches!(err, SearchError::Timeout), "{err}");
    }

    #[tokio::test]
    async fn a_refused_request_is_rejected_and_a_server_error_is_not() {
        for (status, rejected) in [(400, true), (422, true), (500, false), (503, false)] {
            let qw = QuickwitBackend::new(&stub(status).await, Duration::from_secs(5)).unwrap();
            let err = qw
                .search(&IndexSet::new(vec!["i".into()]), &serde_json::json!({}))
                .await
                .unwrap_err();
            assert_eq!(
                matches!(err, SearchError::Rejected(_)),
                rejected,
                "{status}: {err}"
            );
        }
    }

    #[test]
    fn translates_ast_to_query_language() {
        let q = parse(r#""cross of gold" -bryan (silver OR free*) "gold silver"~3"#).unwrap();
        assert_eq!(
            query_string(&q, false).unwrap(),
            r#"(text:"cross of gold" AND text:"gold silver"~3 AND NOT text:bryan AND (text:free* OR text:silver))"#
        );
        assert!(matches!(
            query_string(&parse("gold~1").unwrap(), false),
            Err(SearchError::Unsupported(_))
        ));
    }

    #[test]
    fn phrases_with_common_words_search_the_pairs_field() {
        let q = parse(r#""cross of gold" -bryan "gold silver"~3 "yellow fever""#).unwrap();
        assert_eq!(
            query_string(&q, true).unwrap(),
            "((text_cg:\"cross of_gold gold\" AND text:cross AND text:gold) \
             AND text:\"gold silver\"~3 AND text:\"yellow fever\" AND NOT text:bryan)"
        );
        // A repeated word is required once.
        assert_eq!(
            query_string(&parse(r#""the gold of the cross""#).unwrap(), true).unwrap(),
            "(text_cg:\"the_gold gold of_the the_cross cross\" AND text:gold AND text:cross)"
        );
        // A word that folds to several is only required through the pairs.
        let q = parse("\"of \u{fdfa} gold\"").unwrap();
        let s = query_string(&q, true).unwrap();
        assert!(s.starts_with("(text_cg:\"of_"), "{s}");
        assert!(s.ends_with(" AND text:gold)"), "{s}");
        assert_eq!(s.matches(" AND ").count(), 1, "{s}");
        // Without the field, or for a phrase ending in a common word: `text`.
        assert_eq!(
            query_string(&parse(r#""cross of gold""#).unwrap(), false).unwrap(),
            r#"text:"cross of gold""#
        );
        assert_eq!(
            query_string(&parse(r#""remember the""#).unwrap(), true).unwrap(),
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
        let page = parse_hits(resp, &usnm_core::query::parse("gold").unwrap()).unwrap();
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
        assert_eq!(parse_hits(first, &q).unwrap().days, Some(2));
    }

    #[test]
    fn rejects_partial_failures_without_echoing_them() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3,
            "errors": [{"split_id": "x", "error": "failed on query text:secret"}]
        }))
        .unwrap();
        let err = check_complete(&resp, "text:secret")
            .unwrap_err()
            .to_string();
        assert!(err.contains("1 partial failure"));
        assert!(!err.contains("secret"));
    }

    #[test]
    fn partial_failures_say_what_they_were_about() {
        let resp: SearchResponse = serde_json::from_value(json!({
            "num_hits": 3,
            "errors": [
                "split 01JA9XQ3V5Z8M2K7T4R6N1P0WB: aggregation memory limit exceeded on query text:secret",
                "split 01JA9XQ3V5Z8M2K7T4R6N1P0WC: Aggregation Memory Limit exceeded",
                "request timed out for text:SECRETSECRETSECRETSECRET12",
                "storage error: Azure blob read failed for text:secret"
            ]
        }))
        .unwrap();
        let err = check_complete(&resp, "text:secret OR text:SECRETSECRETSECRETSECRET12")
            .unwrap_err()
            .to_string();
        assert_eq!(
            err,
            "search backend error: quickwit reported 4 partial failure(s): \
             memory_limit×2, storage×1, timeout×1; \
             splits 01JA9XQ3V5Z8M2K7T4R6N1P0WB, 01JA9XQ3V5Z8M2K7T4R6N1P0WC"
        );
        assert!(!err.to_lowercase().contains("secret"), "{err}");
    }

    #[test]
    fn split_ids_are_named_only_when_they_look_like_ulids() {
        assert_eq!(
            splits_named(
                "failed splits: [01JA9XQ3V5Z8M2K7T4R6N1P0WB, 01JA9XQ3V5Z8M2K7T4R6N1P0WB]",
                ""
            ),
            "; splits 01JA9XQ3V5Z8M2K7T4R6N1P0WB"
        );
        // Too short, lower case, a letter Crockford leaves out, or not 0-7 first.
        assert_eq!(
            splits_named(
                "01JA9X 01ja9xq3v5z8m2k7t4r6n1p0wb 01JA9XQ3V5Z8M2K7T4R6N1P0WI 91JA9XQ3V5Z8M2K7T4R6N1P0WB",
                ""
            ),
            ""
        );
        // A word of the query is never named, whatever its case.
        assert_eq!(
            splits_named(
                "no match for 01JA9XQ3V5Z8M2K7T4R6N1P0WB",
                "text:01ja9xq3v5z8m2k7t4r6n1p0wb"
            ),
            ""
        );
    }

    #[test]
    fn causes_are_classified_by_keyword() {
        assert_eq!(
            cause("Aggregation memory limit of 768MB exceeded", ""),
            "memory_limit"
        );
        assert_eq!(
            cause("too many buckets: 210000 > 200000", ""),
            "bucket_limit"
        );
        assert_eq!(cause("deadline exceeded", ""), "timeout");
        assert_eq!(cause("Too many requests", ""), "overloaded");
        assert_eq!(cause("failed to fetch footer", ""), "split");
        assert_eq!(cause("Io error: broken pipe", ""), "storage");
        assert_eq!(cause("", ""), "empty");
        assert_eq!(cause("something else", ""), "other");
        assert_eq!(cause("timed out after 30s", ""), "timeout");
    }

    #[test]
    fn an_echoed_query_does_not_decide_the_cause() {
        // A search for "memory" that failed on storage is a storage failure.
        assert_eq!(
            cause(
                "Azure blob read failed for query text:memory",
                "text:memory"
            ),
            "storage"
        );
        assert_eq!(
            cause("failed for text:\"bucket http\"", "text:\"bucket http\""),
            "other"
        );
        // Words of the query that aren't echoed still count.
        assert_eq!(
            cause("aggregation memory limit exceeded", "text:gold"),
            "memory_limit"
        );
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
