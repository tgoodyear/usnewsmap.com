//! The web app, served by the API from its built files (`USNM_SITE_DIR`,
//! ADR-0009): same origin as `/v1`, deployed in the same image.
//!
//! - Files come from the directory, preferring the `.br` or `.gz` copy the
//!   image build wrote next to each one.
//! - Paths that look like app routes (no file extension, not under
//!   `/assets/`) get `index.html`, so client-side routes survive a reload.
//!   The app's own pages (`/`, `/status`) are 200; any other such path gets
//!   the same shell with a 404, and the app shows a not-found page. Missing
//!   files are a plain 404.
//! - Hashed assets are cached for a year; `index.html` is revalidated on
//!   every load so a release shows up at once.
//! - Search permalinks (`/?q=…`) and `/status` carry `X-Robots-Tag: noindex`,
//!   so crawlers that don't run the app see it too.
//! - Requests for `www.` + the site's host are redirected to the host itself.

use std::path::PathBuf;
use std::sync::Arc;

use axum::extract::{Request, State};
use axum::handler::HandlerWithoutStateExt;
use axum::http::{header, HeaderValue, Method, StatusCode, Uri};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::Router;
use tower_http::services::ServeDir;

/// Headers on every response (the site's and the API's). `connect-src`
/// needs only `'self'` for the API; the rest is the basemap and tiles.
pub const SECURITY_HEADERS: [(&str, &str); 4] = [
    (
        "content-security-policy",
        "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: blob: https:; font-src 'self' https://tiles.openfreemap.org; \
         connect-src 'self' https://*.blob.core.windows.net https://tiles.openfreemap.org; \
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

/// The site as a service for the API router's fallback.
pub fn router(dir: PathBuf) -> Router {
    let index = dir.join("index.html");
    let spa = move |req: Request| {
        let index = index.clone();
        async move { app_route(req, index).await }
    };
    let files = ServeDir::new(dir)
        .precompressed_br()
        .precompressed_gzip()
        .fallback(spa.into_service());
    Router::new()
        .fallback_service(files)
        .layer(middleware::from_fn(cache_control))
        .layer(middleware::from_fn(robots_tag))
}

/// The paths the app renders a page for (`web/src/route.ts`), with or
/// without a trailing slash.
fn is_app_page(path: &str) -> bool {
    matches!(path.trim_end_matches('/'), "" | "/status")
}

/// A path with no file behind it: `index.html` for app routes (404 unless
/// the app has a page there), else a plain 404.
async fn app_route(req: Request, index: PathBuf) -> Response {
    let path = req.uri().path();
    let last = path.rsplit('/').next().unwrap_or_default();
    if path.starts_with("/assets/") || last.contains('.') {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let status = if is_app_page(path) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    };
    let html = (header::CONTENT_TYPE, "text/html; charset=utf-8".to_owned());
    // HEAD: the headers GET would send, without reading or sending the body.
    if req.method() == Method::HEAD {
        return match tokio::fs::metadata(&index).await {
            Ok(meta) => (
                status,
                [html, (header::CONTENT_LENGTH, meta.len().to_string())],
            )
                .into_response(),
            Err(e) => {
                tracing::error!(error = %e, path = %index.display(), "site index missing");
                (StatusCode::NOT_FOUND, "not found").into_response()
            }
        };
    }
    match tokio::fs::read(&index).await {
        Ok(body) => (status, [html], body).into_response(),
        Err(e) => {
            tracing::error!(error = %e, path = %index.display(), "site index missing");
            (StatusCode::NOT_FOUND, "not found").into_response()
        }
    }
}

async fn cache_control(req: Request, next: Next) -> Response {
    let immutable = req.uri().path().starts_with("/assets/");
    let mut resp = next.run(req).await;
    if !resp.status().is_success() {
        return resp;
    }
    let html = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| v.starts_with("text/html"));
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
/// (`/?q=…`), which are endless variations on the home page, and `/status`.
fn noindex(uri: &Uri) -> bool {
    match uri.path().trim_end_matches('/') {
        "/status" => true,
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
    fn app_pages() {
        for path in ["/", "//", "/status", "/status/"] {
            assert!(is_app_page(path), "{path}");
        }
        for path in ["/search/gold", "/statuses", "/does-not-exist", "/status/x"] {
            assert!(!is_app_page(path), "{path}");
        }
    }

    #[test]
    fn noindex_pages() {
        let yes = [
            "/?q=gold",
            "/?from=1896-06-01&q=%22cross+of+gold%22",
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
            "/favicon.svg",
            "/search?q=gold",
        ];
        for uri in no {
            assert!(!noindex(&uri.parse().unwrap()), "{uri}");
        }
    }
}
