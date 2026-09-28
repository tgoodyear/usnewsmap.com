//! The web app, served by the API from its built files (`USNM_SITE_DIR`,
//! ADR-0009): same origin as `/v1`, deployed in the same image.
//!
//! - Files come from the directory, preferring the `.br` or `.gz` copy the
//!   image build wrote next to each one.
//! - Paths that look like app routes (no file extension, not under
//!   `/assets/`) get `index.html`, so client-side routes survive a reload.
//!   Missing files are a plain 404.
//! - Hashed assets are cached for a year; `index.html` is revalidated on
//!   every load so a release shows up at once.

use std::path::PathBuf;

use axum::extract::Request;
use axum::handler::HandlerWithoutStateExt;
use axum::http::{header, HeaderValue, Method, StatusCode};
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
}

/// A path with no file behind it: `index.html` for app routes, else 404.
async fn app_route(req: Request, index: PathBuf) -> Response {
    let path = req.uri().path();
    let last = path.rsplit('/').next().unwrap_or_default();
    if path.starts_with("/assets/") || last.contains('.') {
        return (StatusCode::NOT_FOUND, "not found").into_response();
    }
    if !matches!(*req.method(), Method::GET | Method::HEAD) {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    match tokio::fs::read(&index).await {
        Ok(body) => ([(header::CONTENT_TYPE, "text/html; charset=utf-8")], body).into_response(),
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
