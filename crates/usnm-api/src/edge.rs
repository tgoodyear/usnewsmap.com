//! What Cloudflare may keep of a search (06 §6.5).
//!
//! The home page's example searches stay at the edge for two weeks. Their
//! URLs carry `v`, so a new index version is a new URL, and `g`, the
//! generation of [`generation`], so a release or setting that changes what
//! the same URL returns is one too: nothing is purged. Every other search
//! is `no-store` at the edge and keeps its browser caching.
//!
//! Cloudflare consumes `Cloudflare-CDN-Cache-Control`, so browsers never see
//! it. An edge hit doesn't reach the API, so the web app reports it from
//! [`EXAMPLE`], for the search log (06 §6.8).

use std::collections::HashMap;
use std::sync::LazyLock;

use axum::http::{HeaderMap, HeaderName, HeaderValue};

use crate::prewarm::examples;
use crate::refdata::RefData;
use crate::routes::RESPONSE_FORMAT;

/// The query parameter holding the client's [`generation`].
pub(crate) const GENERATION_KEY: &str = "g";

/// Names the example search a response answers.
pub(crate) const EXAMPLE: &str = "x-usnm-example";

const EDGE_CONTROL: &str = "cloudflare-cdn-cache-control";

/// Fourteen days.
const KEEP: &str = "public, max-age=1209600";
const PASS: &str = "no-store";

/// What changes a response under an unchanged `v`: [`RESPONSE_FORMAT`], and
/// the settings that change searches without changing the version.
pub(crate) fn generation(rd: &RefData) -> String {
    let mut generation = format!("f{RESPONSE_FORMAT}");
    if rd.american_stories_switched_off() {
        generation.push_str("-noas");
    }
    if rd.congress_index_set().is_none() {
        generation.push_str("-nocg");
    }
    generation
}

/// The parameters of a query string in a fixed order, without `v` and `g`.
fn parameters(query: &str) -> Vec<(String, String)> {
    let mut pairs: Vec<_> = form_urlencoded::parse(query.as_bytes())
        .into_owned()
        .filter(|(key, _)| key != "v" && key != GENERATION_KEY)
        .collect();
    pairs.sort();
    pairs
}

static EXAMPLE_IDS: LazyLock<HashMap<Vec<(String, String)>, &'static str>> = LazyLock::new(|| {
    examples()
        .iter()
        .map(|example| (parameters(&example.aggregate), example.id.as_str()))
        .collect()
});

/// Set the edge's caching for a 200 from `/v1/aggregate`: only an example,
/// pinned to the served version and generation, may be kept.
pub(crate) fn mark(
    headers: &mut HeaderMap,
    query: &str,
    pinned: bool,
    requested: Option<&str>,
    serving: &str,
) {
    let example = EXAMPLE_IDS.get(&parameters(query)).copied();
    let id = example.and_then(|id| HeaderValue::from_str(id).ok());
    let id = id.filter(|_| pinned && requested == Some(serving));
    let control = if id.is_some() { KEEP } else { PASS };
    headers.insert(
        HeaderName::from_static(EDGE_CONTROL),
        HeaderValue::from_static(control),
    );
    if let Some(id) = id {
        headers.insert(HeaderName::from_static(EXAMPLE), id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(query: &str, pinned: bool, requested: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        mark(&mut headers, query, pinned, requested, "f8");
        headers
    }

    const WHIGS: &str = "q=Whig+OR+Whigs&from=1834-01-01&to=1856-12-31&bucket=month";

    #[test]
    fn a_pinned_example_is_kept_whatever_the_order_of_its_parameters() {
        let query = "bucket=month&v=x&to=1856-12-31&q=Whig%20OR%20Whigs&g=f8&from=1834-01-01";
        let headers = marked(query, true, Some("f8"));
        assert_eq!(headers[EDGE_CONTROL], "public, max-age=1209600");
        assert_eq!(headers[EXAMPLE], "whigs");
    }

    #[test]
    fn everything_else_passes() {
        for (query, pinned, requested) in [
            (format!("{WHIGS}&v=x"), true, None),
            (format!("{WHIGS}&v=x&g=f7"), true, Some("f7")),
            (WHIGS.to_owned(), false, Some("f8")),
            (format!("{WHIGS}&state=GA&v=x&g=f8"), true, Some("f8")),
            ("q=gold&v=x&g=f8".to_owned(), true, Some("f8")),
        ] {
            let headers = marked(&query, pinned, requested);
            assert_eq!(headers[EDGE_CONTROL], "no-store", "{query}");
            assert!(!headers.contains_key(EXAMPLE), "{query}");
        }
    }

    #[test]
    fn every_example_has_a_header_value() {
        for example in examples() {
            assert!(HeaderValue::from_str(&example.id).is_ok(), "{}", example.id);
        }
        assert_eq!(EXAMPLE_IDS.len(), examples().len());
    }
}
