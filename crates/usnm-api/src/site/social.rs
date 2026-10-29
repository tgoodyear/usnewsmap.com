//! Server-rendered sharing metadata, browser screenshots of search views,
//! and lightweight PNG cards for static pages.

use std::sync::{Arc, LazyLock};

use axum::extract::OriginalUri;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use resvg::{tiny_skia, usvg};
use sha2::{Digest, Sha256};

mod cache;
use crate::AppState;
pub(crate) use cache::Service;

const SITE: &str = "https://usnewsmap.com";
const IMAGE: &str = "/share-image.png";
const FALLBACK: &[u8] = include_bytes!("../../../../web/public/og-image.png");
const RESPONSE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(2);
const VIEW_KEYS: &[&str] = &[
    "q", "source", "mode", "near", "from", "to", "state", "lang", "lccn", "series", "t", "win",
    "bucket", "layer", "norm", "place", "sort", "tab", "z", "c",
];

static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_font_data(include_bytes!("../../assets/NotoSans-Regular.ttf").to_vec());
    Arc::new(fonts)
});

#[derive(Clone)]
struct Card {
    title: String,
    label: &'static str,
    details: String,
    url: String,
    image: String,
}

fn clean(value: &str, limit: usize) -> String {
    value
        .chars()
        .take(limit)
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect()
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

impl Card {
    fn new(path: &str, query: &str) -> Option<Self> {
        let phrase_mode =
            form_urlencoded::parse(query.as_bytes()).all(|(k, v)| k != "mode" || v == "phrase");
        let search = super::search_query_in_mode(path, phrase_mode);
        let search_path = search
            .as_ref()
            .map(|q| super::search_path_in_mode(q, phrase_mode));
        let path = if search.is_some() { "/" } else { path };
        let path = match path.trim_end_matches('/') {
            "" | "/index.html" => "/",
            "/datasets" => "/datasets",
            "/privacy" => "/privacy",
            _ => return None,
        };
        let mut params = Vec::new();
        if let Some(q) = search {
            params.push(("q".to_owned(), q));
        }
        for (key, value) in form_urlencoded::parse(query.as_bytes()) {
            if VIEW_KEYS.contains(&key.as_ref()) && !params.iter().any(|(k, _)| k == &key) {
                // The UI accepts 60 LCCNs of up to 16 characters each.
                let limit = if key == "lccn" { 60 * 17 - 1 } else { 256 };
                params.push((key.into_owned(), clean(&value, limit)));
            }
        }
        let get = |key: &str| {
            params
                .iter()
                .find(|(k, _)| k == key)
                .map_or("", |(_, v)| v.as_str())
        };
        let (title, label, details) = match path {
            "/privacy" => (
                "Privacy".to_owned(),
                "US NEWS MAP",
                "How US News Map handles your data and respects your privacy.".to_owned(),
            ),
            "/datasets" => (
                "Dataset details".to_owned(),
                "US NEWS MAP",
                "Explore the newspaper collections, coverage, and data behind US News Map."
                    .to_owned(),
            ),
            _ => {
                if params.is_empty() {
                    return None;
                }
                let congress = get("source") == "congress";
                let title = if get("q").trim().is_empty() {
                    "Explore the archive".to_owned()
                } else {
                    get("q").to_owned()
                };
                let mut details = Vec::new();
                let date = |key: &str| {
                    let v = get(key);
                    (v.len() == 10 && chrono::NaiveDate::parse_from_str(v, "%Y-%m-%d").is_ok())
                        .then_some(v)
                };
                if date("from").is_some() || date("to").is_some() {
                    details.push(format!(
                        "{} to {}",
                        date("from").unwrap_or("earliest"),
                        date("to").unwrap_or("latest")
                    ));
                }
                let mode = match get("mode") {
                    "all" => "All words".to_owned(),
                    "any" => "Any word".to_owned(),
                    "near" => format!(
                        "Words near each other ({} words)",
                        get("near")
                            .parse::<u8>()
                            .ok()
                            .filter(|n| (1..=20).contains(n))
                            .unwrap_or(5)
                    ),
                    _ => "Exact phrase".to_owned(),
                };
                details.push(mode);
                for (key, label) in if congress {
                    &[("series", "Series")][..]
                } else {
                    &[
                        ("state", "States"),
                        ("lang", "Languages"),
                        ("lccn", "Newspapers"),
                    ][..]
                } {
                    if !get(key).is_empty() {
                        details.push(format!("{label}: {}", get(key)));
                    }
                }
                for (key, label) in [
                    ("place", "Place"),
                    ("t", "Timeline"),
                    ("win", "Window"),
                    ("norm", "Measure"),
                    ("layer", "Layer"),
                    ("tab", "View"),
                ] {
                    if !get(key).is_empty() {
                        details.push(format!("{label}: {}", get(key)));
                    }
                }
                (
                    title,
                    if congress {
                        "SEARCH THE RECORDS OF CONGRESS"
                    } else {
                        "SEARCH HISTORIC NEWSPAPERS"
                    },
                    details.join(" | "),
                )
            }
        };
        let mut page_query = form_urlencoded::Serializer::new(String::new());
        // Static pages do not use search state or tracking parameters.
        if path == "/" {
            for key in VIEW_KEYS {
                if let Some((_, value)) = params.iter().find(|(k, _)| k == key) {
                    page_query.append_pair(key, value);
                }
            }
        }
        let query = page_query.finish();
        let mut url = format!(
            "{SITE}{path}{}{}",
            if query.is_empty() { "" } else { "?" },
            query
        );
        if let Some(path) = search_path {
            let query = form_urlencoded::Serializer::new(String::new())
                .extend_pairs(params.iter().filter(|(k, _)| k != "q"))
                .finish();
            url = format!(
                "{SITE}{path}{}{}",
                if query.is_empty() { "" } else { "?" },
                query
            );
        }
        let mut image_query = form_urlencoded::Serializer::new(String::new());
        image_query.append_pair("page", path);
        if !query.is_empty() {
            image_query.extend_pairs(form_urlencoded::parse(query.as_bytes()));
        }
        // Do not reuse previews cached before degraded basemaps were rejected.
        image_query.append_pair("v", "2");
        let image = format!("{SITE}{IMAGE}?{}", image_query.finish());
        Some(Self {
            title,
            label,
            details,
            url,
            image,
        })
    }

    fn description(&self) -> String {
        format!("{}: {}. {}", self.label, self.title, self.details)
    }
}

pub(super) fn shell(uri: &Uri, index: Vec<u8>) -> Vec<u8> {
    let Some(card) = Card::new(uri.path(), uri.query().unwrap_or_default()) else {
        return index;
    };
    let mut html = match String::from_utf8(index) {
        Ok(html) => html,
        Err(error) => {
            tracing::error!(%error, "site index is not UTF-8");
            return error.into_bytes();
        }
    };
    let title = escape(&format!("{} · US News Map", card.title));
    // Replace whole tags so quotes or tag-shaped text in a query cannot
    // become markup, or be touched by a subsequent replacement.
    for (attribute, name, value) in [
        ("property", "og:title", title.clone()),
        ("name", "twitter:title", title.clone()),
        ("property", "og:description", escape(&card.description())),
        ("name", "twitter:description", escape(&card.description())),
        ("property", "og:url", escape(&card.url)),
        ("property", "og:image", escape(&card.image)),
        ("name", "twitter:image", escape(&card.image)),
        ("property", "og:image:alt", escape(&card.description())),
        ("name", "twitter:image:alt", escape(&card.description())),
    ] {
        let marker = format!("{attribute}=\"{name}\"");
        let tag = html.match_indices("<meta").find_map(|(start, _)| {
            let end = start + html[start..].find('>')?;
            html[start..=end].contains(&marker).then_some(start..=end)
        });
        if let Some(tag) = tag {
            html.replace_range(tag, &format!("<meta {marker} content=\"{value}\" />"));
        }
    }
    html.into_bytes()
}

fn lines(text: &str, width: usize, count: usize) -> Vec<String> {
    let mut remaining: Vec<char> = text.chars().collect();
    let mut lines = Vec::new();
    while !remaining.is_empty() && lines.len() < count {
        let end = if remaining.len() <= width {
            remaining.len()
        } else {
            remaining[..width]
                .iter()
                .rposition(|c| c.is_whitespace())
                .filter(|i| *i > 0)
                .unwrap_or(width)
        };
        lines.push(remaining[..end].iter().collect::<String>());
        remaining.drain(..end);
        while remaining.first().is_some_and(|c| c.is_whitespace()) {
            remaining.remove(0);
        }
    }
    if !remaining.is_empty() {
        if let Some(last) = lines.last_mut() {
            last.pop();
            last.push('…');
        }
    }
    lines
}

fn png(card: &Card) -> Result<Vec<u8>, String> {
    let mut svg = String::from(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="1200" height="630">
        <rect width="1200" height="630" fill="#f7f3e8"/>
        <rect width="16" height="630" fill="#b24c32"/>
        <g font-family="Noto Sans" fill="#242b30">
        <text x="64" y="83" font-size="32">US NEWS MAP</text>
        <path d="M64 110H1136" stroke="#b24c32" stroke-width="2"/>
        "##,
    );
    svg.push_str(&format!(
        r#"<text x="64" y="157" font-size="20">{}</text>"#,
        escape(card.label)
    ));
    // 24 characters at 44px also fits a line of the font's widest glyphs.
    for (i, line) in lines(&card.title, 24, 3).iter().enumerate() {
        svg.push_str(&format!(
            r#"<text x="64" y="{}" font-size="44">{}</text>"#,
            225 + i * 60,
            escape(line)
        ));
    }
    for (i, line) in lines(&card.details, 44, 4).iter().enumerate() {
        svg.push_str(&format!(
            r#"<text x="64" y="{}" font-size="23">{}</text>"#,
            410 + i * 33,
            escape(line)
        ));
    }
    svg.push_str(r#"<text x="64" y="588" font-size="22">usnewsmap.com</text></g></svg>"#);
    let options = usvg::Options {
        fontdb: FONTS.clone(),
        ..Default::default()
    };
    let tree = usvg::Tree::from_str(&svg, &options).map_err(|e| e.to_string())?;
    let mut pixels = tiny_skia::Pixmap::new(1200, 630).ok_or("cannot allocate share image")?;
    resvg::render(
        &tree,
        tiny_skia::Transform::identity(),
        &mut pixels.as_mut(),
    );
    pixels.encode_png().map_err(|e| e.to_string())
}

fn script() -> std::path::PathBuf {
    std::env::var_os("USNM_SHARE_SCRIPT")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../web/scripts/render-share.mjs")
        })
}

async fn screenshot(card: &Card) -> Result<Vec<u8>, String> {
    let bind = std::env::var("USNM_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let port = bind.rsplit_once(':').ok_or("USNM_BIND has no port")?.1;
    let port: u16 = port.parse().map_err(|_| "USNM_BIND has an invalid port")?;
    // Only our own server is navigated, never an origin supplied by a visitor.
    let url = format!("http://127.0.0.1:{port}{}", &card.url[SITE.len()..]);
    let mut command = tokio::process::Command::new("node");
    command
        .arg(script())
        .arg("--url")
        .arg(url)
        .kill_on_drop(true);
    let output = tokio::time::timeout(std::time::Duration::from_secs(40), command.output())
        .await
        .map_err(|_| "share screenshot timed out")?
        .map_err(|_| "could not start share screenshot browser")?;
    if !output.status.success() {
        // Browser errors may contain the query; do not put them in telemetry.
        return Err("share screenshot browser failed".to_owned());
    }
    if !output.stdout.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Err("share screenshot browser did not return a PNG".to_owned());
    }
    Ok(output.stdout)
}

pub(super) async fn image(OriginalUri(uri): OriginalUri, state: Arc<AppState>) -> Response {
    let query = uri.query().unwrap_or_default();
    if query.len() > 8192 {
        return (StatusCode::BAD_REQUEST, "share image query is too long").into_response();
    }
    let path = form_urlencoded::parse(query.as_bytes())
        .find(|(key, _)| key == "page")
        .map(|(_, value)| value.into_owned())
        .unwrap_or_else(|| "/".to_owned());
    let Some(card) = Card::new(&path, query) else {
        return (
            StatusCode::BAD_REQUEST,
            "share image needs a supported page or search view",
        )
            .into_response();
    };
    // Continue warming if the crawler stops waiting, as before.
    let render = tokio::spawn(async move { cached(&state, card, false).await });
    render_response(render).await
}

/// Static pages must never substitute an unrelated generic sharing image.
pub(super) async fn search_map(OriginalUri(uri): OriginalUri, state: Arc<AppState>) -> Response {
    if uri.query().is_some_and(|query| query.len() > 8192) {
        return (StatusCode::BAD_REQUEST, "search map query is too long").into_response();
    }
    let params: Vec<_> =
        form_urlencoded::parse(uri.query().unwrap_or_default().as_bytes()).collect();
    let [(key, query)] = params.as_slice() else {
        return (StatusCode::BAD_REQUEST, "search map requires one query").into_response();
    };
    if key != "q" || query.len() > 2048 {
        return (StatusCode::BAD_REQUEST, "invalid search map query").into_response();
    }
    let path = super::search_path(query);
    let page: Uri = match path.parse() {
        Ok(page) => page,
        Err(_) => return StatusCode::BAD_REQUEST.into_response(),
    };
    if super::noindex(&page, &state.sitemap) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let Some(card) = Card::new(&path, "") else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let render = tokio::spawn(async move { cached(&state, card, false).await });
    map_response(render).await
}

async fn map_response(
    render: tokio::task::JoinHandle<Result<Vec<u8>, Arc<RenderError>>>,
) -> Response {
    match tokio::time::timeout(std::time::Duration::from_secs(45), render).await {
        Ok(Ok(Ok(body))) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=300"),
            ],
            body,
        )
            .into_response(),
        _ => {
            tracing::warn!("static search map unavailable");
            (
                StatusCode::SERVICE_UNAVAILABLE,
                [
                    (header::CACHE_CONTROL, "no-store"),
                    (header::RETRY_AFTER, "10"),
                ],
                "Search map is temporarily unavailable",
            )
                .into_response()
        }
    }
}

fn digest(bytes: impl AsRef<[u8]>) -> String {
    Sha256::digest(bytes.as_ref())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn identity(state: &AppState) -> Result<String, RenderError> {
    let frontend = state
        .previews
        .frontend
        .get_or_try_init(|| async {
            let dir =
                state.config.site_dir.as_ref().ok_or_else(|| {
                    RenderError::Failed("share site is not configured".to_owned())
                })?;
            frontend_identity(dir, &script()).await
        })
        .await?;
    let snap = state.snapshot.load();
    let current = serde_json::to_vec(&snap.refdata.current)
        .map_err(|_| RenderError::Failed("share snapshot cannot be serialized".to_owned()))?;
    Ok(digest(format!(
        "{frontend}\0{}\0{}\0{}\0{:?}",
        digest(current),
        snap.refdata.searches_american_stories(),
        state.config.congress_search,
        state.config.page_language_filter
    )))
}

async fn frontend_identity(
    dir: &std::path::Path,
    script: &std::path::Path,
) -> Result<String, RenderError> {
    let index = tokio::fs::read(dir.join("index.html"))
        .await
        .map_err(|_| RenderError::Failed("share site index cannot be read".to_owned()))?;
    let renderer = tokio::fs::read(script)
        .await
        .map_err(|_| RenderError::Failed("share renderer script cannot be read".to_owned()))?;
    Ok(digest([index, renderer].concat()))
}

/// Default overview camera parameters are added to the URL after MapLibre
/// loads. They describe the same screenshot as a fresh example link.
fn overview_key(image: &str) -> String {
    let query = image.split_once('?').map_or("", |(_, query)| query);
    let mut params: Vec<_> = form_urlencoded::parse(query.as_bytes())
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    let default_zoom = params
        .iter()
        .find(|(k, _)| k == "z")
        .is_none_or(|(_, v)| v == "3.30");
    let default_center = params
        .iter()
        .find(|(k, _)| k == "c")
        .is_none_or(|(_, v)| v == "-96.000,38.500");
    if default_zoom && default_center {
        params.retain(|(k, _)| k != "z" && k != "c");
    }
    params.retain(|(k, v)| !(k == "mode" && v == "phrase"));
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(params)
        .finish()
}

static EXAMPLE_CARDS: LazyLock<Vec<(String, Card)>> = LazyLock::new(|| {
    crate::prewarm::examples()
        .iter()
        .filter_map(|example| {
            let mut query = form_urlencoded::Serializer::new(String::new());
            for (key, value) in &example.view {
                let value = match value {
                    serde_json::Value::String(value) => value.clone(),
                    serde_json::Value::Array(values) => values
                        .iter()
                        .filter_map(|v| v.as_str())
                        .collect::<Vec<_>>()
                        .join(","),
                    _ => {
                        tracing::error!(example = example.id, "unsupported example preview view");
                        return None;
                    }
                };
                query.append_pair(key, &value);
            }
            let phrase = example.view.get("q")?.as_str()?;
            let path = super::search_path_in_mode(phrase, true);
            Card::new(&path, &query.finish()).map(|card| (example.id.clone(), card))
        })
        .collect()
});

async fn cached(
    state: &AppState,
    card: Card,
    background: bool,
) -> Result<Vec<u8>, Arc<RenderError>> {
    let browser = card.url.starts_with(&format!("{SITE}/?"))
        || card.url.starts_with(&format!("{SITE}/search/"));
    if !browser {
        return state
            .previews
            .get(
                format!("static/{}", digest(&card.image)),
                None,
                background,
                async {
                    tokio::task::spawn_blocking(move || png(&card))
                        .await
                        .map_err(|_| RenderError::Failed("share card task failed".to_owned()))?
                        .map_err(RenderError::Failed)
                },
            )
            .await;
    }
    let identity = identity(state).await.map_err(Arc::new)?;
    let normalized = overview_key(&card.image);
    let key = format!("previews/{identity}/{}.png", digest(&normalized));
    let admitted_base = card
        .url
        .strip_prefix(SITE)
        .and_then(|path| path.parse::<Uri>().ok())
        .is_some_and(|uri| {
            uri.path().starts_with("/search/") && !super::noindex(&uri, &state.sitemap)
        });
    let persistent = (admitted_base
        || EXAMPLE_CARDS
            .iter()
            .any(|(_, example)| overview_key(&example.image) == normalized))
    .then(|| state.responses.clone())
    .flatten()
    .map(|store| (store, key.clone()));
    state
        .previews
        .get(key, persistent, background, async {
            let body = screenshot(&card).await.map_err(RenderError::Failed)?;
            if self::identity(state).await? != identity {
                return Err(RenderError::Failed(
                    "share snapshot changed during capture".to_owned(),
                ));
            }
            Ok(body)
        })
        .await
}

fn can_warm(state: &AppState) -> bool {
    !state.warming.load(std::sync::atomic::Ordering::Acquire)
        && state.warm_ups.turn.try_lock().is_ok()
        && state.flights.visitor_searches() == 0
        && state.previews.idle()
}

pub(crate) fn spawn(state: Arc<AppState>) {
    if state.config.site_dir.is_none() {
        return;
    }
    if state.responses.is_none() {
        tracing::warn!("example screenshot warm-up disabled: no persistent response store");
        return;
    }
    tokio::spawn(async move {
        let mut cursor = 0;
        let mut previous = String::new();
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(10)).await;
            if !can_warm(&state) {
                continue;
            }
            let current = match identity(&state).await {
                Ok(current) => current,
                Err(error) => {
                    tracing::warn!(%error, "example screenshot warm-up identity failed");
                    continue;
                }
            };
            if previous != current {
                previous = current;
                cursor = 0;
            }
            let searches = state.sitemap.search_paths();
            let card = if let Some(path) = searches.get(cursor) {
                Card::new(path, "")
            } else {
                EXAMPLE_CARDS
                    .get(cursor - searches.len())
                    .map(|(_, card)| card.clone())
            };
            let Some(card) = card else {
                cursor = 0;
                continue;
            };
            match cached(&state, card, true).await {
                Ok(_) => tracing::info!("search screenshot warmed"),
                Err(error) => {
                    tracing::warn!(%error, "search screenshot warm-up deferred")
                }
            }
            cursor += 1;
        }
    });
}

async fn render_response(
    render: tokio::task::JoinHandle<Result<Vec<u8>, Arc<RenderError>>>,
) -> Response {
    // Dropping the timed-out JoinHandle detaches, rather than aborts, the render.
    // Only successful renders enter the cache; a later request can get the screenshot.
    match tokio::time::timeout(RESPONSE_TIMEOUT, render).await {
        Ok(Ok(Ok(body))) => (
            [
                (header::CONTENT_TYPE, "image/png"),
                (header::CACHE_CONTROL, "public, max-age=86400"),
            ],
            body,
        )
            .into_response(),
        result => {
            tracing::warn!(?result, "serving static share image fallback");
            (
                [
                    (header::CONTENT_TYPE, "image/png"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                FALLBACK,
            )
                .into_response()
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum RenderError {
    #[error("share image renderer is busy")]
    Busy,
    #[error("{0}")]
    Failed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    static IMAGES: LazyLock<moka::future::Cache<String, Vec<u8>>> =
        LazyLock::new(|| moka::future::Cache::new(1024));

    #[tokio::test]
    async fn static_maps_never_return_generic_images_when_capture_fails_or_is_busy() {
        use http_body_util::BodyExt;

        for error in [
            RenderError::Busy,
            RenderError::Failed("capture failed".to_owned()),
        ] {
            let response = map_response(tokio::spawn(async move { Err(Arc::new(error)) })).await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(response.headers()[header::RETRY_AFTER], "10");
            let body = response.into_body().collect().await.unwrap().to_bytes();
            assert!(
                !body.starts_with(b"\x89PNG"),
                "never show another query's map"
            );
        }
    }

    #[tokio::test]
    async fn static_map_endpoint_rejects_unlisted_searches_and_extra_parameters() {
        let state = state().await;
        for (uri, status) in [
            ("/search-map.png", StatusCode::BAD_REQUEST),
            (
                "/search-map.png?q=not-a-listed-search",
                StatusCode::NOT_FOUND,
            ),
            ("/search-map.png?q=whigs&state=NY", StatusCode::BAD_REQUEST),
            ("/search-map.png?q=whigs&q=gold", StatusCode::BAD_REQUEST),
        ] {
            let response = search_map(OriginalUri(uri.parse().unwrap()), state.clone()).await;
            assert_eq!(response.status(), status, "{uri}");
        }
    }

    #[tokio::test]
    async fn changing_the_selected_renderer_invalidates_persisted_identity() {
        let dir = tempfile::tempdir().unwrap();
        let renderer = dir.path().join("renderer.mjs");
        tokio::fs::write(dir.path().join("index.html"), b"same frontend")
            .await
            .unwrap();
        tokio::fs::write(&renderer, b"renderer version one")
            .await
            .unwrap();
        let before = frontend_identity(dir.path(), &renderer).await.unwrap();
        tokio::fs::write(&renderer, b"renderer version two")
            .await
            .unwrap();
        assert_ne!(
            before,
            frontend_identity(dir.path(), &renderer).await.unwrap()
        );
    }

    #[tokio::test]
    async fn detached_warm_up_blocks_previews_even_after_readiness_is_released() {
        let state = state().await;
        let turn = state.warm_ups.claim().turn().await;
        assert!(!state.warming.load(std::sync::atomic::Ordering::Acquire));
        assert!(!can_warm(&state));
        drop(turn);
        assert!(can_warm(&state));
    }

    #[test]
    fn every_example_has_an_exact_preview_and_only_default_camera_aliases_share_it() {
        assert_eq!(EXAMPLE_CARDS.len(), crate::prewarm::examples().len());
        for (_, card) in EXAMPLE_CARDS.iter() {
            let key = overview_key(&card.image);
            assert_eq!(
                key,
                overview_key(&format!("{}&z=3.30&c=-96.000%2C38.500", card.image))
            );
            assert_ne!(
                key,
                overview_key(&format!("{}&z=4.30&c=-96.000%2C38.500", card.image))
            );
            assert_ne!(key, overview_key(&format!("{}&state=IL", card.image)));
        }
        let example = EXAMPLE_CARDS.iter().find(|(id, _)| id == "whigs").unwrap();
        assert_eq!(
            overview_key(&example.1.image),
            overview_key(
                &Card::new(
                    "/search/whigs",
                    "from=1834-01-01&to=1856-12-31&bucket=month"
                )
                .unwrap()
                .image
            )
        );
        let abolitionists = EXAMPLE_CARDS
            .iter()
            .find(|(id, _)| id == "abolitionists")
            .unwrap();
        assert_ne!(
            overview_key(&abolitionists.1.image),
            overview_key(
                &Card::new("/search/abolitionists", "from=1830-01-01&to=1860-12-31")
                    .unwrap()
                    .image
            )
        );
    }

    #[tokio::test]
    async fn slow_render_falls_back_and_still_populates_the_screenshot_cache() {
        use http_body_util::BodyExt;

        let key = "slow-render-fallback-test".to_owned();
        let (release, wait) = tokio::sync::oneshot::channel();
        let (finished, done) = tokio::sync::oneshot::channel();
        let cache_key = key.clone();
        let screenshot = b"completed screenshot".to_vec();
        let body = screenshot.clone();
        let render = tokio::spawn(async move {
            wait.await.unwrap();
            IMAGES.insert(cache_key, body.clone()).await;
            finished.send(()).unwrap();
            Ok(body)
        });
        let response = render_response(render).await;
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            FALLBACK
        );
        assert!(IMAGES.get(&key).await.is_none());
        release.send(()).unwrap();
        done.await.unwrap();
        assert_eq!(IMAGES.get(&key).await.unwrap(), screenshot);
        let response = render_response(tokio::spawn(
            async move { Ok(IMAGES.get(&key).await.unwrap()) },
        ))
        .await;
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            screenshot
        );
    }

    #[tokio::test]
    async fn failed_and_busy_renders_return_an_uncached_static_png_through_middleware() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        for error in [
            RenderError::Busy,
            RenderError::Failed("share screenshot browser failed".to_owned()),
        ] {
            let error = Arc::new(error);
            let app = axum::Router::new()
                .route(
                    "/share-image.png",
                    axum::routing::get(move || {
                        let error = error.clone();
                        async move { render_response(tokio::spawn(async move { Err(error) })).await }
                    }),
                )
                .layer(axum::middleware::from_fn(super::super::cache_control));
            let response = app
                .oneshot(
                    axum::http::Request::builder()
                        .uri("/share-image.png")
                        .body(axum::body::Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
            assert_eq!(response.headers()[header::CONTENT_TYPE], "image/png");
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            assert_eq!(
                response.into_body().collect().await.unwrap().to_bytes(),
                FALLBACK
            );
        }
    }

    #[tokio::test]
    async fn identical_cache_misses_share_one_render_slot() {
        let service = Service::default();
        let card = Card::new("/datasets", "").unwrap();
        let key = card.image.clone();
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let render = || async {
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            png(&card).map_err(RenderError::Failed)
        };
        let (a, b, c) = tokio::join!(
            service.get(key.clone(), None, false, render()),
            service.get(key.clone(), None, false, render()),
            service.get(key, None, false, render()),
        );
        assert_eq!(a.unwrap(), b.unwrap());
        assert_eq!(c.unwrap(), png(&card).unwrap());
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
    }

    #[test]
    fn metadata_uses_the_permalink_and_escapes_untrusted_text() {
        let index = std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/index.html"),
        )
        .unwrap();
        let uri = "/?q=%22%3Cscript%3E%26%22&from=1896-01-01&state=GA&utm_source=test"
            .parse()
            .unwrap();
        let html = String::from_utf8(shell(&uri, index)).unwrap();
        assert!(html.contains("&quot;&lt;script&gt;&amp;&quot;"));
        assert!(html.contains("share-image.png?page=%2F&amp;q="));
        assert!(html.contains("1896-01-01 to latest"));
        assert!(html.contains("States: GA"));
        assert!(!html.contains("cross of gold"));
        assert!(!html.contains("utm_source"));
        assert!(!html.contains("<script>&"));
        // Search permalinks retain the homepage's SEO canonical.
        assert!(html.contains(r#"rel="canonical" href="https://usnewsmap.com/""#));
    }

    #[test]
    fn full_newspaper_filter_survives_metadata_and_image_urls() {
        let lccns = (0..60)
            .map(|i| format!("sn{i:014}"))
            .collect::<Vec<_>>()
            .join(",");
        let query = form_urlencoded::Serializer::new(String::new())
            .append_pair("lccn", &lccns)
            .finish();
        let card = Card::new("/search/gold", &query).unwrap();
        for url in [&card.url, &card.image] {
            let query = url.split_once('?').unwrap().1;
            let filter = form_urlencoded::parse(query.as_bytes())
                .find(|(k, _)| k == "lccn")
                .unwrap()
                .1
                .into_owned();
            assert_eq!(filter, lccns);
        }
    }

    #[test]
    fn cards_cover_static_pages_and_congress_but_not_unknown_routes() {
        assert!(Card::new("/", "").is_none());
        assert!(Card::new("/missing", "q=gold").is_none());
        assert!(Card::new("/", "utm_source=test").is_none());
        let search = Card::new("/search/cross-of-gold", "state=GA").unwrap();
        assert_eq!(search.title, "cross of gold");
        assert_eq!(
            search.url,
            "https://usnewsmap.com/search/cross-of-gold?state=GA"
        );
        assert!(search.image.contains("q=cross+of+gold"));
        for (path, title) in [("/privacy/", "Privacy"), ("/datasets", "Dataset details")] {
            let card = Card::new(path, "").unwrap();
            assert_eq!(card.title, title);
            assert!(card.url.ends_with(path.trim_end_matches('/')));
        }
        let card = Card::new(
            "/index.html",
            "q=gold&source=congress&series=globe&state=GA",
        )
        .unwrap();
        assert_eq!(card.label, "SEARCH THE RECORDS OF CONGRESS");
        assert!(card.details.contains("Series: globe"));
        assert!(!card.details.contains("States"));
        assert!(card.url.starts_with("https://usnewsmap.com/?"));
    }

    #[test]
    fn png_is_a_real_image_and_changes_with_the_shared_view() {
        let a = png(&Card::new("/", "q=gold&state=GA").unwrap()).unwrap();
        let b = png(&Card::new("/", "q=silver&state=CA").unwrap()).unwrap();
        assert_eq!(&a[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(u32::from_be_bytes(a[16..20].try_into().unwrap()), 1200);
        assert_eq!(u32::from_be_bytes(a[20..24].try_into().unwrap()), 630);
        assert_ne!(a, b);
        assert_eq!(lines(&"x".repeat(256), 24, 3).len(), 3);
    }

    async fn state() -> Arc<AppState> {
        let fixtures =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/data");
        let refdata = crate::refdata::RefData::load(&usnm_store::LocalStore::new(fixtures))
            .await
            .unwrap();
        let mut config = crate::config::Config::from_lookup(|_| None).unwrap();
        config.site_dir =
            Some(std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web"));
        Arc::new(AppState::new(
            config,
            Arc::new(usnm_search::memory::MemoryBackend::new()),
            refdata,
        ))
    }

    #[tokio::test]
    async fn static_card_endpoint_serves_png_and_rejects_invalid_views() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let mut state = state().await;
        Arc::get_mut(&mut state).unwrap().config.site_dir = None;
        assert!(identity(&state).await.is_err());
        let app = axum::Router::new().route(
            "/share-image.png",
            axum::routing::get(move |uri| image(uri, state.clone())),
        );
        let request = |method, uri: &str| {
            axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let image = app
            .clone()
            .oneshot(request("GET", "/share-image.png?page=%2Fdatasets"))
            .await
            .unwrap();
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(image.headers()[header::CONTENT_TYPE], "image/png");
        assert_eq!(
            image.headers()[header::CACHE_CONTROL],
            "public, max-age=86400"
        );
        let body = image.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(
            body.as_ref(),
            png(&Card::new("/datasets", "").unwrap()).unwrap()
        );
        for uri in [
            "/share-image.png",
            "/share-image.png?page=%2Funknown&q=gold",
        ] {
            let response = app.clone().oneshot(request("GET", uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }
}
