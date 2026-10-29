//! The web app, served by the API from its built files (`USNM_SITE_DIR`,
//! ADR-0009): same origin as `/v1`, deployed in the same image.
//!
//! - Files come from the directory, preferring the `.br` or `.gz` copy the
//!   image build wrote next to each one.
//! - Paths that look like app routes (no file extension, not under
//!   `/assets/`) get `index.html`, so client-side routes survive a reload.
//!   The app's own pages (`/`, `/datasets`, `/privacy`, `/credits`) are 200; any other
//!   such path gets the same shell with a 404, and the app shows a not-found
//!   page. Missing files are a plain 404, which no cache may keep
//!   (`no-store`): it can be a gap in a deploy. `/privacy` gets its own
//!   title, canonical link and `og:url` in the shell, since it is indexed.
//! - `/status`, the dataset details page's old path, is redirected (301) to
//!   `/datasets` with its query, so existing links keep working.
//! - Hashed assets are cached for a year; `index.html` is revalidated on
//!   every load so a release shows up at once. The app's pages carry an
//!   `ETag` for that, as `ServeDir` gives the home page, so an unchanged page
//!   costs a `304`.
//! - Legacy search links redirect to `/search/<query>`. Curated and promoted base searches
//!   are indexable; other searches and filter variants carry `noindex`.
//! - Requests for `www.` + the site's host are redirected to the host itself.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::handler::HandlerWithoutStateExt;
use axum::http::{header, HeaderMap, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use sha2::{Digest, Sha256};
use tower_http::services::ServeDir;

use crate::error::ApiError;
use crate::AppState;

pub(crate) mod sitemap;
pub(crate) mod social;

/// Headers on every response (the site's and the API's). `connect-src`
/// allows the same-origin API, the OpenFreeMap basemap and public BOINC counts.
///
/// HSTS starts at five minutes, for the host that sends it alone (no
/// `includeSubDomains`) and without `preload`: a browser keeps it for
/// `max-age` even after a release removes it, so it grows in steps (#955).
pub const SECURITY_HEADERS: [(&str, &str); 5] = [
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob: https:; font-src 'self' https://tiles.openfreemap.org; \
         connect-src 'self' https://tiles.openfreemap.org https://boinc.usnewsmap.com; \
         worker-src 'self' blob:; child-src blob:; object-src 'none'; base-uri 'self'; \
         form-action 'self'",
    ),
    ("referrer-policy", "strict-origin-when-cross-origin"),
    ("x-content-type-options", "nosniff"),
    (
        "permissions-policy",
        "geolocation=(), camera=(), microphone=()",
    ),
    ("strict-transport-security", "max-age=300"),
];

/// The longest file name the filesystem takes (Linux `NAME_MAX`).
const NAME_MAX: usize = 255;

/// The longest name `ServeDir` can look up: it also probes `<name>.br` and
/// `<name>.gz`, which must fit `NAME_MAX` too. The site has no names near it.
const NAME_LIMIT: usize = NAME_MAX - ".br".len();

/// The site as a service for the API router's fallback.
pub fn router(dir: PathBuf, state: Arc<AppState>) -> Router {
    let index = dir.join("index.html");
    let spa = {
        let index = index.clone();
        let state = state.clone();
        move |req: Request| {
            let index = index.clone();
            let state = state.clone();
            async move { app_route(req, index, state).await }
        }
    };
    let files = ServeDir::new(dir)
        .precompressed_br()
        .precompressed_gzip()
        .fallback(spa.into_service());
    // A path segment too long to be a file name (or to take the `.br` or `.gz`
    // suffix) can't name one of ours, and `ServeDir` answers the filesystem's
    // "file name too long" with a 500, so such paths skip it and are answered
    // like any other path with no file.
    let sharing = state.clone();
    let robots = state.clone();
    let app_paths = move |req: Request, next: Next| {
        let index = index.clone();
        let state = state.clone();
        async move {
            if req.uri().path() == "/sitemap.xml" && state.config.sitemap_state_url().is_some() {
                return sitemap::response(&state, &req);
            }
            if let Some(location) = matches!(*req.method(), Method::GET | Method::HEAD)
                .then(|| search_redirect(req.uri()))
                .flatten()
            {
                return match HeaderValue::from_str(&location) {
                    Ok(location) => (
                        StatusCode::MOVED_PERMANENTLY,
                        [(header::LOCATION, location)],
                    )
                        .into_response(),
                    Err(_) => (StatusCode::BAD_REQUEST, "bad request").into_response(),
                };
            }
            // Search terms can contain dots, encoded slashes or long segments;
            // they are routes, never files in the static directory.
            if search_query(req.uri().path()).is_some()
                || matches!(req.uri().path(), "/" | "/index.html")
                || req
                    .uri()
                    .path()
                    .split('/')
                    .any(|s| decoded_len(s) > NAME_LIMIT)
            {
                app_route(req, index, state).await
            } else {
                next.run(req).await
            }
        }
    };
    Router::new()
        .route(
            "/share-image.png",
            axum::routing::get(move |uri| social::image(uri, sharing.clone())),
        )
        .fallback_service(files)
        .layer(middleware::from_fn(app_paths))
        .layer(middleware::from_fn(cache_control))
        .layer(middleware::from_fn_with_state(robots, robots_tag))
}

/// How many bytes a path segment is once percent-decoded, as `ServeDir`
/// decodes it before it names a file: each valid `%XX` escape is one byte.
fn decoded_len(segment: &str) -> usize {
    let b = segment.as_bytes();
    let (mut i, mut n) = (0, 0);
    while i < b.len() {
        let escape = b[i] == b'%'
            && i + 2 < b.len()
            && b[i + 1].is_ascii_hexdigit()
            && b[i + 2].is_ascii_hexdigit();
        i += if escape { 3 } else { 1 };
        n += 1;
    }
    n
}

/// The paths the app renders a page for (`web/src/route.ts`), with or
/// without a trailing slash. `/index.html` is the home page's file, which
/// `ServeDir` serves directly; since tower-http 0.7 it no longer serves a
/// file at a path with a trailing slash, so `/index.html/` comes here.
fn is_app_page(path: &str) -> bool {
    search_query(path).is_some()
        || matches!(
            path.trim_end_matches('/'),
            "" | "/datasets" | "/privacy" | "/credits" | "/index.html"
        )
}

/// The readable path segment for a plain exact-phrase search: lowercase
/// words joined by hyphens, quotes dropped. Search is case-insensitive and a
/// hyphen inside a word is a word break, so it means what the query did.
/// `None` when the query has syntax or a mode other than the default, where
/// quotes matter. Mirrors `slugFor` in web/src/route.ts.
fn slug_for(q: &str, phrase_mode: bool) -> Option<String> {
    if !phrase_mode {
        return None;
    }
    let mut text = q.trim();
    match text.strip_prefix('"').and_then(|t| t.strip_suffix('"')) {
        Some(inner) if text.len() >= 2 && !inner.contains('"') => text = inner,
        _ if text
            .split_whitespace()
            .any(|w| matches!(w, "AND" | "OR" | "NOT") || w.starts_with('-')) =>
        {
            return None
        }
        _ => {}
    }
    let words: Vec<String> = text
        .split(|c: char| c.is_whitespace() || c == '-')
        .filter(|w| !w.is_empty())
        .map(|w| {
            // Lowercasing can add a combining mark (İ), which splits a token.
            let lower = w.to_lowercase();
            lower.chars().all(char::is_alphanumeric).then_some(lower)
        })
        .collect::<Option<_>>()?;
    (!words.is_empty()).then(|| words.join("-"))
}

fn is_slug(q: &str) -> bool {
    !q.is_empty()
        && q.split('-').all(|part| {
            !part.is_empty()
                && part
                    .chars()
                    .all(|c| c.is_alphanumeric() && !c.is_uppercase())
        })
}

fn search_path(q: &str) -> String {
    search_path_in_mode(q, true)
}

fn search_path_in_mode(q: &str, phrase_mode: bool) -> String {
    if let Some(slug) = slug_for(q, phrase_mode) {
        let encoded: String = form_urlencoded::byte_serialize(slug.as_bytes()).collect();
        return format!("/search/{encoded}");
    }
    let encoded: String = form_urlencoded::byte_serialize(q.as_bytes()).collect();
    format!("/search/{}", encoded.replace('+', "%20"))
}

/// The query a search path names, read in the default phrase mode.
fn search_query(path: &str) -> Option<String> {
    search_query_in_mode(path, true)
}

/// Outside phrase mode a hyphen is kept: `well-known` is an adjacent phrase
/// there only as one word, and its terms would otherwise become separate ANDs.
fn search_query_in_mode(path: &str, phrase_mode: bool) -> Option<String> {
    let segment = path.strip_prefix("/search/")?;
    let segment = segment.strip_suffix('/').unwrap_or(segment);
    if segment.is_empty() || segment.contains('/') {
        return None;
    }
    let mut bytes = Vec::new();
    let mut chars = segment.bytes();
    while let Some(b) = chars.next() {
        bytes.push(if b == b'%' {
            let hi = char::from(chars.next()?).to_digit(16)?;
            let lo = char::from(chars.next()?).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()?
        } else {
            b
        });
    }
    let mut q = String::from_utf8(bytes).ok()?;
    if phrase_mode && is_slug(&q) {
        q = q.replace('-', " ");
    }
    (q.encode_utf16().count() <= 256 && q != "." && q != "..").then_some(q)
}

fn search_redirect(uri: &Uri) -> Option<String> {
    let mut params: Vec<_> = form_urlencoded::parse(uri.query().unwrap_or("").as_bytes()).collect();
    let phrase_mode = params.iter().all(|(k, v)| k != "mode" || v == "phrase");
    let q = match uri.path().trim_end_matches('/') {
        "" | "/index.html" => params.iter().find(|(k, _)| k == "q")?.1.to_string(),
        _ => search_query_in_mode(uri.path(), phrase_mode)?,
    };
    if q.is_empty() || q == "." || q == ".." || q.encode_utf16().count() > 256 {
        return None;
    }
    params.retain(|(k, _)| k != "q");
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish();
    let path = search_path_in_mode(&q, phrase_mode);
    let target = if query.is_empty() {
        path
    } else {
        format!("{path}?{query}")
    };
    (uri.path_and_query()?.as_str() != target).then_some(target)
}

pub(crate) static CURATED_SEARCHES: std::sync::LazyLock<std::collections::HashSet<String>> =
    std::sync::LazyLock::new(|| {
        #[derive(serde::Deserialize)]
        struct Example {
            view: Query,
        }
        #[derive(serde::Deserialize)]
        struct Query {
            q: String,
        }
        let examples: Vec<Example> =
            serde_json::from_str(include_str!("../../../web/src/examples.json"))
                .expect("valid curated examples");
        examples
            .into_iter()
            .map(|e| search_path(&e.view.q))
            .collect()
    });

/// The dataset details page's path before it was renamed (#294).
const OLD_DATASETS_PATH: &str = "/status";

/// Where a request for `/status` goes: `/datasets`, keeping the query.
fn datasets_redirect(uri: &Uri) -> Option<String> {
    (uri.path().trim_end_matches('/') == OLD_DATASETS_PATH).then(|| match uri.query() {
        Some(q) => format!("/datasets?{q}"),
        None => "/datasets".to_owned(),
    })
}

/// Indexable pages name themselves in the head, even without JavaScript.
fn shell_for(path: &str, index: Vec<u8>) -> Vec<u8> {
    let query = search_query(path);
    let (title, canonical) = if path.trim_end_matches('/') == "/privacy" {
        ("Privacy · US News Map".to_owned(), "/privacy".to_owned())
    } else if path.trim_end_matches('/') == "/datasets" {
        (
            "Dataset details · US News Map".to_owned(),
            "/datasets".to_owned(),
        )
    } else if path.trim_end_matches('/') == "/credits" {
        (
            "OCR credits · US News Map".to_owned(),
            "/credits".to_owned(),
        )
    } else if let Some(q) = &query {
        (format!("{q} · US News Map"), search_path(q))
    } else {
        return index;
    };
    let escape = |s: &str| {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
            .replace('"', "&quot;")
            .replace('\'', "&#39;")
    };
    let title = escape(&title);
    let canonical = format!("https://usnewsmap.com{canonical}");
    let Ok(html) = String::from_utf8(index) else {
        return Vec::new();
    };
    let mut html = html
        .replacen(
            "<title>US News Map</title>",
            &format!("<title>{title}</title>"),
            1,
        )
        .replacen(
            r#"<link rel="canonical" href="https://usnewsmap.com/" />"#,
            &format!(r#"<link rel="canonical" href="{canonical}" />"#),
            1,
        )
        .replacen(
            r#"<meta property="og:url" content="https://usnewsmap.com/" />"#,
            &format!(r#"<meta property="og:url" content="{canonical}" />"#),
            1,
        )
        .replacen(
            r#"<meta property="og:title" content="US News Map" />"#,
            &format!(r#"<meta property="og:title" content="{title}" />"#),
            1,
        );
    if let Some(q) = query {
        html = html.replacen(
            r#"<main class="empty">"#,
            &format!(
                r#"<main class="empty"><h1>Newspaper results for {}</h1>"#,
                escape(&q)
            ),
            1,
        );
    }
    html.into_bytes()
}

fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn insert_content(body: Vec<u8>, content: &str) -> Vec<u8> {
    // The built shell is UTF-8. Preserve its bytes if an invalid file is deployed.
    match String::from_utf8(body) {
        Ok(html) => html
            .replacen("</main>", &format!("{content}</main>"), 1)
            .into_bytes(),
        Err(error) => {
            tracing::error!("site index is not UTF-8");
            error.into_bytes()
        }
    }
}

fn search_landing(body: Vec<u8>, preview: &str) -> Vec<u8> {
    let body = insert_content(body, preview);
    match String::from_utf8(body) {
        Ok(html) => html
            .replacen(
                r#"<div id="root">"#,
                r#"<div id="root"></div><div id="search-landing">"#,
                1,
            )
            .replacen(r#"id="site-fallback""#, r#"id="search-content""#, 1)
            .replacen(
                "The search and the map need JavaScript.",
                "The map and interactive filters need JavaScript; the sample results below do not.",
                1,
            )
            .replacen(
                "<section aria-label=\"Sample results\">",
                "<p><a href=\"/\">Search other newspaper topics</a></p><button id=\"explore-map\" type=\"button\" class=\"button button--primary\" hidden>Explore interactive map</button><section aria-label=\"Sample results\">",
                1,
            )
            .replacen(
                "</main>",
                "</main><footer class=\"empty\" aria-label=\"Search landing information\"><nav aria-label=\"Site information\"><p><a href=\"/privacy\">Privacy</a> · <a href=\"/datasets\">Dataset details</a> · <a href=\"/credits\">OCR credits</a></p></nav></footer>",
                1,
            )
            .into_bytes(),
        Err(error) => {
            tracing::error!("site index is not UTF-8");
            error.into_bytes()
        }
    }
}

fn curated_links() -> String {
    let mut paths: Vec<_> = CURATED_SEARCHES.iter().collect();
    paths.sort();
    let links: String = paths
        .into_iter()
        .map(|path| {
            let q = search_query(path).expect("curated search path");
            format!(r#"<li><a href="{path}">{}</a></li>"#, escape_html(&q))
        })
        .collect();
    format!("<nav aria-label=\"Common newspaper searches\"><h2>Common searches</h2><ul>{links}</ul></nav>")
}

fn preview_query(q: &str) -> String {
    let mut params = form_urlencoded::Serializer::new(String::new());
    params.append_pair("q", q);
    if usnm_core::query::is_plain(q) {
        params.append_pair("mode", "phrase");
    }
    params.finish()
}

async fn search_preview(state: &Arc<AppState>, q: &str) -> Result<String, ApiError> {
    search_preview_snapshot(state, state.snapshot.load_full(), q, false).await
}

#[derive(serde::Deserialize, serde::Serialize)]
pub(super) struct SearchPreview {
    html: String,
    total_hits: u64,
}

pub(crate) async fn search_preview_snapshot(
    state: &Arc<AppState>,
    snap: Arc<crate::Snapshot>,
    q: &str,
    background: bool,
) -> Result<String, ApiError> {
    Ok(search_preview_data(state, snap, q, background).await?.html)
}

async fn search_preview_data(
    state: &Arc<AppState>,
    snap: Arc<crate::Snapshot>,
    q: &str,
    background: bool,
) -> Result<SearchPreview, ApiError> {
    use crate::routes::{cached_body, with_timeout, Job};
    use crate::version::Pinning;
    use usnm_core::ids::PageKey;
    use usnm_core::params::RawParams;
    use usnm_core::time::date_from_day;
    use usnm_search::HitsQuery;

    let raw = preview_query(q);
    let search = snap.refdata.search_request(&RawParams::parse(&raw)?)?;
    let canonical = search.canonical();
    let version = snap.refdata.version().to_owned();
    let key = snap.refdata.search_key(
        &format!("site-preview-{}", sitemap::renderer()),
        &canonical,
        &search.query,
    );
    let st = state.clone();
    let compute = async move {
        let rd = &snap.refdata;
        let indexes = rd.index_set_for_request(&search)?;
        let summary = with_timeout(
            &st,
            st.config.compute_cap,
            snap.backend.summary(
                &indexes,
                &search.query,
                &search.filters,
                &search.bucket_spec(),
            ),
        )
        .await?;
        let demo = if rd.current.synthetic {
            "<p class=\"notice notice--demo\" role=\"note\">Demo data: this server is running on a small synthetic corpus, not real newspapers.</p>"
        } else {
            ""
        };
        let top = summary.places.iter().max_by(|a, b| {
            a.hits
                .cmp(&b.hits)
                .then_with(|| b.place_id.cmp(&a.place_id))
        });
        let Some(top) = top else {
            return Ok(SearchPreview {
                html: format!("<section aria-label=\"Sample results\"><h2>Sample results</h2>{demo}<p>No matching newspaper pages.</p></section>"),
                total_hits: summary.total_hits,
            });
        };
        let place = rd
            .place(&top.place_id)
            .map(|p| format!("{}, {}", p.name, p.state))
            .unwrap_or_else(|| top.place_id.clone());
        let page = HitsQuery {
            place_id: Some(top.place_id.clone()),
            limit: 10,
            ..HitsQuery::default()
        };
        let result = with_timeout(
            &st,
            st.config.compute_cap,
            snap.backend
                .hits(&indexes, &search.query, &search.filters, &page),
        )
        .await?;
        let mut html = format!("<section aria-label=\"Sample results\"><h2>Sample results</h2>{demo}<p>{} matching newspaper pages in total. {} has the most hits ({} pages). Showing up to 10 earliest results from this city.</p><ol>", summary.total_hits, escape_html(&place), result.total);
        for hit in result.hits {
            let title = rd
                .titles
                .get(&hit.lccn)
                .map_or(hit.lccn.as_str(), |title| title.name.as_str());
            let date = date_from_day(hit.day);
            let label = format!("{} — {date}, page {}", escape_html(title), hit.seq);
            let citation = match (!rd.current.synthetic)
                .then(|| PageKey::from_doc_id(&hit.doc_id).ok())
                .flatten()
                .map(|key| key.viewer_url(None))
            {
                Some(url) => format!(r#"<a href="{}">{label}</a>"#, escape_html(&url)),
                None => label,
            };
            html.push_str(&format!("<li><p>{citation}</p>"));
            html.push_str(&preview_sources(&hit, rd));
            for snippet in hit.snippets {
                // Backend snippets already escape text; only highlight tags are markup.
                html.push_str(&format!(
                    "<p>{}</p>",
                    snippet.replace("<mark>", "").replace("</mark>", "")
                ));
            }
            html.push_str("</li>");
        }
        html.push_str(
            "</ol><p>The interactive map and full results require JavaScript.</p></section>",
        );
        Ok::<_, ApiError>(SearchPreview {
            html,
            total_hits: summary.total_hits,
        })
    };
    let job = Job::search("site-preview", background, state.config.compute_cap);
    let (_, body) = cached_body(
        state,
        job,
        key,
        &Pinning::Unversioned,
        &version,
        "/search",
        &canonical,
        compute,
    )
    .await?;
    let body = body.ok_or(ApiError::Busy)?;
    serde_json::from_slice(&body)
        .map_err(|error| ApiError::Backend(format!("invalid search preview cache: {error}")))
}

fn preview_sources(hit: &usnm_search::Hit, rd: &crate::refdata::RefData) -> String {
    let mut html = String::new();
    if let Some(ocr) = crate::routes::ocr_for_hit(hit, rd) {
        let href = ocr.engine.as_deref().map_or_else(
            || "/credits".to_owned(),
            |engine| {
                let fragment: String = form_urlencoded::byte_serialize(engine.as_bytes()).collect();
                format!("/credits#{}", fragment.replace('+', "%20"))
            },
        );
        html.push_str(&format!("<p>Our OCR: machine-read text replacing missing or poorly read Library of Congress text. Expect some misread characters. <a href=\"{}\">Engine and model credits</a>.</p>", escape_html(&href)));
    }
    let matched = hit.matched_in.as_deref().unwrap_or_default();
    if matched == [usnm_search::SNIPPETS_FROM_AMERICAN_STORIES] {
        html.push_str("<p>The Library of Congress text does not contain the match. It was found in American Stories (Dell et al. 2023), a second machine reading of the scans. <a href=\"/credits\">OCR credits</a>.</p>");
    }
    if matched.contains(&usnm_search::SNIPPETS_FROM_LOC_ORIGINAL)
        && !matched.contains(&usnm_search::MATCHED_IN_LOC)
    {
        html.push_str("<p>Our OCR text does not contain the match. It was found in the original Library of Congress OCR text retained beside ours.</p>");
    }
    match hit.snippet_source {
        Some(usnm_search::SNIPPETS_FROM_AMERICAN_STORIES) => {
            html.push_str("<p>These excerpts come from American Stories OCR.</p>");
        }
        Some(usnm_search::SNIPPETS_FROM_LOC_ORIGINAL) => {
            html.push_str("<p>These excerpts come from the original Library of Congress OCR.</p>");
        }
        _ => {}
    }
    html
}

/// A path with no file behind it: `index.html` for app routes (404 unless
/// the app has a page there), else a plain 404.
async fn app_route(req: Request, index: PathBuf, state: Arc<AppState>) -> Response {
    let path = req.uri().path();
    // The last segment ignoring a trailing slash, so `/favicon.svg/` is a
    // missing file (a plain 404), not an app route.
    let last = path
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or_default();
    if path.starts_with("/assets/") || (last.contains('.') && !is_app_page(path)) {
        // HEAD gets the status without a body.
        if req.method() == Method::HEAD {
            return StatusCode::NOT_FOUND.into_response();
        }
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    if let Some(location) = datasets_redirect(req.uri()) {
        return match HeaderValue::from_str(&location) {
            Ok(location) => (
                StatusCode::MOVED_PERMANENTLY,
                [(header::LOCATION, location)],
            )
                .into_response(),
            Err(_) => (StatusCode::BAD_REQUEST, "bad request").into_response(),
        };
    }
    let status = if is_app_page(path) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    let mut body = match tokio::fs::read(&index).await {
        Ok(body) => social::shell(req.uri(), shell_for(path, body)),
        Err(e) => {
            tracing::error!(error = %e, path = %index.display(), "site index missing");
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    };
    if status == StatusCode::OK {
        if let Some(q) = search_query(path) {
            // Only admitted base searches add backend work on a page load.
            if !noindex(req.uri(), &state.sitemap) {
                match search_preview(&state, &q).await {
                    Ok(preview) => body = search_landing(body, &preview),
                    Err(error) => {
                        tracing::warn!("search page preview unavailable");
                        return error.into_response();
                    }
                }
            }
        } else if matches!(path, "/" | "/index.html") {
            body = insert_content(body, &curated_links());
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/html; charset=utf-8"),
    );
    // A page gets a validator; the not-found shell doesn't, since a 404
    // ignores preconditions (RFC 9110 §13.2.1).
    if status == StatusCode::OK {
        let etag = page_etag(&body);
        if not_modified(req.headers(), &etag) {
            // The 304 carries what a cache keeps from it, as the 200 would,
            // `Vary` included: the compression layer adds `Accept-Encoding`
            // to the 200 but never sees this empty body. (CORS adds its own.)
            return (
                StatusCode::NOT_MODIFIED,
                [
                    (header::ETAG, etag),
                    (header::CACHE_CONTROL, HeaderValue::from_static("no-cache")),
                    (header::VARY, HeaderValue::from_static("accept-encoding")),
                ],
            )
                .into_response();
        }
        headers.insert(header::ETAG, etag);
    }
    // HEAD: the headers GET would send, without sending the body.
    if req.method() == Method::HEAD {
        headers.insert(header::CONTENT_LENGTH, HeaderValue::from(body.len()));
        return (status, headers).into_response();
    }
    (status, headers, body).into_response()
}

/// A page's validator, from the bytes it sends, so every replica serving
/// one build sends the same one. Weak, because the compression layer sends
/// those bytes in more than one encoding.
fn page_etag(body: &[u8]) -> HeaderValue {
    let digest = Sha256::digest(body);
    let tag = format!(
        "W/\"{:016x}\"",
        u64::from_be_bytes(digest[..8].try_into().expect("8 bytes"))
    );
    HeaderValue::from_str(&tag).expect("hex is a valid header value")
}

/// Whether `If-None-Match` names `etag` (or is `*`), compared weakly as
/// RFC 9110 §13.1.2 asks: `W/"x"` and `"x"` match.
fn not_modified(headers: &HeaderMap, etag: &HeaderValue) -> bool {
    let Ok(ours) = etag.to_str() else {
        return false;
    };
    let ours = ours.strip_prefix("W/").unwrap_or(ours);
    headers
        .get_all(header::IF_NONE_MATCH)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .any(|v| v.trim() == "*" || entity_tags(v).any(|t| t == ours))
}

/// The tags in an `If-None-Match` list, each with its quotes and without
/// `W/`, up to anything malformed. A tag can hold commas (and `*`), so the
/// list is read tag by tag, not split.
fn entity_tags(list: &str) -> impl Iterator<Item = &str> {
    let mut rest = list;
    std::iter::from_fn(move || {
        rest = rest.trim_start_matches(|c: char| c == ',' || c.is_ascii_whitespace());
        let tag = rest.strip_prefix("W/").unwrap_or(rest);
        // The closing quote, counting the opening one.
        let end = tag.strip_prefix('"')?.find('"')? + 2;
        let (opaque, after) = tag.split_at(end);
        rest = after;
        Some(opaque)
    })
}

async fn cache_control(req: Request, next: Next) -> Response {
    let basemap = req.uri().path().starts_with("/basemap/");
    let archive = basemap && req.uri().path().ends_with(".pmtiles.bin");
    let immutable = req.uri().path().starts_with("/assets/") || basemap;
    let mut resp = next.run(req).await;
    let html = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
    // A missing file: a cache that kept it (the edge's, for one) could go on
    // answering 404 for a file the next deploy brings.
    if resp.status() == StatusCode::NOT_FOUND && !html {
        resp.headers_mut()
            .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        return resp;
    }
    // The 404 app shell names this release's assets, like the 200 one.
    if !resp.status().is_success() && !(html && resp.status() == StatusCode::NOT_FOUND) {
        return resp;
    }
    if resp.headers().contains_key(header::CACHE_CONTROL) {
        return resp;
    }
    if archive && resp.status().is_success() {
        resp.headers_mut().insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/vnd.pmtiles"),
        );
    }
    let value = if immutable && !html {
        "public, max-age=31536000, immutable"
    } else if html {
        "no-cache"
    } else {
        "public, max-age=3600"
    };
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static(value));
    resp
}

/// Only curated or promoted, unfiltered searches are indexable. Legacy search links
/// and the old dataset path keep their noindex policy.
fn noindex(uri: &Uri, sitemap: &sitemap::Service) -> bool {
    if let Some(q) = search_query(uri.path()) {
        let path = search_path(&q);
        return !(CURATED_SEARCHES.contains(&path) || sitemap.contains(&path))
            || uri.query().is_some_and(|q| !q.is_empty());
    }
    match uri.path().trim_end_matches('/') {
        OLD_DATASETS_PATH => true,
        "" | "/index.html" => uri
            .query()
            .is_some_and(|q| form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "q")),
        _ => false,
    }
}

async fn robots_tag(State(state): State<Arc<AppState>>, req: Request, next: Next) -> Response {
    let noindex = noindex(req.uri(), &state.sitemap);
    let mut resp = next.run(req).await;
    if noindex {
        resp.headers_mut().insert(
            header::HeaderName::from_static("x-robots-tag"),
            HeaderValue::from_static("noindex"),
        );
    }
    resp
}

/// Redirects requests for `www.{host}` to `https://{host}`, keeping the path
/// and query (301). `host` is the site's canonical hostname, lowercase.
pub async fn www_redirect(State(host): State<Arc<str>>, req: Request, next: Next) -> Response {
    // HTTP/2 carries the host in the URI's authority, HTTP/1.1 in `Host`.
    let requested = req.uri().host().or_else(|| {
        req.headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            .map(|h| h.split(':').next().unwrap_or(h))
    });
    let is_www = requested.is_some_and(|h| {
        let (h, host) = (h.as_bytes(), host.as_bytes());
        h.len() == host.len() + 4
            && h[..4].eq_ignore_ascii_case(b"www.")
            && h[4..].eq_ignore_ascii_case(host)
    });
    if !is_www {
        return next.run(req).await;
    }
    let path = req.uri().path_and_query().map_or("/", |p| p.as_str());
    let location = format!("https://{host}{path}");
    match HeaderValue::from_str(&location) {
        Ok(location) => (
            StatusCode::MOVED_PERMANENTLY,
            [(header::LOCATION, location)],
        )
            .into_response(),
        Err(_) => (StatusCode::BAD_REQUEST, "bad request").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn previews_match_the_default_interactive_search_mode() {
        use usnm_core::params::RawParams;

        for (q, expected) in [
            ("Spanish influenza", r#""spanish influenza""#),
            ("gold OR silver", "gold OR silver"),
            (r#""cross of gold""#, r#""cross of gold""#),
            ("copperhead*", "copperhead*"),
        ] {
            let raw = RawParams::parse(&preview_query(q)).unwrap();
            let query = usnm_core::query::build(
                raw.get("q").unwrap(),
                raw.get("mode")
                    .map(|mode| usnm_core::query::Mode::parse(mode).unwrap()),
                0,
                0,
                usnm_core::text::Analyzer::default(),
            )
            .unwrap();
            assert_eq!(query.to_string(), expected, "{q}");
        }
    }

    #[test]
    fn connect_src_allows_only_the_api_basemap_and_boinc() {
        let csp = SECURITY_HEADERS
            .iter()
            .find(|(name, _)| *name == "content-security-policy")
            .unwrap()
            .1;
        let connect = csp
            .split(';')
            .map(str::trim)
            .find(|directive| directive.starts_with("connect-src "))
            .unwrap();
        assert_eq!(
            connect,
            "connect-src 'self' https://tiles.openfreemap.org https://boinc.usnewsmap.com"
        );
        assert!(!csp.contains("blob.core.windows.net"));
    }

    #[test]
    fn hsts_is_short_and_for_this_host_alone() {
        let hsts = SECURITY_HEADERS
            .iter()
            .find(|(name, _)| *name == "strict-transport-security")
            .unwrap()
            .1;
        assert_eq!(hsts, "max-age=300");
        let lower = hsts.to_ascii_lowercase();
        assert!(!lower.contains("includesubdomains") && !lower.contains("preload"));
    }

    #[test]
    fn if_none_match_compares_weakly_and_takes_a_list() {
        let etag = page_etag(b"<!doctype html>");
        let tag = etag.to_str().unwrap();
        assert!(tag.starts_with("W/\"") && tag.len() == 2 + 16 + 2, "{tag}");
        assert_eq!(page_etag(b"<!doctype html>"), etag);
        assert_ne!(page_etag(b"<!doctype html> "), etag);
        let strong = tag.trim_start_matches("W/");
        let matches = |values: &[&str]| {
            let mut h = HeaderMap::new();
            for v in values {
                h.append(header::IF_NONE_MATCH, HeaderValue::from_str(v).unwrap());
            }
            not_modified(&h, &etag)
        };
        assert!(matches(&[tag]));
        assert!(matches(&[strong]));
        assert!(matches(&["*"]));
        assert!(matches(&[&format!("\"other\", {tag}")]));
        assert!(matches(&[&format!("\"a,b\",W/\"c\" , {strong}")]));
        assert!(matches(&["\"other\"", tag]));
        assert!(!matches(&[]));
        assert!(!matches(&["\"other\""]));
        assert!(!matches(&["W/\"0000000000000000\""]));
        // One tag that holds commas and `*`, not a list with `*` in it.
        assert!(!matches(&["\"old,*,release\""]));
        // Nothing after a malformed entry counts.
        assert!(!matches(&[&format!("garbage, {tag}")]));
        assert!(!matches(&[&format!("\"unclosed, {tag}")]));
    }

    #[test]
    fn app_pages() {
        for path in [
            "/",
            "//",
            "/datasets",
            "/datasets/",
            "/privacy",
            "/privacy/",
            "/credits",
            "/credits/",
            "/index.html",
            "/index.html/",
            "/search/gold",
            "/search/%22cross%20of%20gold%22/",
            "/search/a%2Fb",
        ] {
            assert!(is_app_page(path), "{path}");
        }
        for path in [
            "/search/",
            "/search/gold/extra",
            "/search/%FF",
            "/search/%",
            "/statuses",
            "/does-not-exist",
            "/status",
            "/status/",
            "/datasets/x",
            "/status/x",
            "/privacy/x",
            "/index.html/x",
            "/favicon.svg/",
        ] {
            assert!(!is_app_page(path), "{path}");
        }
    }

    #[test]
    fn the_datasets_page_names_itself_in_the_head() {
        let index = br#"<title>US News Map</title><link rel="canonical" href="https://usnewsmap.com/" /><meta property="og:url" content="https://usnewsmap.com/" />"#;
        let html = String::from_utf8(shell_for("/datasets/", index.to_vec())).unwrap();
        assert!(html.contains("<title>Dataset details · US News Map</title>"));
        assert!(html.contains(r#"href="https://usnewsmap.com/datasets""#));
        assert!(html.contains(r#"content="https://usnewsmap.com/datasets""#));
    }

    #[test]
    fn the_credits_page_names_itself_in_the_head() {
        let index = br#"<title>US News Map</title><link rel="canonical" href="https://usnewsmap.com/" /><meta property="og:url" content="https://usnewsmap.com/" />"#;
        let html = String::from_utf8(shell_for("/credits/", index.to_vec())).unwrap();
        assert!(html.contains("<title>OCR credits · US News Map</title>"));
        assert!(html.contains(r#"href="https://usnewsmap.com/credits""#));
        assert!(html.contains(r#"content="https://usnewsmap.com/credits""#));
    }

    #[test]
    fn the_privacy_page_names_itself_in_the_head() {
        let index = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/index.html"),
        )
        .unwrap();
        let home = String::from_utf8(shell_for("/", index.clone().into_bytes())).unwrap();
        assert_eq!(home, index);
        let privacy = String::from_utf8(shell_for("/privacy/", index.into_bytes())).unwrap();
        assert!(privacy.contains("<title>Privacy · US News Map</title>"));
        assert!(
            privacy.contains(r#"<link rel="canonical" href="https://usnewsmap.com/privacy" />"#)
        );
        assert!(privacy
            .contains(r#"<meta property="og:url" content="https://usnewsmap.com/privacy" />"#));
        assert!(!privacy.contains(r#"href="https://usnewsmap.com/" />"#));
    }

    #[test]
    fn noindex_pages() {
        let yes = [
            "/?q=gold",
            "/?from=1896-06-01&q=%22cross+of+gold%22",
            "/status",
            "/status/?x=1",
            "/search/whigs?tab=table",
            "/search/not-a-curated-search",
        ];
        for uri in yes {
            assert!(
                noindex(&uri.parse().unwrap(), &sitemap::Service::default()),
                "{uri}"
            );
        }
        let no = [
            "/",
            "/?",
            "/?t=1896-07-01",
            "/?qq=1",
            "/privacy",
            "/privacy?utm_source=x",
            "/datasets",
            "/datasets/?x=1",
            "/credits",
            "/favicon.svg",
            "/search?q=gold",
            "/search/whigs",
        ];
        for uri in no {
            assert!(
                !noindex(&uri.parse().unwrap(), &sitemap::Service::default()),
                "{uri}"
            );
        }
    }

    #[test]
    fn search_paths_preserve_syntax_and_legacy_filters() {
        for q in ["gold", "gold OR silver", "a/b + c? & # = %", "日本"] {
            assert_eq!(search_query(&search_path(q)).as_deref(), Some(q));
        }
        let to = |uri: &str| search_redirect(&uri.parse().unwrap());
        assert_eq!(
            to("/?q=%22cross+of+gold%22&from=1896-06-01&tab=table").as_deref(),
            Some("/search/cross-of-gold?from=1896-06-01&tab=table")
        );
        assert_eq!(to("/index.html/?q=a%2Bb").as_deref(), Some("/search/a%2Bb"));
        assert_eq!(to("/search/gold?q=silver").as_deref(), Some("/search/gold"));
        assert_eq!(to("/search/gold/").as_deref(), Some("/search/gold"));
        for uri in [
            "/search/gold",
            "/?q=",
            "/?q=.",
            "/privacy?q=gold",
            "/v1/hits?q=gold",
        ] {
            assert_eq!(to(uri), None, "{uri}");
        }
    }

    #[test]
    fn plain_phrases_get_readable_lowercase_paths() {
        for (q, path) in [
            ("\"Fifteenth Amendment\"", "/search/fifteenth-amendment"),
            ("Know-Nothing", "/search/know-nothing"),
            ("gold", "/search/gold"),
            ("\"gold OR silver\"", "/search/gold-or-silver"),
            ("gold OR silver", "/search/gold%20OR%20silver"),
            ("-gold", "/search/-gold"),
            (
                "\"Uncle Tom's Cabin\"",
                "/search/%22Uncle%20Tom%27s%20Cabin%22",
            ),
        ] {
            assert_eq!(search_path(q), path, "{q}");
        }
        assert_eq!(
            search_query("/search/fifteenth-amendment").as_deref(),
            Some("fifteenth amendment")
        );
        assert_eq!(
            search_query("/search/Know-Nothing").as_deref(),
            Some("Know-Nothing")
        );
        assert_eq!(
            search_path_in_mode("\"cross of gold\"", false),
            "/search/%22cross%20of%20gold%22"
        );

        let to = |uri: &str| search_redirect(&uri.parse().unwrap());
        assert_eq!(
            to("/search/%22Fifteenth%20Amendment%22").as_deref(),
            Some("/search/fifteenth-amendment")
        );
        assert_eq!(
            to("/search/Know-Nothing?tab=table").as_deref(),
            Some("/search/know-nothing?tab=table")
        );
        assert_eq!(to("/search/fifteenth-amendment"), None);
        assert_eq!(to("/search/%22cross%20of%20gold%22?mode=all"), None,);
        assert_eq!(to("/search/%E6%97%A5%E6%9C%AC"), None);
        assert_eq!(search_path("İstanbul"), "/search/%C4%B0stanbul");
        assert_eq!(to("/search/%C4%B0stanbul"), None);
        assert_eq!(search_path("日本"), "/search/%E6%97%A5%E6%9C%AC");
        assert_eq!(to("/search/well-known?mode=all"), None);
        assert_eq!(
            search_query_in_mode("/search/well-known", false).as_deref(),
            Some("well-known")
        );
    }

    #[test]
    fn search_metadata_is_specific_and_html_escaped() {
        let index = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/index.html"),
        )
        .unwrap();
        let html = String::from_utf8(shell_for(&search_path("<b> & \"gold\""), index)).unwrap();
        assert!(html.contains("<title>&lt;b&gt; &amp; &quot;gold&quot; · US News Map</title>"));
        assert!(html.contains("https://usnewsmap.com/search/%3Cb%3E%20%26%20%22gold%22"));
        assert!(html.contains("<h1>Newspaper results for &lt;b&gt; &amp; &quot;gold&quot;</h1>"));
    }

    #[test]
    fn the_old_status_path_redirects_to_datasets() {
        let to = |uri: &str| datasets_redirect(&uri.parse().unwrap());
        assert_eq!(to("/status").as_deref(), Some("/datasets"));
        assert_eq!(to("/status/").as_deref(), Some("/datasets"));
        assert_eq!(to("/status?x=1&y=2").as_deref(), Some("/datasets?x=1&y=2"));
        for uri in ["/datasets", "/status/x", "/statuses", "/", "/v1/status"] {
            assert_eq!(to(uri), None, "{uri}");
        }
    }
}
