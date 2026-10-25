//! `/v1/openapi.json` (#1047): the document names every public route the
//! router serves and no other, each route's parameters are the ones its
//! handler accepts, and real responses match the documented schemas, with
//! every object closed: a field added to a response and not to its schema
//! (or the other way round) fails here.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, StatusCode};
use serde_json::{json, Value};
use usnm_api::{app, v1_routes, AppState};

use crate::american_stories::american_stories_state;
use crate::common::{config, fixture_backend, get, header_str, ja_state, refdata};
use crate::congress::congress_state;

async fn fixture() -> Arc<AppState> {
    Arc::new(AppState::new(
        config(),
        Arc::new(fixture_backend()),
        refdata().await,
    ))
}

async fn document(s: &Arc<AppState>) -> Value {
    let (status, headers, doc) = get(s, "/v1/openapi.json").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/json"
    );
    // Cached as `/v1/meta` is.
    let (_, meta, _) = get(s, "/v1/meta").await;
    assert_eq!(
        header_str(&headers, header::CACHE_CONTROL),
        header_str(&meta, header::CACHE_CONTROL)
    );
    doc
}

/// The documented route a request goes to.
fn template(uri: &str) -> String {
    let path = uri.split('?').next().unwrap();
    match path.strip_prefix("/v1/titles/") {
        Some(_) => "/v1/titles/{lccn}".into(),
        None => path.into(),
    }
}

#[tokio::test]
async fn the_document_names_every_public_route_and_no_other() {
    let s = fixture().await;
    let doc = document(&s).await;
    assert_eq!(doc["openapi"], "3.1.0");
    assert_eq!(
        doc["servers"],
        json!([{"url": "https://api.usnewsmap.com"}, {"url": "https://usnewsmap.com"}])
    );
    let registered: BTreeSet<&str> = v1_routes().iter().map(|(p, _)| *p).collect();
    for internal in usnm_api::openapi::INTERNAL {
        assert!(registered.contains(internal), "{internal} isn't routed");
    }
    let public: BTreeSet<String> = registered
        .iter()
        .filter(|p| !usnm_api::openapi::INTERNAL.contains(p))
        .map(|p| format!("/v1{p}"))
        .collect();
    let documented: BTreeSet<String> = doc["paths"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(documented, public);
    for (path, item) in doc["paths"].as_object().unwrap() {
        let ops: Vec<&String> = item.as_object().unwrap().keys().collect();
        assert_eq!(ops, ["get"], "{path}");
        // Answered by the route, not the fallback's `no such endpoint`.
        let uri = path.replace("{lccn}", "sn99000001");
        let (status, _, body) = get(&s, &uri).await;
        assert_ne!(status, StatusCode::METHOD_NOT_ALLOWED, "{uri}");
        assert_ne!(body["detail"], "no such endpoint", "{uri}");
    }
    for health in ["/healthz", "/readyz"] {
        assert!(!documented.contains(health));
    }
    // Every reference resolves.
    let mut refs = Vec::new();
    collect_refs(&doc, &mut refs);
    assert!(!refs.is_empty());
    for r in refs {
        let pointer = r.strip_prefix('#').unwrap();
        assert!(doc.pointer(pointer).is_some(), "{r} doesn't resolve");
    }
}

fn collect_refs(v: &Value, out: &mut Vec<String>) {
    match v {
        Value::Object(m) => {
            if let Some(Value::String(r)) = m.get("$ref") {
                out.push(r.clone());
            }
            m.values().for_each(|v| collect_refs(v, out));
        }
        Value::Array(a) => a.iter().for_each(|v| collect_refs(v, out)),
        _ => {}
    }
}

#[tokio::test]
async fn each_route_documents_the_parameters_its_handler_accepts() {
    let s = fixture().await;
    let doc = document(&s).await;
    for (path, item) in doc["paths"].as_object().unwrap() {
        let op = &item["get"];
        let base = path.replace("{lccn}", "sn99000001");
        let names: Vec<&str> = op["parameters"]
            .as_array()
            .map(|ps| {
                ps.iter()
                    .filter(|p| p["in"] == "query")
                    .map(|p| p["name"].as_str().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        let (status, _, body) = get(&s, &format!("{base}?undocumented=1")).await;
        if names.is_empty() {
            // The routes without parameters ignore the query string.
            assert_eq!(status, StatusCode::OK, "{path}: {body}");
            continue;
        }
        assert_eq!(status, StatusCode::BAD_REQUEST, "{path}");
        assert_eq!(body["detail"], "unknown parameter `undocumented`", "{path}");
        for name in names {
            // An empty value counts as absent, so only the key is checked.
            let (_, _, body) = get(&s, &format!("{base}?{name}=")).await;
            let detail = body["detail"].as_str().unwrap_or("");
            assert!(
                !detail.contains("unknown parameter"),
                "{path} {name}: {detail}"
            );
        }
    }
}

/// `q` is optional on `/v1/hits` only where it lists a newspaper's pages
/// without one.
#[tokio::test]
async fn q_is_required_on_hits_unless_pages_are_listed_without_it() {
    let required = |doc: &Value| {
        doc["paths"]["/v1/hits"]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["name"] == "q")
            .unwrap()["required"]
            .clone()
    };
    let mut cfg = config();
    assert_eq!(required(&usnm_api::openapi::document(&cfg)), true);
    cfg.browse_pages = true;
    assert_eq!(required(&usnm_api::openapi::document(&cfg)), false);
}

/// Requests whose responses are checked against the document, on the
/// states that give the responses' optional parts.
async fn exchanges() -> Vec<(Arc<AppState>, Vec<String>)> {
    let plain = [
        "/v1/meta",
        "/v1/places",
        "/v1/places?v=fixture-v1",
        "/v1/places?v=another",
        "/v1/titles",
        "/v1/titles/sn99000001",
        "/v1/titles/sn99000001?v=fixture-v1",
        "/v1/titles/sn99999999",
        "/v1/titles/NOT-AN-LCCN",
        "/v1/aggregate?q=gold",
        "/v1/aggregate?q=gold&from=1896-01-01&to=1896-12-31&bucket=week&state=il,ny",
        "/v1/aggregate?q=gold&lang=eng&front=true",
        "/v1/aggregate?q=gold&lccn=sn99000001&v=fixture-v1",
        "/v1/aggregate?q=gold+silver&mode=near&near=5",
        "/v1/aggregate?q=gold&fuzzy=1",
        "/v1/aggregate?q=%22cross+of",
        "/v1/aggregate?q=gold&format=csv",
        "/v1/aggregate?q=gold&q=silver",
        "/v1/hits?q=gold&place=P00001",
        "/v1/hits?q=gold&place=P00001&sort=relevant&limit=2",
        "/v1/hits?q=gold&lccn=sn99000001&sort=newest",
        "/v1/hits?q=gold&place=P99999",
        "/v1/hits?q=gold",
        "/v1/hits?lccn=sn99000001",
        "/v1/days?q=gold&place=P00001,P00006",
        "/v1/days?q=gold&place=P99999",
        "/v1/days?q=gold",
        "/v1/coverage",
        "/v1/coverage?from=1896-01-01&to=1896-12-31&bucket=month&state=IL&lang=eng",
        "/v1/coverage?from=1896",
        "/v1/status",
        "/v1/versions",
        "/v1/openapi.json",
    ];
    let american_stories = [
        "/v1/aggregate?q=gold",
        "/v1/hits?q=bimetallism&place=P00001",
        "/v1/hits?q=council&place=P00001&limit=50",
        "/v1/days?q=gold&place=P00001,P00006",
    ];
    let japanese = [
        "/v1/meta",
        "/v1/aggregate?q=%E6%88%A6%E4%BA%89",
        "/v1/hits?q=%E6%88%A6%E4%BA%89&place=P00003",
    ];
    let congress = [
        "/v1/meta",
        "/v1/aggregate?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&bucket=month&source=congress",
        "/v1/aggregate?q=gold&source=all&series=globe,annals",
        "/v1/hits?q=%22fugitive+slave%22&from=1850-01-01&to=1850-12-31&source=congress&place=PC001&limit=5",
        "/v1/days?q=gold&source=congress&place=PC001",
        "/v1/hits?q=gold&source=congress&lccn=sn99000001",
    ];
    let browse = [
        "/v1/meta",
        "/v1/hits?lccn=sn99000002&limit=3",
        "/v1/hits?lccn=sn99000002&sort=newest&limit=3",
        "/v1/hits?lccn=sn99000002&sort=relevant",
    ];
    let mut browse_cfg = config();
    browse_cfg.browse_pages = true;
    let owned = |uris: &[&str]| uris.iter().map(|u| u.to_string()).collect::<Vec<_>>();
    vec![
        (fixture().await, owned(&plain)),
        (american_stories_state(true).await, owned(&american_stories)),
        (ja_state().await, owned(&japanese)),
        (congress_state().await, owned(&congress)),
        (
            Arc::new(AppState::new(
                browse_cfg,
                Arc::new(fixture_backend()),
                refdata().await,
            )),
            owned(&browse),
        ),
        (
            moved_state().await,
            owned(&["/v1/titles", "/v1/titles/sn99000002"]),
        ),
        (
            pipeline_state().await,
            owned(&["/v1/status", "/v1/versions"]),
        ),
    ]
}

/// The fixture state with a title in two places by date (#1066), whose
/// periods the titles' responses list.
async fn moved_state() -> Arc<AppState> {
    let mut rd = refdata().await;
    rd.titles.get_mut("sn99000002").unwrap().place_periods = serde_json::from_value(json!([
        {"place_id": "P00001", "to": "1896-06-30"},
        {"place_id": "P00002", "from": "1896-07-01"},
    ]))
    .unwrap();
    Arc::new(AppState::new(config(), Arc::new(fixture_backend()), rd))
}

/// The fixture state reading a pipeline state with batches and index runs.
async fn pipeline_state() -> Arc<AppState> {
    use usnm_state::docs::{DocStore, MemoryDocs};
    let docs = Arc::new(MemoryDocs::default());
    let now = chrono::Utc::now();
    for (id, status, attempts) in [
        ("b1", "queued", 0),
        ("b2", "failed", 5),
        ("b3", "curated", 1),
    ] {
        docs.upsert(
            "batches",
            id,
            &json!({"id": id, "batch": id, "version": 1, "status": status,
                    "attempts": attempts, "updated_at": now}),
        )
        .await
        .unwrap();
    }
    for (id, previous, build) in [
        (
            "v1",
            Value::Null,
            json!({"commit": "abc", "engine": "Quickwit 0.9.1"}),
        ),
        ("v2", json!("v1"), Value::Null),
    ] {
        docs.upsert(
            "index_runs",
            id,
            &json!({"id": id, "index_version": id, "full": true, "indexes": ["b1"],
                    "new_index": "b1", "status": "published", "docs": 9, "pages": 10,
                    "batch_count": 3, "started_at": "2026-10-06T10:00:00Z",
                    "published_at": "2026-10-06T11:00:00Z", "previous_version": previous,
                    "build": build}),
        )
        .await
        .unwrap();
    }
    Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata().await)
            .with_pipeline(usnm_api::status::PipelineSource::Docs(docs)),
    )
}

#[tokio::test]
async fn responses_match_the_documented_schemas() {
    let doc = usnm_api::openapi::document(&config());
    let mut seen: BTreeSet<(String, u16)> = BTreeSet::new();
    let mut bodies = String::new();
    for (s, uris) in exchanges().await {
        let mut queue = uris;
        while let Some(uri) = queue.pop() {
            let resp = raw_get(&s, &uri).await;
            check_response(&doc, &uri, &resp);
            seen.insert((template(&uri), resp.status.as_u16()));
            // Not the document itself, whose schemas name every optional part.
            if resp.status == StatusCode::OK && template(&uri) != "/v1/openapi.json" {
                bodies.push_str(&String::from_utf8_lossy(&resp.bytes));
            }
            // Follow a page of hits to the next.
            if let Some(cursor) = resp.json.get("next_cursor").and_then(Value::as_str) {
                if !uri.contains("cursor=") {
                    queue.push(format!("{uri}&cursor={cursor}"));
                }
            }
        }
    }

    // Still computing: a search slower than a visitor waits.
    let mut cfg = config();
    cfg.search_timeout = Duration::from_millis(50);
    cfg.fixture_slow = Some(("zebrasecret".into(), Duration::from_millis(300)));
    let slow = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let uri = "/v1/aggregate?q=zebrasecret";
    let resp = raw_get(&slow, uri).await;
    assert_eq!(resp.status, StatusCode::ACCEPTED);
    check_response(&doc, uri, &resp);
    seen.insert((template(uri), 202));

    // Over the rate limit (the tests' requests share one client).
    let mut cfg = config();
    cfg.rate_limit = Some(usnm_api::config::RateLimit {
        per_minute: 1.try_into().unwrap(),
        burst: 1.try_into().unwrap(),
    });
    let limited = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    raw_get(&limited, "/v1/meta").await;
    let resp = raw_get(&limited, "/v1/meta").await;
    assert_eq!(resp.status, StatusCode::TOO_MANY_REQUESTS);
    check_response(&doc, "/v1/meta", &resp);
    seen.insert(("/v1/meta".into(), 429));

    // The optional parts were checked too.
    for part in [
        r#""ja":{"#,
        r#""ocr":{"#,
        r#""congress":{"series":"#,
        r#""matched_in":["#,
        r#""snippet_source":"#,
        r#""american_stories_only":"#,
        r#""days":"#,
        r#""build":{"#,
    ] {
        assert!(bodies.contains(part), "no response had {part}");
    }

    // Every route's body was checked, and each kind of answer.
    for path in doc["paths"].as_object().unwrap().keys() {
        assert!(seen.contains(&(path.clone(), 200)), "no 200 from {path}");
    }
    let codes: BTreeSet<u16> = seen.iter().map(|(_, c)| *c).collect();
    for code in [200, 202, 307, 400, 404, 422, 429] {
        assert!(codes.contains(&code), "no {code} checked");
    }
}

struct Exchange {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    bytes: Vec<u8>,
    json: Value,
}

async fn raw_get(s: &Arc<AppState>, uri: &str) -> Exchange {
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    let resp = app(s.clone())
        .oneshot(
            axum::http::Request::builder()
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let status = resp.status();
    let headers = resp.headers().clone();
    let bytes = resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    let json = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    Exchange {
        status,
        headers,
        bytes,
        json,
    }
}

/// `v` itself, or the component a `$ref` names.
fn resolve<'a>(doc: &'a Value, v: &'a Value) -> &'a Value {
    match v.get("$ref").and_then(Value::as_str) {
        Some(r) => doc.pointer(r.strip_prefix('#').unwrap()).unwrap(),
        None => v,
    }
}

fn check_response(doc: &Value, uri: &str, resp: &Exchange) {
    let path = template(uri);
    let code = resp.status.as_u16().to_string();
    let documented = &doc["paths"][&path]["get"]["responses"][&code];
    assert!(
        !documented.is_null(),
        "{uri}: {code} isn't documented: {}",
        resp.json
    );
    let documented = resolve(doc, documented);
    for name in documented["headers"]
        .as_object()
        .into_iter()
        .flatten()
        .map(|(k, _)| k)
    {
        // `ETag` and `Content-Location` go with `v` given or not.
        if ["Retry-After", "Location", "Cache-Control"].contains(&name.as_str()) {
            assert!(
                resp.headers.contains_key(name.as_str()),
                "{uri}: no {name} header"
            );
        }
    }
    match documented["content"].as_object() {
        None => assert!(resp.bytes.is_empty(), "{uri}: {code} has a body"),
        Some(content) => {
            let (media, body) = content.iter().next().unwrap();
            assert_eq!(
                header_str(&resp.headers, header::CONTENT_TYPE),
                media,
                "{uri}"
            );
            if let Err(e) = check(doc, &body["schema"], &resp.json, "$") {
                panic!("{uri}: {code} doesn't match its schema: {e}");
            }
        }
    }
}

/// The JSON Schema keywords [`check`] knows. Annotations are skipped; any
/// other keyword fails, so a schema is never passed for want of a check.
const KEYWORDS: &[&str] = &[
    "$ref",
    "additionalProperties",
    "anyOf",
    "const",
    "default",
    "description",
    "enum",
    "format",
    "items",
    "maxItems",
    "maximum",
    "minItems",
    "minimum",
    "oneOf",
    "properties",
    "required",
    "type",
];

/// Check `v` against `schema`, strictly: an object schema with `properties`
/// and no `additionalProperties` allows no other properties.
fn check(doc: &Value, schema: &Value, v: &Value, at: &str) -> Result<(), String> {
    let s = match schema {
        Value::Bool(true) => return Ok(()),
        Value::Object(s) => s,
        other => panic!("{at}: unexpected schema {other}"),
    };
    if let Some(k) = s.keys().find(|k| !KEYWORDS.contains(&k.as_str())) {
        panic!("{at}: the check doesn't know `{k}`");
    }
    if s.contains_key("$ref") {
        return check(doc, resolve(doc, schema), v, at);
    }
    if let Some(t) = s.get("type") {
        let types: Vec<&str> = match t {
            Value::String(t) => vec![t.as_str()],
            Value::Array(ts) => ts.iter().filter_map(Value::as_str).collect(),
            other => panic!("{at}: type {other}"),
        };
        let fits = types.iter().any(|t| match *t {
            "null" => v.is_null(),
            "boolean" => v.is_boolean(),
            "string" => v.is_string(),
            "integer" => v.is_i64() || v.is_u64(),
            "number" => v.is_number(),
            "array" => v.is_array(),
            "object" => v.is_object(),
            other => panic!("{at}: type {other}"),
        });
        if !fits {
            return Err(format!("{at}: {v} isn't {t}"));
        }
    }
    if let Some(c) = s.get("const") {
        if v != c {
            return Err(format!("{at}: {v} isn't {c}"));
        }
    }
    if let Some(e) = s.get("enum").and_then(Value::as_array) {
        if !e.contains(v) {
            return Err(format!("{at}: {v} isn't one of {e:?}"));
        }
    }
    if let (Some(n), Some(min)) = (v.as_f64(), s.get("minimum").and_then(Value::as_f64)) {
        if n < min {
            return Err(format!("{at}: {n} < {min}"));
        }
    }
    if let (Some(n), Some(max)) = (v.as_f64(), s.get("maximum").and_then(Value::as_f64)) {
        if n > max {
            return Err(format!("{at}: {n} > {max}"));
        }
    }
    if let (Some(f), Some(text)) = (s.get("format").and_then(Value::as_str), v.as_str()) {
        let ok = match f {
            "date" => chrono::NaiveDate::parse_from_str(text, "%Y-%m-%d").is_ok(),
            "date-time" => chrono::DateTime::parse_from_rfc3339(text).is_ok(),
            _ => true,
        };
        if !ok {
            return Err(format!("{at}: {text} isn't a {f}"));
        }
    }
    if let Some(any) = s.get("anyOf").and_then(Value::as_array) {
        let errors: Vec<String> = any
            .iter()
            .filter_map(|o| check(doc, o, v, at).err())
            .collect();
        if errors.len() == any.len() {
            return Err(format!("{at}: none of anyOf: {errors:?}"));
        }
    }
    if let Some(one) = s.get("oneOf").and_then(Value::as_array) {
        let fits = one.iter().filter(|o| check(doc, o, v, at).is_ok()).count();
        if fits != 1 {
            return Err(format!("{at}: {fits} of oneOf fit {v}"));
        }
    }
    if let Some(a) = v.as_array() {
        let len = a.len() as u64;
        if s.get("minItems")
            .and_then(Value::as_u64)
            .is_some_and(|m| len < m)
            || s.get("maxItems")
                .and_then(Value::as_u64)
                .is_some_and(|m| len > m)
        {
            return Err(format!("{at}: {len} items"));
        }
        if let Some(items) = s.get("items") {
            for (i, item) in a.iter().enumerate() {
                check(doc, items, item, &format!("{at}[{i}]"))?;
            }
        }
    }
    if let Some(o) = v.as_object() {
        for r in s
            .get("required")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let r = r.as_str().unwrap();
            if !o.contains_key(r) {
                return Err(format!("{at}: no `{r}`"));
            }
        }
        let props = s.get("properties").and_then(Value::as_object);
        let extra = s.get("additionalProperties");
        for (k, val) in o {
            let at = format!("{at}.{k}");
            match (props.and_then(|p| p.get(k)), extra) {
                (Some(p), _) => check(doc, p, val, &at)?,
                (None, Some(Value::Bool(true))) => {}
                (None, Some(e @ Value::Object(_))) => check(doc, e, val, &at)?,
                (None, None) if props.is_none() => {}
                (None, _) => return Err(format!("{at}: not in the schema")),
            }
        }
    }
    Ok(())
}
