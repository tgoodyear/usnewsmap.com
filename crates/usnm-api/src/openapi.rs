//! `GET /v1/openapi.json` (06 §6.3.2, #1047): the OpenAPI 3.1 description
//! of the public routes, built from the code that serves them so it can't
//! drift. The response schemas are derived from the response types
//! (`schemars`, whose JSON Schema 2020-12 is OpenAPI 3.1's dialect), except
//! `/v1/meta`'s, `/v1/places`' and the `202`'s, whose bodies are built as
//! JSON and described next to the code that builds them. Each route's query
//! parameters are the keys its handler accepts, from the same lists.
//! `tests/api/openapi.rs` checks the document against the router and against
//! real responses.

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue};
use axum::response::{IntoResponse, Response};
use schemars::generate::SchemaSettings;
use schemars::SchemaGenerator;
use serde_json::{json, Map, Value};
use usnm_core::params::{COMMON_KEYS, CONGRESS_SERIES, MAX_LIST_VALUES};
use usnm_core::query;

use crate::config::Config;
use crate::routes::spec;
use crate::AppState;

/// The `/v1` routes the document leaves out: the site's page views.
/// `/healthz` and `/readyz`, outside `/v1`, are left out too.
pub const INTERNAL: &[&str] = &["/beacon"];

/// Where the API answers: its own host, and the site's, which serves the
/// same routes (ADR-0010).
pub const SERVERS: &[&str] = &["https://api.usnewsmap.com", "https://usnewsmap.com"];

/// The document, built once per process: it depends only on the code and
/// the configuration.
pub async fn openapi(State(state): State<Arc<AppState>>) -> Response {
    let body = state
        .openapi
        .get_or_init(|| Arc::new(serde_json::to_vec(&document(&state.config)).unwrap_or_default()))
        .clone();
    let mut resp = body.as_ref().clone().into_response();
    let headers = resp.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    // As `/v1/meta`'s: it changes only with a release.
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("public, max-age=300"),
    );
    resp
}

/// An object schema with exactly `properties`, all required.
pub(crate) fn closed(properties: Value) -> Value {
    let required: Vec<String> = properties
        .as_object()
        .map(|p| p.keys().cloned().collect())
        .unwrap_or_default();
    json!({
        "type": "object",
        "properties": properties,
        "required": required,
        "additionalProperties": false,
    })
}

/// What a route answers besides its body, and so which responses it has.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    /// Version-scoped and cached (06 §6.5): `v`, `ETag` or
    /// `Content-Location`, the `307` for another version, the `202` while
    /// computing, and problems for bad requests and an unavailable backend.
    Versioned,
    /// Served as is; the query string is ignored.
    Plain,
}

/// One operation of the document.
struct Op {
    path: &'static str,
    id: &'static str,
    summary: &'static str,
    description: &'static str,
    kind: Kind,
    /// The query keys the handler accepts.
    keys: Vec<&'static str>,
    body: Value,
    /// Problems beyond those every route of its kind has.
    errors: &'static [u16],
    cache: &'static str,
}

fn search_keys(extra: &[&'static str]) -> Vec<&'static str> {
    COMMON_KEYS.iter().chain(extra).copied().collect()
}

fn ops(config: &Config, gen: &mut SchemaGenerator) -> Vec<Op> {
    let day = "`public, max-age=86400` with `v` (and an `ETag`), `public, max-age=300` without";
    let hits = if config.browse_pages {
        "Pages for `place`, or for a single `lccn`, with snippets and a link to the page at the \
         Library of Congress, oldest first by default. When `next_cursor` isn't null, pass it as \
         `cursor` for the next page. Without `q`, a single `lccn` lists that newspaper's pages \
         by date."
    } else {
        "Pages for `place`, or for a single `lccn`, with snippets and a link to the page at the \
         Library of Congress, oldest first by default. When `next_cursor` isn't null, pass it as \
         `cursor` for the next page."
    };
    vec![
        Op {
            path: "/meta",
            id: "meta",
            summary: "The version being served",
            description: "The `index_version` (what `v` takes), the dates covered, page, newspaper and \
                          place counts, the query limits, the languages `lang` takes and what the \
                          search backend supports. Not version-scoped: it is how clients learn the version.",
            kind: Kind::Plain,
            keys: Vec::new(),
            body: spec::meta(gen),
            errors: &[],
            cache: "public, max-age=300",
        },
        Op {
            path: "/aggregate",
            id: "aggregate",
            summary: "Counts for one search",
            description: "Matching pages in total, per time bucket (`series`), per place and per \
                          newspaper and language, the first and last matching page, and the sparse \
                          place × bucket cube (`cube`: parallel arrays of place index into `places.id`, \
                          bucket index and hits). Buckets are coarsened (`coarsened`) to keep the cube \
                          under the cell cap.",
            kind: Kind::Versioned,
            keys: search_keys(spec::AGGREGATE_KEYS),
            body: spec::aggregate(gen).to_value(),
            errors: &[422],
            cache: day,
        },
        Op {
            path: "/hits",
            id: "hits",
            summary: "Matching pages from one place or newspaper",
            description: hits,
            kind: Kind::Versioned,
            keys: search_keys(spec::HITS_KEYS),
            body: spec::hits(gen).to_value(),
            errors: &[404, 422],
            cache: day,
        },
        Op {
            path: "/days",
            id: "days",
            summary: "Matching pages per day for a few places",
            description: "For one search, the days with matching pages in each `place`, and the pages \
                          on each, for exact median and quartile dates.",
            kind: Kind::Versioned,
            keys: search_keys(spec::DAYS_KEYS),
            body: spec::days(gen).to_value(),
            errors: &[404, 422],
            cache: day,
        },
        Op {
            path: "/places",
            id: "places",
            summary: "Every place, as GeoJSON",
            description: "Every place with its name, state, how precisely it is located, its number of \
                          newspapers and their languages. Its `id` is what `place` takes.",
            kind: Kind::Versioned,
            keys: spec::VERSION_KEYS.to_vec(),
            body: spec::places(),
            errors: &[],
            cache: day,
        },
        Op {
            path: "/coverage",
            id: "coverage",
            summary: "Pages published per place and bucket",
            description: "Pages published, matching or not, per place and time bucket (`pages`: \
                          parallel arrays of place index into `places`, bucket index and pages), for \
                          turning counts into rates.",
            kind: Kind::Versioned,
            keys: spec::COVERAGE_KEYS.to_vec(),
            body: spec::coverage(gen).to_value(),
            errors: &[422],
            cache: day,
        },
        Op {
            path: "/titles",
            id: "titles",
            summary: "Every newspaper",
            description: "Every newspaper in the version, by LCCN, with its place, years of \
                          publication, pages and languages.",
            kind: Kind::Versioned,
            keys: spec::VERSION_KEYS.to_vec(),
            body: spec::titles(gen).to_value(),
            errors: &[],
            cache: day,
        },
        Op {
            path: "/titles/{lccn}",
            id: "title",
            summary: "One newspaper",
            description: "One newspaper, as `/v1/titles` lists it, with its page at the Library of Congress.",
            kind: Kind::Versioned,
            keys: spec::VERSION_KEYS.to_vec(),
            body: spec::title(gen).to_value(),
            errors: &[404],
            cache: day,
        },
        Op {
            path: "/status",
            id: "status",
            summary: "The dataset and the ingest pipeline",
            description: "The published version and its sources, and the ingest pipeline's progress, \
                          for the dataset details page. Sections that need the pipeline state say \
                          `available: false` and why when it can't be read.",
            kind: Kind::Plain,
            keys: Vec::new(),
            body: crate::status::schema(gen).to_value(),
            errors: &[],
            cache: "public, max-age=30",
        },
        Op {
            path: "/versions",
            id: "versions",
            summary: "Every index version",
            description: "Every index version the pipeline built, newest first, with what built each one.",
            kind: Kind::Plain,
            keys: Vec::new(),
            body: crate::versions::schema(gen).to_value(),
            errors: &[],
            cache: "public, max-age=30",
        },
        Op {
            path: "/openapi.json",
            id: "openapi",
            summary: "This document",
            description: "The OpenAPI 3.1 description of the API.",
            kind: Kind::Plain,
            keys: Vec::new(),
            body: json!({ "type": "object" }),
            errors: &[],
            cache: "public, max-age=300",
        },
    ]
}

fn query_param(name: &str, required: bool, description: String, schema: Value) -> Value {
    json!({
        "name": name,
        "in": "query",
        "required": required,
        "description": description,
        "schema": schema,
    })
}

/// A comma-separated list (`style: form`, `explode: false`), sorted and
/// deduplicated by the API.
fn list_param(name: &str, description: String, item: Value, max: usize) -> Value {
    let mut p = query_param(
        name,
        false,
        description,
        json!({ "type": "array", "items": item, "maxItems": max }),
    );
    p["style"] = "form".into();
    p["explode"] = false.into();
    p
}

/// The parameter `name` of the route at `path`. Every key a handler accepts
/// has one: a key added to a handler's list without one fails here, in the
/// tests that build the document.
fn param(path: &str, name: &str, config: &Config) -> Value {
    let search = path != "/coverage";
    match name {
        "q" => query_param(
            "q",
            path != "/hits" || !config.browse_pages,
            format!(
                "The search, up to {} characters and {} terms. Words are all required unless \
                 `mode` says otherwise; a phrase goes in double quotes, `OR` joins alternatives \
                 (up to {}), a leading `-` excludes a word, `word*` matches words starting with \
                 `word` (at least {} letters), `?` and `*` inside a word match one character or \
                 any run, `word~ocr` also matches OCR's common misspellings, and `2 OF (a b c)` \
                 matches pages with at least 2 of up to {} words or phrases.{}",
                query::MAX_QUERY_CHARS,
                query::MAX_TERMS,
                query::MAX_OR_BRANCHES,
                query::MIN_PREFIX_CHARS,
                query::MAX_AT_LEAST_ITEMS,
                if path == "/hits" && config.browse_pages {
                    " Without `q`, a single `lccn` lists that newspaper's pages by date; `mode`, \
                     `near`, `fuzzy`, `bucket` and `sort=relevant` then need `q`."
                } else {
                    ""
                }
            ),
            json!({ "type": "string", "maxLength": query::MAX_QUERY_CHARS }),
        ),
        "mode" => query_param(
            "mode",
            false,
            "How the words of `q` combine. Without it, a quoted `q` is a phrase and any other \
             needs all its words."
                .into(),
            json!({ "type": "string", "enum": ["phrase", "all", "any", "near"] }),
        ),
        "near" => query_param(
            "near",
            false,
            format!(
                "How far apart the words may be with `mode=near`, which needs 1 to {}; ignored \
                 otherwise.",
                query::MAX_SLOP
            ),
            json!({ "type": "integer", "minimum": 0, "maximum": query::MAX_SLOP, "default": 0 }),
        ),
        "fuzzy" => query_param(
            "fuzzy",
            false,
            "Match single words within this many letter edits, for OCR errors; not phrases or \
             `word*`. Answers 422 unless `capabilities.fuzzy` in `/v1/meta` is true."
                .into(),
            json!({ "type": "integer", "minimum": 0, "maximum": query::MAX_FUZZY, "default": 0 }),
        ),
        "from" | "to" => query_param(
            name,
            false,
            format!(
                "The {} date. Dates outside `bounds` in `/v1/meta` move to the nearest bound; \
                 `from` after `to` is a 400.",
                if name == "from" { "first" } else { "last" }
            ),
            json!({ "type": "string", "format": "date" }),
        ),
        "state" => {
            let mut p = list_param(
                "state",
                "USPS state codes, any case: only places in these states.".into(),
                json!({ "type": "string", "pattern": "^[A-Za-z]{2}$" }),
                MAX_LIST_VALUES,
            );
            if !search {
                // Coverage reads its own list, without the cap.
                p["schema"].as_object_mut().map(|s| s.remove("maxItems"));
            }
            p
        }
        "lccn" => list_param(
            "lccn",
            if path == "/hits" {
                "Newspapers by LCCN. Without `place`, a single LCCN selects that newspaper's pages; \
                 with `place`, a filter."
                    .into()
            } else {
                "Newspapers by LCCN: only their pages.".into()
            },
            json!({ "type": "string", "pattern": "^[a-z0-9]{1,16}$" }),
            MAX_LIST_VALUES,
        ),
        "lang" => list_param(
            "lang",
            "Three-letter language codes from `languages` in `/v1/meta`: pages in any of them \
             (by `language_filter`: each page's languages, or its newspaper's)."
                .into(),
            json!({ "type": "string", "pattern": "^[A-Za-z]{3}$" }),
            MAX_LIST_VALUES,
        ),
        "front" => query_param(
            "front",
            false,
            "Front pages only. Newspapers only: not with `source=congress`.".into(),
            json!({ "type": "string", "enum": ["true", "false", "1", "0"], "default": "false" }),
        ),
        "source" => query_param(
            "source",
            false,
            "The corpus searched: the newspapers, the debates and documents of Congress \
             (`congress` in `/v1/meta`), or both."
                .into(),
            json!({ "type": "string", "enum": ["news", "congress", "all"], "default": "news" }),
        ),
        "series" => list_param(
            "series",
            "Congress series; only with `source=congress` or `all`.".into(),
            json!({ "type": "string", "enum": CONGRESS_SERIES }),
            MAX_LIST_VALUES,
        ),
        "bucket" => query_param(
            "bucket",
            false,
            if path == "/days" {
                "Accepted as on the other search routes; the counts are always per day.".into()
            } else {
                "The time bucket. `auto` picks `year` for more than 30 years, `month` for more \
                 than 3, `week` for more than 4 months and `day` otherwise."
                    .into()
            },
            json!({
                "type": "string",
                "enum": ["auto", "year", "month", "week", "day"],
                "default": "auto",
            }),
        ),
        "v" => query_param(
            "v",
            false,
            "The `index_version` the client expects. Another version than the one served is a \
             307 to the same request at the served one; without `v`, the response is cacheable \
             for 5 minutes only and `Content-Location` names the URL with it."
                .into(),
            json!({ "type": "string" }),
        ),
        "format" => query_param(
            "format",
            false,
            "Only `json`; any other is a 422.".into(),
            json!({ "type": "string", "enum": ["json"], "default": "json" }),
        ),
        "place" if path == "/days" => {
            let mut p = list_param(
                "place",
                format!(
                    "1 to {} distinct place ids, in the order the response lists them.",
                    spec::MAX_PLACES
                ),
                json!({ "type": "string" }),
                spec::MAX_PLACES,
            );
            p["required"] = true.into();
            p["schema"]["minItems"] = 1.into();
            p
        }
        "place" => query_param(
            "place",
            false,
            "A place id from `/v1/places`. `/v1/hits` needs `place` or a single `lccn`.".into(),
            json!({ "type": "string" }),
        ),
        "cursor" => query_param(
            "cursor",
            false,
            format!(
                "`next_cursor` from the previous page, with the same other parameters. A search \
                 stops at result {}.",
                config.max_offset
            ),
            json!({ "type": "string" }),
        ),
        "limit" => query_param(
            "limit",
            false,
            "Pages per response.".into(),
            json!({ "type": "integer", "minimum": 1, "maximum": spec::MAX_LIMIT, "default": spec::MAX_LIMIT }),
        ),
        "sort" => query_param(
            "sort",
            false,
            "By date, or most mentions first (`relevant`, which needs `q`).".into(),
            json!({ "type": "string", "enum": ["oldest", "newest", "relevant"], "default": "oldest" }),
        ),
        other => panic!("`{other}` on {path} has no description in the OpenAPI document"),
    }
}

fn problem(schema: &Value, description: &str) -> Value {
    json!({
        "description": description,
        "content": { "application/problem+json": { "schema": schema } },
    })
}

fn with_retry_after(mut response: Value) -> Value {
    response["headers"] = json!({ "Retry-After": { "$ref": "#/components/headers/RetryAfter" } });
    response
}

/// The response components the operations refer to; `problem` is the
/// problem details' schema.
fn responses(problem_schema: &Value) -> Value {
    let problem = |description: &str| problem(problem_schema, description);
    json!({
        "Computing": {
            "description": "Still computing, or waiting for a computation slot. Send the same \
                            request again after `Retry-After`; the computation carries on meanwhile.",
            "headers": { "Retry-After": { "$ref": "#/components/headers/RetryAfter" } },
            "content": { "application/json": { "schema": spec::computing() } },
        },
        "OtherVersion": {
            "description": "`v` names another version than the one served: the same request at the \
                            served one. Never cached.",
            "headers": {
                "Location": {
                    "description": "The canonical request with the served `v`.",
                    "schema": { "type": "string" },
                },
            },
        },
        "BadRequest": problem(
            "A parameter is invalid, unknown or repeated (`/errors/bad-parameter`), or `q` doesn't \
             parse (`/errors/query-syntax`, with the `position`)."
        ),
        "NotFound": problem("An unknown place or newspaper (`/errors/not-found`)."),
        "Unprocessable": problem(
            "An option this version or backend doesn't support (`/errors/unsupported`), or a \
             search too broad to answer (`/errors/query-too-broad`)."
        ),
        "RateLimited": with_retry_after(problem(
            "Too many requests from this client (`/errors/rate-limited`)."
        )),
        "Unavailable": with_retry_after(problem(
            "The search ran past its limit (`/errors/backend-timeout`), every computation slot is \
             taken (`/errors/busy`), or the backend is unavailable (`/errors/backend`)."
        )),
    })
}

fn operation(op: &Op, config: &Config) -> Value {
    let mut parameters: Vec<Value> = op.keys.iter().map(|k| param(op.path, k, config)).collect();
    if op.path == "/titles/{lccn}" {
        parameters.insert(
            0,
            json!({
                "name": "lccn",
                "in": "path",
                "required": true,
                "description": "The newspaper's LCCN; anything else is a 404.",
                "schema": { "type": "string", "pattern": "^[a-z0-9]{1,16}$" },
            }),
        );
    }
    let mut ok_headers = Map::new();
    ok_headers.insert(
        "Cache-Control".into(),
        json!({ "description": op.cache, "schema": { "type": "string" } }),
    );
    if op.kind == Kind::Versioned {
        ok_headers.insert(
            "ETag".into(),
            json!({ "description": "With `v`: `\"{index_version}:{hash}\"`.", "schema": { "type": "string" } }),
        );
        ok_headers.insert(
            "Content-Location".into(),
            json!({ "description": "Without `v`: the canonical request with it.", "schema": { "type": "string" } }),
        );
    }
    let mut responses = Map::new();
    responses.insert(
        "200".into(),
        json!({
            "description": op.summary,
            "headers": ok_headers,
            "content": { "application/json": { "schema": op.body } },
        }),
    );
    let refer = |name: &str| json!({ "$ref": format!("#/components/responses/{name}") });
    if op.kind == Kind::Versioned {
        responses.insert("202".into(), refer("Computing"));
        responses.insert("307".into(), refer("OtherVersion"));
        responses.insert("400".into(), refer("BadRequest"));
    }
    for code in op.errors {
        let name = match code {
            404 => "NotFound",
            422 => "Unprocessable",
            other => panic!("no response for {other}"),
        };
        responses.insert(code.to_string(), refer(name));
    }
    responses.insert("429".into(), refer("RateLimited"));
    if op.kind == Kind::Versioned {
        responses.insert("503".into(), refer("Unavailable"));
    }
    let mut out = json!({
        "operationId": op.id,
        "summary": op.summary,
        "description": op.description,
        "responses": responses,
    });
    if !parameters.is_empty() {
        out["parameters"] = parameters.into();
    }
    out
}

/// The OpenAPI document for an API running with `config`.
pub fn document(config: &Config) -> Value {
    let mut gen = SchemaSettings::draft2020_12()
        .for_serialize()
        .with(|s| {
            s.definitions_path = "/components/schemas".into();
            s.meta_schema = None;
        })
        .into_generator();
    let problem = crate::error::problem_schema(&mut gen).to_value();
    let ops = ops(config, &mut gen);
    let mut paths = Map::new();
    for op in &ops {
        paths.insert(
            format!("/v1{}", op.path),
            json!({ "get": operation(op, config) }),
        );
    }
    let schemas: Map<String, Value> = gen.take_definitions(true).into_iter().collect();
    json!({
        "openapi": "3.1.0",
        "jsonSchemaDialect": "https://json-schema.org/draft/2020-12/schema",
        "info": {
            "title": "US News Map API",
            "version": format!("1.{}", crate::routes::RESPONSE_FORMAT),
            "summary": "Search digitized American newspapers and see where and when words were printed.",
            "description": "Free, read-only and without sign-in. Every route is `GET` and answers \
                            JSON; errors are RFC 9457 problem details. An empty parameter counts as \
                            absent; a repeated one is a 400, and so is an unknown one on the routes \
                            that take parameters. Responses are fixed per `index_version`: pass the \
                            one `/v1/meta` names as `v`. The same routes answer under `/api/v1`. \
                            The version's second number is the response format, which changes with \
                            the shape of a response body.",
        },
        "servers": SERVERS.iter().map(|url| json!({ "url": url })).collect::<Vec<_>>(),
        "paths": paths,
        "components": {
            "schemas": schemas,
            "responses": responses(&problem),
            "headers": {
                "RetryAfter": {
                    "description": "Seconds to wait before asking again.",
                    "schema": { "type": "integer", "minimum": 1 },
                },
            },
        },
    })
}
