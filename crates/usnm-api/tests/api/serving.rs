//! The HTTP layer around the API: persisted responses, rate limits, the site
//! and its redirects, compression and CORS.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::Value;
use tower::ServiceExt;
use usnm_api::{app, AppState};
use usnm_search::memory::MemoryBackend;
use usnm_store::LocalStore;

use crate::common::{
    config, fixture_backend, get, header_str, load_docs, persisted_files, refdata, state_with,
};

#[tokio::test]
async fn slow_responses_persist_and_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("usnm-respcache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let store = Arc::new(LocalStore::new(&dir));
    let mut cfg = config();
    cfg.persist_after = Duration::ZERO;
    let first = Arc::new(
        AppState::new(cfg.clone(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(store.clone()),
    );
    let uri = "/v1/aggregate?q=fever&v=fixture-v1";
    let (status, _, body) = get(&first, uri).await;
    assert_eq!(status, StatusCode::OK);
    let mut files = Vec::new();
    for _ in 0..100 {
        files = persisted_files(&dir);
        if !files.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(files.len(), 1, "one entry under {{version}}/f8/");
    let name = files[0].file_name().unwrap().to_string_lossy().into_owned();
    assert!(
        !name.contains("fever"),
        "search text must not appear in paths"
    );

    // A fresh replica with an empty in-process cache and an empty index still
    // answers from the persisted entry: the body is identical.
    let restarted = Arc::new(
        AppState::new(cfg, Arc::new(MemoryBackend::new()), refdata().await)
            .with_response_store(store),
    );
    let (status, headers, again) = get(&restarted, uri).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(again, body);
    assert!(header_str(&headers, header::ETAG).starts_with("\"fixture-v1:"));

    // An entry that decompresses but isn't JSON is ignored and recomputed.
    std::fs::write(
        &files[0],
        zstd::encode_all(&b"{\"truncated"[..], 3).unwrap(),
    )
    .unwrap();
    let recomputing = Arc::new(
        AppState::new(config(), Arc::new(fixture_backend()), refdata().await)
            .with_response_store(Arc::new(LocalStore::new(&dir))),
    );
    let (status, _, mut fresh) = get(&recomputing, uri).await;
    assert_eq!(status, StatusCode::OK);
    // Recomputed, so only its timing may differ from the original.
    let mut original = body.clone();
    for v in [&mut fresh, &mut original] {
        v.as_object_mut().unwrap().remove("timing_ms");
    }
    // Serving the invalid entry would have parsed as `null` here.
    assert_eq!(fresh, original);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn fast_responses_are_not_persisted() {
    let dir = std::env::temp_dir().join(format!("usnm-respfast-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = config();
    cfg.persist_after = Duration::from_secs(3600);
    let s = Arc::new(
        AppState::new(cfg, Arc::new(fixture_backend()), refdata().await)
            .with_response_store(Arc::new(LocalStore::new(&dir))),
    );
    let (status, _, _) = get(&s, "/v1/aggregate?q=fever&v=fixture-v1").await;
    assert_eq!(status, StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(persisted_files(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

async fn get_from(
    state: &Arc<AppState>,
    uri: &str,
    xff: &str,
) -> (StatusCode, axum::http::HeaderMap) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header("x-forwarded-for", xff)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    (resp.status(), resp.headers().clone())
}

#[tokio::test]
async fn clients_over_their_rate_get_429() {
    let mut cfg = config();
    cfg.rate_limit = Some(usnm_api::config::RateLimit {
        per_minute: 1.try_into().unwrap(),
        burst: 2.try_into().unwrap(),
    });
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    for _ in 0..2 {
        assert_eq!(
            get_from(&s, "/v1/meta", "203.0.113.7").await.0,
            StatusCode::OK
        );
    }
    let (status, headers) = get_from(&s, "/api/v1/meta", "198.51.100.1, 203.0.113.7").await;
    assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(
        header_str(&headers, header::CONTENT_TYPE),
        "application/problem+json"
    );
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    let retry: u64 = header_str(&headers, header::RETRY_AFTER).parse().unwrap();
    assert!((1..=60).contains(&retry));
    // Other clients and health probes are unaffected.
    assert_eq!(
        get_from(&s, "/v1/meta", "203.0.113.8").await.0,
        StatusCode::OK
    );
    assert_eq!(
        get_from(&s, "/healthz", "203.0.113.7").await.0,
        StatusCode::OK
    );
}

/// A GET from `peer` (the TCP peer the server sees) with these headers.
async fn get_from_peer(
    state: &Arc<AppState>,
    uri: &str,
    peer: &str,
    headers: &[(&str, &str)],
) -> StatusCode {
    let mut req = Request::builder().uri(uri);
    for (name, value) in headers {
        req = req.header(*name, *value);
    }
    let mut req = req.body(Body::empty()).unwrap();
    req.extensions_mut().insert(axum::extract::ConnectInfo(
        peer.parse::<std::net::SocketAddr>().unwrap(),
    ));
    app(state.clone()).oneshot(req).await.unwrap().status()
}

#[tokio::test]
async fn behind_a_local_cloudflared_clients_are_told_apart_by_cf_connecting_ip() {
    let mut cfg = config();
    cfg.trust_cf_connecting_ip = true;
    cfg.rate_limit = Some(usnm_api::config::RateLimit {
        per_minute: 1.try_into().unwrap(),
        burst: 2.try_into().unwrap(),
    });
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let local = "127.0.0.1:50000";
    // Every request reaches the API from the proxy on this host, with its
    // own X-Forwarded-For entry; Cloudflare's header tells clients apart.
    let a = [
        ("cf-connecting-ip", "203.0.113.7"),
        ("x-forwarded-for", "127.0.0.1"),
    ];
    for _ in 0..2 {
        assert_eq!(
            get_from_peer(&s, "/v1/meta", local, &a).await,
            StatusCode::OK
        );
    }
    assert_eq!(
        get_from_peer(&s, "/v1/meta", local, &a).await,
        StatusCode::TOO_MANY_REQUESTS
    );
    let b = [
        ("cf-connecting-ip", "203.0.113.8"),
        ("x-forwarded-for", "127.0.0.1"),
    ];
    assert_eq!(
        get_from_peer(&s, "/v1/meta", local, &b).await,
        StatusCode::OK
    );
    // From anywhere else the header is the client's own claim: the peer's
    // address counts, so naming a fresh address gets no fresh bucket.
    let remote = "192.0.2.9:50000";
    for (i, ip) in ["198.51.100.1", "198.51.100.2"].iter().enumerate() {
        let status = get_from_peer(&s, "/v1/meta", remote, &[("cf-connecting-ip", ip)]).await;
        assert_eq!(status, StatusCode::OK, "{i}");
    }
    assert_eq!(
        get_from_peer(
            &s,
            "/v1/meta",
            remote,
            &[("cf-connecting-ip", "198.51.100.3")]
        )
        .await,
        StatusCode::TOO_MANY_REQUESTS
    );
}

/// A built site: an index, a hashed asset with a precompressed copy, a favicon.
/// Each test names its own, since tests run in parallel.
fn site_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("usnm-site-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("assets")).unwrap();
    std::fs::write(dir.join("index.html"), "<!doctype html><div id=root></div>").unwrap();
    std::fs::write(dir.join("assets/index-abc.js"), "console.log(1)").unwrap();
    std::fs::write(dir.join("assets/index-abc.js.gz"), b"gzipped").unwrap();
    std::fs::write(dir.join("favicon.svg"), "<svg/>").unwrap();
    dir
}

fn site_dir_of(s: &AppState) -> PathBuf {
    s.config.site_dir.clone().unwrap()
}

async fn get_site(
    state: &Arc<AppState>,
    uri: &str,
    encoding: Option<&str>,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let mut req = Request::builder().uri(uri);
    if let Some(e) = encoding {
        req = req.header(header::ACCEPT_ENCODING, e);
    }
    let resp = app(state.clone())
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn basemap_archive_has_immutable_unencoded_byte_ranges() {
    let dir = site_dir("basemap-ranges");
    let path = "basemap/test-v1/overview.pmtiles.bin";
    std::fs::create_dir_all(dir.join("basemap/test-v1")).unwrap();
    let data = b"PMTiles".repeat(1024);
    std::fs::write(dir.join(path), &data).unwrap();
    let mut cfg = config();
    cfg.site_dir = Some(dir.clone());
    let state = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    for (range, status, expected) in [
        (None, StatusCode::OK, &data[..]),
        (Some("bytes=0-6"), StatusCode::PARTIAL_CONTENT, &data[..7]),
    ] {
        let mut req = Request::builder()
            .uri(format!("/{path}"))
            .header(header::ACCEPT_ENCODING, "br, gzip");
        if let Some(value) = range {
            req = req.header(header::RANGE, value);
        }
        let resp = app(state.clone())
            .oneshot(req.body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert_eq!(resp.status(), status);
        assert_eq!(
            header_str(resp.headers(), header::CACHE_CONTROL),
            "public, max-age=31536000, immutable"
        );
        assert_eq!(
            header_str(resp.headers(), header::CONTENT_TYPE),
            "application/vnd.pmtiles"
        );
        assert_eq!(
            header_str(resp.headers(), header::CONTENT_LENGTH),
            expected.len().to_string()
        );
        assert!(!resp.headers().contains_key(header::CONTENT_ENCODING));
        if range.is_some() {
            assert_eq!(
                header_str(resp.headers(), header::CONTENT_RANGE),
                format!("bytes 0-6/{}", data.len())
            );
        }
        assert_eq!(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .as_ref(),
            expected
        );
    }
    let (status, headers, _) = get_site(&state, "/basemap/test-v1/missing.pmtiles.bin", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-store");
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn serves_the_site_alongside_the_api() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("serve"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));

    // The app shell, revalidated on every load, with the security headers.
    let (status, h, body) = get_site(&s, "/", None).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("id=root"));
    assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
    assert!(header_str(&h, header::CONTENT_SECURITY_POLICY).contains("connect-src 'self'"));
    assert_eq!(header_str(&h, header::X_CONTENT_TYPE_OPTIONS), "nosniff");
    assert_eq!(
        header_str(&h, header::STRICT_TRANSPORT_SECURITY),
        "max-age=300"
    );

    // The app's pages get the shell too, so a reload keeps working. The file
    // itself is served with or without a trailing slash (web/src/route.ts).
    for uri in [
        "/datasets",
        "/datasets/",
        "/index.html",
        "/index.html/",
        "/search/gold",
        "/search/U.S.",
        "/search/a%2Fb",
    ] {
        let (status, h, body) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(body.contains("id=root"), "{uri}");
        assert!(header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
    }

    // The dataset details page's old path redirects, keeping the query (#294).
    for (uri, location) in [
        ("/status", "/datasets"),
        ("/status/", "/datasets"),
        ("/status?x=1", "/datasets?x=1"),
    ] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY, "{uri}");
        assert_eq!(header_str(&h, header::LOCATION), location, "{uri}");
    }

    // Other paths get the shell, where the app shows a not-found page, with a
    // 404; so do paths with a segment too long to be a file name, which the
    // filesystem would refuse (a 500 from the file service).
    let long = format!("/{}", "a".repeat(300));
    let long_nested = format!("/assets/{}/x", "b".repeat(256));
    for uri in [
        "/search/gold/extra",
        "/does-not-exist",
        "/statuses",
        long.as_str(),
    ] {
        let (status, h, body) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert!(body.contains("id=root"), "{uri}");
        assert!(header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache", "{uri}");
    }
    let (status, _, _) = get_site(&s, &long_nested, None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a long name under /assets/ is a plain 404"
    );
    // The limit is on the decoded name, as the file service decodes it: a
    // 100-byte file name sent as 300 bytes of escapes is still served.
    let name = format!("{}.txt", "d".repeat(96));
    std::fs::write(site_dir_of(&s).join(&name), "found").unwrap();
    let escaped: String = name.bytes().map(|b| format!("%{b:02X}")).collect();
    assert!(escaped.len() > 255);
    let (status, _, body) = get_site(&s, &format!("/{escaped}"), None).await;
    assert_eq!((status, body.as_str()), (StatusCode::OK, "found"));
    // 253 bytes fits NAME_MAX, but `ServeDir` would also probe the 256-byte
    // `.gz` name for a client that accepts gzip.
    let (status, _, _) = get_site(&s, &format!("/{}.js", "e".repeat(250)), Some("gzip")).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a name that can't take .gz is a plain 404"
    );
    let (status, _, _) = get_site(&s, &format!("/{}.js", "c".repeat(400)), None).await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "a long file name is a plain 404"
    );

    // HEAD on an app route: GET's headers and status, no body.
    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/datasets")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::OK);
    let len = std::fs::metadata(s.config.site_dir.as_ref().unwrap().join("index.html"))
        .unwrap()
        .len();
    assert_eq!(
        header_str(resp.headers(), header::CONTENT_LENGTH),
        len.to_string()
    );
    assert!(header_str(resp.headers(), header::CONTENT_TYPE).starts_with("text/html"));
    assert!(resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());

    // Hashed assets: cached for a year, the precompressed copy when accepted.
    let (status, h, body) = get_site(&s, "/assets/index-abc.js", Some("gzip")).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, "gzipped");
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "gzip");
    assert_eq!(
        header_str(&h, header::CACHE_CONTROL),
        "public, max-age=31536000, immutable"
    );
    let (_, h, body) = get_site(&s, "/assets/index-abc.js", None).await;
    assert_eq!(body, "console.log(1)");
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "");

    let (status, h, _) = get_site(&s, "/favicon.svg", None).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&h, header::CACHE_CONTROL),
        "public, max-age=3600"
    );

    let well_known = site_dir_of(&s).join(".well-known");
    std::fs::create_dir_all(&well_known).unwrap();
    let public = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../web/public/.well-known");
    for name in [
        "security.txt",
        "trust.txt",
        "gpc.json",
        "assetlinks.json",
        "dnt-policy.txt",
    ] {
        std::fs::copy(public.join(name), well_known.join(name)).unwrap();
        let (status, h, body) = get_site(&s, &format!("/.well-known/{name}"), None).await;
        assert_eq!(status, StatusCode::OK, "{name}");
        assert!(!body.is_empty(), "{name}");
        let media_type = if name.ends_with(".json") {
            "application/json"
        } else {
            "text/plain"
        };
        assert!(
            header_str(&h, header::CONTENT_TYPE).starts_with(media_type),
            "{name}: {}",
            header_str(&h, header::CONTENT_TYPE)
        );
    }

    // A file path with a trailing slash is a missing file, not an app route.
    let (status, h, body) = get_site(&s, "/favicon.svg/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!body.contains("id=root"));
    assert!(!header_str(&h, header::CONTENT_TYPE).starts_with("text/html"));
    for uri in ["/favicon.svg/", "/assets/missing.js"] {
        let resp = app(s.clone())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(resp.status(), StatusCode::NOT_FOUND, "{uri}");
        assert!(
            resp.into_body()
                .collect()
                .await
                .unwrap()
                .to_bytes()
                .is_empty(),
            "{uri}"
        );
    }

    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri("/does-not-exist")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
    assert!(header_str(resp.headers(), header::CONTENT_TYPE).starts_with("text/html"));
    assert_eq!(
        header_str(resp.headers(), header::CACHE_CONTROL),
        "no-cache"
    );

    // Missing files are 404s, not the shell, and no cache keeps them: one
    // can be a gap in a deploy.
    for uri in ["/assets/gone-123.js", "/gone.txt", "/favicon.svg/"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::NOT_FOUND, "{uri}");
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-store", "{uri}");
        let resp = app(s.clone())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            header_str(resp.headers(), header::CACHE_CONTROL),
            "no-store",
            "HEAD {uri}"
        );
    }

    // The API is unchanged, and its unknown paths stay problem details.
    let (status, h, body) = get(&s, "/v1/meta").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        header_str(&h, header::STRICT_TRANSPORT_SECURITY),
        "max-age=300"
    );
    assert!(body["index_version"].is_string());
    let (status, h, _) = get(&s, "/v1/nope").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        header_str(&h, header::CONTENT_TYPE),
        "application/problem+json"
    );
    let (status, _, _) = get(&s, "/healthz").await;
    assert_eq!(status, StatusCode::OK);
}

/// A GET of the site with `If-None-Match` set to `tag`.
async fn get_if_none_match(
    state: &Arc<AppState>,
    uri: &str,
    tag: &str,
) -> (StatusCode, axum::http::HeaderMap, String) {
    let resp = app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::IF_NONE_MATCH, tag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, headers) = (resp.status(), resp.headers().clone());
    let bytes = resp.into_body().collect().await.unwrap().to_bytes();
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

#[tokio::test]
async fn the_apps_pages_revalidate_with_an_etag() {
    let mut cfg = config();
    let dir = site_dir("etag");
    // With the title the privacy and credits pages rename.
    std::fs::write(
        dir.join("index.html"),
        "<!doctype html><title>US News Map</title><div id=root></div>",
    )
    .unwrap();
    cfg.site_dir = Some(dir);
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let mut tags = Vec::new();
    for uri in ["/datasets", "/privacy", "/credits"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache", "{uri}");
        let tag = header_str(&h, header::ETAG).to_owned();
        assert!(tag.starts_with("W/\""), "{uri}: {tag}");

        // The tag it was sent: a 304 with no body, still revalidated next time.
        let (status, h304, body304) = get_if_none_match(&s, uri, &tag).await;
        assert_eq!(status, StatusCode::NOT_MODIFIED, "{uri}");
        assert!(body304.is_empty(), "{uri}");
        assert_eq!(header_str(&h304, header::ETAG), tag, "{uri}");
        assert_eq!(
            header_str(&h304, header::CACHE_CONTROL),
            "no-cache",
            "{uri}"
        );
        let vary: Vec<_> = h304
            .get_all(header::VARY)
            .iter()
            .filter_map(|v| v.to_str().ok())
            .collect();
        assert!(vary.contains(&"accept-encoding"), "{uri}: {vary:?}");
        assert!(header_str(&h304, header::CONTENT_SECURITY_POLICY).contains("default-src"));

        // Another tag: the page itself.
        let (status, _, again) = get_if_none_match(&s, uri, "W/\"0000000000000000\"").await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert!(again.contains("id=root"), "{uri}");

        // HEAD sends the same tag.
        let resp = app(s.clone())
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri(uri)
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(header_str(resp.headers(), header::ETAG), tag, "HEAD {uri}");
        tags.push(tag);
    }
    // Each page's own shell, so each its own tag.
    tags.sort();
    tags.dedup();
    assert_eq!(tags.len(), 3);
    // The dataset page remains indexable on revalidation.
    let (_, h, _) = get_site(&s, "/datasets", None).await;
    let (_, h304, _) = get_if_none_match(&s, "/datasets", header_str(&h, header::ETAG)).await;
    assert_eq!(
        header_str(&h304, header::HeaderName::from_static("x-robots-tag")),
        ""
    );

    // A release changes the shell, and with it the tag.
    std::fs::write(
        site_dir_of(&s).join("index.html"),
        "<!doctype html><div id=root></div><script src=/assets/index-def.js></script>",
    )
    .unwrap();
    let (status, _, body) = get_if_none_match(&s, "/datasets", header_str(&h, header::ETAG)).await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.contains("index-def.js"));

    // The not-found shell has no tag and ignores the condition.
    let (status, h, _) = get_if_none_match(&s, "/does-not-exist", "*").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(header_str(&h, header::ETAG), "");
    assert_eq!(header_str(&h, header::CACHE_CONTROL), "no-cache");
}

#[tokio::test]
async fn uncurated_searches_are_noindex_and_site_pages_are_indexable() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("noindex"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let robots = header::HeaderName::from_static("x-robots-tag");
    for uri in ["/search/x", "/search/cross-of-gold?from=1896-06-01"] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, robots.clone()), "noindex", "{uri}");
    }
    // The old path's redirect too.
    let (status, h, _) = get_site(&s, "/status", None).await;
    assert_eq!(status, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(header_str(&h, robots.clone()), "noindex");
    for uri in [
        "/",
        "/?t=1896-07-01",
        "/favicon.svg",
        "/search/whigs",
        "/datasets",
        "/credits",
    ] {
        let (status, h, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::OK, "{uri}");
        assert_eq!(header_str(&h, robots.clone()), "", "{uri}");
    }
}

#[tokio::test]
async fn legacy_searches_redirect_and_curated_results_render_without_javascript() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("search-routes"));
    std::fs::write(
        cfg.site_dir.as_ref().unwrap().join("index.html"),
        include_str!("../../../../web/index.html"),
    )
    .unwrap();
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    for (uri, location) in [
        (
            "/?q=%22cross+of+gold%22&tab=table",
            "/search/cross-of-gold?tab=table",
        ),
        ("/index.html?q=gold", "/search/gold"),
        ("/search/whigs/", "/search/whigs"),
    ] {
        let (status, headers, _) = get_site(&s, uri, None).await;
        assert_eq!(status, StatusCode::MOVED_PERMANENTLY, "{uri}");
        assert_eq!(header_str(&headers, header::LOCATION), location);
    }
    let uri = "/search/cross-of-gold";
    let (status, headers, body) = get_site(&s, uri, None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(header_str(&headers, header::CACHE_CONTROL), "no-cache");
    assert!(body.contains("<title>cross of gold · US News Map</title>"));
    assert!(body
        .contains(r#"<link rel="canonical" href="https://usnewsmap.com/search/cross-of-gold" />"#));
    assert!(body.contains("matching newspaper pages"));
    assert!(body.contains("1896-07-18"), "{body}");
    assert!(
        body.contains("Fixture City B (New York area), NY has the most hits (42 pages)"),
        "{body}"
    );
    assert!(body.contains("cross of gold"), "{body}");
    assert!(body.contains("<ol><li>"), "{body}");
    assert!(body.contains(r#"<div id="root"></div><div id="search-landing">"#));
    assert!(!body.contains(r#"id="site-fallback""#));
    assert!(!body.contains(r#"class="visually-hidden" inert"#));
    assert!(body.contains(r#"id="explore-map""#));
    for dynamic in [
        "/search/not-a-listed-search",
        "/search/cross-of-gold?from=1896-06-01",
    ] {
        let (status, headers, body) = get_site(&s, dynamic, None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            header_str(&headers, header::HeaderName::from_static("x-robots-tag")),
            "noindex"
        );
        assert!(!body.contains(r#"id="search-landing""#), "{dynamic}");
        assert!(body.contains(r#"<div id="root">"#), "{dynamic}");
    }
    let resp = app(s.clone())
        .oneshot(
            Request::builder()
                .method("HEAD")
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(resp.status(), status);
    assert_eq!(
        header_str(resp.headers(), header::CONTENT_LENGTH),
        body.len().to_string()
    );
    assert!(resp
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .is_empty());
    let (_, _, home) = get_site(&s, "/", None).await;
    assert!(home.contains(r#"<nav aria-label="Common newspaper searches">"#));
    assert!(home.contains(r#"<a href="/search/whigs">whigs</a>"#));
}

#[tokio::test]
async fn search_previews_preserve_escaped_snippet_text_without_double_escaping() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("search-snippet-entities"));
    std::fs::write(
        cfg.site_dir.as_ref().unwrap().join("index.html"),
        include_str!("../../../../web/index.html"),
    )
    .unwrap();
    let mut doc = load_docs("pages-base-fixture").remove(0);
    doc.text = "Reader's & Co. <archive>. cross of gold.".to_owned();
    let mut backend = MemoryBackend::new();
    backend.add_index("pages-base-fixture", vec![doc]);
    backend.add_index("pages-delta-fixture-1", vec![]);
    let s = Arc::new(AppState::new(cfg, Arc::new(backend), refdata().await));
    let (status, _, body) = get_site(&s, "/search/cross-of-gold", None).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert!(
        body.contains("Reader&#39;s &amp; Co. &lt;archive&gt;"),
        "{body}"
    );
    assert!(
        !body.contains("&amp;#39;") && !body.contains("&amp;lt;"),
        "{body}"
    );
    assert!(
        !body.contains("<archive>") && !body.contains("<mark>"),
        "{body}"
    );
}

async fn get_host(state: &Arc<AppState>, host: &str, uri: &str) -> axum::response::Response {
    app(state.clone())
        .oneshot(
            Request::builder()
                .uri(uri)
                .header(header::HOST, host)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
}

#[tokio::test]
async fn www_redirects_to_the_apex() {
    let mut cfg = config();
    cfg.site_dir = Some(site_dir("www"));
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let cases = [
        ("www.usnewsmap.com", "/", "https://usnewsmap.com/"),
        (
            "WWW.usnewsmap.com:443",
            "/?q=%22cross+of+gold%22&from=1896-06-01",
            "https://usnewsmap.com/?q=%22cross+of+gold%22&from=1896-06-01",
        ),
        (
            "www.usnewsmap.com",
            "/status",
            "https://usnewsmap.com/status",
        ),
        (
            "www.usnewsmap.com",
            "/v1/meta",
            "https://usnewsmap.com/v1/meta",
        ),
    ];
    for (host, uri, location) in cases {
        let resp = get_host(&s, host, uri).await;
        assert_eq!(resp.status(), StatusCode::MOVED_PERMANENTLY, "{host}{uri}");
        assert_eq!(header_str(resp.headers(), header::LOCATION), location);
        assert_eq!(
            header_str(resp.headers(), header::X_CONTENT_TYPE_OPTIONS),
            "nosniff"
        );
        assert!(header_str(resp.headers(), header::CONTENT_SECURITY_POLICY).contains("default-src"));
        assert_eq!(
            header_str(resp.headers(), header::STRICT_TRANSPORT_SECURITY),
            "max-age=300"
        );
    }
    // HTTP/2 puts the host in the URI.
    let resp = get_site(&s, "https://www.usnewsmap.com/status?x=1", None).await;
    assert_eq!(resp.0, StatusCode::MOVED_PERMANENTLY);
    assert_eq!(
        header_str(&resp.1, header::LOCATION),
        "https://usnewsmap.com/status?x=1"
    );
    // Other hosts are served as they are.
    for host in [
        "usnewsmap.com",
        "api.usnewsmap.com",
        "www.example.com",
        "127.0.0.1:8080",
    ] {
        let resp = get_host(&s, host, "/").await;
        assert_eq!(resp.status(), StatusCode::OK, "{host}");
    }
}

#[tokio::test]
async fn without_a_site_only_the_api_answers() {
    let s = state_with(None).await;
    let (status, _, _) = get_site(&s, "/", None).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    let (status, _, _) = get_site(&s, "/v1/meta", None).await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn api_responses_are_compressed_and_cors_is_limited_to_allowed_origins() {
    let mut cfg = config();
    cfg.allowed_origins = vec!["https://usnewsmap.com".to_owned()];
    let s = Arc::new(AppState::new(
        cfg,
        Arc::new(fixture_backend()),
        refdata().await,
    ));
    let send = |origin: &'static str, encoding: Option<&'static str>| {
        let s = s.clone();
        async move {
            let mut req = Request::builder()
                .uri("/v1/meta")
                .header(header::ORIGIN, origin);
            if let Some(e) = encoding {
                req = req.header(header::ACCEPT_ENCODING, e);
            }
            app(s)
                .oneshot(req.body(Body::empty()).unwrap())
                .await
                .unwrap()
        }
    };
    let vary = |h: &axum::http::HeaderMap| {
        h.get_all(header::VARY)
            .iter()
            .map(|v| v.to_str().unwrap().to_ascii_lowercase())
            .collect::<Vec<_>>()
            .join(", ")
    };

    // Gzip when accepted, with the CORS header for an allowed origin.
    let resp = send("https://usnewsmap.com", Some("gzip")).await;
    assert_eq!(resp.status(), StatusCode::OK);
    let h = resp.headers().clone();
    assert_eq!(header_str(&h, header::CONTENT_ENCODING), "gzip");
    assert_eq!(
        header_str(&h, header::ACCESS_CONTROL_ALLOW_ORIGIN),
        "https://usnewsmap.com"
    );
    assert!(vary(&h).contains("accept-encoding"), "{}", vary(&h));
    assert!(vary(&h).contains("origin"), "{}", vary(&h));
    let gzipped = resp.into_body().collect().await.unwrap().to_bytes();
    let mut gz = flate2::read::GzDecoder::new(&gzipped[..]);
    let mut json = String::new();
    std::io::Read::read_to_string(&mut gz, &mut json).unwrap();
    let meta: Value = serde_json::from_str(&json).unwrap();
    assert_eq!(meta["index_version"], "fixture-v1");

    // Identity when compression isn't asked for, or is refused in favour of identity.
    for encoding in [None, Some("identity"), Some("gzip;q=0")] {
        let resp = send("https://usnewsmap.com", encoding).await;
        assert_eq!(resp.status(), StatusCode::OK, "{encoding:?}");
        assert_eq!(
            header_str(resp.headers(), header::CONTENT_ENCODING),
            "",
            "{encoding:?}"
        );
    }

    // Another origin gets the response but no CORS grant.
    let resp = send("https://elsewhere.example", None).await;
    assert_eq!(resp.status(), StatusCode::OK);
    assert_eq!(
        header_str(resp.headers(), header::ACCESS_CONTROL_ALLOW_ORIGIN),
        ""
    );
}
