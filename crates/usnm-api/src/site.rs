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
//! - Search permalinks (`/?q=…`) and `/datasets` carry `X-Robots-Tag: noindex`,
//!   so crawlers that don't run the app see it too.
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

/// Headers on every response (the site's and the API's). `connect-src`
/// allows the same-origin API, the OpenFreeMap basemap and public BOINC counts.
pub const SECURITY_HEADERS: [(&str, &str); 4] = [
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
];

/// The longest file name the filesystem takes (Linux `NAME_MAX`).
const NAME_MAX: usize = 255;

/// The longest name `ServeDir` can look up: it also probes `<name>.br` and
/// `<name>.gz`, which must fit `NAME_MAX` too. The site has no names near it.
const NAME_LIMIT: usize = NAME_MAX - ".br".len();

/// The site as a service for the API router's fallback.
pub fn router(dir: PathBuf) -> Router {
    let index = dir.join("index.html");
    let spa = {
        let index = index.clone();
        move |req: Request| {
            let index = index.clone();
            async move { app_route(req, index).await }
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
    let long_names = move |req: Request, next: Next| {
        let index = index.clone();
        async move {
            if req
                .uri()
                .path()
                .split('/')
                .any(|s| decoded_len(s) > NAME_LIMIT)
            {
                app_route(req, index).await
            } else {
                next.run(req).await
            }
        }
    };
    Router::new()
        .fallback_service(files)
        .layer(middleware::from_fn(long_names))
        .layer(middleware::from_fn(cache_control))
        .layer(middleware::from_fn(robots_tag))
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
    matches!(
        path.trim_end_matches('/'),
        "" | "/datasets" | "/privacy" | "/credits" | "/index.html"
    )
}

/// The dataset details page's path before it was renamed (#294).
const OLD_DATASETS_PATH: &str = "/status";

/// Where a request for `/status` goes: `/datasets`, keeping the query.
fn datasets_redirect(uri: &Uri) -> Option<String> {
    (uri.path().trim_end_matches('/') == OLD_DATASETS_PATH).then(|| match uri.query() {
        Some(q) => format!("/datasets?{q}"),
        None => "/datasets".to_owned(),
    })
}

/// Indexable static pages name themselves in the head, not the home page.
fn shell_for(path: &str, index: Vec<u8>) -> Vec<u8> {
    let (page, title) = match path.trim_end_matches('/') {
        "/privacy" => ("privacy", "Privacy"),
        "/credits" => ("credits", "OCR credits"),
        _ => return index,
    };
    let Ok(html) = String::from_utf8(index) else {
        return Vec::new();
    };
    html.replacen(
        "<title>US News Map</title>",
        &format!("<title>{title} · US News Map</title>"),
        1,
    )
    .replacen(
        r#"<link rel="canonical" href="https://usnewsmap.com/" />"#,
        &format!(r#"<link rel="canonical" href="https://usnewsmap.com/{page}" />"#),
        1,
    )
    .replacen(
        r#"<meta property="og:url" content="https://usnewsmap.com/" />"#,
        &format!(r#"<meta property="og:url" content="https://usnewsmap.com/{page}" />"#),
        1,
    )
    .into_bytes()
}

/// A path with no file behind it: `index.html` for app routes (404 unless
/// the app has a page there), else a plain 404.
async fn app_route(req: Request, index: PathBuf) -> Response {
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
    let body = match tokio::fs::read(&index).await {
        Ok(body) => shell_for(path, body),
        Err(e) => {
            tracing::error!(error = %e, path = %index.display(), "site index missing");
            return (StatusCode::NOT_FOUND, "not found").into_response();
        }
    };
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
    let immutable = req.uri().path().starts_with("/assets/");
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
    let value = if immutable {
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

/// Pages that shouldn't appear in search results: search permalinks
/// (`/?q=…`), which are endless variations on the home page, and the
/// dataset details page, which changes often (and its old path, `/status`).
fn noindex(uri: &Uri) -> bool {
    match uri.path().trim_end_matches('/') {
        "/datasets" | OLD_DATASETS_PATH => true,
        "" | "/index.html" => uri
            .query()
            .is_some_and(|q| form_urlencoded::parse(q.as_bytes()).any(|(k, _)| k == "q")),
        _ => false,
    }
}

async fn robots_tag(req: Request, next: Next) -> Response {
    let noindex = noindex(req.uri());
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
        ] {
            assert!(is_app_page(path), "{path}");
        }
        for path in [
            "/search/gold",
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
            "/datasets",
            "/datasets/?x=1",
            "/status",
            "/status/?x=1",
        ];
        for uri in yes {
            assert!(noindex(&uri.parse().unwrap()), "{uri}");
        }
        let no = [
            "/",
            "/?",
            "/?t=1896-07-01",
            "/?qq=1",
            "/privacy",
            "/privacy?utm_source=x",
            "/favicon.svg",
            "/search?q=gold",
        ];
        for uri in no {
            assert!(!noindex(&uri.parse().unwrap()), "{uri}");
        }
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
