use std::sync::Arc;
use std::time::Duration;

use axum::extract::{OriginalUri, State};
use axum::http::{header, HeaderValue, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde_json::json;
use usnm_core::params::RawParams;
use usnm_core::query;

use super::{cached, Ctx, Job};
use crate::error::ApiError;
use crate::{version, AppState};

/// Anyone can call it, so every failure gets the same body. The reason,
/// which can hold the searcher's address, is logged instead (a backend
/// failure), or was logged by the reload that found it (a missing index).
pub async fn readyz(State(state): State<Arc<AppState>>) -> Response {
    let not_ready = || (StatusCode::SERVICE_UNAVAILABLE, "not ready").into_response();
    // Not ready until the first version's caches are warm (or the cap passes),
    // so the first visitors after a start don't wait on cold searches.
    if state.warming.load(std::sync::atomic::Ordering::Relaxed) {
        return not_ready();
    }
    // A publish this engine can't serve: the replica serves the version it
    // has, but a restart would load none.
    if state.config.ready_requires_published_indexes && state.missing_index.load().is_some() {
        return not_ready();
    }
    let backend = state.snapshot.load().backend.clone();
    match tokio::time::timeout(Duration::from_secs(2), backend.health()).await {
        Ok(Ok(())) => (StatusCode::OK, "ready").into_response(),
        Ok(Err(e)) => {
            tracing::warn!(error = %e, "not ready: the search backend's health check failed");
            not_ready()
        }
        Err(_) => {
            tracing::warn!("not ready: the search backend's health check timed out");
            not_ready()
        }
    }
}

/// Unversioned by design: it is how clients learn the current version.
pub async fn meta(State(state): State<Arc<AppState>>) -> Response {
    let snap = state.snapshot.load();
    let rd = &snap.refdata;
    let c = &rd.current;
    let body = json!({
        "index_version": c.index_version,
        "indexes": c.indexes,
        "bounds": c.bounds,
        "published_at": c.published_at,
        "synthetic": c.synthetic,
        "places": rd.places.len(),
        "titles": rd.titles.len(),
        "pages": rd.pages,
        // The Japanese pages we OCR (#139): searched by queries with Japanese words.
        "ja": c.ja,
        // Whether searches cover American Stories' text (05 §5.5.4): the
        // version has it and `USNM_AMERICAN_STORIES_SEARCH` leaves it on,
        // or it shares one field with LoC's (05 §5.5.7).
        "american_stories": rd.searches_american_stories(),
        // A searched field per text (1) or one for both (2), 05 §5.5.7.
        "text_layout": rd.text_layout().version(),
        // The Congress pages (#229), searched with `source=congress`: null
        // unless the version has a Congress set this API can search and
        // `USNM_CONGRESS_SEARCH` leaves it on. Every page sits at one
        // reserved place, Washington, DC, kept apart from DC's newspapers.
        "congress": rd.congress_index_set().and(c.congress.as_ref()).map(|cg| json!({
            "series": cg.series,
            "place": {
                "id": crate::refdata::CONGRESS_PLACE,
                "name": crate::refdata::CONGRESS_PLACE_NAME,
                "lat": 38.8899,
                "lon": -77.0091,
            },
        })),
        // What `lang=` matches (06 §6.3.1): each page's own languages on
        // a version with them (05 §5.5.8), its title's otherwise.
        "language_filter": if rd.uses_page_language() { "page" } else { "title" },
        "languages": filter_languages(rd),
        // Whether `/v1/hits` lists a title's pages without `q` (#274,
        // `USNM_BROWSE_PAGES`): the site offers "Browse pages" only then.
        "browse_pages": state.config.browse_pages,
        "capabilities": snap.backend.capabilities(),
        "limits": {
            "max_query_chars": query::MAX_QUERY_CHARS,
            "max_terms": query::MAX_TERMS,
            "max_or_branches": query::MAX_OR_BRANCHES,
            "max_slop": query::MAX_SLOP,
            "max_fuzzy": query::MAX_FUZZY,
            "min_prefix_chars": query::MIN_PREFIX_CHARS,
            "max_at_least_items": query::MAX_AT_LEAST_ITEMS,
            "max_ocr_variants": usnm_core::ocr_variants::MAX_OCR_VARIANTS
        }
    });
    let mut resp = Json(body).into_response();
    resp.headers_mut().insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

/// The schema of [`meta`]'s body, which it builds as JSON, for
/// `/v1/openapi.json`. `tests/api/openapi.rs` checks real bodies against it,
/// every field required and no others allowed, so a field added to one and
/// not the other fails.
pub(crate) fn meta_schema(gen: &mut schemars::SchemaGenerator) -> serde_json::Value {
    let count = json!({ "type": "integer", "minimum": 0 });
    let limits: serde_json::Map<String, serde_json::Value> = [
        "max_query_chars",
        "max_terms",
        "max_or_branches",
        "max_slop",
        "max_fuzzy",
        "min_prefix_chars",
        "max_at_least_items",
        "max_ocr_variants",
    ]
    .into_iter()
    .map(|k| (k.to_owned(), count.clone()))
    .collect();
    let object = crate::openapi::closed;
    let mut body = object(json!({
        "index_version": { "type": "string", "description": "The version being served: what `v` takes." },
        "indexes": { "type": "array", "items": { "type": "string" } },
        "bounds": gen.subschema_for::<crate::refdata::Bounds>(),
        "published_at": { "type": "string" },
        "synthetic": { "type": "boolean", "description": "The corpus is the test fixture, not real pages." },
        "places": count,
        "titles": count,
        "pages": count,
        "ja": {
            "description": "The Japanese pages we OCR, searched by queries with Japanese words; `null` when the version has none.",
            "anyOf": [gen.subschema_for::<crate::refdata::JaIndexes>(), { "type": "null" }],
        },
        "american_stories": { "type": "boolean", "description": "Searches cover American Stories' text as well as the Library of Congress's OCR." },
        "text_layout": { "type": "integer", "enum": [1, 2], "description": "A searched field per text (1) or one for both (2)." },
        "congress": {
            "description": "The Congress pages, searched with `source=congress` or `all`; `null` unless the version has them and this API searches them.",
            "anyOf": [
                object(json!({
                    "series": { "type": "array", "items": gen.subschema_for::<crate::refdata::CongressSeries>() },
                    "place": object(json!({
                        "id": { "type": "string" },
                        "name": { "type": "string" },
                        "lat": { "type": "number" },
                        "lon": { "type": "number" },
                    })),
                })),
                { "type": "null" },
            ],
        },
        "language_filter": { "type": "string", "enum": ["page", "title"], "description": "What `lang` matches: each page's own languages, or its newspaper's." },
        "languages": {
            "type": "array",
            "description": "The codes `lang` takes, most pages first.",
            "items": object(json!({
                "code": { "type": "string" },
                "name": { "type": "string" },
                "titles": count,
                "pages": { "type": ["integer", "null"], "minimum": 0 },
            })),
        },
        "browse_pages": { "type": "boolean", "description": "`/v1/hits` lists one newspaper's pages without `q`." },
        "capabilities": gen.subschema_for::<usnm_search::Capabilities>(),
        "limits": object(limits.into()),
    }));
    body["description"] = "The version being served and what it holds.".into();
    body
}

/// The schema of [`places`]' body, a GeoJSON feature collection it builds as
/// JSON, for `/v1/openapi.json`; checked like [`meta_schema`].
pub(crate) fn places_schema() -> serde_json::Value {
    let count = json!({ "type": "integer", "minimum": 0 });
    let object = crate::openapi::closed;
    let feature = object(json!({
        "type": { "const": "Feature" },
        "id": { "type": "string", "description": "The place id `place` takes, such as `P00001`." },
        "geometry": object(json!({
            "type": { "const": "Point" },
            "coordinates": {
                "type": "array",
                "description": "Longitude, latitude.",
                "items": { "type": "number" },
                "minItems": 2,
                "maxItems": 2,
            },
        })),
        "properties": object(json!({
            "name": { "type": "string" },
            "state": { "type": "string" },
            "precision": { "type": "string", "description": "How precisely the place is located: `city`, `county` or `state`." },
            "titles": count,
            "languages": { "type": "array", "items": { "type": "string" }, "description": "Catalog codes of its newspapers' languages." },
            "language_titles": {
                "type": "object",
                "description": "Its newspapers listing each language; one in several counts in each.",
                "additionalProperties": count,
            },
        })),
    }));
    object(json!({
        "type": { "const": "FeatureCollection" },
        "index_version": { "type": "string" },
        "features": { "type": "array", "items": feature },
    }))
}

/// The choices for the site's language filter: each catalog language the
/// `lang` parameter accepts, with its titles and (when the snapshot records
/// them) pages, most pages first. A title in several languages counts in each,
/// as a search filtered to any of them finds its pages. On a version with
/// page languages (05 §5.5.8) the pages are those written in the language,
/// a page in two languages counting in each. Languages without a
/// three-letter code, and titles with none recorded, can't be filtered on, so
/// they are left out (06 §6.3.2).
fn filter_languages(rd: &crate::refdata::RefData) -> Vec<serde_json::Value> {
    let by_page = rd.page_language_pages();
    let mut rows: Vec<(String, String, usize, Option<u64>)> =
        crate::status::assemble::by_language(rd)
            .rows
            .into_iter()
            .filter_map(|r| {
                let code = r.code?;
                usnm_core::page_language::is_code(&code).then(|| {
                    let pages = match &by_page {
                        Some(p) => Some(p.get(&code).copied().unwrap_or(0)),
                        None => r.pages,
                    };
                    (code, r.name, r.titles, pages)
                })
            })
            .collect();
    if by_page.is_some() {
        rows.sort_by(|a, b| {
            b.3.cmp(&a.3)
                .then_with(|| b.2.cmp(&a.2))
                .then_with(|| a.1.cmp(&b.1))
        });
    }
    rows.into_iter()
        .map(|(code, name, titles, pages)| {
            json!({"code": code, "name": name, "titles": titles, "pages": pages})
        })
        .collect()
}

/// All places as GeoJSON (06 §6.3.2), versioned like the search endpoints.
pub async fn places(
    State(state): State<Arc<AppState>>,
    OriginalUri(uri): OriginalUri,
) -> Result<Response, ApiError> {
    let ctx = Ctx::serving(&state);
    places_in(&state, ctx, &uri).await
}

pub(crate) async fn places_in(
    state: &Arc<AppState>,
    ctx: Ctx,
    uri: &Uri,
) -> Result<Response, ApiError> {
    let raw = RawParams::parse(uri.query().unwrap_or(""))?;
    raw.reject_only(super::VERSION_KEYS)?;
    let job = Job::reference("places", ctx.warm_up, ctx.timeout);
    let snap = ctx.snap;
    let serving = snap.refdata.version().to_owned();
    let pinning = match version::check(raw.get("v"), &serving, uri.path(), "") {
        Ok(p) => p,
        Err(redirect) => return Ok(*redirect),
    };
    // Reference data alone, like coverage: no American Stories mark.
    let key = format!("{serving}|places|");
    let compute = async move {
        let rd = &snap.refdata;
        let mut title_counts = std::collections::HashMap::<&str, usize>::new();
        // The languages the place's titles are printed in (catalog codes,
        // e.g. "eng", "ger"), with the titles that list each; a title in
        // several languages counts in each. The map's relative-rate view
        // names them for a place where any title isn't in English (doc 11,
        // 11.14).
        let mut languages =
            std::collections::HashMap::<&str, std::collections::BTreeMap<&str, usize>>::new();
        for t in rd.titles.values() {
            let own: std::collections::BTreeSet<&str> =
                t.languages.iter().map(String::as_str).collect();
            // A title that moved counts in each of its places.
            for id in t.place_ids() {
                *title_counts.entry(id).or_default() += 1;
                let place = languages.entry(id).or_default();
                for l in &own {
                    *place.entry(l).or_default() += 1;
                }
            }
        }
        let features: Vec<_> = rd
            .places
            .iter()
            .map(|p| {
                json!({
                    "type": "Feature",
                    "id": p.id,
                    "geometry": { "type": "Point", "coordinates": [p.lon, p.lat] },
                    "properties": {
                        "name": p.name,
                        "state": p.state,
                        "precision": p.precision,
                        "titles": title_counts.get(p.id.as_str()).copied().unwrap_or(0),
                        "languages": languages.get(p.id.as_str()).map(|l| l.keys().collect::<Vec<_>>()).unwrap_or_default(),
                        "language_titles": languages.get(p.id.as_str()).cloned().unwrap_or_default()
                    }
                })
            })
            .collect();
        Ok::<_, ApiError>(json!({
            "type": "FeatureCollection",
            "index_version": rd.version(),
            "features": features
        }))
    };
    cached(state, job, key, &pinning, &serving, uri.path(), "", compute).await
}
