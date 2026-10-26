//! Server-rendered sharing metadata, browser screenshots of search views,
//! and lightweight PNG cards for static pages.

use std::sync::{Arc, LazyLock};

use axum::extract::OriginalUri;
use axum::http::{header, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use resvg::{tiny_skia, usvg};

const SITE: &str = "https://usnewsmap.com";
const IMAGE: &str = "/share-image.png";
const VIEW_KEYS: &[&str] = &[
    "q", "source", "mode", "near", "from", "to", "state", "lang", "lccn", "series", "t", "win",
    "bucket", "layer", "norm", "place", "sort", "tab", "z", "c",
];

static FONTS: LazyLock<Arc<usvg::fontdb::Database>> = LazyLock::new(|| {
    let mut fonts = usvg::fontdb::Database::new();
    fonts.load_font_data(include_bytes!("../../assets/NotoSans-Regular.ttf").to_vec());
    Arc::new(fonts)
});

static IMAGES: LazyLock<moka::future::Cache<String, Vec<u8>>> = LazyLock::new(|| {
    moka::future::Cache::builder()
        .max_capacity(128)
        .time_to_live(std::time::Duration::from_secs(3600))
        .build()
});
static RENDERS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

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

async fn screenshot(card: &Card) -> Result<Vec<u8>, String> {
    let bind = std::env::var("USNM_BIND").unwrap_or_else(|_| "0.0.0.0:8080".to_owned());
    let port = bind.rsplit_once(':').ok_or("USNM_BIND has no port")?.1;
    let port: u16 = port.parse().map_err(|_| "USNM_BIND has an invalid port")?;
    // Only our own server is navigated, never an origin supplied by a visitor.
    let url = format!("http://127.0.0.1:{port}{}", &card.url[SITE.len()..]);
    let script = std::env::var_os("USNM_SHARE_SCRIPT").unwrap_or_else(|| {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../web/scripts/render-share.mjs")
            .into_os_string()
    });
    let mut command = tokio::process::Command::new("node");
    command.arg(script).arg("--url").arg(url).kill_on_drop(true);
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

pub(super) async fn image(OriginalUri(uri): OriginalUri) -> Response {
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
    if let Some(body) = IMAGES.get(&card.image).await {
        return ([(header::CONTENT_TYPE, "image/png")], body).into_response();
    }
    let Ok(permit) = RENDERS.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::RETRY_AFTER, "1")],
            "share image renderer is busy",
        )
            .into_response();
    };
    let key = card.image.clone();
    // Finish and cache a render even if the crawler stops waiting. The permit
    // bounds both browsers and static-card rasterization until they finish.
    match tokio::spawn(async move {
        let _permit = permit;
        let body = if card.url.starts_with(&format!("{SITE}/?"))
            || card.url.starts_with(&format!("{SITE}/search/"))
        {
            screenshot(&card).await?
        } else {
            tokio::task::spawn_blocking(move || png(&card))
                .await
                .map_err(|_| "share card task failed".to_owned())??
        };
        IMAGES.insert(key, body.clone()).await;
        Ok::<_, String>(body)
    })
    .await
    {
        Ok(Ok(body)) => ([(header::CONTENT_TYPE, "image/png")], body).into_response(),
        result => {
            tracing::error!(?result, "could not render share image");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "could not render share image; try again shortly",
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[tokio::test]
    async fn static_card_endpoint_serves_png_and_rejects_invalid_views() {
        use http_body_util::BodyExt;
        use tower::ServiceExt;

        let app = axum::Router::new().route("/share-image.png", axum::routing::get(image));
        let request = |method, uri: &str| {
            axum::http::Request::builder()
                .method(method)
                .uri(uri)
                .body(axum::body::Body::empty())
                .unwrap()
        };
        let image = app
            .clone()
            .oneshot(request("GET", "/share-image.png?page=%2Fprivacy"))
            .await
            .unwrap();
        assert_eq!(image.status(), StatusCode::OK);
        assert_eq!(image.headers()[header::CONTENT_TYPE], "image/png");
        let body = image.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(&body[..8], b"\x89PNG\r\n\x1a\n");
        for uri in [
            "/share-image.png",
            "/share-image.png?page=%2Funknown&q=gold",
        ] {
            let response = app.clone().oneshot(request("GET", uri)).await.unwrap();
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        }
    }
}
