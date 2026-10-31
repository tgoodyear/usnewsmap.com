//! What Cloudflare may keep of a search (06 §6.5).
//!
//! Searches stay at the edge for two weeks. Their URLs carry `v`, so a new
//! index version is a new URL, and `g`, the generation of [`generation`], so
//! a release or setting that changes what the same URL returns is one too:
//! nothing is purged. A request without both is `no-store` at the edge and
//! keeps its browser caching. One with another `g` is `no-store` for browsers
//! too: during a rollout, an old replica can be asked for the new generation.
//!
//! Cloudflare consumes `Cloudflare-CDN-Cache-Control`, so browsers never see
//! it. An edge hit doesn't reach the API, so the web app reports it, for the
//! search log (`POST /v1/search-report`, 06 §6.8).

use axum::http::{header, HeaderMap, HeaderName, HeaderValue};

use crate::refdata::RefData;
use crate::routes::RESPONSE_FORMAT;

/// The query parameter holding the client's [`generation`].
pub(crate) const GENERATION_KEY: &str = "g";

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

/// Set the edge's caching for a 200 from `/v1/aggregate`: only a response
/// pinned to the served version and generation may be kept.
pub(crate) fn mark(headers: &mut HeaderMap, pinned: bool, requested: Option<&str>, serving: &str) {
    let keep = pinned && requested == Some(serving);
    headers.insert(
        HeaderName::from_static(EDGE_CONTROL),
        HeaderValue::from_static(if keep { KEEP } else { PASS }),
    );
    if requested.is_some_and(|g| g != serving) {
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static(PASS));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn marked(pinned: bool, requested: Option<&str>) -> HeaderMap {
        let mut headers = HeaderMap::new();
        mark(&mut headers, pinned, requested, "f8");
        headers
    }

    #[test]
    fn only_the_served_version_and_generation_are_kept() {
        assert_eq!(
            marked(true, Some("f8"))[EDGE_CONTROL],
            "public, max-age=1209600"
        );
        for (pinned, requested) in [(true, None), (true, Some("f7")), (false, Some("f8"))] {
            assert_eq!(marked(pinned, requested)[EDGE_CONTROL], "no-store");
        }
    }

    #[test]
    fn another_generation_is_not_kept_by_browsers_either() {
        assert_eq!(marked(true, Some("f7"))[header::CACHE_CONTROL], "no-store");
        assert!(!marked(true, Some("f8")).contains_key(header::CACHE_CONTROL));
        assert!(!marked(true, None).contains_key(header::CACHE_CONTROL));
    }
}
